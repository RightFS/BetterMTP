# Kalam read-only adapter slice

## Scope and validation status

The `mtp-backend-kalam` crate is a narrow dynamic C ABI boundary, not a complete MTP backend. It does not link, copy, redistribute, or build the legacy Go/USB libraries. The explicit caller-provided absolute library path is the only selection mechanism: no OS/version/architecture guessing, bundled fallback, or default historical seg5 library.

This slice permits only Initialize, FetchDeviceInfo, FetchStorages, Walk, FileExists and Dispose. There is no callable upload, download, delete, rename, mkdir, overwrite, replace or cancel API. `SingleFileTransferPlan` / `TransferDirection` are typed planning data only; they cannot invoke native code, do not assert destination safety, and are not serialized legacy transfer requests. Worker normalization, generations, timeout/termination, protocol, and CLI path selection belong to the parent implementation.

Evidence baseline and release gates remain in [kalam-integration.md](kalam-integration.md). Legacy OpenMTP is read-only. The controlled C fixture does **not** represent a real device, a Go runtime, or prove any dylib/USB compatibility. This adapter task compiled both C fixture variants with `cc -std=c11 -Wall -Wextra -Werror -dynamiclib` on arm64 macOS; it did not execute Cargo, load historical Kalam, or access a device. Parent owns Cargo compilation/test results; update validation claims only from those actual results. Source fixture is [kalam_fixture.c](../crates/backend-kalam/tests/fixtures/kalam_fixture.c).

## Parent integration API

Dependencies in [crate manifest](../crates/backend-kalam/Cargo.toml): `libloading = '=0.8.9'`, workspace `serde`, `serde_json`, `thiserror`. `free`, `dup`, `dup2` are direct C externs, so there is no new libc dependency. The binding crate deliberately does not inherit the workspace `unsafe_code = forbid`; it denies `unsafe_op_in_unsafe_fn`. All native unsafe stays here; core, worker, protocol and application must retain their existing unsafe prohibition.

Public API in [lib.rs](../crates/backend-kalam/src/lib.rs):

```rust
use mtp_backend_kalam::{Kalam, WalkRequest, ExistsRequest};

// Once, at worker startup, before native loading or any concurrent threads:
let mut protocol_stdout = mtp_backend_kalam::isolate_native_stdout()?;
// Use this returned File for ALL protocol output; native stdout now -> stderr.
// Configure stderr as null at the parent process boundary unless deliberately
// enabled for diagnostics (native text can contain private device paths).
let mut kalam = Kalam::load(explicit_absolute_library_path)?;
let initialized = kalam.initialize()?;
let device = kalam.device_info()?;
let storages = kalam.storages()?;
let entries = kalam.walk(&WalkRequest::directory(storage_id, "/"))?;
let exists = kalam.exists(&ExistsRequest {
    storage_id, files: vec!["/some-file".into()],
})?;
let disposed = kalam.dispose()?;
```

All six operations return `Result<serde_json::Value, KalamError>`. Successful Values are **complete raw envelopes**, not just `data`; native capitalization and extra fields are preserved. The adapter checks an object envelope with string `errorType` and `error`; either nonempty value is an explicit `KalamError::Native`. Missing/mistyped fields and malformed JSON are errors. No operation or callback name is treated as proof of success. Device data must contain object `mtpDeviceInfo` and `usbDeviceInfo`; list operations accept array or null (Go nil slice); Dispose must return true. Per-entry/storage vendor schemas and reference normalization remain worker responsibilities.

Optional typed `KDevice` keeps each vendor device-info object as Value. `KEntry` models the documented legacy walk fields including u32 `object_id`/`parent_id` and signed i64 `size`; these are raw native data, not yet validated domain references. Storage stays raw Value to avoid inventing an unverified third-party Go storage schema.

