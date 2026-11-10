#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]
//! True child-worker service tests with controlled C code, never real USB.
use mtp_application::{
    ApplicationError, BackendConfig, FileConflictPolicy, TransferDirection, WorkerClient,
};
use mtp_backend_api::{ErrorCode, LocalPath};
use mtp_domain::{DomainError, StorageId};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    directory: PathBuf,
    library: PathBuf,
}
impl Fixture {
    fn new(flags: &[&str]) -> Self {
        let temp = fs::canonicalize(std::env::temp_dir()).unwrap();
        let directory = temp.join(format!(
            "mtp-fd-worker-test-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        let library = directory.join(if cfg!(target_os = "macos") {
            "fixture.dylib"
        } else {
            "fixture.so"
        });
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/transfer_fixture.c");
        let mut command = Command::new("cc");
        command.args(["-std=c11", "-Wall", "-Wextra", "-Werror"]);
        if flags.contains(&"-DWORKER_MUTATE_SOURCE") {
            // Test-only local path provisioned at compilation, never in the
            // real worker/native transfer request or process-wide environment.
            let source =
                serde_json::to_string(directory.join("report.txt").to_str().unwrap()).unwrap();
            command.arg(format!("-DWORKER_MUTATE_SOURCE_PATH={source}"));
        }
        if cfg!(target_os = "macos") {
            command.arg("-dynamiclib");
        } else {
            command.args(["-shared", "-fPIC"]);
        }
        let result = command
            .args(flags)
            .arg(source)
            .arg("-o")
            .arg(&library)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        Self { directory, library }
    }
    fn dir(&self, name: &str) -> PathBuf {
        let directory = self.directory.join(name);
        fs::create_dir(&directory).unwrap();
        directory
    }
    fn client(&self) -> WorkerClient {
        WorkerClient::spawn(
            Path::new(env!("CARGO_BIN_EXE_mtp-worker")),
            &BackendConfig::Kalam {
                library_path: self.library.clone(),
            },
        )
        .unwrap()
    }
    fn connected(
        &self,
    ) -> (
        WorkerClient,
        mtp_domain::RemoteDirectoryRef,
        mtp_application::RemoteEntry,
    ) {
        let mut client = self.client();
        assert!(client.hello().capabilities.upload && client.hello().capabilities.download);
        let session = client.connect().unwrap();
        client.storages().unwrap();
        let root = session.root(StorageId(65537));
        let source = client.list(&root).unwrap().remove(0);
        (client, root, source)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        assert_eq!(
            self.directory.parent(),
            Some(fs::canonicalize(std::env::temp_dir()).unwrap().as_path())
        );
        assert!(
            self.directory
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("mtp-fd-worker-test-")
        );
        let _ = fs::remove_dir_all(&self.directory);
    }
}
fn journal(path: &LocalPath) -> serde_json::Value {
    serde_json::from_slice(&fs::read(path.to_path_buf()).unwrap()).unwrap()
}
fn journals(directory: &Path) -> Vec<serde_json::Value> {
    fs::read_dir(directory)
        .unwrap()
        .filter_map(|entry| {
            let path = entry.unwrap().path();
            if !path.is_dir() || !path.file_name()?.to_str()?.starts_with(".mtp-stage-") {
                return None;
            }
            let file = fs::read_dir(path)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .find(|path| {
                    path.file_name()
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .starts_with(".journal-")
                        && path
                            .extension()
                            .is_some_and(|extension| extension == "json")
                })?;
            Some(serde_json::from_slice(&fs::read(file).unwrap()).unwrap())
        })
        .collect()
}

