//! Restricted live-device acceptance. Default is read-only; no delete/mkdir/retry
//! remote API exists here. Write mode uses generated files only in storage 65537
//! /tmp and preserves all phone objects and private local evidence on every exit.
#![forbid(unsafe_code)]
use mtp_application::{
    ApplicationError, BackendConfig, FileConflictPolicy, LocalPath, RemoteEntry, SessionInfo,
    TransferDirection, TransferReceipt, WorkerClient,
};
use mtp_domain::{RemoteDirectoryRef, RemoteFilename, StorageId};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    error::Error,
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Component, Path, PathBuf},
    process::ExitCode,
};

const STORAGE: u32 = 65537;
const REMOTE: &str = "/tmp";
const BLOB_BYTES: usize = 128 * 1024;
const HELP: &str = "Usage: mtp-acceptance --library <absolute-vetted-library> --worker <absolute-worker> --storage-id 65537 --remote-directory /tmp --local-cache <absolute-project>/.cache/device-acceptance [--read-only | --write-confirmed] [--verify-report <absolute-project-cache/run-32hex/results.json>]\nDefault: --read-only (no local run files and no phone writes).\n--write-confirmed is only for separately authorized generated-file acceptance in existing /tmp; no root fallback, phone mkdir, deletion, automatic retry, or existing-user-file downloads.";
type Result<T> = std::result::Result<T, Failure>;

#[derive(Debug)]
struct Failure {
    class: String,
    message: &'static str,
    requires_review: bool,
    journal_path: Option<LocalPath>,
}
impl Failure {
    fn denied(message: &'static str) -> Self {
        Self {
            class: "guard_rejected".into(),
            message,
            requires_review: false,
            journal_path: None,
        }
    }
    fn verify(message: &'static str) -> Self {
        Self {
            class: "verification_failed".into(),
            message,
            requires_review: true,
            journal_path: None,
        }
    }
    fn report(&self) -> Value {
        json!({"class":self.class,"message":self.message,"requiresReview":self.requires_review,"journalPath":self.journal_path})
    }
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.class, self.message)
    }
}
impl Error for Failure {}
impl From<io::Error> for Failure {
    fn from(_: io::Error) -> Self {
        Self {
            class: "local_io".into(),
            message: "local I/O failed; retained evidence must not be deleted",
            requires_review: false,
            journal_path: None,
        }
    }
}
impl From<serde_json::Error> for Failure {
    fn from(_: serde_json::Error) -> Self {
        Self {
            class: "local_json".into(),
            message: "local report or journal encoding failed",
            requires_review: false,
            journal_path: None,
        }
    }
}
impl From<ApplicationError> for Failure {
    fn from(error: ApplicationError) -> Self {
        match error {
            ApplicationError::UnknownOutcome { journal_path } => Self {
                class: "unknown_outcome".into(),
                message: "RequiresReview: do not retry or delete phone/local objects",
                requires_review: true,
                journal_path,
            },
            ApplicationError::Backend {
                code,
                message,
                requires_review,
            } => Self {
                class: safe_backend_category(&message).map_or_else(
                    || format!("backend_{code:?}"),
                    |category| format!("backend_{category}"),
                ),
                message: "backend rejected the operation; native details withheld",
                requires_review,
                journal_path: None,
            },
            _ => Self {
                class: "application_failure".into(),
                message: "worker/session operation failed; no automatic retry",
                requires_review: false,
                journal_path: None,
            },
        }
    }
}

