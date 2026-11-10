# Kalam adapter slice — read-only compatibility and FD v2

## Scope and validation status

The `mtp-backend-kalam` crate is a narrow dynamic C ABI boundary, not a complete MTP backend. It does not link, copy, redistribute, or build the legacy Go/USB libraries. The explicit caller-provided absolute library path is the only selection mechanism: no OS/version/architecture guessing, bundled fallback, or default historical seg5 library. The caller must select a **known trusted source/artifact**; an exported marker is not authentication of arbitrary native code.

Required read-only exports remain Initialize, FetchDeviceInfo, FetchStorages, Walk, FileExists and Dispose. Optional exact-v2 `InspectUploadDestination` provides independent count-only read-only diagnostics, with no transfer fallback. Optional **SafetyVersion exactly 2** plus the corresponding SafeUploadSingle/SafeDownloadSingle export enables synchronous FD-only single-file transfer methods. No legacy UploadFiles/DownloadFiles symbols are resolved or called. There is no delete, rename, mkdir, overwrite, replace or cancel API. `SingleFileTransferPlan` / `TransferDirection` remain inert planning data, not transfer authorization. Worker normalization, generations, staging ownership, final local commit, timeout/termination, protocol, and CLI path selection belong to the parent implementation.

Evidence baseline and release gates remain in [kalam-integration.md](kalam-integration.md). The new native ABI and source constraints are documented in [native safety](kalam-native-safety.md), with the inspected [generated FD v2 header](../target/native/arm64/kalam.h). Legacy OpenMTP remains read-only. Controlled C fixtures do **not** represent a real device or a Go runtime and do not prove dylib/USB compatibility. On 2026-11-10 the earlier adapter-only FD baseline `cargo test -p mtp-backend-kalam` passed: four top-level tests, two ignored subprocess drivers; both old ABI scenarios and FD v2/version-gating scenarios ran via their drivers. Baseline `cargo clippy -p mtp-backend-kalam --all-targets -- -D warnings` also passed using installed stable Rust and the repository Cargo cache. The later **count-only inspection extension has only offline source/fixture compilation and rustfmt validation in this delegated task**: six controlled C variants compiled with `cc -std=c11 -Wall -Wextra -Werror -dynamiclib`; inspection Rust fixture tests await parent-unified Cargo, and are not claimed run here. Real phone/USB access is paused; this delegated task has never loaded a historical/new real Kalam library or performed device IO. Source fixtures are [kalam_fixture.c](../crates/backend-kalam/tests/fixtures/kalam_fixture.c) and its optional [FD extension](../crates/backend-kalam/tests/fixtures/safe_fd.inc).

## Parent integration API

Dependencies in [crate manifest](../crates/backend-kalam/Cargo.toml): `libloading = '=0.8.9'`, `libc = '=0.2.191'` (existing locked version, for portable FD flag inspection), workspace `serde`, `serde_json`, `thiserror`. `free`, `dup`, `dup2` remain direct C externs. The binding crate deliberately does not inherit the workspace `unsafe_code = forbid`; it denies `unsafe_op_in_unsafe_fn`. All native unsafe stays here; core, worker, protocol and application must retain their existing unsafe prohibition.

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

### Optional SafetyVersion 2 FD transfers

[Transfer boundary](../crates/backend-kalam/src/transfer.rs) exports:

```rust
pub enum SafeUploadPolicy { KeepBoth }
pub struct SafeUploadRequest {
    pub storage_id: u32,
    pub parent_id: Option<u32>,
    pub destination: String,
    pub requested_name: String,
    pub policy: SafeUploadPolicy,
    pub expected_size: u64,
}
pub struct SafeDownloadRequest {
    pub storage_id: u32,
    pub object_id: u32,
    pub parent_id: Option<u32>,
    pub expected_size: u64,
}
// All request fields serialize camelCase; KeepBoth -> "keepBoth".
// File handles are supplied by the worker; this crate never opens IO paths.
pub fn supports_upload(&self) -> bool;
pub fn supports_download(&self) -> bool;
pub fn upload(&mut self, request: &SafeUploadRequest, source: &File) -> Result<Value>;
pub fn download(&mut self, request: &SafeDownloadRequest, output: &File) -> Result<Value>;
```

At load, an optional constant `SafetyVersion() -> u32` is called under the trusted-library contract. **Only exactly 2** permits resolving the inspected FD function signatures. Missing marker, v1, unknown future versions, or a missing corresponding safe symbol cause transfer capability false / `TransferUnavailable`, without rejecting the six required read-only exports. The v1 same-name transfer symbols had a different signature; never invoke them by guessing argument count. The two directions are independently gated. Capability predicates describe the loaded ABI; operations additionally require Ready state. No fallback to legacy transfer, rename or delete exists.