#[test]
fn fd_upload_snapshots_content_and_saves_verified_receipt_without_touching_source() {
    let fixture = Fixture::new(&[]);
    let (mut client, root, _) = fixture.connected();
    let source = fixture.directory.join("report.txt");
    fs::write(&source, b"payload").unwrap();
    let cache = fixture.dir("cache");
    let receipt = client
        .upload_file(&source, "report.txt", &root, &cache)
        .unwrap();
    assert_eq!(receipt.direction, TransferDirection::Upload);
    assert_eq!(receipt.bytes, 7);
    assert_eq!(receipt.object_id, Some(123));
    assert_eq!(receipt.local_path, None);
    assert_eq!(
        receipt.actual_name,
        "report-mtp-0123456789abcdef0123456789abcdef.txt"
    );
    assert_eq!(fs::read(&source).unwrap(), b"payload");
    let journal = journal(&receipt.journal_path);
    assert_eq!(journal["state"], "Committed");
    assert_eq!(journal["native_dispatched"], true);
    assert_eq!(journal["remote_upload"]["object_id"], 123);
    let payload = receipt
        .journal_path
        .to_path_buf()
        .parent()
        .unwrap()
        .join(journal["payload_name"].as_str().unwrap());
    assert_eq!(fs::read(payload).unwrap(), fs::read(source).unwrap());
    client.shutdown().unwrap();
}

#[test]
fn captured_snapshot_upload_completes_even_if_original_changes_during_native_call() {
    let fixture = Fixture::new(&["-DWORKER_MUTATE_SOURCE"]);
    let (mut client, root, _) = fixture.connected();
    let source = fixture.directory.join("report.txt");
    fs::write(&source, b"payload").unwrap();
    let receipt = client
        .upload_file(&source, "report.txt", &root, &fixture.dir("cache"))
        .unwrap();
    assert_eq!(receipt.direction, TransferDirection::Upload);
    assert_eq!(receipt.bytes, 7);
    assert_eq!(receipt.object_id, Some(123));
    assert_eq!(fs::read(&source).unwrap(), b"changed");
    let record = journal(&receipt.journal_path);
    assert_eq!(record["state"], "Committed");
    assert_eq!(record["publication_confirmed"], true);
    assert_eq!(record["native_dispatched"], true);
    assert!(record["failure"].is_null());
    let staged = receipt
        .journal_path
        .to_path_buf()
        .parent()
        .unwrap()
        .join(record["payload_name"].as_str().unwrap());
    assert_eq!(fs::read(staged).unwrap(), b"payload");
    assert!(client.session().is_some());
    client.shutdown().unwrap();
}

#[test]
fn fd_download_publishes_exact_content_and_preserves_committed_journal() {
    let fixture = Fixture::new(&[]);
    let (mut client, _, source) = fixture.connected();
    let destination = fixture.dir("destination");
    let receipt = client
        .download_file(&source.reference, &destination, FileConflictPolicy::Skip)
        .unwrap();
    assert_eq!(receipt.direction, TransferDirection::Download);
    assert_eq!(receipt.bytes, 7);
    assert_eq!(receipt.actual_name, "a.txt");
    assert_eq!(
        fs::read(receipt.local_path.unwrap().to_path_buf()).unwrap(),
        b"payload"
    );
    let record = journal(&receipt.journal_path);
    assert_eq!(record["state"], "Committed");
    assert_eq!(record["native_dispatched"], true);
    assert_eq!(record["publication_confirmed"], true);
    client.shutdown().unwrap();
}

#[test]
fn zero_byte_upload_and_download_are_real_verified_empty_results() {
    let fixture = Fixture::new(&["-DWORKER_EMPTY"]);
    let (mut client, root, remote) = fixture.connected();
    let source = fixture.directory.join("report.txt");
    fs::write(&source, []).unwrap();
    let uploaded = client
        .upload_file(&source, "report.txt", &root, &fixture.dir("cache"))
        .unwrap();
    assert_eq!(uploaded.bytes, 0);
    assert_eq!(journal(&uploaded.journal_path)["native_dispatched"], true);
    let downloaded = client
        .download_file(
            &remote.reference,
            &fixture.dir("destination"),
            FileConflictPolicy::Skip,
        )
        .unwrap();
    assert_eq!(downloaded.bytes, 0);
    assert!(
        fs::read(downloaded.local_path.unwrap().to_path_buf())
            .unwrap()
            .is_empty()
    );
    assert!(fs::read(source).unwrap().is_empty());
    client.shutdown().unwrap();
}