/// Re-whitelist exact worker literals; never derive a class from a raw native
/// type/message or a prefix that may include driver paths, serials, or data.
fn safe_backend_category(message: &str) -> Option<&'static str> {
    match message {
        "native failure: ErrorMtpDetectFailed" => Some("ErrorMtpDetectFailed"),
        "native failure: ErrorMtpLockExists" => Some("ErrorMtpLockExists"),
        "native failure: ErrorDeviceChanged" => Some("ErrorDeviceChanged"),
        "native failure: ErrorDeviceSetup" => Some("ErrorDeviceSetup"),
        "native failure: ErrorMultipleDevice" => Some("ErrorMultipleDevice"),
        "native failure: ErrorAllowStorageAccess" => Some("ErrorAllowStorageAccess"),
        "native failure: ErrorDeviceLocked" => Some("ErrorDeviceLocked"),
        "native failure: ErrorDeviceInfo" => Some("ErrorDeviceInfo"),
        "native failure: ErrorStorageInfo" => Some("ErrorStorageInfo"),
        "native failure: ErrorNoStorage" => Some("ErrorNoStorage"),
        "native failure: ErrorStorageFull" => Some("ErrorStorageFull"),
        "native failure: ErrorListDirectory" => Some("ErrorListDirectory"),
        "native failure: ErrorFileNotFound" => Some("ErrorFileNotFound"),
        "native failure: ErrorFilePermission" => Some("ErrorFilePermission"),
        "native failure: ErrorLocalFileRead" => Some("ErrorLocalFileRead"),
        "native failure: ErrorInvalidPath" => Some("ErrorInvalidPath"),
        "native failure: ErrorFileTransfer" => Some("ErrorFileTransfer"),
        "native failure: ErrorFileObjectRead" => Some("ErrorFileObjectRead"),
        "native failure: ErrorSendObject" => Some("ErrorSendObject"),
        "native failure: ErrorGeneral" => Some("ErrorGeneral"),
        "native failure: ErrorNeedsReview" => Some("ErrorNeedsReview"),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    ReadOnly,
    WriteConfirmed,
}
#[derive(Debug)]
struct Config {
    library: PathBuf,
    worker: PathBuf,
    storage: u32,
    remote: String,
    cache: PathBuf,
    mode: Mode,
    verify_report: Option<PathBuf>,
}
fn project_cache() -> Result<PathBuf> {
    let project = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| Failure::denied("cannot locate this project"))?;
    Ok(fs::canonicalize(project)?.join(".cache/device-acceptance"))
}
fn absolute(value: OsString) -> Result<PathBuf> {
    let path = PathBuf::from(value);
    LocalPath::from_path(&path)
        .map_err(|_| Failure::denied("all local paths must be absolute and contain no NUL"))?;
    Ok(path)
}
fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Option<Config>> {
    let mut args = args.into_iter().peekable();
    if args.peek().is_some_and(|arg| arg == "--help") {
        args.next();
        if args.next().is_some() {
            return Err(Failure::denied(
                "--help cannot be combined with other arguments",
            ));
        }
        return Ok(None);
    }
    let mut library = None;
    let mut worker = None;
    let mut storage = None;
    let mut remote = None;
    let mut cache = None;
    let mut mode = None;
    let mut verify_report = None;
    while let Some(flag) = args.next() {
        if flag == "--read-only" || flag == "--write-confirmed" {
            if mode.is_some() {
                return Err(Failure::denied("select one mode exactly once"));
            }
            mode = Some(if flag == "--write-confirmed" {
                Mode::WriteConfirmed
            } else {
                Mode::ReadOnly
            });
            continue;
        }
        let value = args
            .next()
            .ok_or_else(|| Failure::denied("option value missing"))?;
        match flag.to_str() {
            Some("--library") if library.is_none() => library = Some(absolute(value)?),
            Some("--worker") if worker.is_none() => worker = Some(absolute(value)?),
            Some("--storage-id") if storage.is_none() => {
                storage = Some(
                    value
                        .to_str()
                        .and_then(|value| value.parse::<u32>().ok())
                        .ok_or_else(|| Failure::denied("storage ID must be a u32"))?,
                )
            }
            Some("--remote-directory") if remote.is_none() => {
                remote =
                    Some(value.into_string().map_err(|_| {
                        Failure::denied("remote directory must be representable UTF-8")
                    })?)
            }
            Some("--local-cache") if cache.is_none() => cache = Some(absolute(value)?),
            Some("--verify-report") if verify_report.is_none() => {
                verify_report = Some(absolute(value)?)
            }
            _ => return Err(Failure::denied("unknown or repeated option")),
        }
    }
    let config = Config {
        library: library.ok_or_else(|| Failure::denied("explicit vetted library required"))?,
        worker: worker.ok_or_else(|| Failure::denied("explicit worker required"))?,
        storage: storage.ok_or_else(|| Failure::denied("explicit storage ID required"))?,
        remote: remote.ok_or_else(|| Failure::denied("explicit remote directory required"))?,
        cache: cache.ok_or_else(|| Failure::denied("explicit project-local cache required"))?,
        mode: mode.unwrap_or(Mode::ReadOnly),
        verify_report,
    };
    if config.storage != STORAGE || config.remote != REMOTE {
        return Err(Failure::denied(
            "authorization is limited to storage 65537 and exact existing /tmp",
        ));
    }
    if config.cache != project_cache()? {
        return Err(Failure::denied(
            "local cache must be this project's .cache/device-acceptance",
        ));
    }
    if let Some(path) = &config.verify_report {
        if config.mode != Mode::ReadOnly {
            return Err(Failure::denied(
                "--verify-report is read-only audit data, never write/retry authority",
            ));
        }
        validate_report_path(path, &config.cache)?;
    }
    Ok(Some(config))
}

fn exact_directory(entries: &[RemoteEntry], name: &str) -> Result<RemoteDirectoryRef> {
    let mut matches = entries.iter().filter(|entry| entry.name == name);
    let entry = matches.next().ok_or_else(|| {
        Failure::denied("authorized directory does not exist; never fall back to root or create it")
    })?;
    if matches.next().is_some() || !entry.directory {
        return Err(Failure::denied(
            "authorized target is not one unambiguous directory",
        ));
    }
    Ok(RemoteDirectoryRef::from(entry.reference.clone()))
}
struct Target {
    directory: RemoteDirectoryRef,
    root_count: usize,
    entries: Vec<RemoteEntry>,
}
fn locate(client: &mut WorkerClient, session: &SessionInfo, config: &Config) -> Result<Target> {
    println!("PHASE=storages");
    let storages = client.storages()?;
    if !storages.iter().any(|storage| storage.id == config.storage) {
        return Err(Failure::denied("authorized current storage is unavailable"));
    }
    println!("PHASE=root");
    let root = client.list(&session.root(StorageId(config.storage)))?;
    // Config permits only canonical /tmp; traverse its component by cached
    // ObjectId reference, never a displayed path string passed to a write call.
    println!("PHASE=resolve_tmp");
    let directory = exact_directory(&root, "tmp")?;
    println!("PHASE=tmplist");
    let entries = client.list(&directory)?;
    Ok(Target {
        directory,
        root_count: root.len(),
        entries,
    })
}
fn metadata(entries: &[RemoteEntry]) -> BTreeMap<u32, Value> {
    entries
        .iter()
        .map(|entry| {
            (
                entry.reference.object().0,
                json!({"name":entry.name,"directory":entry.directory,"bytes":entry.byte_size}),
            )
        })
        .collect()
}
fn baseline_preserved(before: &BTreeMap<u32, Value>, after: &BTreeMap<u32, Value>) -> bool {
    before
        .iter()
        .all(|(id, value)| after.get(id) == Some(value))
}
const MAX_REPORT_BYTES: u64 = 4 * 1024 * 1024;