**Parent identity JSON is intentionally asymmetric:** upload always emits `parentId`; None emits explicit null and means root. Upload None / Some(0) / Some(u32::MAX) requires destination `/`; a non-root destination requires Some(nonreserved directory ID), and a non-root ID cannot label `/`. Download None **omits** `parentId` and tells native **not to check the parent**; it does not mean root. Worker/domain root must explicitly pass `Some(u32::MAX)` (Some(0) is native-equivalent). Native revalidates download source and upload destination directory IDs/storage; labels alone are not ownership.

Storage IDs 0/u32::MAX and download object IDs 0/u32::MAX are rejected. Other u32 parent IDs are real directory IDs; 0/MAX are explicit root sentinels. Destination directories obey the canonical POSIX path rules above. Requested names must be a nonempty basename without slash, backslash, NUL, control characters, `.` or `..`. Only KeepBoth can be constructed/serialized: Replace and original-name commit have no enum variant. Expected size must be <=i64::MAX; it is never truncated into a native signed byte count.

The adapter calls File metadata / `F_GETFL` on the supplied descriptor, never source/output pathname. Source must be regular, readable, and have actual length exactly expectedSize. Output must be regular, writable, **empty**, and not O_APPEND. Native receives only the borrowed `AsRawFd()` integer until synchronous return; it Dup/Close owns only its duplicate. Dup shares offsets, including native Seek(0), so callers must exclusively hold the upload snapshot / private download stage with **no concurrent reads, writes, seeks or alias activity**. `&File` alone cannot prove filesystem exclusivity, immutability, no hardlinks, or stage provenance; the worker must establish them. Upload must use a private immutable snapshot. Download must use an owned empty private stage created by trusted directory-FD-relative EXCL/NOFOLLOW operations; this binding does not create or infer such ownership from an ambient path.

Done is the unique success criterion, not 100% progress. Both directions require data object, `committed:true`, `bytes == expectedSize`, a nonreserved u32 objectId, and no `needsReview:true` (a present review field must be false). Download objectId must equal the request, and the original stage FD length is rechecked after native return. Upload additionally requires `needsReview:false`, safe `actualName` different from the requested original name, a bounded v2 `-mtp-<32 lowercase hex>` final name, and `path` exactly destination+actualName. Malformed results, false/uncertain commit, byte/identity/name mismatch or callback faults quarantine the owner; none are reported as success. Upload committed means a new KeepBoth object's metadata was verified, not a hash/durability/atomic original-name overwrite guarantee. Download committed means **only private stage completion**, never final destination publication; worker still owns no-clobber inode-checked commit and journal policy.

`KalamError::Native { error_type: String, message: String, data: Option<Value> }` preserves non-null native error data, including bytes/objectId/path/needsReview uncertainty metadata. Missing/null data maps None. Display is the constant `Kalam native error (details withheld)` and never dumps message, errorType, or data; these strings can contain private paths. Debug/typed fields remain sensitive and must not be blindly logged. Worker should classify the **typed** native error_type, e.g. ErrorMtpDetectFailed -> NotConnected and ErrorNeedsReview -> UnknownOutcome/NeedsReview, rather than matching Display or callback names. Local validation / capability errors occur before native IO and leave Ready unchanged. Every native/callback/result-protocol transfer error transitions to Failed; no transfer or read can be retried in that owner. Do not infer rollback, delete partial objects or automatically resend an uncertain upload.

### Optional count-only destination inspection

[Inspection boundary](../crates/backend-kalam/src/inspect.rs) adds an **independent read-only** export, gated by the cached exact marker 2 and optional `InspectUploadDestination(char *json, callback_address done)` symbol:

```rust
pub struct InspectUploadDestinationRequest {
    pub storage_id: u32,
    pub parent_id: Option<u32>,
    pub destination: String,
}
pub fn supports_inspect(&self) -> bool;
pub fn inspect_upload_destination(
    &mut self, request: &InspectUploadDestinationRequest,
) -> Result<serde_json::Value, KalamError>;
```

All three fields serialize camelCase. **parentId is always present**, including explicit null for None=root, never the download-style unchecked-parent omission. Null/0/MAX parent requires `/`; non-root parent requires a canonical non-root destination. Storage 0/MAX is invalid. Requests over the native export's **4096 JSON byte input limit** are rejected locally; callback payloads still use the global 1 MiB limit. Unknown request fields are rejected by typed deserialization; there is no source, requested name, expected size, FD, object-write policy, or transfer argument.