#[test]
fn skip_existing_target_is_not_success_and_never_clobbers() {
    let fixture = Fixture::new(&[]);
    let (mut client, _, remote) = fixture.connected();
    let destination = fixture.dir("destination");
    fs::write(destination.join("a.txt"), b"original").unwrap();
    assert!(matches!(
        client.download_file(&remote.reference, &destination, FileConflictPolicy::Skip),
        Err(ApplicationError::Backend {
            code: ErrorCode::Skipped,
            requires_review: false,
            ..
        })
    ));
    assert_eq!(fs::read(destination.join("a.txt")).unwrap(), b"original");
    assert!(client.session().is_some());
    assert_eq!(journals(&destination)[0]["state"], "Skipped");
    client.shutdown().unwrap();
}

#[test]
fn keep_both_returns_published_name_without_altering_existing_content() {
    let fixture = Fixture::new(&[]);
    let (mut client, _, remote) = fixture.connected();
    let destination = fixture.dir("destination");
    fs::write(destination.join("a.txt"), b"original").unwrap();
    let receipt = client
        .download_file(
            &remote.reference,
            &destination,
            FileConflictPolicy::KeepBoth,
        )
        .unwrap();
    assert_ne!(receipt.actual_name, "a.txt");
    assert_eq!(fs::read(destination.join("a.txt")).unwrap(), b"original");
    assert_eq!(
        fs::read(receipt.local_path.unwrap().to_path_buf()).unwrap(),
        b"payload"
    );
    client.shutdown().unwrap();
}

#[test]
fn nonroot_refs_pass_native_cached_parent_id_and_path_together() {
    let fixture = Fixture::new(&["-DWORKER_NONROOT"]);
    let (mut client, _, directory) = fixture.connected();
    assert!(directory.directory);
    let parent = mtp_domain::RemoteDirectoryRef::from(directory.reference);
    let remote = client.list(&parent).unwrap().remove(0);
    let source = fixture.directory.join("report.txt");
    fs::write(&source, b"payload").unwrap();
    let uploaded = client
        .upload_file(&source, "report.txt", &parent, &fixture.dir("cache"))
        .unwrap();
    assert!(
        journal(&uploaded.journal_path)["remote_upload"]["path"]
            .as_str()
            .unwrap()
            .starts_with("/folder/")
    );
    let downloaded = client
        .download_file(
            &remote.reference,
            &fixture.dir("destination"),
            FileConflictPolicy::Skip,
        )
        .unwrap();
    assert_eq!(
        fs::read(downloaded.local_path.unwrap().to_path_buf()).unwrap(),
        b"payload"
    );
    client.shutdown().unwrap();
}

#[test]
fn bad_filename_and_relative_paths_fail_before_dispatch() {
    let fixture = Fixture::new(&[]);
    let (mut client, root, remote) = fixture.connected();
    let cache = fixture.dir("cache");
    let source = fixture.directory.join("report.txt");
    fs::write(&source, b"payload").unwrap();
    assert!(matches!(
        client.upload_file(&source, "../report.txt", &root, &cache),
        Err(ApplicationError::Domain(DomainError::InvalidRemoteFilename))
    ));
    assert!(matches!(
        client.upload_file(Path::new("relative"), "report.txt", &root, &cache),
        Err(ApplicationError::Io(_))
    ));
    assert!(matches!(
        client.download_file(
            &remote.reference,
            Path::new("relative"),
            FileConflictPolicy::Skip
        ),
        Err(ApplicationError::Io(_))
    ));
    assert_eq!(fs::read_dir(cache).unwrap().count(), 0);
    assert!(client.session().is_some());
    client.shutdown().unwrap();
}

#[test]
fn symlink_source_and_destination_ancestor_never_write_victim() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new(&[]);
    let (mut client, root, remote) = fixture.connected();
    let victim = fixture.directory.join("victim.txt");
    fs::write(&victim, b"payload").unwrap();
    let source_link = fixture.directory.join("report.txt");
    symlink(&victim, &source_link).unwrap();
    assert!(matches!(
        client.upload_file(&source_link, "report.txt", &root, &fixture.dir("cache")),
        Err(ApplicationError::Backend {
            requires_review: false,
            ..
        })
    ));
    let victim_directory = fixture.dir("victim-directory");
    let destination_link = fixture.directory.join("linked-directory");
    symlink(&victim_directory, &destination_link).unwrap();
    assert!(matches!(
        client.download_file(
            &remote.reference,
            &destination_link,
            FileConflictPolicy::Skip
        ),
        Err(ApplicationError::Backend {
            requires_review: false,
            ..
        })
    ));
    assert_eq!(fs::read(&victim).unwrap(), b"payload");
    assert_eq!(fs::read_dir(victim_directory).unwrap().count(), 0);
    client.shutdown().unwrap();
}

