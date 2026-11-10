#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Controlled SafetyVersion 2 fixture tests. Not real USB/device validation.
use crate::*;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::Ordering;

#[test]
#[cfg(all(unix, target_pointer_width = "64"))]
fn safe_fd_fixture_contract() {
    let directory = temp_directory();
    for (variant, flags, scenarios) in [
        ("legacy", vec![], vec!["unavailable"]),
        (
            "v1",
            vec!["-DSAFETY_FD_V2", "-DSAFETY_VERSION=1"],
            vec!["unavailable"],
        ),
        (
            "v3",
            vec!["-DSAFETY_FD_V2", "-DSAFETY_VERSION=3"],
            vec!["unavailable"],
        ),
        (
            "no-upload",
            vec!["-DSAFETY_FD_V2", "-DOMIT_SAFE_UPLOAD"],
            vec!["no-upload"],
        ),
        (
            "no-download",
            vec!["-DSAFETY_FD_V2", "-DOMIT_SAFE_DOWNLOAD"],
            vec!["no-download"],
        ),
        (
            "v2",
            vec!["-DSAFETY_FD_V2"],
            vec![
                "normal",
                "local-validation",
                "wrong-bytes",
                "uncommitted",
                "review",
                "zero-id",
                "reserved-id",
                "unsafe-name",
                "metadata-error",
                "progress-json",
                "progress-limit",
                "stage-length",
                "wrong-id",
                "duplicate",
                "missing",
                "done-json",
                "progress-null",
                "progress-error",
                "original-name",
                "event-outside",
            ],
        ),
    ] {
        let path = compile(&directory, variant, &flags);
        for scenario in scenarios {
            run_child(&path, scenario);
        }
    }
}

fn temp_directory() -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let directory =
        std::env::temp_dir().join(format!("mtp-safe-fd-{}-{stamp}", std::process::id()));
    std::fs::create_dir(&directory).expect("private fixture directory");
    directory
}

