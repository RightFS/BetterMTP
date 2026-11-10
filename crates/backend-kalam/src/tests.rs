#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use std::process::{Command, Output};
use std::sync::atomic::Ordering;

// All actual loads run in distinct subprocesses: production's permanent owner
// guard must not be disabled/reset simply to make unit tests pass in one process.
#[test]
#[cfg(all(unix, target_pointer_width = "64"))]
fn c_fixture_contract() {
    let directory = fixture_directory();
    let library = compile_fixture(&directory, false);
    let missing = compile_fixture(&directory, true);
    for scenario in [
        "normal",
        "native",
        "duplicate",
        "missing",
        "json",
        "null",
        "oversize",
        "envelope",
        "error-type",
        "shape",
        "exact-limit",
        "nil-list",
        "init-failure",
        "dispose-failure",
        "dispose-uninitialized",
        "drop",
        "stdout",
        "stdout-closed-stderr",
        "unexpected",
    ] {
        run_child(&library, scenario);
    }
    run_child(&missing, "missing-symbol");
}

fn fixture_directory() -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let directory =
        std::env::temp_dir().join(format!("mtp-kalam-fixture-{}-{stamp}", std::process::id()));
    std::fs::create_dir(&directory).expect("create unique fixture directory");
    directory
}

fn compile_fixture(directory: &Path, missing: bool) -> PathBuf {
    let suffix = if cfg!(target_os = "macos") {
        "dylib"
    } else {
        "so"
    };
    let output = directory.join(format!(
        "{}.{suffix}",
        if missing { "missing" } else { "fixture" }
    ));
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/kalam_fixture.c");
    let mut command = Command::new("cc");
    command.args(["-std=c11", "-Wall", "-Wextra", "-Werror"]);
    if cfg!(target_os = "macos") {
        command.arg("-dynamiclib");
    } else {
        command.args(["-shared", "-fPIC"]);
    }
    if missing {
        command.arg("-DOMIT_WALK");
    }
    let result = command
        .arg(source)
        .arg("-o")
        .arg(&output)
        .output()
        .expect("cc is required for C ABI tests");
    assert!(
        result.status.success(),
        "C fixture compile failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    output
}

fn run_child(path: &Path, scenario: &str) -> Output {
    let output = Command::new(std::env::current_exe().expect("unit test executable"))
        .args([
            "--exact",
            "tests::fixture_child",
            "--ignored",
            "--nocapture",
        ])
        .env("MTP_KALAM_TEST_LIBRARY", path)
        .env("MTP_KALAM_TEST_SCENARIO", scenario)
        .output()
        .expect("run isolated fixture subprocess");
    assert!(
        output.status.success(),
        "fixture scenario {scenario} failed\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if scenario == "stdout" {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stdout.contains("fixture-protocol-safe"));
        assert!(!stdout.contains("fixture-native-stdout"));
        assert!(stderr.contains("fixture-native-stdout"));
    }
    output
}

#[test]
#[ignore = "subprocess driver; c_fixture_contract supplies explicit fixture environment"]
fn fixture_child() {
    let Some(path) = std::env::var_os("MTP_KALAM_TEST_LIBRARY") else {
        return;
    };
    let scenario = std::env::var("MTP_KALAM_TEST_SCENARIO").expect("fixture scenario");
    let path = PathBuf::from(path);
    assert!(matches!(
        Kalam::load(Path::new("relative.so")),
        Err(KalamError::LibraryPath)
    ));
    assert!(matches!(
        Kalam::load(&path.with_file_name("nonexistent-library")),
        Err(KalamError::Load { .. })
    ));

    if scenario == "missing-symbol" {
        assert!(matches!(
            Kalam::load(&path),
            Err(KalamError::MissingSymbol { symbol: "Walk", .. })
        ));
        assert!(matches!(
            Kalam::load(&path),
            Err(KalamError::OwnerAlreadyClaimed)
        ));
        return;
    }
    #[cfg(unix)]
    if scenario == "stdout-closed-stderr" {
        unsafe extern "C" {
            fn close(fd: c_int) -> c_int;
        }
        // SAFETY: this is an isolated subprocess and fd 2 is intentionally closed.
        assert_eq!(unsafe { close(2) }, 0);
        assert!(isolate_native_stdout().is_err());
        return;
    }
    let mut protocol = if scenario == "stdout" {
        Some(isolate_native_stdout().expect("isolate stdout"))
    } else {
        None
    };
    let mut kalam = Kalam::load(&path).expect("load only controlled fixture");
    assert!(matches!(
        Kalam::load(&path),
        Err(KalamError::OwnerAlreadyClaimed)
    ));
    assert!(matches!(kalam.storages(), Err(KalamError::State { .. })));
    // SAFETY: private fixture-only instrumentation signatures, library pinned.
    let set_mode: unsafe extern "C" fn(c_int) =
        unsafe { symbol(&kalam._library, b"FixtureSetMode\0", "FixtureSetMode") }
            .expect("mode fixture symbol");
    let allocations: unsafe extern "C" fn() -> u32 = unsafe {
        symbol(
            &kalam._library,
            b"FixtureAllocations\0",
            "FixtureAllocations",
        )
    }
    .expect("allocation fixture symbol");
    let native_print: unsafe extern "C" fn() =
        unsafe { symbol(&kalam._library, b"FixturePrint\0", "FixturePrint") }
            .expect("print fixture symbol");
    let freed = || FREED_PAYLOADS.load(Ordering::SeqCst);
    // Counts increment immediately after real C free returns, not a mock free.
    let balanced = || assert_eq!(freed(), unsafe { allocations() } as usize);

    if scenario == "dispose-uninitialized" {
        assert_eq!(
            kalam.dispose().expect("local uninitialized cleanup")["data"],
            true
        );
        assert_eq!(
            freed(),
            0,
            "uninitialized Dispose must not call native code"
        );
        balanced();
        assert!(matches!(kalam.initialize(), Err(KalamError::State { .. })));
        assert!(matches!(kalam.dispose(), Err(KalamError::State { .. })));
        return;
    }

    if scenario == "init-failure" {
        unsafe { set_mode(1) };
        assert!(matches!(kalam.initialize(), Err(KalamError::Native { .. })));
        assert!(matches!(kalam.initialize(), Err(KalamError::State { .. })));
        balanced();
        unsafe { set_mode(0) };
        assert_eq!(
            kalam.dispose().expect("cleanup partial initialization")["data"],
            true
        );
        balanced();
        return;
    }

    let initialized = kalam.initialize().expect("initialize fixture");
    let device: KDevice =
        serde_json::from_value(initialized["data"].clone()).expect("typed fixture device");
    assert_eq!(device.mtp_device_info["SerialNumber"], "fixture-only");
    assert!(matches!(kalam.initialize(), Err(KalamError::State { .. })));
    balanced();

    if scenario == "normal" {
        kalam.device_info().expect("device info");
        assert_eq!(
            kalam.storages().expect("storages")["data"][0]["StorageId"],
            65537
        );
        let walk = kalam
            .walk(&WalkRequest::directory(65537, "/"))
            .expect("walk ABI and JSON input");
        let entries: Vec<KEntry> =
            serde_json::from_value(walk["data"].clone()).expect("typed entries");
        assert_eq!(entries[0].object_id, 42);
        assert_eq!(entries[0].size, 3);
        let exists = kalam
            .exists(&ExistsRequest {
                storage_id: 65537,
                files: vec!["/a.txt".into()],
            })
            .expect("exists ABI and capital Files input");
        assert_eq!(exists["data"][0]["exists"], true);
        for invalid in [
            "relative", "/../x", "/./x", "/x//y", "/x/", "/x\\y", "/x\0y",
        ] {
            assert!(matches!(
                kalam.walk(&WalkRequest::directory(65537, invalid)),
                Err(KalamError::Input(_))
            ));
        }
        assert!(matches!(
            kalam.exists(&ExistsRequest {
                storage_id: 65537,
                files: vec![]
            }),
            Err(KalamError::Input(_))
        ));
        // Local input validation does not invalidate a ready native owner.
        kalam.storages().expect("ready after local input error");
    } else if scenario == "stdout" {
        use std::io::Write;
        unsafe { native_print() };
        writeln!(
            protocol.as_mut().expect("duplicate protocol File"),
            "fixture-protocol-safe"
        )
        .expect("write protocol");
    } else if scenario == "drop" {
        let before = freed();
        drop(kalam);
        assert_eq!(
            freed(),
            before + 1,
            "Drop made one Dispose callback and freed payload"
        );
        assert!(matches!(
            Kalam::load(&path),
            Err(KalamError::OwnerAlreadyClaimed)
        ));
        balanced();
        return;
    } else if scenario == "dispose-failure" {
        unsafe { set_mode(9) };
        assert!(matches!(kalam.dispose(), Err(KalamError::Envelope(_))));
        let before = freed();
        assert!(matches!(kalam.dispose(), Err(KalamError::State { .. })));
        drop(kalam);
        assert_eq!(freed(), before, "uncertain cleanup is never retried");
        balanced();
        return;
    } else if scenario == "unexpected" {
        // SAFETY: valid transferred malloc pointer delivered without an active
        // operation; late callbacks must be freed and poison further calls.
        unsafe extern "C" {
            fn malloc(size: usize) -> *mut c_void;
        }
        let payload = unsafe { malloc(3) }.cast::<c_char>();
        assert!(!payload.is_null());
        unsafe { std::ptr::copy_nonoverlapping(c"{}".as_ptr().cast::<u8>(), payload.cast(), 3) };
        terminal_callback(payload);
        assert!(matches!(
            kalam.storages(),
            Err(KalamError::UnexpectedCallback)
        ));
        assert!(matches!(
            kalam.dispose(),
            Err(KalamError::UnexpectedCallback)
        ));
        return;
    } else {
        let mode = match scenario.as_str() {
            "native" => 1,
            "duplicate" => 2,
            "missing" => 3,
            "json" => 4,
            "null" => 5,
            "oversize" => 6,
            "envelope" => 7,
            "error-type" => 8,
            "shape" => 9,
            "exact-limit" => 10,
            "nil-list" => 11,
            _ => panic!("unknown scenario {scenario}"),
        };
        unsafe { set_mode(mode) };
        let result = kalam.storages();
        match scenario.as_str() {
            "native" | "error-type" => assert!(matches!(result, Err(KalamError::Native { .. }))),
            "duplicate" => assert!(matches!(result, Err(KalamError::DuplicateCallback))),
            "missing" => assert!(matches!(result, Err(KalamError::MissingCallback))),
            "json" => assert!(matches!(result, Err(KalamError::Json(_)))),
            "null" => assert!(matches!(result, Err(KalamError::NullCallback))),
            "oversize" => assert!(matches!(result, Err(KalamError::CallbackTooLarge))),
            "envelope" | "shape" => assert!(matches!(result, Err(KalamError::Envelope(_)))),
            "exact-limit" => assert_eq!(
                result.expect("exactly 1 MiB is accepted")["padding"]
                    .as_str()
                    .expect("padding")
                    .len(),
                MAX_CALLBACK_BYTES - 50
            ),
            "nil-list" => assert!(result.expect("Go nil slice is empty data")["data"].is_null()),
            _ => unreachable!(),
        }
        balanced(); // Includes malformed/error/oversize and both duplicate strings.
        if mode < 10 {
            assert!(matches!(kalam.storages(), Err(KalamError::State { .. })));
        }
        unsafe { set_mode(0) };
    }
    assert_eq!(kalam.dispose().expect("explicit dispose")["data"], true);
    balanced();
    assert!(matches!(kalam.storages(), Err(KalamError::State { .. })));
    assert!(matches!(kalam.dispose(), Err(KalamError::State { .. })));
    assert!(matches!(
        Kalam::load(&path),
        Err(KalamError::OwnerAlreadyClaimed)
    ));
}

#[test]
fn request_and_envelope_contract() {
    let input = input_json(&ExistsRequest {
        storage_id: 1,
        files: vec!["/unicode-手机".into()],
    })
    .expect("serialize input");
    let value: Value = serde_json::from_slice(input.as_bytes()).expect("JSON input");
    assert_eq!(value["Files"][0], "/unicode-手机");
    assert!(value.get("files").is_none());
    assert!(
        validate_envelope(serde_json::json!({"errorType":"", "error":"", "data":true})).is_ok()
    );
    assert!(matches!(
        validate_envelope(serde_json::json!({"errorType":"", "error":"bad", "data":true})),
        Err(KalamError::Native { .. })
    ));
    assert!(matches!(
        validate_envelope(serde_json::json!({"errorType":7, "error":"", "data":true})),
        Err(KalamError::Envelope(_))
    ));
    assert!(matches!(
        validate_envelope(serde_json::json!({"errorType":"", "error":""})),
        Err(KalamError::Envelope(_))
    ));
    let oversized = ExistsRequest {
        storage_id: 1,
        files: vec![format!("/{}", "x".repeat(MAX_CALLBACK_BYTES))],
    };
    assert!(matches!(input_json(&oversized), Err(KalamError::Input(_))));
}