#[test]
fn symlink_target_skip_does_not_modify_victim() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new(&[]);
    let (mut client, _, remote) = fixture.connected();
    let destination = fixture.dir("destination");
    let victim = fixture.directory.join("victim");
    fs::write(&victim, b"victim-original").unwrap();
    symlink(&victim, destination.join("a.txt")).unwrap();
    assert!(matches!(
        client.download_file(&remote.reference, &destination, FileConflictPolicy::Skip),
        Err(ApplicationError::Backend {
            code: ErrorCode::Skipped,
            ..
        })
    ));
    assert_eq!(fs::read(victim).unwrap(), b"victim-original");
    client.shutdown().unwrap();
}

#[test]
fn stale_refs_are_rejected_locally_before_upload_or_download() {
    let fixture = Fixture::new(&[]);
    let (mut old_client, old_root, old_remote) = fixture.connected();
    old_client.disconnect().unwrap();
    let (mut client, _, _) = fixture.connected();
    let source = fixture.directory.join("report.txt");
    fs::write(&source, b"payload").unwrap();
    let cache = fixture.dir("cache");
    assert!(matches!(
        client.upload_file(&source, "report.txt", &old_root, &cache),
        Err(ApplicationError::Domain(DomainError::StaleSession { .. }))
    ));
    let destination = fixture.dir("destination");
    assert!(matches!(
        client.download_file(
            &old_remote.reference,
            &destination,
            FileConflictPolicy::Skip
        ),
        Err(ApplicationError::Domain(DomainError::StaleSession { .. }))
    ));
    assert_eq!(fs::read_dir(cache).unwrap().count(), 0);
    assert_eq!(fs::read_dir(destination).unwrap().count(), 0);
    client.shutdown().unwrap();
}

#[test]
fn legacy_marker_capabilities_are_false_and_services_do_not_dispatch() {
    let fixture = Fixture::new(&["-DSAFETY_VERSION=1"]);
    let mut client = fixture.client();
    assert!(!client.hello().capabilities.upload && !client.hello().capabilities.download);
    let session = client.connect().unwrap();
    client.storages().unwrap();
    let root = session.root(StorageId(65537));
    let remote = client.list(&root).unwrap().remove(0);
    let source = fixture.directory.join("report.txt");
    fs::write(&source, b"payload").unwrap();
    let cache = fixture.dir("cache");
    let destination = fixture.dir("destination");
    assert!(matches!(
        client.upload_file(&source, "report.txt", &root, &cache),
        Err(ApplicationError::Backend {
            code: ErrorCode::Unsupported,
            ..
        })
    ));
    assert!(matches!(
        client.download_file(&remote.reference, &destination, FileConflictPolicy::Skip),
        Err(ApplicationError::Backend {
            code: ErrorCode::Unsupported,
            ..
        })
    ));
    assert_eq!(fs::read_dir(cache).unwrap().count(), 0);
    assert_eq!(fs::read_dir(destination).unwrap().count(), 0);
    client.shutdown().unwrap();
}

#[test]
fn native_uncertainty_preserves_private_metadata_in_journal_not_display() {
    let fixture = Fixture::new(&["-DWORKER_MODE=26"]);
    let (mut client, root, _) = fixture.connected();
    let source = fixture.directory.join("report.txt");
    fs::write(&source, b"payload").unwrap();
    let error = client
        .upload_file(&source, "report.txt", &root, &fixture.dir("cache"))
        .unwrap_err();
    assert!(!error.to_string().contains("/device/private"));
    let ApplicationError::UnknownOutcome {
        journal_path: Some(path),
    } = error
    else {
        panic!("must retain review journal");
    };
    let record = journal(&path);
    assert_eq!(record["state"], "NeedsReview");
    assert_eq!(record["native_dispatched"], true);
    let metadata: serde_json::Value =
        serde_json::from_str(record["failure"].as_str().unwrap()).unwrap();
    assert_eq!(metadata["nativeError"]["data"]["objectId"], 123);
    assert_eq!(
        metadata["nativeError"]["data"]["path"],
        "/device/private/report.txt"
    );
    assert!(client.session().is_none());
    assert_eq!(fs::read(source).unwrap(), b"payload");
}

