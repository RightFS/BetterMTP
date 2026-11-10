#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use std::collections::hash_map::DefaultHasher;
use std::fs as stdfs;
use std::hash::{Hash, Hasher};
use std::os::unix::fs::symlink;
use std::sync::{Arc, Barrier};
use std::thread;

/// Only this test fixture may recurse. Verify an absolute canonical temp prefix,
/// exact generated child name, non-symlink directory and ownership token first.
struct Fixture {
    root: PathBuf,
    temp: PathBuf,
    owner: String,
}

impl Fixture {
    fn new() -> Self {
        let temp = std::env::temp_dir().canonicalize().unwrap();
        let owner = Uuid::new_v4().to_string();
        let root = temp.join(format!("mtp-transfer-test-{owner}"));
        stdfs::create_dir(&root).unwrap();
        stdfs::write(root.join(".fixture-owner"), &owner).unwrap();
        Self { root, temp, owner }
    }

    fn directory(&self, name: &str) -> PathBuf {
        validate_basename(name).unwrap();
        let path = self.root.join(name);
        stdfs::create_dir(&path).unwrap();
        path
    }

    fn download(&self, name: &str, bytes: &[u8]) -> DownloadStage {
        let stage = DownloadStage::new(&self.root, name, bytes.len() as u64).unwrap();
        stdfs::write(stage.payload_path(), bytes).unwrap();
        stage
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        assert!(self.root.is_absolute());
        assert!(self.temp.is_absolute());
        assert!(self.root.starts_with(&self.temp));
        assert_eq!(self.root.parent(), Some(self.temp.as_path()));
        assert_eq!(
            self.root.file_name().unwrap(),
            OsStr::new(&format!("mtp-transfer-test-{}", self.owner))
        );
        assert!(
            !stdfs::symlink_metadata(&self.root)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(self.root.canonicalize().unwrap(), self.root);
        let marker = self.root.join(".fixture-owner");
        assert!(stdfs::symlink_metadata(&marker).unwrap().is_file());
        assert_eq!(stdfs::read_to_string(&marker).unwrap(), self.owner);
        stdfs::remove_dir_all(&self.root).unwrap();
    }
}

fn hash(bytes: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

fn read_journal(path: &Path) -> Journal {
    serde_json::from_slice(&stdfs::read(path).unwrap()).unwrap()
}

#[test]
fn publishes_verified_bytes() {
    let fixture = Fixture::new();
    let mut stage = fixture.download("photo.jpg", b"downloaded");
    assert_eq!(stage.safe_name(), "photo.jpg");
    assert_eq!(
        stage.payload_path(),
        stage.staging_directory().join("photo.jpg")
    );
    let final_path = stage.commit().unwrap();
    assert_eq!(final_path, fixture.root.join("photo.jpg"));
    assert_eq!(stdfs::read(final_path).unwrap(), b"downloaded");
    assert!(!stage.payload_path().exists());
    let journal = read_journal(stage.journal_path());
    assert_eq!(journal.state, TaskState::Committed);
    assert!(journal.publication_confirmed);
}

#[test]
fn zero_byte_download_is_valid() {
    let fixture = Fixture::new();
    let mut stage = fixture.download("empty", b"");
    assert_eq!(stdfs::metadata(stage.commit().unwrap()).unwrap().len(), 0);
}

#[test]
fn existing_target_hash_and_bytes_unchanged() {
    let fixture = Fixture::new();
    let target = fixture.root.join("existing.bin");
    let bytes: Vec<u8> = (0..=255).collect();
    stdfs::write(&target, &bytes).unwrap();
    let before = hash(&bytes);
    let mut stage = fixture.download("existing.bin", b"NOT ORIGINAL");
    assert!(matches!(stage.commit(), Err(TransferError::Skipped(_))));
    let after = stdfs::read(target).unwrap();
    assert_eq!(after, bytes);
    assert_eq!(hash(&after), before);
    assert_eq!(read_journal(stage.journal_path()).state, TaskState::Skipped);
    assert!(!stage.journal().publication_confirmed);
}

#[test]
fn construction_never_reserves_final_target() {
    let fixture = Fixture::new();
    let stage = DownloadStage::new(&fixture.root, "future", 42).unwrap();
    assert!(!fixture.root.join("future").exists());
    assert!(stage.payload_path().is_file());
    assert_eq!(stdfs::metadata(stage.payload_path()).unwrap().len(), 0);
}

#[test]
fn staging_directories_are_random_and_distinct() {
    let fixture = Fixture::new();
    let a = DownloadStage::new(&fixture.root, "same", 0).unwrap();
    let b = DownloadStage::new(&fixture.root, "same", 0).unwrap();
    assert_ne!(a.staging_directory(), b.staging_directory());
    assert_ne!(a.journal().owner, b.journal().owner);
}

#[test]
fn short_payload_needs_review_and_is_retained() {
    let fixture = Fixture::new();
    let mut stage = DownloadStage::new(&fixture.root, "short", 10).unwrap();
    stdfs::write(stage.payload_path(), b"tiny").unwrap();
    assert!(matches!(
        stage.commit(),
        Err(TransferError::SizeMismatch {
            expected: 10,
            actual: 4
        })
    ));
    assert_eq!(
        read_journal(stage.journal_path()).state,
        TaskState::NeedsReview
    );
    assert!(stage.payload_path().exists());
    assert!(!fixture.root.join("short").exists());
}

#[test]
fn oversized_payload_needs_review() {
    let fixture = Fixture::new();
    let mut stage = DownloadStage::new(&fixture.root, "large", 1).unwrap();
    stdfs::write(stage.payload_path(), b"too large").unwrap();
    assert!(matches!(
        stage.commit(),
        Err(TransferError::SizeMismatch { .. })
    ));
    assert_eq!(stage.journal().state, TaskState::NeedsReview);
}

#[test]
fn expected_u64_does_not_narrow() {
    let fixture = Fixture::new();
    let mut stage = DownloadStage::new(&fixture.root, "huge", u64::MAX).unwrap();
    assert!(matches!(
        stage.commit(),
        Err(TransferError::SizeMismatch {
            expected: u64::MAX,
            actual: 0
        })
    ));
}

#[test]
fn keep_both_retains_original_and_extension() {
    let fixture = Fixture::new();
    let target = fixture.root.join("photo.jpg");
    stdfs::write(&target, b"old").unwrap();
    let mut stage =
        DownloadStage::with_policy(&fixture.root, "photo.jpg", 3, ConflictPolicy::KeepBoth)
            .unwrap();
    stdfs::write(stage.payload_path(), b"new").unwrap();
    let published = stage.commit().unwrap();
    assert_ne!(published, target);
    assert_eq!(published.extension(), Some(OsStr::new("jpg")));
    assert_eq!(stdfs::read(published).unwrap(), b"new");
    assert_eq!(stdfs::read(target).unwrap(), b"old");
}

#[test]
fn keep_both_uses_requested_name_when_free() {
    let fixture = Fixture::new();
    let mut stage =
        DownloadStage::with_policy(&fixture.root, "free", 0, ConflictPolicy::KeepBoth).unwrap();
    assert_eq!(stage.commit().unwrap(), fixture.root.join("free"));
}

#[test]
fn unsafe_names_are_rejected_without_side_effects() {
    let fixture = Fixture::new();
    for name in [
        "",
        ".",
        "..",
        "../escape",
        "a/b",
        "a\\b",
        "/abs",
        "x\0y",
        "x\ny",
        "x\ry",
        "x\ty",
    ] {
        assert!(
            matches!(
                DownloadStage::new(&fixture.root, name, 0),
                Err(TransferError::UnsafeName(_))
            ),
            "{name:?}"
        );
    }
    assert_eq!(stdfs::read_dir(&fixture.root).unwrap().count(), 1);
}

#[test]
fn long_names_rejected_by_utf8_byte_count() {
    assert!(validate_basename(&"a".repeat(256)).is_err());
    assert!(validate_basename(&"界".repeat(86)).is_err());
    assert!(validate_basename(&"a".repeat(255)).is_ok());
}

#[test]
fn unicode_and_spaces_are_valid_basenames() {
    let fixture = Fixture::new();
    let mut stage = fixture.download("旅行 photo.jpg", b"safe");
    assert_eq!(stdfs::read(stage.commit().unwrap()).unwrap(), b"safe");
}

#[test]
fn destination_symlink_rejected() {
    let fixture = Fixture::new();
    let actual = fixture.directory("actual");
    let alias = fixture.root.join("alias");
    symlink(actual, &alias).unwrap();
    assert!(DownloadStage::new(&alias, "no", 0).is_err());
}

#[test]
fn ancestor_symlink_rejected() {
    let fixture = Fixture::new();
    let actual = fixture.directory("actual");
    stdfs::create_dir(actual.join("child")).unwrap();
    let alias = fixture.root.join("alias");
    symlink(actual, &alias).unwrap();
    assert!(DownloadStage::new(&alias.join("child"), "no", 0).is_err());
}

#[test]
fn destination_parent_traversal_rejected() {
    let fixture = Fixture::new();
    let child = fixture.directory("child");
    assert!(matches!(
        DownloadStage::new(&child.join(".."), "no", 0),
        Err(TransferError::UnsafeName(_))
    ));
}

#[test]
fn existing_symlink_target_never_followed_or_overwritten() {
    let fixture = Fixture::new();
    let real = fixture.root.join("user-data");
    stdfs::write(&real, b"user bytes").unwrap();
    let target = fixture.root.join("alias");
    symlink(&real, &target).unwrap();
    let mut stage = fixture.download("alias", b"bad");
    assert!(matches!(stage.commit(), Err(TransferError::Skipped(_))));
    assert!(
        stdfs::symlink_metadata(target)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(stdfs::read(real).unwrap(), b"user bytes");
}

#[test]
fn dangling_symlink_target_is_also_a_conflict() {
    let fixture = Fixture::new();
    let target = fixture.root.join("dangling");
    symlink(fixture.root.join("missing"), &target).unwrap();
    let mut stage = fixture.download("dangling", b"new");
    assert!(matches!(stage.commit(), Err(TransferError::Skipped(_))));
    assert!(
        stdfs::symlink_metadata(target)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn existing_directory_is_not_replaced() {
    let fixture = Fixture::new();
    let target = fixture.directory("dir");
    stdfs::write(target.join("user"), b"keep").unwrap();
    let mut stage = fixture.download("dir", b"bad");
    assert!(stage.commit().is_err());
    assert_eq!(stdfs::read(target.join("user")).unwrap(), b"keep");
}

#[test]
fn destination_rebinding_refuses_commit_and_cleanup() {
    let fixture = Fixture::new();
    let destination = fixture.directory("destination");
    let mut stage = DownloadStage::new(&destination, "result", 0).unwrap();
    let moved = fixture.root.join("moved");
    // The absolute old/new fixture paths are checked before moving.
    assert_eq!(destination.parent(), Some(fixture.root.as_path()));
    assert_eq!(moved.parent(), Some(fixture.root.as_path()));
    stdfs::rename(&destination, &moved).unwrap();
    stdfs::create_dir(&destination).unwrap();
    assert!(matches!(stage.commit(), Err(TransferError::Ownership)));
    assert!(stage.cleanup_owned().is_err());
    assert!(!destination.join("result").exists());
    assert!(!moved.join("result").exists());
}

#[test]
fn payload_symlink_swap_rejected_without_deleting_user_file() {
    let fixture = Fixture::new();
    let mut stage = fixture.download("payload", b"abc");
    let user = fixture.root.join("user");
    stdfs::write(&user, b"abc").unwrap();
    assert!(stage.payload_path().is_absolute());
    assert!(stage.payload_path().starts_with(stage.staging_directory()));
    assert_eq!(
        stage.journal().payload_identity,
        FileIdentity::of(&stdfs::symlink_metadata(stage.payload_path()).unwrap())
    );
    stdfs::remove_file(stage.payload_path()).unwrap();
    symlink(&user, stage.payload_path()).unwrap();
    assert!(matches!(stage.commit(), Err(TransferError::Ownership)));
    assert!(stage.cleanup_owned().is_err());
    assert_eq!(stdfs::read(user).unwrap(), b"abc");
}

#[test]
fn hardlinked_payload_rejected() {
    let fixture = Fixture::new();
    let mut stage = fixture.download("payload", b"abc");
    let alias = fixture.root.join("alias");
    stdfs::hard_link(stage.payload_path(), &alias).unwrap();
    assert!(matches!(stage.commit(), Err(TransferError::NotRegular)));
    assert_eq!(stdfs::read(alias).unwrap(), b"abc");
}

#[test]
fn unknown_stage_child_blocks_cleanup() {
    let fixture = Fixture::new();
    let mut stage = fixture.download("payload", b"abc");
    let unknown = stage.staging_directory().join("not-ours");
    stdfs::write(&unknown, b"user").unwrap();
    assert!(matches!(
        stage.cleanup_owned(),
        Err(TransferError::Ownership)
    ));
    assert_eq!(stdfs::read(unknown).unwrap(), b"user");
    assert!(stage.payload_path().exists());
    assert!(stage.journal_path().exists());
}

#[test]
fn marker_tampering_blocks_cleanup() {
    let fixture = Fixture::new();
    let mut stage = fixture.download("payload", b"abc");
    let marker = stage.staging_directory().join(&stage.journal().marker_name);
    stdfs::write(marker, b"forged owner").unwrap();
    assert!(matches!(
        stage.cleanup_owned(),
        Err(TransferError::Ownership)
    ));
    assert!(stage.payload_path().exists());
}

#[test]
fn journal_inode_swap_blocks_cleanup_and_save() {
    let fixture = Fixture::new();
    let mut stage = fixture.download("payload", b"abc");
    // Retain old inode via link so inode reuse cannot defeat the test.
    stdfs::hard_link(stage.journal_path(), fixture.root.join("old-journal")).unwrap();
    assert!(stage.journal_path().is_absolute());
    assert!(stage.journal_path().starts_with(stage.staging_directory()));
    assert_eq!(
        named_identity(&stage.stage.directory, &stage.journal().journal_name).unwrap(),
        stage.stage.journal_identity
    );
    stdfs::remove_file(stage.journal_path()).unwrap();
    stdfs::write(stage.journal_path(), b"USER JOURNAL").unwrap();
    assert!(stage.mark_needs_review("cancelled").is_err());
    assert!(stage.cleanup_owned().is_err());
    assert_eq!(stdfs::read(stage.journal_path()).unwrap(), b"USER JOURNAL");
}

#[test]
fn successful_cleanup_never_removes_final_target() {
    let fixture = Fixture::new();
    let mut stage = fixture.download("final", b"keep forever");
    let final_path = stage.commit().unwrap();
    let directory = stage.staging_directory().to_owned();
    stage.cleanup_owned().unwrap();
    assert!(!directory.exists());
    assert_eq!(stdfs::read(final_path).unwrap(), b"keep forever");
    stage.cleanup_owned().unwrap();
}

#[test]
fn failed_stage_is_left_for_review_on_drop() {
    let fixture = Fixture::new();
    let stage = fixture.download("abandoned", b"partial");
    let payload = stage.payload_path().to_owned();
    let journal = stage.journal_path().to_owned();
    drop(stage);
    assert_eq!(read_journal(&journal).state, TaskState::NeedsReview);
    assert_eq!(stdfs::read(payload).unwrap(), b"partial");
}

#[test]
fn explicit_failure_does_not_delete_stage() {
    let fixture = Fixture::new();
    let mut stage = fixture.download("cancelled", b"partial");
    stage.mark_needs_review("backend timeout").unwrap();
    assert!(stage.payload_path().exists());
    assert_eq!(
        read_journal(stage.journal_path()).failure.as_deref(),
        Some("backend timeout")
    );
    assert!(matches!(
        stage.commit(),
        Err(TransferError::InvalidState(TaskState::NeedsReview))
    ));
}

#[test]
fn atomic_journal_saves_leave_parseable_document() {
    let fixture = Fixture::new();
    let mut stage = fixture.download("file", b"abc");
    let old_identity = stage.stage.journal_identity;
    stage.mark_needs_review("cancelled").unwrap();
    assert_ne!(old_identity, stage.stage.journal_identity);
    let journal = read_journal(stage.journal_path());
    assert_eq!(journal.version, 1);
    assert_eq!(journal.owner, stage.journal().owner);
    assert_eq!(
        stdfs::read_dir(stage.staging_directory()).unwrap().count(),
        3
    );
}

#[test]
fn no_replace_race_has_exactly_one_winner() {
    let fixture = Fixture::new();
    let barrier = Arc::new(Barrier::new(12));
    let handles: Vec<_> = (0u8..12)
        .map(|index| {
            let root = fixture.root.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                let mut stage = DownloadStage::new(&root, "winner", 128).unwrap();
                stdfs::write(stage.payload_path(), [index; 128]).unwrap();
                barrier.wait();
                match stage.commit() {
                    Ok(path) => Some((index, path)),
                    Err(TransferError::Skipped(_)) => None,
                    Err(error) => panic!("unexpected publication error: {error}"),
                }
            })
        })
        .collect();
    let winners: Vec<_> = handles
        .into_iter()
        .filter_map(|handle| handle.join().unwrap())
        .collect();
    assert_eq!(winners.len(), 1);
    assert_eq!(stdfs::read(&winners[0].1).unwrap(), vec![winners[0].0; 128]);
}

#[test]
fn target_created_after_staging_is_not_overwritten() {
    let fixture = Fixture::new();
    let mut stage = fixture.download("race", b"loser");
    let target = fixture.root.join("race");
    stdfs::write(&target, b"winner").unwrap();
    assert!(matches!(stage.commit(), Err(TransferError::Skipped(_))));
    assert_eq!(stdfs::read(target).unwrap(), b"winner");
}

#[test]
fn repeated_commit_does_not_publish_again() {
    let fixture = Fixture::new();
    let mut stage = fixture.download("once", b"once");
    stage.commit().unwrap();
    assert!(matches!(
        stage.commit(),
        Err(TransferError::InvalidState(TaskState::Committed))
    ));
}

#[test]
fn upload_copies_source_with_random_remote_basename() {
    let fixture = Fixture::new();
    let source = fixture.root.join("private photo.JPG");
    stdfs::write(&source, b"upload bytes").unwrap();
    let cache = fixture.directory("cache");
    let mut stage = UploadStage::new(&source, &cache).unwrap();
    assert_eq!(stage.file_size(), 12);
    assert_ne!(stage.safe_name(), "private photo.JPG");
    assert!(stage.safe_name().starts_with("mtp-"));
    assert_eq!(stage.staged_path().extension(), Some(OsStr::new("JPG")));
    assert_eq!(stdfs::read(stage.staged_path()).unwrap(), b"upload bytes");
    assert_eq!(stage.journal().state, TaskState::Ready);
    stage.check_source_unchanged().unwrap();
    stage.validate_staged().unwrap();
    stage.cleanup_owned().unwrap();
    assert_eq!(stdfs::read(source).unwrap(), b"upload bytes");
}

#[test]
fn zero_byte_upload_is_valid() {
    let fixture = Fixture::new();
    let source = fixture.root.join("empty");
    stdfs::write(&source, b"").unwrap();
    let stage = UploadStage::new(&source, &fixture.directory("cache")).unwrap();
    assert_eq!(stage.file_size(), 0);
    assert_eq!(stdfs::metadata(stage.staged_path()).unwrap().len(), 0);
}

#[test]
fn upload_rejects_source_symlink() {
    let fixture = Fixture::new();
    let source = fixture.root.join("real");
    stdfs::write(&source, b"secret").unwrap();
    let link = fixture.root.join("link");
    symlink(source, &link).unwrap();
    assert!(UploadStage::new(&link, &fixture.directory("cache")).is_err());
}

#[test]
fn upload_rejects_source_ancestor_symlink() {
    let fixture = Fixture::new();
    let actual = fixture.directory("actual");
    stdfs::write(actual.join("source"), b"secret").unwrap();
    let alias = fixture.root.join("alias");
    symlink(actual, &alias).unwrap();
    assert!(UploadStage::new(&alias.join("source"), &fixture.directory("cache")).is_err());
}

#[test]
fn upload_rejects_nonregular_and_hardlinked_source() {
    let fixture = Fixture::new();
    let cache = fixture.directory("cache");
    assert!(UploadStage::new(&fixture.directory("directory-source"), &cache).is_err());
    let source = fixture.root.join("source");
    stdfs::write(&source, b"data").unwrap();
    stdfs::hard_link(&source, fixture.root.join("alias")).unwrap();
    assert!(matches!(
        UploadStage::new(&source, &cache),
        Err(TransferError::NotRegular)
    ));
}

#[test]
fn upload_detects_source_size_change_snapshot_is_unchanged() {
    let fixture = Fixture::new();
    let source = fixture.root.join("source");
    stdfs::write(&source, b"old").unwrap();
    let mut stage = UploadStage::new(&source, &fixture.directory("cache")).unwrap();
    stdfs::write(&source, b"different size").unwrap();
    assert!(matches!(
        stage.check_source_unchanged(),
        Err(TransferError::SourceChanged)
    ));
    assert_eq!(stdfs::read(stage.staged_path()).unwrap(), b"old");
    assert_eq!(
        read_journal(stage.journal_path()).state,
        TaskState::NeedsReview
    );
}

#[test]
fn upload_detects_same_size_change_using_metadata_snapshot() {
    let fixture = Fixture::new();
    let source = fixture.root.join("source");
    stdfs::write(&source, b"old").unwrap();
    let mut stage = UploadStage::new(&source, &fixture.directory("cache")).unwrap();
    // Force a deterministic metadata difference without timing sleeps.
    let times = std::fs::FileTimes::new().set_modified(SystemTime::UNIX_EPOCH);
    File::options()
        .write(true)
        .open(&source)
        .unwrap()
        .set_times(times)
        .unwrap();
    assert!(matches!(
        stage.check_source_unchanged(),
        Err(TransferError::SourceChanged)
    ));
}

#[test]
fn upload_detects_source_inode_replacement() {
    let fixture = Fixture::new();
    let source = fixture.root.join("source");
    stdfs::write(&source, b"old").unwrap();
    let mut stage = UploadStage::new(&source, &fixture.directory("cache")).unwrap();
    assert!(source.is_absolute());
    assert_eq!(source.parent(), Some(fixture.root.as_path()));
    assert_eq!(
        FileIdentity::of(&stdfs::symlink_metadata(&source).unwrap()),
        stage.snapshot.identity
    );
    stdfs::remove_file(&source).unwrap();
    stdfs::write(&source, b"new").unwrap();
    assert!(matches!(
        stage.check_source_unchanged(),
        Err(TransferError::SourceChanged)
    ));
}

#[test]
fn upload_rejects_symlink_cache() {
    let fixture = Fixture::new();
    let source = fixture.root.join("source");
    stdfs::write(&source, b"data").unwrap();
    let actual = fixture.directory("cache");
    let alias = fixture.root.join("cache-alias");
    symlink(actual, &alias).unwrap();
    assert!(UploadStage::new(&source, &alias).is_err());
}

#[test]
fn upload_random_names_are_unique_and_portable() {
    let fixture = Fixture::new();
    let source = fixture.root.join("source.bad-extension!");
    stdfs::write(&source, b"data").unwrap();
    let cache = fixture.directory("cache");
    let a = UploadStage::new(&source, &cache).unwrap();
    let b = UploadStage::new(&source, &cache).unwrap();
    assert_ne!(a.safe_name(), b.safe_name());
    assert!(
        a.safe_name()
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    );
    validate_basename(a.safe_name()).unwrap();
}

#[test]
fn cleanup_partial_stage_only_removes_owned_files() {
    let fixture = Fixture::new();
    let target = fixture.root.join("existing");
    stdfs::write(&target, b"user").unwrap();
    let mut stage = fixture.download("existing", b"partial");
    stage.mark_needs_review("cancelled").unwrap();
    let path = stage.staging_directory().to_owned();
    stage.cleanup_owned().unwrap();
    assert!(!path.exists());
    assert_eq!(stdfs::read(target).unwrap(), b"user");
}

#[test]
fn download_borrowed_file_writes_bound_inode_not_rebound_path() {
    let fixture = Fixture::new();
    let destination = fixture.directory("destination");
    let mut stage = DownloadStage::new(&destination, "payload", 4).unwrap();
    let original_directory = fixture.root.join("original-destination");
    assert!(destination.is_absolute());
    assert_eq!(destination.parent(), Some(fixture.root.as_path()));
    assert_eq!(original_directory.parent(), Some(fixture.root.as_path()));
    assert_eq!(
        FileIdentity::of(&stdfs::metadata(&destination).unwrap()),
        stage.journal().parent_identity
    );
    stdfs::rename(&destination, &original_directory).unwrap();
    let user_directory = fixture.directory("user-directory");
    let fake_stage = user_directory.join(stage.staging_directory().file_name().unwrap());
    stdfs::create_dir(&fake_stage).unwrap();
    let user_payload = fake_stage.join(stage.safe_name());
    stdfs::write(&user_payload, b"USER BYTES").unwrap();
    symlink(&user_directory, &destination).unwrap();
    let mut file = stage.payload_file();
    assert_eq!(
        FileIdentity::of(&file.metadata().unwrap()),
        stage.journal().payload_identity
    );
    file.write_all(b"SAFE").unwrap();
    assert_eq!(stdfs::read(&user_payload).unwrap(), b"USER BYTES");
    let actual_payload = original_directory
        .join(stage.journal().stage_name.clone())
        .join(stage.safe_name());
    assert_eq!(stdfs::read(actual_payload).unwrap(), b"SAFE");
    assert!(stage.commit().is_err());
    assert_eq!(stdfs::read(user_payload).unwrap(), b"USER BYTES");
}

#[test]
fn upload_borrowed_file_is_snapshot_and_starts_at_zero() {
    let fixture = Fixture::new();
    let source = fixture.root.join("source");
    stdfs::write(&source, b"SNAPSHOT").unwrap();
    let stage = UploadStage::new(&source, &fixture.directory("cache")).unwrap();
    stdfs::write(&source, b"CHANGED!").unwrap();
    let mut file = stage.staged_file();
    assert_eq!(
        FileIdentity::of(&file.metadata().unwrap()),
        stage.journal().payload_identity
    );
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"SNAPSHOT");
}

#[test]
fn upload_recheck_missing_source_is_source_changed() {
    let fixture = Fixture::new();
    let source = fixture.root.join("source");
    stdfs::write(&source, b"data").unwrap();
    let mut stage = UploadStage::new(&source, &fixture.directory("cache")).unwrap();
    assert!(source.is_absolute());
    assert_eq!(source.parent(), Some(fixture.root.as_path()));
    assert_eq!(
        FileIdentity::of(&stdfs::symlink_metadata(&source).unwrap()),
        stage.snapshot.identity
    );
    stdfs::remove_file(&source).unwrap();
    assert!(matches!(
        stage.check_source_unchanged(),
        Err(TransferError::SourceChanged)
    ));
    assert_eq!(stage.journal().state, TaskState::NeedsReview);
    assert_eq!(stdfs::read(stage.staged_path()).unwrap(), b"data");
}

#[test]
fn upload_recheck_added_hardlink_is_source_changed() {
    let fixture = Fixture::new();
    let source = fixture.root.join("source");
    stdfs::write(&source, b"data").unwrap();
    let mut stage = UploadStage::new(&source, &fixture.directory("cache")).unwrap();
    stdfs::hard_link(&source, fixture.root.join("new-alias")).unwrap();
    assert!(matches!(
        stage.check_source_unchanged(),
        Err(TransferError::SourceChanged)
    ));
    assert_eq!(stage.journal().state, TaskState::NeedsReview);
    assert_eq!(stdfs::read(source).unwrap(), b"data");
}

#[test]
fn upload_recheck_symlink_replacement_is_source_changed_without_following() {
    let fixture = Fixture::new();
    let source = fixture.root.join("source");
    stdfs::write(&source, b"data").unwrap();
    let mut stage = UploadStage::new(&source, &fixture.directory("cache")).unwrap();
    let user = fixture.root.join("user");
    stdfs::write(&user, b"USER").unwrap();
    assert!(source.is_absolute());
    assert_eq!(source.parent(), Some(fixture.root.as_path()));
    assert_eq!(
        FileIdentity::of(&stdfs::symlink_metadata(&source).unwrap()),
        stage.snapshot.identity
    );
    stdfs::remove_file(&source).unwrap();
    symlink(&user, &source).unwrap();
    assert!(matches!(
        stage.check_source_unchanged(),
        Err(TransferError::SourceChanged)
    ));
    assert_eq!(stdfs::read(stage.staged_path()).unwrap(), b"data");
    assert_eq!(stdfs::read(user).unwrap(), b"USER");
}

#[test]
fn stage_permissions_are_private() {
    let fixture = Fixture::new();
    let stage = fixture.download("file", b"data");
    assert_eq!(
        stdfs::metadata(stage.staging_directory()).unwrap().mode() & 0o077,
        0
    );
    assert_eq!(
        stdfs::metadata(stage.payload_path()).unwrap().mode() & 0o077,
        0
    );
    assert_eq!(
        stdfs::metadata(stage.journal_path()).unwrap().mode() & 0o077,
        0
    );
}
