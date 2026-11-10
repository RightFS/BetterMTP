#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]
use mtp_application::{ApplicationError, BackendConfig, WorkerClient};
use mtp_domain::{DomainError, StorageId};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn build(flags: &[&str]) -> Self {
        let path = std::env::temp_dir().join(format!(
            "mtp-worker-client-test-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        let library = path.join(if cfg!(target_os = "macos") {
            "fixture.dylib"
        } else {
            "fixture.so"
        });
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../backend-kalam/tests/fixtures/kalam_fixture.c");
        let mut command = Command::new("cc");
        command.args(["-std=c11", "-Wall", "-Wextra", "-Werror"]);
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
        Self(library)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let directory = self.0.parent().unwrap();
        assert_eq!(directory.parent(), Some(std::env::temp_dir().as_path()));
        assert!(
            directory
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("mtp-worker-client-test-")
        );
        let _ = fs::remove_dir_all(directory);
    }
}
fn worker() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_mtp-worker"))
}

#[test]
fn application_tokens_reject_objects_from_a_previous_worker_process() {
    let mut first = WorkerClient::spawn(worker(), &BackendConfig::Fake).unwrap();
    let old = first.connect().unwrap();
    let old_root = old.root(StorageId(1));
    first.shutdown().unwrap();
    let mut replacement = WorkerClient::spawn(worker(), &BackendConfig::Fake).unwrap();
    let current = replacement.connect().unwrap();
    assert_ne!(old.generation, current.generation);
    assert!(matches!(
        replacement.list(&old_root),
        Err(ApplicationError::Domain(DomainError::StaleSession { .. }))
    ));
    assert_eq!(
        replacement.list(&current.root(StorageId(1))).unwrap().len(),
        2
    );
    replacement.shutdown().unwrap();
}

#[test]
fn real_worker_controlled_c_abi_fixture_supports_browsing_and_shutdown() {
    let fixture = Fixture::build(&[]);
    let mut client = WorkerClient::spawn(
        worker(),
        &BackendConfig::Kalam {
            library_path: fixture.0.clone(),
        },
    )
    .unwrap();
    assert_eq!(client.hello().backend, "kalam");
    assert!(!client.hello().capabilities.simulated);
    assert!(!client.hello().capabilities.upload);
    assert!(!client.hello().capabilities.download);
    let session = client.connect().unwrap();
    let storages = client.storages().unwrap();
    assert_eq!(storages.len(), 1);
    assert_eq!(storages[0].id, 65537);
    let entries = client.list(&session.root(StorageId(65537))).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "a.txt");
    assert_eq!(entries[0].byte_size, Some(3));
    client.shutdown().unwrap();
}

#[test]
fn native_stdout_is_isolated_from_the_binary_control_transport() {
    let fixture = Fixture::build(&["-DFIXTURE_CHATTER"]);
    let mut client = WorkerClient::spawn(
        worker(),
        &BackendConfig::Kalam {
            library_path: fixture.0.clone(),
        },
    )
    .unwrap();
    let session = client.connect().unwrap();
    assert!(!session.capabilities.simulated);
    assert_eq!(client.storages().unwrap().len(), 1);
    client.shutdown().unwrap();
}

#[test]
fn stalled_native_call_is_killed_and_cannot_reuse_its_session() {
    let fixture = Fixture::build(&["-DFIXTURE_STALL"]);
    let mut client = WorkerClient::spawn(
        worker(),
        &BackendConfig::Kalam {
            library_path: fixture.0.clone(),
        },
    )
    .unwrap();
    // Library preload/handshake may contend with parallel fixture subprocesses.
    // Only the deliberately stalled native Initialize gets the short deadline.
    client.set_deadline(Duration::from_secs(1));
    assert!(matches!(
        client.connect(),
        Err(ApplicationError::TimedOut(_))
    ));
    assert!(client.session().is_none());
    assert!(matches!(
        client.connect(),
        Err(ApplicationError::Disconnected)
    ));
}

#[test]
fn invalid_native_library_never_silently_falls_back_to_fake() {
    let path = std::env::temp_dir().join(format!("mtp-nonexistent-library-{}", std::process::id()));
    assert!(!path.exists());
    let mut client =
        WorkerClient::spawn(worker(), &BackendConfig::Kalam { library_path: path }).unwrap();
    assert_eq!(client.hello().backend, "kalam");
    assert!(matches!(
        client.connect(),
        Err(ApplicationError::Backend { .. })
    ));
    assert!(client.session().is_none());
}