#[test]
fn download_native_failure_never_publishes_or_overwrites_and_retains_stage() {
    let fixture = Fixture::new(&["-DWORKER_MODE=26"]);
    let (mut client, _, remote) = fixture.connected();
    let destination = fixture.dir("destination");
    fs::write(destination.join("a.txt"), b"original").unwrap();
    let error = client
        .download_file(
            &remote.reference,
            &destination,
            FileConflictPolicy::KeepBoth,
        )
        .unwrap_err();
    let ApplicationError::UnknownOutcome {
        journal_path: Some(path),
    } = error
    else {
        panic!("must retain review journal");
    };
    assert_eq!(journal(&path)["state"], "NeedsReview");
    assert_eq!(fs::read(destination.join("a.txt")).unwrap(), b"original");
    assert!(client.session().is_none());
}

#[test]
fn worker_death_during_upload_is_unknown_not_canceled_with_durable_dispatch_journal() {
    let fixture = Fixture::new(&["-DWORKER_DIE"]);
    let (mut client, root, _) = fixture.connected();
    let source = fixture.directory.join("report.txt");
    fs::write(&source, b"payload").unwrap();
    let cache = fixture.dir("cache");
    assert!(matches!(
        client.upload_file(&source, "report.txt", &root, &cache),
        Err(ApplicationError::UnknownOutcome { .. })
    ));
    assert!(client.session().is_none());
    let records = journals(&cache);
    let record = &records[0];
    // _exit skips Drop: Ready plus durable dispatch evidence still requires
    // review and must not be interpreted as an undispatched/retryable task.
    assert_eq!(record["state"], "Ready");
    assert_eq!(record["native_dispatched"], true);
    assert_eq!(fs::read(source).unwrap(), b"payload");
}

#[test]
fn download_timeout_is_unknown_and_durable_dispatch_evidence_survives_kill() {
    let fixture = Fixture::new(&["-DWORKER_STALL"]);
    let (mut client, _, remote) = fixture.connected();
    client.set_deadline(Duration::from_millis(100));
    let destination = fixture.dir("destination");
    assert!(matches!(
        client.download_file(&remote.reference, &destination, FileConflictPolicy::Skip),
        Err(ApplicationError::UnknownOutcome { .. })
    ));
    assert!(!destination.join("a.txt").exists());
    assert_eq!(journals(&destination)[0]["native_dispatched"], true);
    assert!(client.session().is_none());
}

#[test]
fn malformed_native_done_cannot_be_a_service_success_or_retried() {
    for flag in ["-DWORKER_MODE=32", "-DWORKER_MODE=33", "-DWORKER_MODE=34"] {
        let fixture = Fixture::new(&[flag]);
        let (mut client, _, remote) = fixture.connected();
        let destination = fixture.dir("destination");
        assert!(matches!(
            client.download_file(&remote.reference, &destination, FileConflictPolicy::Skip),
            Err(ApplicationError::UnknownOutcome {
                journal_path: Some(_)
            })
        ));
        assert!(!destination.join("a.txt").exists());
        assert_eq!(journals(&destination)[0]["state"], "NeedsReview");
        assert!(client.session().is_none());
    }
}

#[test]
fn successful_write_receipts_from_wrong_raw_generation_are_unknown_not_success() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new(&[]);
    let program = fixture.directory.join("protocol-worker");
    fs::write(
        &program,
        include_bytes!("fixtures/wrong_generation_worker.py"),
    )
    .unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
    let source = fixture.directory.join("report.txt");
    fs::write(&source, b"payload").unwrap();
    let cache = fixture.dir("cache");
    let destination = fixture.dir("destination");
    for upload in [true, false] {
        let mut client = WorkerClient::spawn(
            &program,
            &BackendConfig::Kalam {
                library_path: fixture.library.clone(),
            },
        )
        .unwrap();
        let session = client.connect().unwrap();
        let result = if upload {
            client.upload_file(
                &source,
                "report.txt",
                &session.root(StorageId(65537)),
                &cache,
            )
        } else {
            let remote = mtp_domain::RemoteRef::new(
                session.device_key,
                session.generation,
                StorageId(65537),
                mtp_domain::ObjectId(42),
            );
            client.download_file(&remote, &destination, FileConflictPolicy::Skip)
        };
        assert!(matches!(
            result,
            Err(ApplicationError::UnknownOutcome { .. })
        ));
        assert!(client.session().is_none());
    }
    assert_eq!(fs::read_dir(cache).unwrap().count(), 0);
    assert_eq!(fs::read_dir(destination).unwrap().count(), 0);
    assert_eq!(fs::read(source).unwrap(), b"payload");
}

