#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Isolated count-only inspection C ABI tests. No real native library/device.
use crate::*;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::Ordering;

#[test]
#[cfg(all(unix, target_pointer_width = "64"))]
fn inspection_fixture_contract() {
    let directory = temp_directory();
    for (name, flags, scenarios) in [
        ("legacy", vec![], vec!["unsupported"]),
        (
            "v1",
            vec!["-DSAFETY_INSPECT", "-DSAFETY_VERSION=1"],
            vec!["unsupported"],
        ),
        (
            "v3",
            vec!["-DSAFETY_INSPECT", "-DSAFETY_VERSION=3"],
            vec!["unsupported"],
        ),
        ("v2-no-inspect", vec!["-DSAFETY_FD_V2"], vec!["unsupported"]),
        ("v2-inspect-only", vec!["-DSAFETY_INSPECT"], vec!["normal"]),
        (
            "v2-full",
            vec!["-DSAFETY_FD_V2", "-DSAFETY_INSPECT"],
            vec![
                "normal",
                "local-validation",
                "writes",
                "incomplete",
                "negative",
                "over-u64",
                "fraction",
                "phase",
                "operation",
                "private-field",
                "native",
                "duplicate",
                "missing",
                "json",
                "null",
                "oversize",
                "private-envelope",
                "max-u64",
                "control-label",
                "unexpected",
            ],
        ),
    ] {
        let path = compile(&directory, name, &flags);
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
    let directory = std::env::temp_dir().join(format!(
        "mtp-inspection-fixture-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir(&directory).expect("unique fixture directory");
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
    let output = command
        .args(flags)
        .arg(source)
        .arg("-o")
        .arg(&path)
        .output()
        .expect("compile inspection fixture");
    assert!(
        output.status.success(),
        "fixture compile: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    path
}

fn run_child(path: &Path, scenario: &str) {
    let output = Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "inspect_tests::fixture_child",
            "--ignored",
            "--nocapture",
        ])
        .env("MTP_KALAM_INSPECT_LIBRARY", path)
        .env("MTP_KALAM_INSPECT_SCENARIO", scenario)
        .output()
        .expect("isolated inspection subprocess");
    assert!(
        output.status.success(),
        "inspection scenario {scenario} failed\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn request(parent_id: Option<u32>, destination: &str) -> InspectUploadDestinationRequest {
    InspectUploadDestinationRequest {
        storage_id: 65537,
        parent_id,
        destination: destination.into(),
    }
}

#[test]
#[ignore = "subprocess driver for inspection_fixture_contract"]
fn fixture_child() {
    let Some(path) = std::env::var_os("MTP_KALAM_INSPECT_LIBRARY") else {
        return;
    };
    let scenario = std::env::var("MTP_KALAM_INSPECT_SCENARIO").expect("scenario");
    let mut kalam = Kalam::load(&PathBuf::from(path)).expect("controlled C fixture");
    assert!(matches!(
        kalam.inspect_upload_destination(&request(None, "/")),
        Err(KalamError::State { .. })
    ));
    kalam.initialize().expect("fixture initialize");
    // SAFETY: fixture-only known instrumentation signatures, library pinned.
    let set_mode: unsafe extern "C" fn(std::ffi::c_int) =
        unsafe { symbol(&kalam._library, b"FixtureSetMode\0", "FixtureSetMode") }
            .expect("mode instrumentation");
    let allocations: unsafe extern "C" fn() -> u32 = unsafe {
        symbol(
            &kalam._library,
            b"FixtureAllocations\0",
            "FixtureAllocations",
        )
    }
    .expect("allocation instrumentation");
    let inspection_calls = unsafe {
        kalam
            ._library
            .get::<unsafe extern "C" fn() -> u32>(b"FixtureInspectionCalls\0")
    }
    .ok()
    .map(|value| *value);
    let transfer_calls = unsafe {
        kalam
            ._library
            .get::<unsafe extern "C" fn() -> u32>(b"FixtureTransferCalls\0")
    }
    .ok()
    .map(|value| *value);
    let balanced = || {
        assert_eq!(
            FREED_PAYLOADS.load(Ordering::SeqCst),
            unsafe { allocations() } as usize
        )
    };
    let no_transfers = || {
        if let Some(count) = transfer_calls {
            assert_eq!(
                unsafe { count() },
                0,
                "inspection never dispatches a transfer/FD operation"
            );
        }
    };
    let count = || inspection_calls.map_or(0, |count| unsafe { count() });

    if scenario == "unsupported" {
        assert!(!kalam.supports_inspect());
        assert!(matches!(
            kalam.inspect_upload_destination(&request(None, "/")),
            Err(KalamError::InspectionUnavailable)
        ));
        assert_eq!(count(), 0);
        no_transfers();
        kalam
            .storages()
            .expect("legacy/version mismatch remains read compatible");
        kalam.dispose().expect("dispose");
        balanced();
        return;
    }
    assert!(kalam.supports_inspect());
    if scenario == "normal" {
        for (parent, destination) in [
            (None, "/"),
            (Some(0), "/"),
            (Some(u32::MAX), "/"),
            (Some(55), "/tmp"),
        ] {
            let value = kalam
                .inspect_upload_destination(&request(parent, destination))
                .expect("strict independent root/nonroot JSON ABI");
            assert_eq!(value["data"]["noWrites"], true);
            assert_eq!(value["data"]["complete"], true);
            assert_eq!(value["data"]["destinationChildCount"].as_u64(), Some(3));
            assert_eq!(value["data"]["existingHandleCount"].as_u64(), Some(100));
            assert_eq!(
                value["data"].as_object().expect("count-only object").len(),
                6
            );
        }
        assert_eq!(count(), 4);
    } else if scenario == "local-validation" {
        for storage in [0, u32::MAX] {
            let mut invalid = request(None, "/");
            invalid.storage_id = storage;
            assert!(matches!(
                kalam.inspect_upload_destination(&invalid),
                Err(KalamError::Input(_))
            ));
        }
        for path in ["relative", "/a/../b", "/a//b", "/a/", "/a\\b", "/a\0b"] {
            assert!(matches!(
                kalam.inspect_upload_destination(&request(Some(55), path)),
                Err(KalamError::Input(_))
            ));
        }
        for (parent, path) in [
            (None, "/tmp"),
            (Some(0), "/tmp"),
            (Some(u32::MAX), "/tmp"),
            (Some(55), "/"),
        ] {
            assert!(matches!(
                kalam.inspect_upload_destination(&request(parent, path)),
                Err(KalamError::Input(_))
            ));
        }
        for bytes in [4096, MAX_CALLBACK_BYTES] {
            let oversized = request(Some(55), &format!("/{}", "x".repeat(bytes)));
            assert!(matches!(
                kalam.inspect_upload_destination(&oversized),
                Err(KalamError::Input(_))
            ));
        }
        assert_eq!(count(), 0, "local validation precedes any native call");
        kalam.storages().expect("local errors do not quarantine");
    } else if scenario == "unexpected" {
        unsafe extern "C" {
            fn malloc(size: usize) -> *mut std::ffi::c_void;
        }
        let payload = unsafe { malloc(3) }.cast::<std::ffi::c_char>();
        assert!(!payload.is_null());
        unsafe { std::ptr::copy_nonoverlapping(c"{}".as_ptr().cast::<u8>(), payload.cast(), 3) };
        terminal_callback(payload);
        assert!(matches!(
            kalam.inspect_upload_destination(&request(None, "/")),
            Err(KalamError::UnexpectedCallback)
        ));
        assert_eq!(count(), 0);
        no_transfers();
        assert_eq!(
            FREED_PAYLOADS.load(Ordering::SeqCst),
            unsafe { allocations() } as usize + 1
        );
        assert!(matches!(kalam.storages(), Err(KalamError::State { .. })));
        assert!(matches!(
            kalam.dispose(),
            Err(KalamError::UnexpectedCallback)
        ));
        return;
    } else {
        let mode = match scenario.as_str() {
            "writes" => 50,
            "incomplete" => 51,
            "negative" => 52,
            "over-u64" => 53,
            "fraction" => 54,
            "phase" => 55,
            "operation" => 56,
            "private-field" => 57,
            "native" => 58,
            "duplicate" => 59,
            "missing" => 60,
            "json" => 61,
            "null" => 62,
            "oversize" => 63,
            "private-envelope" => 64,
            "max-u64" => 65,
            "control-label" => 66,
            _ => panic!("unknown inspection fixture scenario"),
        };
        unsafe { set_mode(mode) };
        let result = kalam.inspect_upload_destination(&request(Some(55), "/tmp"));
        match mode {
            58 => {
                let error = result.expect_err("native read failure");
                assert!(!error.to_string().contains("private"));
                assert!(!error.to_string().contains("tmp"));
                match error {
                    KalamError::Native {
                        error_type,
                        data: Some(data),
                        ..
                    } => {
                        assert_eq!(error_type, "ErrorGeneral");
                        assert_eq!(data["noWrites"], true);
                        assert_eq!(data["phase"], "snapshotExistingHandles");
                        assert_eq!(data["operation"], "GetObjectHandles(allStoragesAllParents)");
                    }
                    other => panic!("lost private diagnostic metadata: {other}"),
                }
            }
            59 => assert!(matches!(result, Err(KalamError::DuplicateCallback))),
            60 => assert!(matches!(result, Err(KalamError::MissingCallback))),
            61 => assert!(matches!(result, Err(KalamError::Json(_)))),
            62 => assert!(matches!(result, Err(KalamError::NullCallback))),
            63 => assert!(matches!(result, Err(KalamError::CallbackTooLarge))),
            65 => {
                let value = result.expect("precise u64 max count");
                assert_eq!(
                    value["data"]["existingHandleCount"].as_u64(),
                    Some(u64::MAX)
                );
            }
            _ => {
                let error = result.expect_err("invalid count-only result");
                assert!(matches!(error, KalamError::Envelope(_)));
                assert!(!error.to_string().contains("private"));
            }
        }
        assert_eq!(count(), 1);
        if mode != 65 {
            assert!(
                matches!(kalam.storages(), Err(KalamError::State { .. })),
                "native/protocol failures quarantine"
            );
        }
        unsafe { set_mode(0) };
    }
    no_transfers();
    balanced();
    kalam.dispose().expect("dispose");
    balanced();
}
