//! Local staging and publication primitives, not a transport implementation.
//! See `docs/local-transfer-safety.md` for the trust and durability boundaries.
#![forbid(unsafe_code)]

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
compile_error!("mtp-transfer MVP supports macOS and Linux only; no unsafe fallback");

use std::ffi::OsStr;
use std::fs::{File, Metadata};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use rustix::fs::{self, AtFlags, Dir, Mode, OFlags, RenameFlags};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

pub type Result<T> = std::result::Result<T, TransferError>;

#[derive(Debug, Error)]
pub enum TransferError {
    #[error("unsafe local basename or path: {0}")]
    UnsafeName(String),
    #[error("local I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("journal JSON failed: {0}")]
    Journal(#[from] serde_json::Error),
    #[error("expected {expected} bytes, received {actual}")]
    SizeMismatch { expected: u64, actual: u64 },
    #[error("destination already exists (Skip): {0}")]
    Skipped(PathBuf),
    #[error("stage ownership or directory binding could not be proved")]
    Ownership,
    #[error("upload source changed during snapshot copy")]
    SourceChanged,
    #[error("source/payload must be a regular, singly-linked file")]
    NotRegular,
    #[error("operation not valid for task state {0:?}")]
    InvalidState(TaskState),
    #[error("invalid verified remote upload receipt: {0}")]
    InvalidReceipt(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConflictPolicy {
    /// Never touch the existing target. `commit` returns `TransferError::Skipped`.
    Skip,
    /// Try the requested name, then a fresh random basename preserving extension.
    KeepBoth,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskState {
    Staging,
    Ready,
    Publishing,
    Committed,
    Skipped,
    NeedsReview,
    Cleaned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StageKind {
    Download,
    Upload,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileIdentity {
    pub device: u64,
    pub inode: u64,
}

impl FileIdentity {
    fn of(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

/// Native metadata already verified/authorized by the worker's current call.
/// This local receipt does not prove content hashes or device durability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteUploadReceipt {
    pub storage_id: u32,
    pub object_id: u32,
    pub actual_name: String,
    pub bytes: u64,
    /// Optional canonical absolute remote path, not a local filesystem path.
    #[serde(default)]
    pub path: Option<String>,
}

impl RemoteUploadReceipt {
    fn validate(&self, expected_size: u64) -> Result<()> {
        if self.bytes != expected_size {
            return Err(TransferError::SizeMismatch {
                expected: expected_size,
                actual: self.bytes,
            });
        }
        if self.storage_id == 0
            || self.storage_id == u32::MAX
            || self.object_id == 0
            || self.object_id == u32::MAX
        {
            return Err(TransferError::InvalidReceipt(
                "storage/object ID is zero or reserved MAX".to_owned(),
            ));
        }
        validate_basename(&self.actual_name)?;
        if let Some(path) = &self.path {
            let suffix = path.strip_prefix('/').ok_or_else(|| {
                TransferError::InvalidReceipt(
                    "remote path must be canonical and absolute".to_owned(),
                )
            })?;
            // Inspect raw slash-separated components: Path::components would
            // normalize '.', duplicate slashes, etc. and hide malformed inputs.
            for component in suffix.split('/') {
                validate_basename(component).map_err(|_| {
                    TransferError::InvalidReceipt(
                        "remote path has an unsafe/noncanonical component".to_owned(),
                    )
                })?;
            }
            if suffix.rsplit('/').next() != Some(self.actual_name.as_str()) {
                return Err(TransferError::InvalidReceipt(
                    "remote path basename differs from actual_name".to_owned(),
                ));
            }
        }
        Ok(())
    }
}

/// Audit data only. Deserializing a journal does NOT grant cleanup authority.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Journal {
    pub version: u32,
    pub owner: String,
    pub kind: StageKind,
    pub state: TaskState,
    #[serde(with = "unix_path_serde")]
    pub parent_path: PathBuf,
    pub stage_name: String,
    pub payload_name: String,
    pub marker_name: String,
    pub journal_name: String,
    pub parent_identity: FileIdentity,
    pub stage_identity: FileIdentity,
    pub payload_identity: FileIdentity,
    pub marker_identity: FileIdentity,
    pub expected_size: u64,
    pub final_name: Option<String>,
    pub published_name: Option<String>,
    /// True after local publication succeeds, or an authorized worker records
    /// a verified native upload receipt. Never proof of device durability.
    /// Crash recovery must still inspect identities; JSON alone is not authority.
    pub publication_confirmed: bool,
    #[serde(default)]
    pub remote_upload: Option<RemoteUploadReceipt>,
    /// Durable intent/evidence written before native download dispatch. It does
    /// not authorize replay and may be true even if the native call never ran.
    #[serde(default)]
    pub native_dispatched: bool,
    pub failure: Option<String>,
}

// Journal audit paths preserve exact Unix bytes. They never authorize cleanup.
mod unix_path_serde {
    use std::ffi::OsString;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::path::{Path, PathBuf};

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum EncodedPath {
        Legacy(String),
        Bytes(Vec<u8>),
    }

    pub fn serialize<S: Serializer>(
        path: &Path,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        let bytes = path.as_os_str().as_bytes();
        if !path.is_absolute() || bytes.contains(&0) {
            return Err(serde::ser::Error::custom(
                "journal parent path must be absolute and contain no NUL",
            ));
        }
        bytes.serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<PathBuf, D::Error> {
        let bytes = match EncodedPath::deserialize(deserializer)? {
            EncodedPath::Legacy(text) => text.into_bytes(),
            EncodedPath::Bytes(bytes) => bytes,
        };
        if bytes.contains(&0) {
            return Err(serde::de::Error::custom("journal parent path contains NUL"));
        }
        let path = PathBuf::from(OsString::from_vec(bytes));
        if !path.is_absolute() {
            return Err(serde::de::Error::custom(
                "journal parent path must be absolute",
            ));
        }
        Ok(path)
    }
}

/// Reject separators on both platforms, NUL/control characters, dot entries,
/// and overlong UTF-8 names. No path normalization silently repairs input.
pub fn validate_basename(name: &str) -> Result<()> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.len() > 255
        || name
            .chars()
            .any(|c| c == '/' || c == '\\' || c.is_control())
    {
        return Err(TransferError::UnsafeName(name.to_owned()));
    }
    Ok(())
}

fn validate_source_basename(name: &OsStr) -> Result<()> {
    let bytes = name.as_bytes();
    if bytes.is_empty()
        || bytes == b"."
        || bytes == b".."
        || bytes.len() > 255
        || bytes
            .iter()
            .any(|b| *b == b'/' || *b == b'\\' || b.is_ascii_control())
    {
        return Err(TransferError::UnsafeName(
            "invalid local source basename bytes".to_owned(),
        ));
    }
    Ok(())
}

fn errno(error: rustix::io::Errno) -> TransferError {
    TransferError::Io(error.into())
}

fn directory_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

/// Open every path component separately; O_NOFOLLOW on only the last component
/// would still follow ancestor symlinks. Keep the resulting directory FD alive.
fn open_directory(path: &Path) -> Result<(File, PathBuf)> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut directory = File::from(fs::open("/", directory_flags(), Mode::empty()).map_err(errno)?);
    let mut normalized = PathBuf::from("/");
    for component in absolute.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => {
                directory = File::from(
                    fs::openat(&directory, name, directory_flags(), Mode::empty())
                        .map_err(errno)?,
                );
                normalized.push(name);
            }
            _ => return Err(TransferError::UnsafeName(absolute.display().to_string())),
        }
    }
    Ok((directory, normalized))
}

fn create_file(directory: &File, name: &str) -> Result<File> {
    Ok(File::from(
        fs::openat(
            directory,
            name,
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(errno)?,
    ))
}

fn open_regular(directory: &File, name: &OsStr) -> Result<File> {
    let file = File::from(
        fs::openat(
            directory,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(errno)?,
    );
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(TransferError::NotRegular);
    }
    Ok(file)
}

// Darwin/Linux Stat device/inode types differ in width and signedness. Keep
// explicit u64 casts consistent with Unix MetadataExt identity conversion;
// an individual cast can be redundant on one target but needed on another.
#[allow(clippy::unnecessary_cast)]
fn named_identity(directory: &File, name: &str) -> Result<Option<FileIdentity>> {
    match fs::statat(directory, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => Ok(Some(FileIdentity {
            device: stat.st_dev as u64,
            inode: stat.st_ino as u64,
        })),
        Err(rustix::io::Errno::NOENT) => Ok(None),
        Err(error) => Err(errno(error)),
    }
}

fn random_basename(original: &str) -> String {
    // Only a small portable extension is carried forward; the stem is never reused.
    let extension = Path::new(original)
        .extension()
        .and_then(OsStr::to_str)
        .filter(|s| !s.is_empty() && s.len() <= 32 && s.bytes().all(|b| b.is_ascii_alphanumeric()));
    let name = format!("mtp-{}", Uuid::new_v4());
    match extension {
        Some(extension) => format!("{name}.{extension}"),
        None => name,
    }
}

struct OwnedStage {
    parent: File,
    directory: File,
    payload: File,
    marker: File,
    parent_path: PathBuf,
    stage_path: PathBuf,
    payload_path: PathBuf,
    journal_path: PathBuf,
    journal: Journal,
    journal_identity: Option<FileIdentity>,
    cleaned: bool,
}

impl OwnedStage {
    fn create(
        parent_path: &Path,
        payload_name: &str,
        size: u64,
        kind: StageKind,
        final_name: Option<String>,
    ) -> Result<Self> {
        validate_basename(payload_name)?;
        let (parent, parent_path) = open_directory(parent_path)?;
        let owner = Uuid::new_v4().to_string();
        let stage_name = format!(".mtp-stage-{owner}");
        // mkdirat is exclusive; a collision is never opened or reused.
        fs::mkdirat(
            &parent,
            stage_name.as_str(),
            Mode::RUSR | Mode::WUSR | Mode::XUSR,
        )
        .map_err(errno)?;
        let directory = File::from(
            fs::openat(
                &parent,
                stage_name.as_str(),
                directory_flags(),
                Mode::empty(),
            )
            .map_err(errno)?,
        );
        let marker_name = format!(".owner-{owner}");
        let journal_name = format!(".journal-{owner}.json");
        if payload_name == marker_name || payload_name == journal_name {
            return Err(TransferError::Ownership);
        }
        let mut marker = create_file(&directory, &marker_name)?;
        marker.write_all(owner.as_bytes())?;
        marker.sync_all()?;
        let payload = create_file(&directory, payload_name)?;
        let stage_path = parent_path.join(&stage_name);
        let journal = Journal {
            version: 1,
            owner,
            kind,
            state: TaskState::Staging,
            parent_path: parent_path.clone(),
            stage_name,
            payload_name: payload_name.to_owned(),
            marker_name,
            journal_name,
            parent_identity: FileIdentity::of(&parent.metadata()?),
            stage_identity: FileIdentity::of(&directory.metadata()?),
            payload_identity: FileIdentity::of(&payload.metadata()?),
            marker_identity: FileIdentity::of(&marker.metadata()?),
            expected_size: size,
            final_name,
            published_name: None,
            publication_confirmed: false,
            remote_upload: None,
            native_dispatched: false,
            failure: None,
        };
        let mut stage = Self {
            parent,
            directory,
            payload,
            marker,
            payload_path: stage_path.join(payload_name),
            journal_path: stage_path.join(&journal.journal_name),
            parent_path,
            stage_path,
            journal,
            journal_identity: None,
            cleaned: false,
        };
        stage.save()?;
        stage.parent.sync_all()?;
        Ok(stage)
    }

    fn binding(&self) -> Result<()> {
        let (directory, _) = open_directory(&self.parent_path)?;
        if FileIdentity::of(&directory.metadata()?) != self.journal.parent_identity
            || named_identity(&self.parent, &self.journal.stage_name)?
                != Some(self.journal.stage_identity)
            || FileIdentity::of(&self.directory.metadata()?) != self.journal.stage_identity
        {
            return Err(TransferError::Ownership);
        }
        Ok(())
    }

    fn ownership(&mut self) -> Result<()> {
        self.binding()?;
        if named_identity(&self.directory, &self.journal.marker_name)?
            != Some(self.journal.marker_identity)
            || FileIdentity::of(&self.marker.metadata()?) != self.journal.marker_identity
        {
            return Err(TransferError::Ownership);
        }
        self.marker.seek(SeekFrom::Start(0))?;
        let mut marker = String::new();
        (&mut self.marker).take(128).read_to_string(&mut marker)?;
        if marker != self.journal.owner {
            return Err(TransferError::Ownership);
        }
        Ok(())
    }

    fn validate_payload(&mut self) -> Result<()> {
        self.ownership()?;
        if named_identity(&self.directory, &self.journal.payload_name)?
            != Some(self.journal.payload_identity)
            || FileIdentity::of(&self.payload.metadata()?) != self.journal.payload_identity
        {
            return Err(TransferError::Ownership);
        }
        let metadata = self.payload.metadata()?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(TransferError::NotRegular);
        }
        if metadata.len() != self.journal.expected_size {
            return Err(TransferError::SizeMismatch {
                expected: self.journal.expected_size,
                actual: metadata.len(),
            });
        }
        self.payload.sync_all()?;
        Ok(())
    }

    fn save(&mut self) -> Result<()> {
        self.ownership()?;
        if named_identity(&self.directory, &self.journal.journal_name)? != self.journal_identity {
            return Err(TransferError::Ownership);
        }
        let temporary = format!(".journal-save-{}", Uuid::new_v4());
        let mut file = create_file(&self.directory, &temporary)?;
        let identity = FileIdentity::of(&file.metadata()?);
        let bytes = serde_json::to_vec_pretty(&self.journal)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        if self.journal_identity.is_none() {
            fs::renameat_with(
                &self.directory,
                temporary.as_str(),
                &self.directory,
                self.journal.journal_name.as_str(),
                RenameFlags::NOREPLACE,
            )
            .map_err(errno)?;
        } else {
            // This replaces ONLY this live stage's proved-owned journal, never a
            // destination file. The directory is private and not shared with writers.
            fs::renameat(
                &self.directory,
                temporary.as_str(),
                &self.directory,
                self.journal.journal_name.as_str(),
            )
            .map_err(errno)?;
        }
        self.journal_identity = Some(identity);
        self.directory.sync_all()?;
        Ok(())
    }

    fn failure(&mut self, error: &TransferError) {
        self.journal.state = TaskState::NeedsReview;
        self.journal.failure = Some(error.to_string());
        // Preserve original error. A journal I/O/ownership failure must never cause
        // deletion, and may prevent persisting NeedsReview; live state still reflects it.
        let _ = self.save();
    }

    fn cleanup(&mut self) -> Result<()> {
        if self.cleaned {
            return Ok(());
        }
        self.ownership()?;
        if named_identity(&self.directory, &self.journal.journal_name)? != self.journal_identity {
            return Err(TransferError::Ownership);
        }
        let payload_identity = named_identity(&self.directory, &self.journal.payload_name)?;
        if payload_identity.is_some() && payload_identity != Some(self.journal.payload_identity) {
            return Err(TransferError::Ownership);
        }
        // Never recurse. Reject unknown children BEFORE removing ownership evidence.
        for entry in Dir::read_from(&self.directory).map_err(errno)? {
            let entry = entry.map_err(errno)?;
            let name = entry.file_name().to_bytes();
            if name != b"."
                && name != b".."
                && name != self.journal.payload_name.as_bytes()
                && name != self.journal.marker_name.as_bytes()
                && name != self.journal.journal_name.as_bytes()
            {
                return Err(TransferError::Ownership);
            }
        }
        if payload_identity.is_some() {
            fs::unlinkat(
                &self.directory,
                self.journal.payload_name.as_str(),
                AtFlags::empty(),
            )
            .map_err(errno)?;
        }
        // Journals are deleted only with the stage upon an explicit cleanup call.
        fs::unlinkat(
            &self.directory,
            self.journal.journal_name.as_str(),
            AtFlags::empty(),
        )
        .map_err(errno)?;
        fs::unlinkat(
            &self.directory,
            self.journal.marker_name.as_str(),
            AtFlags::empty(),
        )
        .map_err(errno)?;
        fs::unlinkat(
            &self.parent,
            self.journal.stage_name.as_str(),
            AtFlags::REMOVEDIR,
        )
        .map_err(errno)?;
        self.parent.sync_all()?;
        self.journal.state = TaskState::Cleaned;
        self.cleaned = true;
        Ok(())
    }
}

impl Drop for OwnedStage {
    fn drop(&mut self) {
        if !self.cleaned
            && matches!(
                self.journal.state,
                TaskState::Staging | TaskState::Ready | TaskState::Publishing
            )
        {
            self.failure(&TransferError::InvalidState(self.journal.state));
        }
        // Intentionally no automatic file/directory cleanup, including failures.
    }
}

/// A private stage inside a bound destination directory, hence on the same FS.
/// The payload basename matches `final_name` for single-file Kalam downloads.
pub struct DownloadStage {
    stage: OwnedStage,
    policy: ConflictPolicy,
}

impl DownloadStage {
    pub fn new(destination: &Path, final_name: &str, expected_size: u64) -> Result<Self> {
        Self::with_policy(destination, final_name, expected_size, ConflictPolicy::Skip)
    }

    pub fn with_policy(
        destination: &Path,
        final_name: &str,
        expected_size: u64,
        policy: ConflictPolicy,
    ) -> Result<Self> {
        validate_basename(final_name)?;
        Ok(Self {
            stage: OwnedStage::create(
                destination,
                final_name,
                expected_size,
                StageKind::Download,
                Some(final_name.to_owned()),
            )?,
            policy,
        })
    }

    /// Borrow the already-created payload FD for safe transport bindings.
    /// Native code must duplicate it, close only its duplicate, and finish all
    /// writes before commit. Never reopen payload_path through ambient paths.
    pub fn payload_file(&self) -> &File {
        &self.stage.payload
    }

    /// Informational/legacy compatibility only; safe workers must use payload_file.
    pub fn payload_path(&self) -> &Path {
        &self.stage.payload_path
    }
    pub fn staging_directory(&self) -> &Path {
        &self.stage.stage_path
    }
    pub fn safe_name(&self) -> &str {
        &self.stage.journal.payload_name
    }
    pub fn journal_path(&self) -> &Path {
        &self.stage.journal_path
    }
    pub fn journal(&self) -> &Journal {
        &self.stage.journal
    }

    /// Persist dispatch intent before entering native/USB code. On success the
    /// state stays Staging so commit remains valid; SIGKILL leaves this evidence.
    /// Never call native code if this fails, and never replay a marked stage.
    pub fn mark_dispatched(&mut self) -> Result<()> {
        if self.stage.journal.state != TaskState::Staging || self.stage.journal.native_dispatched {
            return Err(TransferError::InvalidState(self.stage.journal.state));
        }
        self.stage.journal.native_dispatched = true;
        let result = self.stage.save();
        if let Err(error) = &result {
            self.stage.failure(error);
        }
        result
    }

    /// The backend must have closed all payload writers before calling commit.
    /// Default Skip is an explicit error, never false success using old bytes.
    /// Publication can succeed before journal/fsync fails: NeedsReview plus
    /// `journal().published_name` tells callers NEVER to retry/delete the target.
    pub fn commit(&mut self) -> Result<PathBuf> {
        if self.stage.journal.state != TaskState::Staging {
            return Err(TransferError::InvalidState(self.stage.journal.state));
        }
        let result = self.commit_inner();
        if let Err(error) = &result {
            if matches!(error, TransferError::Skipped(_)) {
                self.stage.journal.state = TaskState::Skipped;
                self.stage.journal.failure = Some(error.to_string());
                if let Err(save_error) = self.stage.save() {
                    self.stage.failure(&save_error);
                }
            } else {
                self.stage.failure(error);
            }
        }
        result
    }

    fn commit_inner(&mut self) -> Result<PathBuf> {
        self.stage.validate_payload()?;
        let mut candidate = self.stage.journal.payload_name.clone();
        loop {
            self.stage.journal.state = TaskState::Publishing;
            // Persist the candidate before the atomic syscall for crash review.
            self.stage.journal.published_name = Some(candidate.clone());
            self.stage.save()?;
            self.stage.validate_payload()?;
            match fs::renameat_with(
                &self.stage.directory,
                self.stage.journal.payload_name.as_str(),
                &self.stage.parent,
                candidate.as_str(),
                RenameFlags::NOREPLACE,
            ) {
                Ok(()) => break,
                Err(rustix::io::Errno::EXIST) => {
                    self.stage.journal.published_name = None;
                    if self.policy == ConflictPolicy::Skip {
                        return Err(TransferError::Skipped(
                            self.stage.parent_path.join(candidate),
                        ));
                    }
                    candidate = random_basename(&self.stage.journal.payload_name);
                }
                Err(error) => return Err(errno(error)),
            }
        }
        self.stage.journal.publication_confirmed = true;
        self.stage.parent.sync_all()?;
        self.stage.directory.sync_all()?;
        self.stage.binding()?;
        self.stage.journal.state = TaskState::Committed;
        self.stage.journal.failure = None;
        self.stage.save()?;
        Ok(self.stage.parent_path.join(candidate))
    }

    /// Retain a cancellation/backend failure for review. Does not delete files.
    pub fn mark_needs_review(&mut self, reason: &str) -> Result<()> {
        if matches!(
            self.stage.journal.state,
            TaskState::Committed | TaskState::Cleaned
        ) {
            return Err(TransferError::InvalidState(self.stage.journal.state));
        }
        self.stage.journal.state = TaskState::NeedsReview;
        self.stage.journal.failure = Some(reason.to_owned());
        self.stage.save()
    }

    /// Explicitly discard only the owned stage; NEVER unlink a published target.
    pub fn cleanup_owned(&mut self) -> Result<()> {
        let result = self.stage.cleanup();
        if let Err(error) = &result {
            self.stage.failure(error);
        }
        result
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SourceSnapshot {
    identity: FileIdentity,
    size: u64,
    modified: SystemTime,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl SourceSnapshot {
    fn of(metadata: &Metadata) -> Result<Self> {
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(TransferError::NotRegular);
        }
        Ok(Self {
            identity: FileIdentity::of(metadata),
            size: metadata.len(),
            modified: metadata.modified()?,
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        })
    }

    fn matches(&self, metadata: &Metadata) -> Result<bool> {
        // A source that was valid at construction may now be unlinked (nlink=0)
        // or hard-linked (nlink>1). Those are changes, not initial NotRegular.
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Ok(false);
        }
        Ok(Self::of(metadata)? == *self)
    }
}

fn source_recheck_error(error: TransferError) -> TransferError {
    match error {
        TransferError::NotRegular => TransferError::SourceChanged,
        TransferError::Io(ref io_error)
            if matches!(
                io_error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) || io_error.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error()) =>
        {
            TransferError::SourceChanged
        }
        other => other,
    }
}

/// An immutable-by-contract local upload snapshot with a random remote-safe name.
/// No device operations or remote rename/publication are performed by this crate.
pub struct UploadStage {
    stage: OwnedStage,
    source: File,
    source_parent: File,
    source_name: std::ffi::OsString,
    source_path: PathBuf,
    snapshot: SourceSnapshot,
}

impl UploadStage {
    pub fn new(source: &Path, app_cache: &Path) -> Result<Self> {
        let source_name = source
            .file_name()
            .ok_or_else(|| TransferError::UnsafeName(source.display().to_string()))?
            .to_owned();
        validate_source_basename(&source_name)?;
        let (source_parent, parent_path) =
            open_directory(source.parent().unwrap_or_else(|| Path::new(".")))?;
        let mut source_file = open_regular(&source_parent, &source_name)?;
        let snapshot = SourceSnapshot::of(&source_file.metadata()?)?;
        // UTF-8 is only an optional extension hint, never used to rebuild I/O paths.
        let payload_name = random_basename(source_name.to_str().unwrap_or("snapshot"));
        let mut stage = OwnedStage::create(
            app_cache,
            &payload_name,
            snapshot.size,
            StageKind::Upload,
            None,
        )?;
        let copy_result = (|| -> Result<()> {
            // Bounded copy: a growing source cannot make the snapshot grow unbounded.
            // io::copy uses a bounded buffer; limit reads to the recorded size.
            let copied = io::copy(
                &mut (&mut source_file).take(snapshot.size),
                &mut stage.payload,
            )?;
            let mut extra = [0u8; 1];
            if copied != snapshot.size
                || source_file.read(&mut extra)? != 0
                || !snapshot.matches(&source_file.metadata()?)?
            {
                return Err(TransferError::SourceChanged);
            }
            let current =
                open_regular(&source_parent, &source_name).map_err(source_recheck_error)?;
            if !snapshot.matches(&current.metadata()?)? {
                return Err(TransferError::SourceChanged);
            }
            stage.validate_payload()?;
            stage.payload.seek(SeekFrom::Start(0))?;
            stage.journal.state = TaskState::Ready;
            stage.save()?;
            Ok(())
        })();
        if let Err(error) = copy_result {
            stage.failure(&error);
            return Err(error);
        }
        Ok(Self {
            stage,
            source: source_file,
            source_parent,
            source_name,
            source_path: parent_path.join(source.file_name().ok_or(TransferError::Ownership)?),
            snapshot,
        })
    }

    /// Borrow the snapshot's existing FD for safe transport bindings.
    /// Native code must duplicate it and close only its duplicate. Duplicates
    /// share offsets; explicitly seek to zero (or use positioned reads) before
    /// upload and do not write/truncate this snapshot.
    pub fn staged_file(&self) -> &File {
        &self.stage.payload
    }

    /// Informational/legacy compatibility only; safe workers must use staged_file.
    pub fn staged_path(&self) -> &Path {
        &self.stage.payload_path
    }
    pub fn staging_directory(&self) -> &Path {
        &self.stage.stage_path
    }
    pub fn safe_name(&self) -> &str {
        &self.stage.journal.payload_name
    }
    pub fn file_size(&self) -> u64 {
        self.stage.journal.expected_size
    }
    pub fn journal_path(&self) -> &Path {
        &self.stage.journal_path
    }
    pub fn journal(&self) -> &Journal {
        &self.stage.journal
    }

    /// Record metadata from the worker's verified current native upload call.
    /// The worker MUST persist dispatch-pending NeedsReview before USB work and
    /// verify the receipt against that call; this type alone is not authorization.
    /// No source recheck is required: the immutable captured snapshot was sent.
    /// On save/ownership failure retain the receipt and possible-publication
    /// evidence in live NeedsReview state; do not retry upload or delete objects.
    pub fn mark_uploaded(&mut self, receipt: RemoteUploadReceipt) -> Result<()> {
        if !matches!(
            self.stage.journal.state,
            TaskState::Ready | TaskState::NeedsReview
        ) || self.stage.journal.publication_confirmed
            || self.stage.journal.remote_upload.is_some()
            || self.stage.cleaned
        {
            return Err(TransferError::InvalidState(self.stage.journal.state));
        }
        if let Err(error) = receipt.validate(self.file_size()) {
            self.stage.failure(&error);
            return Err(error);
        }
        self.stage.journal.publication_confirmed = true;
        self.stage.journal.published_name = Some(receipt.actual_name.clone());
        self.stage.journal.remote_upload = Some(receipt);
        let result = (|| -> Result<()> {
            self.stage.validate_payload()?;
            self.stage.journal.state = TaskState::Committed;
            self.stage.journal.failure = None;
            self.stage.save()
        })();
        if let Err(error) = &result {
            self.stage.failure(error);
        }
        result
    }

    /// Optional pre-dispatch source recheck; detects inode/size/mtime/ctime
    /// changes, not hashes. After committed upload, an error is only a warning
    /// and never downgrades the receipt: captured snapshot bytes were uploaded.
    pub fn check_source_unchanged(&mut self) -> Result<()> {
        if self.stage.cleaned {
            return Err(TransferError::InvalidState(TaskState::Cleaned));
        }
        let result = (|| -> Result<()> {
            let current = open_regular(&self.source_parent, &self.source_name)
                .map_err(source_recheck_error)?;
            let (current_parent, _) =
                open_directory(self.source_path.parent().ok_or(TransferError::Ownership)?)
                    .map_err(source_recheck_error)?;
            if FileIdentity::of(&current_parent.metadata()?)
                != FileIdentity::of(&self.source_parent.metadata()?)
                || !self.snapshot.matches(&self.source.metadata()?)?
                || !self.snapshot.matches(&current.metadata()?)?
            {
                return Err(TransferError::SourceChanged);
            }
            Ok(())
        })();
        if let Err(error) = &result
            && self.stage.journal.state != TaskState::Committed
        {
            self.stage.failure(error);
        }
        result
    }

    /// Verify snapshot ownership/size and sync again before transport reads it.
    pub fn validate_staged(&mut self) -> Result<()> {
        if !matches!(
            self.stage.journal.state,
            TaskState::Ready | TaskState::NeedsReview
        ) || self.stage.journal.publication_confirmed
            || self.stage.cleaned
        {
            return Err(TransferError::InvalidState(self.stage.journal.state));
        }
        let result = self.stage.validate_payload();
        if let Err(error) = &result {
            self.stage.failure(error);
        }
        result
    }

    pub fn mark_needs_review(&mut self, reason: &str) -> Result<()> {
        if self.stage.cleaned
            || matches!(
                self.stage.journal.state,
                TaskState::Committed | TaskState::Skipped
            )
        {
            return Err(TransferError::InvalidState(self.stage.journal.state));
        }
        self.stage.journal.state = TaskState::NeedsReview;
        self.stage.journal.failure = Some(reason.to_owned());
        self.stage.save()
    }

    pub fn cleanup_owned(&mut self) -> Result<()> {
        let result = self.stage.cleanup();
        if let Err(error) = &result {
            self.stage.failure(error);
        }
        result
    }
}

#[cfg(test)]
mod tests;