fn compile(directory: &Path, name: &str, flags: &[&str]) -> PathBuf {
    let suffix = if cfg!(target_os = "macos") {
        "dylib"
    } else {
        "so"
    };
    let path = directory.join(format!("{name}.{suffix}"));
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/kalam_fixture.c");
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
        .arg(&path)
        .output()
        .expect("cc fixture");
    assert!(
        result.status.success(),
        "FD fixture compile failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    path
}

fn run_child(path: &Path, scenario: &str) {
    let output = Command::new(std::env::current_exe().expect("unit test executable"))
        .args([
            "--exact",
            "transfer_tests::fixture_child",
            "--ignored",
            "--nocapture",
        ])
        .env("MTP_KALAM_FD_LIBRARY", path)
        .env("MTP_KALAM_FD_SCENARIO", scenario)
        .output()
        .expect("fresh FD fixture subprocess");
    assert!(
        output.status.success(),
        "FD scenario {scenario} failed\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn upload_request() -> SafeUploadRequest {
    SafeUploadRequest {
        storage_id: 65537,
        parent_id: None,
        destination: "/".into(),
        requested_name: "report.txt".into(),
        policy: SafeUploadPolicy::KeepBoth,
        expected_size: 7,
    }
}
fn download_request() -> SafeDownloadRequest {
    SafeDownloadRequest {
        storage_id: 65537,
        object_id: 42,
        parent_id: Some(u32::MAX),
        expected_size: 7,
    }
}

#[cfg(unix)]
fn create_stage(path: &Path) -> File {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .expect("create owned stage")
}
fn read_descriptor(file: &File) -> Vec<u8> {
    let mut reader = file;
    reader
        .seek(SeekFrom::Start(0))
        .expect("original descriptor remains open and seekable");
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .expect("read original descriptor");
    bytes
}

#[test]
#[cfg(unix)]
#[ignore = "subprocess driver for safe_fd_fixture_contract"]
fn fixture_child() {
    let Some(path) = std::env::var_os("MTP_KALAM_FD_LIBRARY") else {
        return;
    };
    let scenario = std::env::var("MTP_KALAM_FD_SCENARIO").expect("scenario");
    let mut kalam = Kalam::load(&PathBuf::from(path)).expect("controlled fixture load");
    kalam.initialize().expect("fixture initialization");
    let set_mode: unsafe extern "C" fn(std::ffi::c_int) =
        unsafe { symbol(&kalam._library, b"FixtureSetMode\0", "FixtureSetMode") }
            .expect("fixture set mode");
    let allocations: unsafe extern "C" fn() -> u32 = unsafe {
        symbol(
            &kalam._library,
            b"FixtureAllocations\0",
            "FixtureAllocations",
        )
    }
    .expect("fixture allocations");
    let balanced = || {
        assert_eq!(
            FREED_PAYLOADS.load(Ordering::SeqCst),
            unsafe { allocations() } as usize
        )
    };
    let directory = temp_directory();
    let source_path = directory.join("snapshot");
    let mut staged = create_stage(&source_path);
    staged.write_all(b"payload").expect("upload snapshot bytes");
    drop(staged);
    let source = File::open(&source_path).expect("exclusively held immutable snapshot");
    let output_path = directory.join("download-stage");
    let output = create_stage(&output_path);
    let upload = upload_request();
    let download = download_request();

    if scenario == "unavailable" || scenario == "no-upload" || scenario == "no-download" {
        let can_upload = scenario == "no-download";
        let can_download = scenario == "no-upload";
        assert_eq!(kalam.supports_upload(), can_upload);
        assert_eq!(kalam.supports_download(), can_download);
        if !can_upload {
            assert!(matches!(
                kalam.upload(&upload, &source),
                Err(KalamError::TransferUnavailable { .. })
            ));
        }
        if !can_download {
            assert!(matches!(
                kalam.download(&download, &output),
                Err(KalamError::TransferUnavailable { .. })
            ));
        }
        assert_eq!(output.metadata().expect("stage still empty").len(), 0);
        kalam
            .storages()
            .expect("read-only compatibility remains ready");
        kalam.dispose().expect("dispose");
        balanced();
        return;
    }
    assert!(kalam.supports_upload() && kalam.supports_download());
    let calls: unsafe extern "C" fn() -> u32 = unsafe {
        symbol(
            &kalam._library,
            b"FixtureTransferCalls\0",
            "FixtureTransferCalls",
        )
    }
    .expect("transfer count");

    if scenario == "local-validation" {
        for name in ["", ".", "..", "a/b", "a\\b", "a\0b", "a\nb", "a\u{0085}b"] {
            let mut request = upload.clone();
            request.requested_name = name.into();
            assert!(matches!(
                kalam.upload(&request, &source),
                Err(KalamError::Input(_))
            ));
        }
        for destination in ["relative", "/../a", "/a//b", "/a/", "/a\\b", "/a\0b"] {
            let mut request = upload.clone();
            request.destination = destination.into();
            assert!(matches!(
                kalam.upload(&request, &source),
                Err(KalamError::Input(_))
            ));
        }
        for (destination, parent) in [
            ("/folder", None),
            ("/folder", Some(0)),
            ("/folder", Some(u32::MAX)),
            ("/", Some(99)),
        ] {
            let mut request = upload.clone();
            request.destination = destination.into();
            request.parent_id = parent;
            assert!(matches!(
                kalam.upload(&request, &source),
                Err(KalamError::Input(_))
            ));
        }
        let mut wrong = upload.clone();
        wrong.expected_size = 8;
        assert!(matches!(
            kalam.upload(&wrong, &source),
            Err(KalamError::Input(_))
        ));
        wrong.expected_size = i64::MAX as u64 + 1;
        assert!(matches!(
            kalam.upload(&wrong, &source),
            Err(KalamError::Input(_))
        ));
        for id in [0, u32::MAX] {
            let mut invalid = download.clone();
            invalid.object_id = id;
            assert!(matches!(
                kalam.download(&invalid, &output),
                Err(KalamError::Input(_))
            ));
            let mut invalid = upload.clone();
            invalid.storage_id = id;
            assert!(matches!(
                kalam.upload(&invalid, &source),
                Err(KalamError::Input(_))
            ));
        }
        let mut too_large = download.clone();
        too_large.expected_size = i64::MAX as u64 + 1;
        assert!(matches!(
            kalam.download(&too_large, &output),
            Err(KalamError::Input(_))
        ));
        let directory_fd = File::open(&directory).expect("directory descriptor");
        assert!(matches!(
            kalam.upload(&upload, &directory_fd),
            Err(KalamError::Input(_))
        ));
        assert!(matches!(
            kalam.download(&download, &directory_fd),
            Err(KalamError::Input(_))
        ));
        let mut writer = &output;
        writer.write_all(b"x").expect("nonempty stage");
        assert!(matches!(
            kalam.download(&download, &output),
            Err(KalamError::Input(_))
        ));
        let append_path = directory.join("append-stage");
        let append = OpenOptions::new()
            .append(true)
            .create_new(true)
            .mode(0o600)
            .open(append_path)
            .expect("append fd");
        assert!(matches!(
            kalam.download(&download, &append),
            Err(KalamError::Input(_))
        ));
        let readonly = File::open(&output_path).expect("readonly nonempty");
        assert!(matches!(
            kalam.download(&download, &readonly),
            Err(KalamError::Input(_))
        ));
        let writeonly = OpenOptions::new()
            .write(true)
            .open(&source_path)
            .expect("writeonly snapshot");
        assert!(matches!(
            kalam.upload(&upload, &writeonly),
            Err(KalamError::Input(_))
        ));
        assert_eq!(
            unsafe { calls() },
            0,
            "all local validation rejects before native"
        );
        kalam.storages().expect("local errors do not quarantine");
    } else if scenario == "normal" {
        // Replace both pathnames after descriptors are bound. The native API has
        // no source/output paths, and must touch only the original owned inode.
        let victim_path = directory.join("victim");
        std::fs::write(&victim_path, b"must-stay").expect("victim fixture");
        let source_ino = source.metadata().expect("source inode").ino();
        assert_eq!(source_path.parent(), Some(directory.as_path()));
        std::fs::rename(&source_path, directory.join("bound-snapshot"))
            .expect("rebind snapshot path");
        symlink(&victim_path, &source_path).expect("source path replaced with symlink");
        let value = kalam.upload(&upload, &source).expect("FD upload");
        assert_eq!(value["data"]["bytes"], 7);
        assert_eq!(value["data"]["objectId"], 123);
        assert_eq!(
            source.metadata().expect("original source fd open").ino(),
            source_ino
        );
        assert_eq!(read_descriptor(&source), b"payload");
        balanced(); // pre + repeated progress + one done each independently freed.
        let output_ino = output.metadata().expect("stage inode").ino();
        assert_eq!(output_path.parent(), Some(directory.as_path()));
        std::fs::rename(&output_path, directory.join("bound-download-stage"))
            .expect("rebind stage path");
        symlink(&victim_path, &output_path).expect("stage path replaced with symlink");
        let value = kalam.download(&download, &output).expect("FD download");
        assert_eq!(value["data"]["objectId"], 42);
        assert_eq!(
            output.metadata().expect("original stage fd open").ino(),
            output_ino
        );
        assert_eq!(read_descriptor(&output), b"payload");
        assert_eq!(
            std::fs::read(&victim_path).expect("victim remains untouched"),
            b"must-stay"
        );
        assert_eq!(unsafe { calls() }, 2);
    } else if scenario == "event-outside" {
        unsafe extern "C" {
            fn malloc(size: usize) -> *mut std::ffi::c_void;
        }
        let payload = unsafe { malloc(3) }.cast::<std::ffi::c_char>();
        assert!(!payload.is_null());
        unsafe { std::ptr::copy_nonoverlapping(c"{}".as_ptr().cast::<u8>(), payload.cast(), 3) };
        progress_callback(payload);
        assert!(matches!(
            kalam.upload(&upload, &source),
            Err(KalamError::UnexpectedCallback)
        ));
        assert_eq!(unsafe { calls() }, 0);
        assert!(matches!(
            kalam.dispose(),
            Err(KalamError::UnexpectedCallback)
        ));
        return; // extra explicit malloc is not counted by C fixture allocator.
    } else {
        let mode = match scenario.as_str() {
            "wrong-bytes" => 20,
            "uncommitted" => 21,
            "review" => 22,
            "zero-id" => 23,
            "reserved-id" => 24,
            "unsafe-name" => 25,
            "metadata-error" => 26,
            "progress-json" => 28,
            "progress-limit" => 29,
            "stage-length" => 30,
            "wrong-id" => 31,
            "duplicate" => 32,
            "missing" => 33,
            "done-json" => 34,
            "progress-null" => 35,
            "progress-error" => 36,
            "original-name" => 37,
            _ => panic!("unknown scenario"),
        };
        unsafe { set_mode(mode) };
        let result = if [25, 26, 37].contains(&mode) {
            kalam.upload(&upload, &source)
        } else {
            kalam.download(&download, &output)
        };
        match mode {
            26 => {
                let error = result.expect_err("uncertain transfer must fail");
                assert!(!error.to_string().contains("private"));
                assert!(!error.to_string().contains("report.txt"));
                match error {
                    KalamError::Native {
                        error_type,
                        data: Some(data),
                        ..
                    } => {
                        assert_eq!(error_type, "ErrorNeedsReview");
                        assert_eq!(data["needsReview"], true);
                        assert_eq!(data["bytes"], 3);
                        assert_eq!(data["path"], "/device/private/report.txt");
                    }
                    other => panic!("missing uncertainty data: {other}"),
                }
            }
            28 | 34 => assert!(matches!(result, Err(KalamError::Json(_)))),
            29 => assert!(matches!(result, Err(KalamError::CallbackTooLarge))),
            32 => assert!(matches!(result, Err(KalamError::DuplicateCallback))),
            33 => assert!(matches!(result, Err(KalamError::MissingCallback))),
            35 => assert!(matches!(result, Err(KalamError::NullCallback))),
            36 => assert!(matches!(result, Err(KalamError::Native { .. }))),
            _ => assert!(matches!(result, Err(KalamError::Envelope(_)))),
        }
        assert!(
            matches!(kalam.storages(), Err(KalamError::State { .. })),
            "all native/result errors quarantine"
        );
        assert!(
            source.metadata().is_ok() && output.metadata().is_ok(),
            "native never closes original descriptors"
        );
        unsafe { set_mode(0) };
    }
    balanced();
    kalam.dispose().expect("cleanup");
    balanced();
}

#[test]
fn safe_request_serialization() {
    let upload = serde_json::to_value(upload_request()).expect("upload JSON");
    assert_eq!(upload["policy"], "keepBoth");
    assert!(upload.get("parentId").is_some_and(Value::is_null));
    assert!(upload.get("source").is_none());
    let mut download = download_request();
    assert_eq!(
        serde_json::to_value(&download).expect("explicit root")["parentId"],
        u32::MAX
    );
    download.parent_id = None;
    assert!(
        serde_json::to_value(&download)
            .expect("unchecked parent")
            .get("parentId")
            .is_none()
    );
    assert!(serde_json::from_value::<SafeUploadPolicy>(serde_json::json!("replace")).is_err());
}
