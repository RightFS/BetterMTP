# Restricted device acceptance CLI

`mtp-acceptance` is a separate binary in the probe package; `default-run = "mtp-probe"` preserves the original `cargo run -p mtp-probe` behavior. This CLI uses the same blocking `WorkerClient` single-file service as UI/CLI transfers. It never loads native Kalam in the acceptance process and never substitutes the fake backend.

## Authorization and defaults

The current acceptance authorization is **primary/internal storage ID 65537, existing remote directory `/tmp` only** (the phone UI's `/内部存储/tmp/`). This build rejects every other storage/directory, including root, trailing slash aliases, traversal, and relative paths. It does not create the phone directory, fall back to root, remove any phone object, or automatically retry any failed operation.

Required arguments:

```text
mtp-acceptance --library <absolute-vetted-kalam.dylib> \
  --worker <absolute-mtp-worker> \
  --storage-id 65537 --remote-directory /tmp \
  --local-cache <absolute-this-project>/.cache/device-acceptance \
  [--read-only | --write-confirmed] \
  [--verify-report <absolute-project-cache>/run-<32-lowercase-hex>/results.json]
```

**No mode flag means read-only.** Modes/options cannot be repeated or combined. `--help` alone prints usage without connecting. Library loading is trusted code execution: supply the explicitly reviewed packaged native artifact, not a discovered/system-search library. Absolute, no-NUL native local paths remain byte-exact; remote names must be representable UTF-8.

The cache must be exactly this checkout's `.cache/device-acceptance` absolute path. Read-only mode does not create any local acceptance directory or result file. Write mode rejects symlink/non-directory cache ancestors and creates only an exclusive random `run-<128-bit-random-hex>` local run directory with mode 0700. Generated sources and result files are exclusive mode 0600; existing runs are never reused, overwritten, scanned, or deleted. These local directories are trusted app-owned acceptance state; same-user hostile mutation is outside the CLI's report-directory trust boundary. Actual file transfer staging retains the stricter service's directory-FD/NOFOLLOW ownership guarantees.

## Read-only phase

1. Connect to one explicit Kalam worker and enumerate current storages.
2. List the selected storage root in memory. Resolve the exact `tmp` entry as one unambiguous directory via its cached object reference, not a label-derived native write path.
3. List that directory and print only device display/model, storage ID, root count, tmp object ID, and tmp count. No serial/device key or other private entry names are printed.
4. Shut down the worker. No upload/download, phone mkdir/delete, local test files, or fake-device fallback occurs.

The parent should review this CLI and execute this read-only command first. Static `PHASE=spawn/connect/storages/root/resolve_tmp/tmplist` progress lines identify the last attempted read operation without serials or entry names. Native failures expose only exact inspected enum categories through a worker-side literal whitelist and a second CLI-side exact-message whitelist (for example `backend_ErrorDeviceSetup`, `backend_ErrorMtpLockExists`, or `backend_ErrorListDirectory`). Unknown or extended types/messages stay withheld; no raw driver text, path, serial, or native data is interpolated. These categories are diagnostic evidence, not permission to kill another session, bypass a busy device, or retry automatically. A connected Samsung device or a successful browse alone does not prove file-transfer acceptance.

## Historical baseline verification (read-only audit)

Optionally add `--verify-report <absolute-project-cache>/run-<32-lowercase-hex>/results.json` to a read-only command. Omitting a mode remains read-only; combining verification with `--write-confirmed` is rejected. The report is audit data, **never cleanup, retry, or upload authority**.

Only the exact caller-selected `results.json` under this project's generated acceptance run directory may be read. Raw pathname bytes must be absolute/no-NUL with normal components; relative paths, traversal, dot aliases, duplicate slashes, trailing slash aliases, other cache roots, payload/source/download filenames, nested files, and malformed run IDs are rejected. Every ancestor must be a non-symlink directory. The report is opened read-only with NOFOLLOW, required to be a single-link regular file no larger than 4 MiB, and checked against pre/post FD/path inode, size, and content-metadata identities. The existing app-owned cache remains the trust boundary. No source payload, downloaded file, unrelated user body, earlier-run directory listing, or journal body is read.

The JSON must have version 1, write-confirmed report scope, storage 65537, remote `/tmp`, a known historical state, and a canonical object-ID map containing name/kind/unsigned-size-or-null metadata. These fields are parsed for comparison only; device keys, caller paths, receipts, arbitrary native data, and other report fields never become executable references or write commands.

After a live read-only listing of `/tmp`, stdout prints one `BASELINE_COMPARISON={...}` JSON object containing only fixed field names, counts, scope constants, and booleans: `allBaselineIDsNameSizeKindPreserved`, `baselineObjectCount`, `currentObjectCount`, `extraObjectCount`, `missingBaselineObjectCount`, and whether historical NeedsReview is retained. No existing filenames, serials, file contents, or raw native text are printed. Extra IDs are counted separately from missing/changed baseline IDs; unchanged subset metadata does not prove that an interrupted native upload had no side effects or that user contents are unchanged. A missing/changed baseline produces a review failure after printing the summary and still only shuts down the worker.

**Historical `results.json`, its state, marker facts, and all journals are never rewritten.** In particular, a historical NeedsReview remains NeedsReview even when current baseline metadata matches; an old `native_dispatched: false` is not repaired retroactively and does not prove no USB call. This minimal implementation writes no new comparison/evidence file; the parent may separately record the stdout summary. No retry, cleanup, upload/download, or phone deletion is introduced.

## Explicit write-confirmed phase

Execute only after the parent explicitly approves the reviewed write phase and the read-only phase confirms the existing `/tmp` reference. The flag is an explicit CLI gate, not additional authorization to arbitrary phone paths.

The run takes an in-memory baseline of all `/tmp` entries' object ID, name, directory flag, and byte size. Their metadata is recorded only in the private local report, not stdout. It creates its own local deterministic 128 KiB payload and empty file, with generated Chinese/space-containing basenames and a random run token. It never uploads a user-selected existing local file and never downloads an existing unrelated phone file.

For the generated payload:

- Upload through `WorkerClient::upload_file`, using the resolved current `RemoteDirectoryRef` and a newly created private upload cache. Native v2 KeepBoth must return a distinct final name and new object ID.
- Immediately record the receipt/known created object ID/actual name locally. Inspect the retained committed upload journal and snapshot, verifying exact generated bytes and that the generated original source remains unchanged.
- Re-list only `/tmp`; require the uploaded actual name, ID, file kind, and exact size to agree. Check that all original baseline IDs/names/sizes/kinds remain unchanged.
- Download only that newly created object's `RemoteRef` into a fresh private destination with KeepBoth and compare exact bytes.
- Explicitly exercise one Skip against the now-existing generated local target. Require `ErrorCode::Skipped` rather than success, and verify the first downloaded bytes remain unchanged.
- Explicitly exercise one additional KeepBoth download of the same generated object. Require a different published name/path, exact bytes, and retention of the first download. These are planned independent conflict checks, not retries after a failure.

Upload/download the generated zero-byte file similarly. Finally re-list `/tmp` and require every original baseline object ID/name/size/kind to remain unchanged. This is **metadata preservation**, not a content-hash claim for users' pre-existing files: their contents are never read. Generated-file exact byte comparison validates those test payloads without promising end-to-end general-purpose hashing or device persistence beyond the inspected native receipt contract.

## Reports and failure behavior

Stdout prints a `LOCAL_REPORT=.../results.json` path, then only summary counts/model/IDs and final status. Detailed generated phone-object IDs/actual names, receipts, baseline/after metadata, checks, and byte-exact local journal paths stay in the private local JSON report.

The report is persisted before each native dispatch, and receipts are persisted immediately after success. A completed transfer run also records `workerShutdownConfirmed`; a failed shutdown does not leave the report claiming the entire acceptance run passed. Updates use an exclusive temporary file, fsync, owned report-identity checks, rename, and directory fsync. The CLI catalogs journals only within its own newly created local run tree, including explicit Skip stages; it never scans older cache runs or arbitrary user directories. Transfer stages/journals are intentionally retained even after success.

Any failure stops the planned sequence. Unknown outcomes return RequiresReview (exit code 3), retain the service-provided journal path when known, and report verified objects already created. An uncertain upload may have created an additional object without returning a receipt: `mayHaveUnrecordedPhoneObjects` explicitly records this possibility. No missing reply is interpreted as “nothing was created.” Other failures use exit code 1; success is exit 0. Native error details are withheld from console/report error text and remain in the service's private journal metadata when available.

Success and failure alike perform only worker shutdown at the end; they never delete/retry phone objects, remove local evidence, or claim USB cancellation. Users can manually inspect the generated phone files later under `/tmp`. Do not introduce pressure tests, directory transfer, deliberate device-disconnect tests, or cleanup into this initial acceptance pass.

## Build and validation boundary

Probe-only offline validation uses the repository's pinned Rust environment:

```text
cargo test -p mtp-probe
cargo build -p mtp-probe --bins
cargo clippy -p mtp-probe --all-targets -- -D warnings
```

Guard tests do not connect a device and cover default read-only selection, explicit write gating, conflicting/repeated/unknown arguments, relative paths, exact authorized storage/directory/cache scope, absent tmp without root fallback, baseline preservation semantics, and review-journal redaction. Hardware commands must be run separately by the parent after review; unit tests/build success never imply that real-device transfers have passed.