`supports_inspect()` is false for absent symbol/marker, v1 or any unknown version. Legacy six read operations remain usable, and missing inspection does not gate FD transfers. The method requires Ready; dispatch uses only the normal read callback path and the cached inspection pointer. It never dispatches an upload dry-run, SafeUploadSingle, download, rename, delete or legacy fallback. No syscall opens or accepts an IO pathname/FD in this Rust method. Native operation mutex covers the complete synchronous invocation; callback remains one owned, bounded, copied/free terminal result with catch_unwind and duplicate/missing detection.

A successful full envelope must contain exactly errorType/error/data and an exactly six-field count-only data object: `noWrites:true`, `complete:true`, phase **`complete`**, operation **`InspectedUploadDestinationReads`**, precise u64 `destinationChildCount` and `existingHandleCount`. These native success labels are a finite fixed allowlist, not interpolated into error Display. Negative/fractional/string/overflow counts, different labels, false/incomplete results or any extra path/name/metadata/list/FD field fail closed and quarantine the owner. Error `data` is preserved privately through existing `KalamError::Native`; Display remains withheld, including unknown native error types. Missing capability uses distinct `InspectionUnavailable`; capability/local-input rejection leaves Ready unchanged.

Inspection binds the original directory identity, snapshots destination handles/filenames internally and reads all global IDs, then returns only counts. `noWrites` means **no object/content/directory mutation**, not absence of USB OUT command headers needed to request read-only MTP metadata. The known trusted native source, not a JSON boolean alone, supplies this property. This is a read-only diagnostic, not confirmation that an uncertain upload was committed or rolled back. It cannot clear UnknownOutcome, authorize retries/deletion, recover a failed owner, or establish a consistent cross-time snapshot. After a previous timeout/UnknownOutcome, the parent must obtain explicit read-only authorization and use a fresh initialized isolated owner. The binding performs no real phone access in its tests.

### Owner and lifecycle

- `Kalam` is deliberately `!Send + !Sync`, with `&mut self` operations: one serial thread owner.
- A process-global mutex allows only one **ever loaded** Kalam instance in that worker. No sharing Go globals between different sessions/libraries, even sequentially after Drop. A loader failure before obtaining a Library can be retried; after obtaining any Library, even missing required symbols means a new worker is required.
- Once loaded, the `Library` immediately enters `ManuallyDrop`. Its destructor is intentionally never invoked, including early symbol-resolution error paths. Dispose releases native device references, not the Go runtime mapping. The OS reclaims the mapping at worker process exit.
- Loaded -> initialize once -> Ready. Initialization failure can already have allocated USB resources, so it transitions to Failed, with cleanup permitted. Any native/ABI/envelope failure in a read or FD transfer operation also transitions to Failed; no subsequent read/initialize is allowed. Local request validation failure does not invalidate Ready.
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
| Original upload/download File descriptor | Rust worker, exclusively held | Borrowed integer only until synchronous return; native never closes the original |
| Native Dup of staged File descriptor | Go export | Go owns and closes the duplicate before done/return; shared offset requires exclusive caller use |
| Preprocess/progress CString | Transferred native allocation | Independently bounded copy/parse/discard/free; never counts as terminal success |

The callback contains Rust unwind with `catch_unwind` around copying, parsing and state recording; its ownership guard sits outside that closure, so panic still releases the native string. Mutex poison is recovered without another unwind. OOM/allocator abort, malicious pointer access and native faults are process failures, not recoverable callback errors.

The cap is **1,048,576 JSON bytes, excluding the terminating NUL**. The callback reads at most cap+1 bytes to locate NUL and copies at most cap bytes. This is not a defense against an invalid/unterminated native allocation: trusted native must provide a readable C.CString. Exactly-cap JSON is accepted. ErrorType/error checking happens on the Rust-owned copy, after native free.

