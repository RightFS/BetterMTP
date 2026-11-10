# Single-file transfer service

Status: worker/application implementation with controlled-process test coverage awaiting coordinated validation. **No attached-device upload/download acceptance has been performed; this is not a claim that the usable hardware MVP is complete.** The simulated backend remains read-only and is never switched to real USB implicitly.

## UI and CLI entry points

Run the blocking `WorkerClient` on a background executor, never during UI render. Select `BackendConfig::Kalam` with an explicitly vetted, absolute native-library path. Library loading is itself trusted code execution; the optional `SafetyVersion == 2` marker is ABI gating, not authentication.

1. Spawn a worker, inspect Hello capabilities, connect, enumerate current storages, and list the destination/source directory.
2. Retain the returned `RemoteDirectoryRef` / `RemoteRef`. References use process-wide application generations, translated to this worker's wire generation at dispatch. Do not synthesize paths from displayed labels, reuse references after reconnect, or pass remote display paths to the worker.
3. Upload one local file:
   `upload_file(source: &Path, requested_name: &str, dest: &RemoteDirectoryRef, cache: &Path) -> Result<TransferReceipt, ApplicationError>`.
   The caller explicitly obtains a representable native filename with `source.file_name().and_then(OsStr::to_str)`; reject a name that cannot be represented rather than using lossy conversion. The cache must be an existing app-owned directory.
4. Download one cached remote file:
   `download_file(source: &RemoteRef, dest: &Path, policy: FileConflictPolicy) -> Result<TransferReceipt, ApplicationError>`.
   `dest` is an existing local **directory**, not an arbitrary destination filename. The worker derives the filename from validated cached native metadata. `Skip` returns an explicit non-success `ErrorCode::Skipped` if a target exists. `KeepBoth` publishes the requested name if vacant, otherwise a random no-clobber alternative; use the receipt's actual name/path.

`TransferReceipt` contains direction, actual name, byte count, optional remote object ID, optional exact local path (download only), and exact journal path. Success is emitted only after native completion is verified and the final publication/receipt is recorded. This verifies sizes/identities and controlled payload content in fixtures; it does **not** promise end-to-end device hash validation or storage durability beyond the inspected native contract.

## Capability and ownership boundaries

The worker preloads the trusted library on its sole owner thread **before Hello**, without initializing a USB session, to call `supports_upload()` / `supports_download()`. Only inspected optional v2 FD symbols advertise transfers. Legacy v1 or missing markers/symbols advertise read-only capabilities; legacy write exports are never called. A load failure still produces a Kalam read-only Hello, then Connect returns the actual error rather than simulating a session. `Kalam` remains `!Send + !Sync`; worker code has no unsafe FFI implementation. Native stdout remains separated from the framed protocol.

The protocol adds only `UploadFile` and `DownloadFile`; file bytes never enter IPC. Unix `LocalPath` serializes exact `Vec<u8>` bytes, validating absolute/no-NUL at construction and deserialization. Native transfer requests contain no local path: Go receives a borrowed FD and may close only its duplicate. Local operations are always owned by Rust staging primitives.

## Upload transaction

- Validate application references and capabilities before dispatch. The worker rechecks generation, current storage membership, filename safety, and a cached directory reference. Root uses native `parentId: null` plus `destination: "/"`; non-root sends the original cached parent ID **and** validated native path together, enabling native ID/path correspondence checks against rename or same-name replacement.
- Create `UploadStage` using NOFOLLOW, private ownership-marked staging. Snapshot a regular source without altering it; check source identity/metadata again and validate the staged payload before USB. The native API reads only `stage.staged_file()`.
- After source/staged validation and size checks, call `UploadStage::mark_dispatched()` to durably persist `native_dispatched: true` while retaining Ready, **before** invoking native upload. Repeated dispatch is rejected. A save failure returns a local error and does not call native. Early invalid-name/source/size failures are not dispatches. A hard-killed worker may therefore leave a Ready journal with `native_dispatched: true`; that is uncertainty evidence for RequiresReview, not authority to replay. Native failure subsequently persists NeedsReview, while verified completion retains the flag in Committed. Historical journals produced before this fix may show `native_dispatched: false` even after an attempted upload; their recorded facts are never rewritten or treated as proof that no USB call occurred.
- Native v2 KeepBoth chooses a final `original-stem-mtp-crypto128.ext` name without replacing the original remote filename. Require the full success envelope: `committed: true`, exact byte count, valid object ID, safe actual name/path, and `needsReview: false`.
- Persist typed `RemoteUploadReceipt` through `mark_uploaded` and retain its committed journal. The transferred content is the validated immutable snapshot: a later change to the original source does **not** change verified upload success into UnknownOutcome/RequiresReview. Source identity/metadata checks remain mandatory before USB dispatch. A native completion or durable receipt-persistence error after dispatch does become `UnknownOutcome`/RequiresReview; do not delete the journal/stage or retry.