// macOS APFS rejects creating this byte sequence with EILSEQ; the codec
// itself remains covered by platform-independent in-memory protocol tests.
#[cfg(target_os = "linux")]
#[test]
fn explicit_remote_name_does_not_lossily_reconstruct_non_utf8_source_path() {
    use std::os::unix::ffi::OsStringExt;
    let fixture = Fixture::new(&[]);
    let (mut client, root, _) = fixture.connected();
    let source = fixture
        .directory
        .join(std::ffi::OsString::from_vec(b"local-\xff.txt".to_vec()));
    fs::write(&source, b"payload").unwrap();
    let receipt = client
        .upload_file(&source, "report.txt", &root, &fixture.dir("cache"))
        .unwrap();
    assert_eq!(receipt.bytes, 7);
    assert_eq!(fs::read(source).unwrap(), b"payload");
    assert_eq!(journal(&receipt.journal_path)["state"], "Committed");
    client.shutdown().unwrap();
}

#[test]
fn stop_handle_interrupts_transfer_without_waiting_for_client_mutex() {
    let fixture = Fixture::new(&["-DWORKER_STALL"]);
    let (mut client, _, remote) = fixture.connected();
    let stop = client.stop_handle();
    let destination = fixture.dir("destination");
    let threaded_destination = destination.clone();
    let thread = std::thread::spawn(move || {
        client.download_file(
            &remote.reference,
            &threaded_destination,
            FileConflictPolicy::Skip,
        )
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while !journals(&destination)
        .iter()
        .any(|record| record["native_dispatched"] == true)
    {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    let started = std::time::Instant::now();
    stop.request_stop().unwrap();
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(matches!(
        thread.join().unwrap(),
        Err(ApplicationError::UnknownOutcome { .. })
    ));
    assert_eq!(journals(&destination)[0]["native_dispatched"], true);
    assert!(!destination.join("a.txt").exists());
    stop.request_stop().unwrap();
}

#[test]
fn stop_handle_interrupts_stalled_connect_without_business_client_lock() {
    let fixture = Fixture::new(&["-DFIXTURE_STALL"]);
    let mut client = fixture.client();
    let stop = client.stop_handle();
    let thread = std::thread::spawn(move || client.connect());
    stop.request_stop().unwrap();
    assert!(matches!(
        thread.join().unwrap(),
        Err(ApplicationError::Disconnected | ApplicationError::Protocol(_))
    ));
}

#[test]
fn retained_stop_handle_on_completed_child_is_safe_and_idempotent() {
    let fixture = Fixture::new(&[]);
    let (client, _, _) = fixture.connected();
    let stop = client.stop_handle();
    client.shutdown().unwrap();
    stop.request_stop().unwrap();
    stop.request_stop().unwrap();
}

// APFS rejects invalid UTF-8 directory names before staging starts.
#[cfg(target_os = "linux")]
#[test]
fn byte_exact_non_utf8_local_directory_survives_wire_and_publication() {
    use std::os::unix::ffi::OsStringExt;
    let fixture = Fixture::new(&[]);
    let (mut client, _, remote) = fixture.connected();
    let destination = fixture
        .directory
        .join(std::ffi::OsString::from_vec(b"destination-\xff".to_vec()));
    fs::create_dir(&destination).unwrap();
    let receipt = client
        .download_file(&remote.reference, &destination, FileConflictPolicy::Skip)
        .unwrap();
    assert_eq!(
        receipt.local_path,
        Some(LocalPath::from_path(&destination.join("a.txt")).unwrap())
    );
    assert_eq!(fs::read(destination.join("a.txt")).unwrap(), b"payload");
    client.shutdown().unwrap();
}