fn validate_report_path(path: &Path, cache: &Path) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    LocalPath::from_path(path)
        .map_err(|_| Failure::denied("verification report must be absolute without NUL"))?;
    // Path::components normalizes '.' and duplicate separators, so validate raw
    // bytes first. No lossy conversion or silently normalized alias is accepted.
    if path
        .as_os_str()
        .as_bytes()
        .split(|byte| *byte == b'/')
        .skip(1)
        .any(|component| component.is_empty() || component == b"." || component == b"..")
    {
        return Err(Failure::denied(
            "verification report path contains non-normal components",
        ));
    }
    let relative = path.strip_prefix(cache).map_err(|_| {
        Failure::denied("verification report is outside the project acceptance cache")
    })?;
    let mut components = relative.components();
    let Some(Component::Normal(run)) = components.next() else {
        return Err(Failure::denied(
            "verification report has no owned run directory",
        ));
    };
    let run = run
        .to_str()
        .and_then(|name| name.strip_prefix("run-"))
        .ok_or_else(|| {
            Failure::denied("verification report run directory is not a generated run ID")
        })?;
    if run.len() != 32
        || !run
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || components.next() != Some(Component::Normal(std::ffi::OsStr::new("results.json")))
        || components.next().is_some()
    {
        return Err(Failure::denied(
            "verification report must be exactly cache/run-32-lowercase-hex/results.json",
        ));
    }
    Ok(())
}

struct HistoricalBaseline {
    entries: BTreeMap<u32, Value>,
    needs_review: bool,
}
fn parse_historical_baseline(value: &Value) -> Result<HistoricalBaseline> {
    if value["version"].as_u64() != Some(1)
        || value["mode"].as_str() != Some("write_confirmed")
        || value["storageId"].as_u64() != Some(u64::from(STORAGE))
        || value["remoteDirectory"].as_str() != Some(REMOTE)
    {
        return Err(Failure::denied(
            "historical report does not have the expected version/storage/directory scope",
        ));
    }
    let state = value["state"]
        .as_str()
        .filter(|state| {
            matches!(
                *state,
                "Prepared" | "Running" | "Passed" | "Failed" | "NeedsReview"
            )
        })
        .ok_or_else(|| Failure::denied("historical report has an unknown state"))?;
    let raw = value["baselineMetadata"]
        .as_object()
        .ok_or_else(|| Failure::denied("historical report has no baseline metadata map"))?;
    let mut entries = BTreeMap::new();
    for (key, value) in raw {
        let id = key
            .parse::<u32>()
            .ok()
            .filter(|id| *id != 0 && *id != u32::MAX && id.to_string() == *key)
            .ok_or_else(|| Failure::denied("historical baseline object ID is not canonical"))?;
        let item = value
            .as_object()
            .filter(|item| item.len() == 3)
            .ok_or_else(|| Failure::denied("historical baseline metadata shape is invalid"))?;
        if item.get("name").and_then(Value::as_str).is_none()
            || item.get("directory").and_then(Value::as_bool).is_none()
            || !item
                .get("bytes")
                .is_some_and(|bytes| bytes.is_null() || bytes.as_u64().is_some())
        {
            return Err(Failure::denied(
                "historical baseline name/kind/size metadata is invalid",
            ));
        }
        entries.insert(id, value.clone());
    }
    Ok(HistoricalBaseline {
        entries,
        needs_review: state == "NeedsReview",
    })
}

fn read_historical_baseline(path: &Path, cache: &Path) -> Result<HistoricalBaseline> {
    validate_report_path(path, cache)?;
    let mut ancestors = Vec::new();
    let mut current = PathBuf::from("/");
    for component in path
        .parent()
        .ok_or_else(|| Failure::denied("verification report parent missing"))?
        .components()
    {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => current.push(name),
            _ => return Err(Failure::denied("non-normal report ancestor")),
        }
        let info = fs::symlink_metadata(&current)?;
        if !info.is_dir() || info.file_type().is_symlink() {
            return Err(Failure::denied(
                "verification report ancestor is a symlink or non-directory",
            ));
        }
        ancestors.push((current.clone(), info.dev(), info.ino()));
    }
    let before = fs::symlink_metadata(path)?;
    if !before.is_file()
        || before.file_type().is_symlink()
        || before.nlink() != 1
        || before.len() > MAX_REPORT_BYTES
    {
        return Err(Failure::denied(
            "verification report must be one bounded regular non-symlink file",
        ));
    }
    // These are the inspected Darwin/Linux O_NOFOLLOW ABI constants passed
    // through safe std OpenOptionsExt; no unsafe FFI or additional dependency.
    #[cfg(target_os = "macos")]
    const NOFOLLOW: i32 = 0x0100;
    #[cfg(target_os = "linux")]
    const NOFOLLOW: i32 = 0x20000;
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    const NOFOLLOW: i32 = 0;
    if NOFOLLOW == 0 {
        return Err(Failure::denied(
            "verification report NOFOLLOW is unsupported on this platform",
        ));
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(NOFOLLOW)
        .open(path)?;
    let opened = file.metadata()?;
    if !opened.is_file()
        || opened.nlink() != 1
        || (opened.dev(), opened.ino(), opened.len()) != (before.dev(), before.ino(), before.len())
    {
        return Err(Failure::denied(
            "verification report identity changed before reading",
        ));
    }
    // Recheck ancestors before reading any body; report/audit input remains
    // inside the trusted app-owned cache, not a path to payloads or user files.
    for (ancestor, device, inode) in &ancestors {
        let info = fs::symlink_metadata(ancestor)?;
        if !info.is_dir()
            || info.file_type().is_symlink()
            || (info.dev(), info.ino()) != (*device, *inode)
        {
            return Err(Failure::denied(
                "verification report ancestor identity changed",
            ));
        }
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_REPORT_BYTES + 1)
        .read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    let linked = fs::symlink_metadata(path)?;
    if bytes.len() as u64 > MAX_REPORT_BYTES
        || bytes.len() as u64 != opened.len()
        || after.nlink() != 1
        || (
            after.dev(),
            after.ino(),
            after.len(),
            after.mtime(),
            after.mtime_nsec(),
            after.ctime(),
            after.ctime_nsec(),
        ) != (
            opened.dev(),
            opened.ino(),
            opened.len(),
            opened.mtime(),
            opened.mtime_nsec(),
            opened.ctime(),
            opened.ctime_nsec(),
        )
        || !linked.is_file()
        || linked.file_type().is_symlink()
        || (linked.dev(), linked.ino()) != (opened.dev(), opened.ino())
    {
        return Err(Failure::denied(
            "verification report identity/content metadata changed during reading",
        ));
    }
    for (ancestor, device, inode) in &ancestors {
        let info = fs::symlink_metadata(ancestor)?;
        if !info.is_dir()
            || info.file_type().is_symlink()
            || (info.dev(), info.ino()) != (*device, *inode)
        {
            return Err(Failure::denied(
                "verification report ancestor changed during reading",
            ));
        }
    }
    parse_historical_baseline(&serde_json::from_slice::<Value>(&bytes)?)
}