## Download transaction

- Require a file cached under the current storage/session/object ID, with known unsigned size and native parent metadata. Directories and unknown metadata are explicitly unsupported.
- Cache root parent metadata as explicit `Some(u32::MAX)` (native root `0`/MAX are equivalent). Never send `None` for root: the FD API defines `None` as **no parent check**. Non-root retains the original native parent ID.
- Create `DownloadStage::with_policy` in the destination directory; persist `mark_dispatched` while retaining the Staging state so later commit is legal. Pass only `stage.payload_file()` to native. Native `committed: true` means the private stage is complete, **not** that a user destination was published.
- Require the exact requested object ID and byte count. Then Rust verifies stage ownership/size and uses atomic NOREPLACE publication. Skip is not success and never reports bytes from an existing target. KeepBoth returns the actual published basename and byte-exact path.
- Publication may already have happened when a later fsync/journal save fails. Such errors invalidate the owner and return RequiresReview with the retained journal; never delete or retry the potentially published file. A native failure retains its error type/message/data only inside the private local journal, not in Display/UI error text.

## Uncertainty, lifecycle, and timeouts

`ApplicationError::UnknownOutcome { journal_path: Option<LocalPath> }` is an explicit RequiresReview barrier. A transport timeout, crash, malformed/missing/duplicate result, request/generation mismatch, or lost write reply is not success and not Canceled, regardless of the underlying transport error class. The client invalidates and reaps the worker. Native transfer errors also invalidate the owner. No automatic retry or fallback exists. A journal path is available in worker-reported failures; transport loss can leave it unknown to the client, while the private stage retains its durable dispatch evidence.

Handshake/read defaults remain 20 seconds. Transfer defaults are size-based: 120 seconds plus approximately one second per MiB, capped at two hours; unknown estimates use the cap. `set_deadline(Duration)` is an explicit override for controlled tests/callers, not a native cancellation feature.

A cloned `WorkerStopHandle`, obtained through `WorkerClient::stop_handle()`, exposes `request_stop() -> io::Result<()>` independently of the business-client mutex. UI shutdown/release should keep this handle separately and request process stop immediately, even if a transfer owns the client mutex. The handle safely owns `Child` via `Arc<Mutex<Child>>`, checks already-exited state, and kills without a bare cached PID or waiting for the business client. The client observes EOF and reaps the process. **Process stop is not USB cancel** and dispatched transfers still require review; `cancel_current` remains false.

No startup full-tree scan, cleanup, automatic journal removal, or reconstruction of cleanup authority from JSON is implemented. Stages and journals are intentionally retained, including committed receipts and Skip evidence. Explicit cleanup belongs to a later ownership-verified recovery workflow and must never unlink published user targets.

## Controlled validation and remaining acceptance

New worker integration tests execute the actual worker child with a controlled C FD fixture (no Go runtime, USB, or real device): snapshot/published content equality, original-file preservation, empty files, Skip/KeepBoth, root and non-root parent identity, invalid local/remote inputs, source/target/ancestor symlinks, stale application references, legacy capability gates, native uncertainty metadata, malformed terminal replies, process death, timeout, independent stop handles, and non-UTF8 directory paths. Protocol tests cover byte-exact paths, invalid path rejection, and older errors without journal fields. The fake transfer path is explicitly tested as Unsupported without local file access.

The integration target now contains **21 tests on macOS and 23 on Linux**. The parent observed all 20 previously applicable macOS cases passing in the initial coordinated run; the additional controlled-source-mutation regression awaits the next coordinated run. That regression changes only a fixture-owned original source inside the native call and verifies that its FD-captured `payload` still uploads successfully with a Committed journal, rather than becoming UnknownOutcome merely because the original now contains `changed`. Two filesystem-creation cases (non-UTF8 source basename and destination directory) are Linux-only because macOS APFS rejects these byte sequences with `EILSEQ` / `Illegal byte sequence` before the transfer service runs; this is not an MTP or codec failure, and no alternative filesystem workaround is attempted. Exact non-UTF8 `LocalPath` serialization/deserialization remains covered by pure-memory protocol tests on macOS as well. Linux filesystem cases still require a Linux test run.

The parent coordinates Cargo tests and strict clippy to avoid competition with the binding/transfer agents. Hardware acceptance still requires a connected device: actual upload/download, original destination/source preservation, content/hash verification, root/non-root identity behavior, reconnect/stale references, interruption/review evidence, and UI/CLI use of the same service. Do not call this hardware MVP complete until those checks have been observed.