The inspected synchronous read-only exports and explicitly gated **v2 FD** exports are supported. Every operation must finish its terminal callback(s) and all event callbacks before returning. A global callback slot stays valid for the process lifetime, avoiding a Rust stack context escaping into native code. Missing terminal callback is detected on return; repeated terminal callbacks are a Result error and every distinct allocation is released. V2 preprocess and progress have independent function-address callbacks and a separate event state: each payload is bounded, copied/JSON-parsed, envelope-validated, discarded and freed, without incrementing the terminal count. Event faults prevent successful completion; a terminal Native error takes precedence so ErrorNeedsReview metadata is retained. Events arriving after terminal done are protocol errors. An observed callback outside an active appropriate invocation is freed and permanently poisons future native calls. Since this ABI has no user context/generation token, a malicious or unsupported delayed callback overlapping a different call cannot be reliably attributed; arbitrary asynchronous exports are therefore expressly outside the contract. Transfers' **legacy** goroutine/progress machinery is never bound.

Native must transfer each allocation exactly once. Reusing an already-freed pointer, freeing a transferred string itself, allocating with an incompatible allocator, or mutating/retaining input after return violates the ABI ownership contract. No Rust wrapper can safely dereference/free such pointers. The inspected shim uses C.CString, hence system C malloc/free is the chosen release pair.

## Controlled tests and remaining gates

[Test implementation](../crates/backend-kalam/src/tests.rs) compiles the C fixture with system `cc` only when tests run. There is no production build script or compiler invocation during a normal crate build. Every actual library load scenario runs in a fresh subprocess; tests do not bypass or reset the permanent single-owner marker.

Coverage scenarios: exact function-address ABI; explicit absolute path and missing load/symbol errors; input casing/JSON and canonical paths; typed device/walk data; initialization error plus cleanup; native message/errorType propagation; malformed JSON/envelope/data; null/duplicate/missing callback; over-limit and exactly-limit JSON; Go nil list; ownership release on successful/error/duplicate/oversize paths; uninitialized local Dispose without native invocation; Drop cleanup; terminal Dispose failure and no retry; permanent no-reload guard; late-callback poisoning; stdout separation and closed-stderr failure. A test-only atomic increments immediately after actual C free returns and is compared with the fixture's actual malloc allocation count, so error/duplicate release is tested without a fake deallocator. The fixture uses separate malloc strings for duplicate callbacks; intentionally double-freeing the same pointer is not a valid native contract test.

[FD tests](../crates/backend-kalam/src/transfer_tests.rs) additionally build legacy/no-marker, marker v1, unknown v3, v2 missing-upload, v2 missing-download and full-v2 controlled libraries. Cases cover asymmetric parent serialization, KeepBoth-only policy, descriptor flags/regular type/actual source length/empty-stage checks, size bounds and reserved IDs, pathname rebinding to symlink victims after original FD acquisition, writes confined to original stage inode, original descriptors remaining usable, repeated progress freeing without duplicate-terminal errors, malformed/oversize/null/error events, wrong bytes/object IDs/stage length, unsafe/original committed names, committed false/needsReview rejection, metadata-preserving native error with redacted Display, and transfer-result quarantine. Every load runs in a fresh subprocess; the production owner marker is never reset for tests.

[Inspection tests](../crates/backend-kalam/src/inspect_tests.rs) and the optional [count-only C fixture](../crates/backend-kalam/tests/fixtures/inspect.inc) add legacy/v1/v3/missing-inspect gating, inspect-only ABI, exact root/non-root three-field inputs, local input bounds and identity checks, strict flags/labels/counters, no private success fields, Native diagnostic retention/redacted Display, duplicate/missing/null/oversize/malformed callbacks, u64 precision and permanent poison. Separate inspection and FD-operation counters assert that an inspection never invokes the safe transfer routes. These Rust tests are **pending parent-unified Cargo**, not real USB/device evidence; the six corresponding C variants compiled offline successfully. Existing test modules and FD fixture behavior remain unchanged outside the new explicitly guarded inspection include.

The earlier FD baseline `cargo test -p mtp-backend-kalam` and strict adapter-only all-targets clippy passed. The inspection extension awaits parent-unified Cargo; this delegated task did not run Cargo for it. The `cc` fixture toolchain is required; missing compiler fails the test rather than silently claiming ABI validation. Subprocess tests capture output and report it only on failure. Test fixture artifacts live in unique OS temporary directories and are not distributed production libraries.

Remaining real gates: parent workspace/worker protocol integration validation; vetted native source/license/build chain; real device identity/storage/read-only listing checks; **real USB single-file upload/download, no-clobber firmware behavior, disconnect/uncertain outcomes, >4GiB data and hash verification**; arm64/x86_64 release validation. Caller staging provenance, generation checks, inode-checked local no-replace commit, journal and cancellation/uncertain-result policy are outside this binding and must be wired/tested independently. Native marker/dlopen and C fixtures cannot substitute for those gates.