/// Audit comparison only: this never grants retry, cleanup, or upload authority.
fn baseline_summary(historical: &HistoricalBaseline, current: &BTreeMap<u32, Value>) -> Value {
    json!({
        "allBaselineIDsNameSizeKindPreserved": baseline_preserved(&historical.entries, current),
        "baselineObjectCount": historical.entries.len(),
        "currentObjectCount": current.len(),
        "extraObjectCount": current.keys().filter(|id| !historical.entries.contains_key(*id)).count(),
        "missingBaselineObjectCount": historical.entries.keys().filter(|id| !current.contains_key(*id)).count(),
        "historicalNeedsReviewUnchanged": historical.needs_review,
        "historicalReportNotModified": true,
        "reportReadWithNoFollowAndIdentityChecks": true,
        "authorizesRetryOrCleanup": false,
        "userFileContentsRead": false,
        "storageId": STORAGE, "remoteDirectory": REMOTE
    })
}

fn receipt_value(receipt: &TransferReceipt) -> Value {
    json!({"direction":receipt.direction,"actualName":receipt.actual_name,"bytes":receipt.bytes,"objectId":receipt.object_id,"localPath":receipt.local_path,"journalPath":receipt.journal_path})
}
fn random_id() -> Result<String> {
    let mut bytes = [0; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
fn private_directory(path: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    builder.create(path)?;
    File::open(path)?.sync_all()?;
    Ok(())
}
fn ensure_cache(path: &Path) -> Result<()> {
    let mut current = PathBuf::from("/");
    for component in path.components() {
        match component {
            Component::RootDir => continue,
            Component::Normal(name) => current.push(name),
            _ => {
                return Err(Failure::denied(
                    "cache path must be canonical without traversal",
                ));
            }
        }
        match fs::symlink_metadata(&current) {
            Ok(info) if info.is_dir() && !info.file_type().is_symlink() => {}
            Ok(_) => {
                return Err(Failure::denied(
                    "cache path contains a symlink or non-directory",
                ));
            }
            Err(error)
                if error.kind() == io::ErrorKind::NotFound
                    && (current == path
                        || current
                            == path
                                .parent()
                                .ok_or_else(|| Failure::denied("invalid cache parent"))?) =>
            {
                private_directory(&current)?
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}
fn exclusive_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

struct Report {
    root: PathBuf,
    root_identity: (u64, u64),
    result_identity: Option<(u64, u64)>,
    value: Value,
}
impl Report {
    fn new(cache: &Path, session: &SessionInfo, before: BTreeMap<u32, Value>) -> Result<Self> {
        ensure_cache(cache)?;
        let root = cache.join(format!("run-{}", random_id()?));
        private_directory(&root)?;
        File::open(cache)?.sync_all()?;
        let info = fs::symlink_metadata(&root)?;
        let mut report = Self {
            root,
            root_identity: (info.dev(), info.ino()),
            result_identity: None,
            value: json!({
                "version":1,"mode":"write_confirmed","storageId":STORAGE,"remoteDirectory":REMOTE,
                "deviceModel":session.display_name,"state":"Prepared","phase":"preflight",
                "baselineMetadata":before,"createdPhoneObjects":[],"transfers":[],"checks":{},
                "noPhoneDeletion":true,"noAutomaticRetry":true,"existingUserFileContentNotRead":true,
                "metadataPreservationIsNotUserFileContentHashVerification":true,
                "mayHaveUnrecordedPhoneObjects":false
            }),
        };
        report.save()?;
        Ok(report)
    }
    fn save(&mut self) -> Result<()> {
        let info = fs::symlink_metadata(&self.root)?;
        if !info.is_dir()
            || info.file_type().is_symlink()
            || (info.dev(), info.ino()) != self.root_identity
        {
            return Err(Failure::verify("private report directory identity changed"));
        }
        let target = self.root.join("results.json");
        match fs::symlink_metadata(&target) {
            Ok(info)
                if info.is_file()
                    && !info.file_type().is_symlink()
                    && Some((info.dev(), info.ino())) == self.result_identity => {}
            Err(error)
                if error.kind() == io::ErrorKind::NotFound && self.result_identity.is_none() => {}
            _ => {
                return Err(Failure::verify(
                    "owned report identity changed; never overwrite an unrelated file",
                ));
            }
        }
        let temporary = self.root.join(format!(".results-{}.json", random_id()?));
        exclusive_file(&temporary, &serde_json::to_vec_pretty(&self.value)?)?;
        let info = fs::symlink_metadata(&temporary)?;
        fs::rename(&temporary, &target)?;
        self.result_identity = Some((info.dev(), info.ino()));
        File::open(&self.root)?.sync_all()?;
        Ok(())
    }
    fn phase(&mut self, phase: &str) -> Result<()> {
        self.value["phase"] = phase.into();
        self.value["state"] = "Running".into();
        self.save()
    }
    fn transfer(&mut self, receipt: &TransferReceipt) -> Result<()> {
        self.value["transfers"]
            .as_array_mut()
            .ok_or_else(|| Failure::verify("invalid report transfer list"))?
            .push(receipt_value(receipt));
        if receipt.direction == TransferDirection::Upload {
            self.value["createdPhoneObjects"].as_array_mut().ok_or_else(|| Failure::verify("invalid report object list"))?.push(json!({"objectId":receipt.object_id,"actualName":receipt.actual_name,"bytes":receipt.bytes,"journalPath":receipt.journal_path}));
        }
        self.save()
    }
    fn journals(&mut self) -> Result<()> {
        // Enumerate only this newly created owned run tree, never startup scan,
        // user destinations, phone paths, or journals from earlier runs.
        let mut found = Vec::new();
        collect_journals(&self.root, 0, &mut found)?;
        self.value["localJournals"] = json!(found);
        self.value["journalSearchScope"] = json!(LocalPath::from_path(&self.root)?);
        self.save()
    }
}
fn collect_journals(path: &Path, depth: usize, output: &mut Vec<LocalPath>) -> Result<()> {
    if depth > 5 {
        return Err(Failure::verify("unexpected local run tree depth"));
    }
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let info = entry.file_type()?;
        if info.is_symlink() {
            return Err(Failure::verify("unexpected symlink in generated run tree"));
        }
        let path = entry.path();
        if info.is_dir() {
            collect_journals(&path, depth + 1, output)?;
        } else if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(".journal-") && name.ends_with(".json"))
        {
            output.push(LocalPath::from_path(&path)?);
        }
    }
    Ok(())
}
fn verify_receipt(
    receipt: &TransferReceipt,
    direction: TransferDirection,
    bytes: usize,
) -> Result<u32> {
    if receipt.direction != direction || receipt.bytes != bytes as u64 {
        return Err(Failure::verify(
            "transfer receipt direction/byte count mismatch",
        ));
    }
    receipt
        .object_id
        .filter(|id| *id != 0 && *id != u32::MAX)
        .ok_or_else(|| Failure::verify("transfer receipt has no valid object identity"))
}
fn verify_snapshot(receipt: &TransferReceipt, expected: &[u8]) -> Result<()> {
    let path = receipt.journal_path.to_path_buf();
    let data: Value = serde_json::from_slice(&fs::read(&path)?)?;
    if data["state"] != "Committed"
        || data["remote_upload"]["object_id"].as_u64() != receipt.object_id.map(u64::from)
    {
        return Err(Failure::verify(
            "upload journal is not the matching committed receipt",
        ));
    }
    let name = data["payload_name"]
        .as_str()
        .ok_or_else(|| Failure::verify("upload journal has no snapshot basename"))?;
    if RemoteFilename::new(name).is_err() {
        return Err(Failure::verify(
            "unsafe snapshot basename in upload journal",
        ));
    }
    let payload = path
        .parent()
        .ok_or_else(|| Failure::verify("upload journal has no parent"))?
        .join(name);
    if fs::read(payload)? != expected {
        return Err(Failure::verify(
            "retained immutable snapshot content mismatch",
        ));
    }
    Ok(())
}
fn verify_download(
    receipt: &TransferReceipt,
    destination: &Path,
    expected: &[u8],
) -> Result<PathBuf> {
    verify_receipt(receipt, TransferDirection::Download, expected.len())?;
    let path = receipt
        .local_path
        .as_ref()
        .ok_or_else(|| Failure::verify("download receipt missing local publication"))?
        .to_path_buf();
    if path.parent() != Some(destination)
        || path.file_name().and_then(|name| name.to_str()) != Some(receipt.actual_name.as_str())
    {
        return Err(Failure::verify(
            "download publication escaped its fresh destination",
        ));
    }
    if fs::read(&path)? != expected {
        return Err(Failure::verify("downloaded generated content mismatch"));
    }
    Ok(path)
}
fn upload_and_download(
    client: &mut WorkerClient,
    target: &RemoteDirectoryRef,
    before: &BTreeMap<u32, Value>,
    report: &mut Report,
    stem: &str,
    expected: &[u8],
    conflicts: bool,
) -> Result<()> {
    let source = report.root.join(format!("{stem}.txt"));
    exclusive_file(&source, expected)?;
    let cache = report.root.join(format!("{stem}-upload-cache"));
    private_directory(&cache)?;
    let destination = report.root.join(format!("{stem}-downloads"));
    private_directory(&destination)?;
    let requested = source
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Failure::denied("generated native name is not representable"))?;
    report.phase(&format!("{stem}:upload_dispatch_pending"))?;
    let uploaded = client.upload_file(&source, requested, target, &cache)?;
    report.transfer(&uploaded)?;
    let id = verify_receipt(&uploaded, TransferDirection::Upload, expected.len())?;
    if before.contains_key(&id) || uploaded.actual_name == requested {
        return Err(Failure::verify(
            "upload did not create a distinct KeepBoth identity/name",
        ));
    }
    verify_snapshot(&uploaded, expected)?;
    if fs::read(&source)? != expected {
        return Err(Failure::verify("generated upload source was modified"));
    }
    let current = client.list(target)?;
    if !baseline_preserved(before, &metadata(&current)) {
        return Err(Failure::verify(
            "pre-existing target metadata changed; no claim about cause or user file content",
        ));
    }
    let remote = current
        .iter()
        .find(|entry| {
            entry.reference.object().0 == id
                && entry.name == uploaded.actual_name
                && !entry.directory
                && entry.byte_size == Some(expected.len() as u64)
        })
        .ok_or_else(|| {
            Failure::verify("new upload receipt is absent from current target listing")
        })?;
    report.phase(&format!("{stem}:download_dispatch_pending"))?;
    let downloaded = client.download_file(
        &remote.reference,
        &destination,
        FileConflictPolicy::KeepBoth,
    )?;
    report.transfer(&downloaded)?;
    if verify_receipt(&downloaded, TransferDirection::Download, expected.len())? != id {
        return Err(Failure::verify("download receipt object mismatch"));
    }
    let first = verify_download(&downloaded, &destination, expected)?;
    if conflicts {
        report.phase("blob:skip_existing_dispatch_pending")?;
        match client.download_file(&remote.reference, &destination, FileConflictPolicy::Skip) {
            Err(ApplicationError::Backend {
                code: mtp_backend_api::ErrorCode::Skipped,
                requires_review: false,
                ..
            }) => {}
            Err(error) => return Err(error.into()),
            Ok(receipt) => {
                report.transfer(&receipt)?;
                return Err(Failure::verify("Skip unexpectedly returned success"));
            }
        }
        if fs::read(&first)? != expected {
            return Err(Failure::verify(
                "Skip altered the original generated download",
            ));
        }
        report.value["checks"]["skipExistingRetained"] = true.into();
        report.save()?;
        report.phase("blob:keep_both_dispatch_pending")?;
        let second = client.download_file(
            &remote.reference,
            &destination,
            FileConflictPolicy::KeepBoth,
        )?;
        report.transfer(&second)?;
        if verify_receipt(&second, TransferDirection::Download, expected.len())? != id {
            return Err(Failure::verify("KeepBoth receipt object mismatch"));
        }
        let second_path = verify_download(&second, &destination, expected)?;
        if second_path == first
            || second.actual_name == downloaded.actual_name
            || fs::read(&first)? != expected
        {
            return Err(Failure::verify(
                "KeepBoth did not preserve the first publication",
            ));
        }
        report.value["checks"]["keepBothDistinctAndOriginalRetained"] = true.into();
    }
    report.value["checks"][if expected.is_empty() {
        "emptyUploadDownloadExact"
    } else {
        "blobUploadDownloadExact"
    }] = true.into();
    report.save()
}
fn write_acceptance(
    client: &mut WorkerClient,
    session: &SessionInfo,
    target: &Target,
    config: &Config,
) -> Result<Report> {
    if !session.capabilities.upload || !session.capabilities.download {
        return Err(Failure::denied(
            "write-confirmed requires verified upload and download capabilities",
        ));
    }
    let before = metadata(&target.entries);
    let mut report = Report::new(&config.cache, session, before.clone())?;
    println!(
        "LOCAL_REPORT={}",
        report.root.join("results.json").display()
    );
    let result = (|| {
        let token = random_id()?;
        let bytes: Vec<u8> = (0..BLOB_BYTES)
            .map(|index| ((index * 31 + 17) % 251) as u8)
            .collect();
        upload_and_download(
            client,
            &target.directory,
            &before,
            &mut report,
            &format!("MTP 验收 {token}"),
            &bytes,
            true,
        )?;
        upload_and_download(
            client,
            &target.directory,
            &before,
            &mut report,
            &format!("MTP 空文件 {token}"),
            &[],
            false,
        )?;
        report.phase("final_metadata_check")?;
        let after = metadata(&client.list(&target.directory)?);
        report.value["afterMetadata"] = json!(after);
        if !baseline_preserved(&before, &after) {
            return Err(Failure::verify(
                "pre-existing authorized-directory metadata changed",
            ));
        }
        report.value["checks"]["allPreExistingIdsNamesSizesPreserved"] = true.into();
        report.value["state"] = "Passed".into();
        report.value["phase"] = "complete".into();
        report.journals()?;
        Ok(())
    })();
    if let Err(mut error) = result {
        // Any local evidence failure after an upload may conceal a phone commit.
        if report.value["createdPhoneObjects"]
            .as_array()
            .is_some_and(|objects| !objects.is_empty())
            && (error.class == "local_io" || error.class == "local_json")
        {
            error.requires_review = true;
        }
        report.value["state"] = if error.requires_review {
            "NeedsReview"
        } else {
            "Failed"
        }
        .into();
        report.value["failure"] = error.report();
        report.value["mayHaveUnrecordedPhoneObjects"] = json!(
            error.requires_review
                && report.value["phase"]
                    .as_str()
                    .is_some_and(|phase| phase.ends_with("upload_dispatch_pending"))
        );
        if report.journals().is_err() {
            eprintln!(
                "local report update failed; preserve the printed run directory and all worker journals"
            );
        }
        return Err(error);
    }
    Ok(report)
}
fn run(config: Config) -> Result<()> {
    let historical = if let Some(path) = &config.verify_report {
        println!("PHASE=verify_report");
        Some(read_historical_baseline(path, &config.cache)?)
    } else {
        None
    };
    println!("PHASE=spawn");
    let mut client = WorkerClient::spawn(
        &config.worker,
        &BackendConfig::Kalam {
            library_path: config.library.clone(),
        },
    )?;
    let mut completed_report = None;
    let result = (|| {
        println!("PHASE=connect");
        let session = client.connect()?;
        let target = locate(&mut client, &session, &config)?;
        let parent = target
            .directory
            .parent()
            .ok_or_else(|| Failure::denied("authorized directory resolved to root; rejected"))?;
        println!(
            "MODEL={} STORAGE={} ROOT_COUNT={} TMP_ID={} TMP_COUNT={} MODE={}",
            session.display_name,
            config.storage,
            target.root_count,
            parent.0,
            target.entries.len(),
            if config.mode == Mode::ReadOnly {
                "read_only"
            } else {
                "write_confirmed"
            }
        );
        if let Some(historical) = &historical {
            let summary = baseline_summary(historical, &metadata(&target.entries));
            println!("BASELINE_COMPARISON={summary}");
            if summary["allBaselineIDsNameSizeKindPreserved"] != true {
                return Err(Failure::verify(
                    "historical baseline metadata differs; no retry or cleanup authorized",
                ));
            }
        }
        if config.mode == Mode::WriteConfirmed {
            completed_report = Some(write_acceptance(&mut client, &session, &target, &config)?);
        }
        Ok(())
    })();
    let shutdown = client.shutdown().map_err(Failure::from);
    if let Some(mut report) = completed_report {
        report.value["workerShutdownConfirmed"] = json!(shutdown.is_ok());
        if let Err(error) = &shutdown {
            report.value["state"] = if error.requires_review {
                "NeedsReview"
            } else {
                "Failed"
            }
            .into();
            report.value["failure"] = error.report();
            report.value["phase"] = "worker_shutdown".into();
        }
        report.save().map_err(|mut error| {
            error.requires_review = true;
            error
        })?;
    }
    match (result, shutdown) {
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => {
            println!(
                "PASS: {}",
                if config.mode == Mode::ReadOnly {
                    "existing /tmp resolved by current object references; no local or phone writes"
                } else {
                    "generated-only single-file checks; retained phone objects and local evidence; no user-file content claim"
                }
            );
            Ok(())
        }
    }
}
fn main() -> ExitCode {
    match parse(std::env::args_os().skip(1)).and_then(|config| match config {
        Some(config) => run(config),
        None => {
            println!("{HELP}");
            Ok(())
        }
    }) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("acceptance stopped: {error}");
            ExitCode::from(if error.requires_review { 3 } else { 1 })
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    fn args() -> Vec<OsString> {
        vec![
            "--library".into(),
            "/trusted/kalam.dylib".into(),
            "--worker".into(),
            "/trusted/mtp-worker".into(),
            "--storage-id".into(),
            "65537".into(),
            "--remote-directory".into(),
            "/tmp".into(),
            "--local-cache".into(),
            project_cache().unwrap().into_os_string(),
        ]
    }
    fn verification_path() -> PathBuf {
        project_cache()
            .unwrap()
            .join("run-0123456789abcdef0123456789abcdef/results.json")
    }
    fn historical_value() -> Value {
        json!({"version":1,"mode":"write_confirmed","storageId":STORAGE,"remoteDirectory":REMOTE,
            "state":"NeedsReview","baselineMetadata":{"42":{"name":"private-original-name","directory":false,"bytes":7}}})
    }
    #[test]
    fn report_verification_defaults_to_read_only_and_accepts_only_exact_owned_path() {
        let mut values = args();
        values.extend([
            "--verify-report".into(),
            verification_path().into_os_string(),
        ]);
        let config = parse(values).unwrap().unwrap();
        assert_eq!(config.mode, Mode::ReadOnly);
        assert_eq!(config.verify_report, Some(verification_path()));
    }
    #[test]
    fn report_verification_is_rejected_in_write_mode_or_when_repeated() {
        let mut values = args();
        values.extend([
            "--write-confirmed".into(),
            "--verify-report".into(),
            verification_path().into_os_string(),
        ]);
        assert!(parse(values).is_err());
        let mut values = args();
        for _ in 0..2 {
            values.extend([
                "--verify-report".into(),
                verification_path().into_os_string(),
            ]);
        }
        assert!(parse(values).is_err());
    }
    #[test]
    fn report_path_rejects_relative_traversal_aliases_other_bodies_and_foreign_cache() {
        let cache = project_cache().unwrap();
        let run = "run-0123456789abcdef0123456789abcdef";
        for path in [
            PathBuf::from("relative/results.json"),
            PathBuf::from("/tmp/results.json"),
            cache.join("results.json"),
            cache.join("run-invalid/results.json"),
            cache.join(format!("{run}/payload")),
            cache.join(format!("{run}/source.txt")),
            cache.join(format!("{run}/../{run}/results.json")),
            cache.join(format!("{run}/./results.json")),
            cache.join(format!("{run}//results.json")),
            cache.join(format!("{run}/results.json/")),
            cache.join("run-0123456789ABCDEF0123456789ABCDEF/results.json"),
        ] {
            assert!(
                validate_report_path(&path, &cache).is_err(),
                "guard must reject an unsafe report path"
            );
        }
        use std::os::unix::ffi::OsStringExt;
        let nul = PathBuf::from(OsString::from_vec(b"/absolute/run/results.json\0".to_vec()));
        assert!(validate_report_path(&nul, &cache).is_err());
    }
    #[test]
    fn historical_baseline_requires_exact_version_storage_directory_and_metadata_shape() {
        for (field, value) in [
            ("version", json!(2)),
            ("mode", json!("read_only")),
            ("storageId", json!(65538)),
            ("remoteDirectory", json!("/")),
            ("state", json!("arbitrary-private-state")),
        ] {
            let mut historical = historical_value();
            historical[field] = value;
            assert!(parse_historical_baseline(&historical).is_err());
        }
        let mut historical = historical_value();
        historical["baselineMetadata"]["42"]["bytes"] = json!("7");
        assert!(parse_historical_baseline(&historical).is_err());
        let mut historical = historical_value();
        historical["baselineMetadata"]["42"]
            .as_object_mut()
            .unwrap()
            .remove("directory");
        assert!(parse_historical_baseline(&historical).is_err());
        let mut historical = historical_value();
        historical["baselineMetadata"] =
            json!({"042":{"name":"private","directory":false,"bytes":7}});
        assert!(parse_historical_baseline(&historical).is_err());
    }
    #[test]
    fn historical_comparison_distinguishes_unchanged_added_changed_and_missing_ids() {
        let historical = parse_historical_baseline(&historical_value()).unwrap();
        let mut current = historical.entries.clone();
        let summary = baseline_summary(&historical, &current);
        assert_eq!(summary["allBaselineIDsNameSizeKindPreserved"], true);
        assert_eq!(summary["extraObjectCount"], 0);
        assert_eq!(summary["historicalNeedsReviewUnchanged"], true);
        assert_eq!(summary["authorizesRetryOrCleanup"], false);
        assert!(!summary.to_string().contains("private-original-name"));
        current.insert(
            99,
            json!({"name":"extra-private-name","directory":true,"bytes":null}),
        );
        let summary = baseline_summary(&historical, &current);
        assert_eq!(summary["allBaselineIDsNameSizeKindPreserved"], true);
        assert_eq!(summary["extraObjectCount"], 1);
        assert!(!summary.to_string().contains("extra-private-name"));
        for (field, value) in [
            ("name", json!("changed-private-name")),
            ("bytes", json!(8)),
            ("directory", json!(true)),
        ] {
            let mut changed = current.clone();
            changed.get_mut(&42).unwrap()[field] = value;
            assert_eq!(
                baseline_summary(&historical, &changed)["allBaselineIDsNameSizeKindPreserved"],
                false
            );
        }
        current.remove(&42);
        let summary = baseline_summary(&historical, &current);
        assert_eq!(summary["allBaselineIDsNameSizeKindPreserved"], false);
        assert_eq!(summary["missingBaselineObjectCount"], 1);
        assert_eq!(summary["extraObjectCount"], 1);
    }

    #[test]
    fn known_static_native_messages_become_safe_diagnostic_classes() {
        for category in [
            "ErrorDeviceSetup",
            "ErrorMtpLockExists",
            "ErrorListDirectory",
        ] {
            let failure = Failure::from(ApplicationError::Backend {
                code: mtp_backend_api::ErrorCode::BackendFailure,
                message: format!("native failure: {category}"),
                requires_review: false,
            });
            assert_eq!(failure.class, format!("backend_{category}"));
            assert!(!failure.requires_review);
        }
    }
    #[test]
    fn arbitrary_or_extended_native_messages_stay_withheld() {
        for message in [
            "/private/device/file",
            "native failure: ErrorDeviceSetup /private/device/file",
            "native failure: ErrorMtpLockExists\nserial=secret",
            "native failure: UnknownNativeType",
        ] {
            let failure = Failure::from(ApplicationError::Backend {
                code: mtp_backend_api::ErrorCode::BackendFailure,
                message: message.into(),
                requires_review: false,
            });
            assert_eq!(failure.class, "backend_BackendFailure");
            assert!(!failure.to_string().contains("/private"));
            assert!(!failure.to_string().contains("secret"));
            assert!(!failure.report().to_string().contains("/private"));
            assert!(!failure.report().to_string().contains("secret"));
        }
    }

    #[test]
    fn no_mode_defaults_to_read_only() {
        assert_eq!(parse(args()).unwrap().unwrap().mode, Mode::ReadOnly);
    }
    #[test]
    fn confirmed_write_requires_explicit_mode() {
        let mut values = args();
        values.push("--write-confirmed".into());
        assert_eq!(parse(values).unwrap().unwrap().mode, Mode::WriteConfirmed);
    }
    #[test]
    fn conflicting_or_repeated_modes_are_rejected() {
        for last in ["--write-confirmed", "--read-only"] {
            let mut values = args();
            values.extend(["--read-only".into(), last.into()]);
            assert!(parse(values).is_err());
        }
    }
    #[test]
    fn unauthorized_remote_roots_and_paths_are_rejected() {
        for remote in ["/", "/tmp/", "/tmp/../tmp", "/other", "tmp"] {
            let mut values = args();
            values[7] = remote.into();
            assert!(parse(values).is_err());
        }
    }
    #[test]
    fn other_storage_is_rejected() {
        let mut values = args();
        values[5] = "65538".into();
        assert!(parse(values).is_err());
    }
    #[test]
    fn relative_worker_library_and_cache_are_rejected() {
        for index in [1, 3, 9] {
            let mut values = args();
            values[index] = "relative".into();
            assert!(parse(values).is_err());
        }
    }
    #[test]
    fn arbitrary_local_cache_is_rejected() {
        let mut values = args();
        values[9] = "/tmp/unowned".into();
        assert!(parse(values).is_err());
    }
    #[test]
    fn duplicate_options_and_unknown_options_are_rejected() {
        for option in ["--worker", "--backend"] {
            let mut values = args();
            values.extend([option.into(), "/other".into()]);
            assert!(parse(values).is_err());
        }
    }
    #[test]
    fn incomplete_or_empty_arguments_are_rejected() {
        assert!(parse(Vec::<OsString>::new()).is_err());
        let mut values = args();
        values.pop();
        assert!(parse(values).is_err());
    }
    #[test]
    fn help_never_connects_or_selects_write_mode() {
        assert!(parse(vec!["--help".into()]).unwrap().is_none());
        assert!(parse(vec!["--help".into(), "--write-confirmed".into()]).is_err());
    }
    #[test]
    fn absent_tmp_is_rejected_without_root_fallback() {
        assert!(exact_directory(&[], "tmp").is_err());
    }
    #[test]
    fn baseline_requires_every_original_id_name_size() {
        let before = BTreeMap::from([(42, json!({"name":"untouched","bytes":7}))]);
        let mut after = before.clone();
        after.insert(123, json!({"name":"generated","bytes":0}));
        assert!(baseline_preserved(&before, &after));
        after.insert(42, json!({"name":"changed","bytes":7}));
        assert!(!baseline_preserved(&before, &after));
        after.remove(&42);
        assert!(!baseline_preserved(&before, &after));
    }
    #[test]
    fn unknown_outcome_retains_review_and_journal_without_private_display() {
        let path = LocalPath::from_path(Path::new("/private/run/journal.json")).unwrap();
        let failure = Failure::from(ApplicationError::UnknownOutcome {
            journal_path: Some(path.clone()),
        });
        assert!(failure.requires_review);
        assert_eq!(failure.journal_path, Some(path));
        assert!(!failure.to_string().contains("/private"));
    }
}