`WalkRequest` fields: `storage_id`, `full_path`, `recursive`, `skip_disallowed_files`, `skip_hidden_files`, serialized camelCase. `directory` is nonrecursive, skip disallowed=true, skip hidden=false. `ExistsRequest` fields: `storage_id` and `files`; JSON is `storageId` and **`Files`** (actual Go struct tag). Paths must be canonical absolute POSIX paths; root `/` is valid, while relative paths, NUL, backslash, empty interior/trailing components and `.` / `..` are rejected before native invocation. Requests and callback payloads are bounded to 1 MiB. Listings larger than that return a clear limit error rather than growing unbounded memory.

### Owner and lifecycle

- `Kalam` is deliberately `!Send + !Sync`, with `&mut self` operations: one serial thread owner.
- A process-global mutex allows only one **ever loaded** Kalam instance in that worker. No sharing Go globals between different sessions/libraries, even sequentially after Drop. A loader failure before obtaining a Library can be retried; after obtaining any Library, even missing required symbols means a new worker is required.
- Once loaded, the `Library` immediately enters `ManuallyDrop`. Its destructor is intentionally never invoked, including early symbol-resolution error paths. Dispose releases native device references, not the Go runtime mapping. The OS reclaims the mapping at worker process exit.
- Loaded -> initialize once -> Ready. Initialization failure can already have allocated USB resources, so it transitions to Failed, with cleanup permitted. Any native/ABI/envelope failure in a read operation also transitions to Failed; no subsequent read/initialize is allowed. Local request validation failure does not invalidate Ready.
- Dispose is terminal, even on error; uncertain cleanup is never retried. Calling Dispose before any initialize attempt performs local cleanup only and returns a successful envelope without invoking Go's uninitialized cleanup. Explicit Dispose is preferred to observe errors; Drop provides one best-effort Dispose after an initialize attempt if not already terminal, containing Rust unwind and ignoring cleanup errors. Native hangs, process aborts, faults, and Go panics cannot be recovered by this Rust Result contract.
- Worker timeout policy must terminate/quarantine the worker; there is no native Cancel export. Do not unload Go as a timeout or recovery strategy. Do not automatically retry uncertain writes in future slices.

### Stdout isolation

`isolate_native_stdout() -> io::Result<File>` is Unix-only. `dup(1)` creates a uniquely owned protocol descriptor. `dup2(2,1)` redirects process-global stdout to the already configured diagnostic sink. On failure, only the new duplicate is closed by File RAII; existing stdout is never explicitly closed. Closed stderr is rejected (including the case where dup temporarily filled fd 2). The worker must call this once before loading: dylib constructors and legacy Go dependencies can print even though the Rust adapter itself never prints. Returning the protocol duplicate avoids unsafe fd manipulation in the worker and preserves its core unsafe ban. Windows reports Unsupported; native adapter loading additionally requires 64-bit Unix.

This is process-wide isolation, not a thread-local logger change. Do not invoke it in the UI parent or a shared process. The parent controls stderr visibility and any native-error redaction in normalized responses.

## ABI and ownership safety contract

The inspected [legacy header](../../openmtp/build/mac/bin/arm64/kalam.h) declares `on_cb_result_t*`, where the typedef is itself a function pointer. However [send_to_js shim](../../openmtp/ffi/kalam/native/send_to_js/main.go) casts the **argument address itself** to `on_cb_result_t` and calls it; it never dereferences a function-pointer slot. Rust represents the argument as opaque `*mut c_void` and passes the callback function's address, not `&callback`. The C fixture reproduces exactly this unusual header/shim combination.

Native library trust is required: signatures, C allocation validity, synchronous callbacks and platform function/data pointer conventions cannot be proven from arbitrary caller-provided machine code. This is why loading is confined to an explicitly selected, isolated worker. A Rust safe API does not make loading untrusted native code safe.

Ownership table:

| Allocation/reference | Owner while call runs | Release policy |
| --- | --- | --- |
| Input CString JSON | Rust call scope | Go may read but not retain/free; Rust releases after synchronous export returns |
| Callback `C.CString(json)` | Transferred by native to callback | Callback takes ownership immediately, bounds scan/copies/parses and calls C `free` exactly once before callback returns |
| Copied JSON Value | Rust | Normal Rust ownership; never borrows native string |
| Each repeated callback's distinct CString | Independently transferred to callback | Freed even though operation returns DuplicateCallback |
| Error, invalid UTF-8/JSON, oversize payload | Callback | Same free rule; no early-return leak |
| Null callback payload | No allocation | Returns NullCallback, no free(null) necessary |
| Pinned dynamic library/runtime | Worker process | Deliberate ManuallyDrop retention until OS teardown |

The callback contains Rust unwind with `catch_unwind` around copying, parsing and state recording; its ownership guard sits outside that closure, so panic still releases the native string. Mutex poison is recovered without another unwind. OOM/allocator abort, malicious pointer access and native faults are process failures, not recoverable callback errors.

The cap is **1,048,576 JSON bytes, excluding the terminating NUL**. The callback reads at most cap+1 bytes to locate NUL and copies at most cap bytes. This is not a defense against an invalid/unterminated native allocation: trusted native must provide a readable C.CString. Exactly-cap JSON is accepted. ErrorType/error checking happens on the Rust-owned copy, after native free.

Only the inspected synchronous read-only exports are supported. Every operation must finish its terminal callback(s) before returning. A global callback slot stays valid for the process lifetime, avoiding a Rust stack context escaping into native code. Missing callback is detected on return; repeated callbacks are a Result error and every distinct allocation is released. An observed callback outside an active invocation is freed and permanently poisons future native calls. Since this ABI has no user context/generation token, a malicious or unsupported delayed callback overlapping a different call cannot be reliably attributed; arbitrary asynchronous exports are therefore expressly outside the contract. Transfers' old goroutine/progress machinery is **not** bound in this slice.

Native must transfer each allocation exactly once. Reusing an already-freed pointer, freeing a transferred string itself, allocating with an incompatible allocator, or mutating/retaining input after return violates the ABI ownership contract. No Rust wrapper can safely dereference/free such pointers. The inspected shim uses C.CString, hence system C malloc/free is the chosen release pair.

## Controlled tests and remaining gates

[Test implementation](../crates/backend-kalam/src/tests.rs) compiles the C fixture with system `cc` only when tests run. There is no production build script or compiler invocation during a normal crate build. Every actual library load scenario runs in a fresh subprocess; tests do not bypass or reset the permanent single-owner marker.

Coverage scenarios: exact function-address ABI; explicit absolute path and missing load/symbol errors; input casing/JSON and canonical paths; typed device/walk data; initialization error plus cleanup; native message/errorType propagation; malformed JSON/envelope/data; null/duplicate/missing callback; over-limit and exactly-limit JSON; Go nil list; ownership release on successful/error/duplicate/oversize paths; uninitialized local Dispose without native invocation; Drop cleanup; terminal Dispose failure and no retry; permanent no-reload guard; late-callback poisoning; stdout separation and closed-stderr failure. A test-only atomic increments immediately after actual C free returns and is compared with the fixture's actual malloc allocation count, so error/duplicate release is tested without a fake deallocator. The fixture uses separate malloc strings for duplicate callbacks; intentionally double-freeing the same pointer is not a valid native contract test.

The parent should run `cargo test -p mtp-backend-kalam` after registering the workspace member and installing/resolving the exact dependency versions. The `cc` fixture toolchain is required; missing compiler fails the test rather than silently claiming ABI validation. Subprocess tests capture output and report it only on failure. Test fixture artifacts live in unique OS temporary directories and are not distributed production libraries.

Remaining real gates: parent Cargo validation; actual worker protocol lifecycle tests; vetted native source/license/build chain; process-isolated real dylib Initialize/Dispose smoke test; real device identity/storage/read-only listing checks; arm64/x86_64 release validation. Safe single-file transfers need typed generation checks, staging destinations, overwrite-independent primitives, destination policy, journaling and cancellation/uncertain-result rules before exposing any callable transfer method. The fixture cannot substitute for those gates.
