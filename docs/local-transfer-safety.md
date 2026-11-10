# Local transfer staging safety (MVP)

`mtp-transfer` provides **local transaction tools only**. It does not implement MTP download/upload, recursive transfer, device rename, remote atomicity, conflict resolution on a device, transport cancellation, or a complete transfer feature. The worker remains responsible for those operations and truthful UI state.

## Integration API

```rust
use mtp_transfer::{ConflictPolicy, DownloadStage, TransferError, UploadStage};

let mut stage = DownloadStage::new(destination, final_name, expected_u64)?;
// Only single-file download. Rust holds this File until native work completes.
// Native backend dup()s the borrowed raw FD, truncates/seeks/writes its dup,
// and closes ONLY that dup before reporting completion. No ambient path opens.
backend_safe_download_one(stage.payload_file())?;
// payload_path/staging_directory/safe_name are informational/legacy compatibility.
let committed_path = match stage.commit() {
    Ok(path) => path,
    Err(TransferError::Skipped(path)) => {
        // Report skipped, NOT completed using the existing file's bytes.
        // An explicit cleanup_owned() may now discard only the private stage.
        return report_skipped(path);
    }
    Err(error) => {
        // Keep stage/journal. publication may already have happened; inspect
        // stage.journal().state, published_name and publication_confirmed.
        return report_needs_review(stage.journal_path(), error);
    }
};
// Optional explicit cleanup only AFTER the caller records the successful result.
stage.cleanup_owned()?;

let mut upload = UploadStage::new(source, app_cache)?;
upload.validate_staged()?;
// Random safe_name may serve as the unique remote temp basename.
// Native code dup()s the snapshot FD, seeks to 0 (dup shares offsets), reads,
// and closes ONLY its dup; it must never write/truncate the snapshot.
backend_safe_upload_one(upload.staged_file(), upload.safe_name(), upload.file_size())?;
// Optional source recheck before reporting success. The remote temp publication
// is NOT implemented here; worker must enforce its own remote safety policy.
upload.check_source_unchanged()?;
upload.cleanup_owned()?;
```

The example's backend/report functions are illustrative, not crate functions. Safe worker bindings **must use `DownloadStage::payload_file() -> &std::fs::File` and `UploadStage::staged_file() -> &std::fs::File`**, converting the borrowed File with safe `AsRawFd`/`AsFd` in Rust. Native code must duplicate the borrowed descriptor, retain/close only its own duplicate, and finish before the Rust stage is committed/cleaned/dropped. `dup` shares the file offset; upload constructors rewind the snapshot to zero, but native upload should explicitly seek to zero or use positioned reads. Download native code must truncate and reset the duplicate offset before writing; uploaded snapshots must remain read-only by contract (the internal payload File is RDWR).

**Do not use legacy ambient-path download/upload in the safe worker.** An `os.Create(payload_path)` can follow a rebound ancestor/symlink and overwrite unrelated user files *before* post-transfer inode validation detects anything. Upload ambient reopening can likewise read unrelated files after rebinding. Path getters are retained only for display/audit and legacy single-file basename compatibility; they are not safe I/O authority. FD-based transport isolates data I/O from such pathname races while commit/cleanup keep their own binding/ownership checks.

Worker backend errors/cancellation should call `mark_needs_review(reason)` while retaining the live stage. `DownloadStage::with_policy(destination, final_name, expected_size, ConflictPolicy::KeepBoth)` opts into KeepBoth. `commit(&mut self) -> Result<PathBuf>` defaults to Skip; there is deliberately **no Replace enum variant or overwrite API**. `journal_path()`, `journal()`, `staging_directory()`, `safe_name()` are available on both stages. Upload exposes `staged_path()` and `file_size()`; download exposes `payload_path()`.

Only the parent integration task may add the workspace member/dependency or worker usage. This crate does not edit root, worker, app, or application manifests/source.

## Creation and directory binding

- Directory paths are opened from `/`, **one normal component per `openat`** with `RDONLY | DIRECTORY | NOFOLLOW | CLOEXEC`. The destination/cache and all their ancestor symlinks are refused. `..` is refused, never normalized away. Relative paths are anchored to the current directory at construction. `.` is harmless and accepted.
- On macOS `/tmp` and `/var` are commonly symlink aliases. Production callers should supply the actual physical directory path (for example an already-selected trusted `/private/...` path). Tests resolve the system temp root once before creating their own UUID-owned fixture; arbitrary user paths are not silently canonicalized to permit symlinks.
- The actual opened destination/cache directory `File` (FD) stays alive for every stage, journal, publication and deletion operation. Commit is not `exists()` + path rename. Directory identities `(device, inode)` are checked against the live FD and physical pathname before sensitive work; rebinding, renaming or swapping that directory causes refusal. Final operations remain FD-relative even if a pathname race happens after the check.
- A fresh `.mtp-stage-<UUIDv4>` directory is created with exclusive `mkdirat`, mode `0700`. A collision is an error; no existing directory is adopted. The payload and random-token marker/journal names are created `CREATE | EXCL | NOFOLLOW`, mode `0600`. Creation never opens, reserves, truncates, or overwrites a final target.
- Download payload basename is exactly the validated requested name for display and legacy single-file compatibility. A safe downloader **must write/truncate the already-created payload through `payload_file()`/its duplicate FD**, never reopen the pathname. Replacing its inode, writing another basename, or downloading multiple entries is not supported and fails ownership checks.
- Basenames reject empty strings, `.`, `..`, `/`, `\`, NUL/control characters and more than 255 UTF-8 bytes. No untrusted name becomes a path component sequence. Unicode and spaces are permitted for local final names.

## Verified no-clobber download publication

1. Prove parent/stage FD identities and marker identity plus UUID contents. Prove the payload name still identifies the originally-created open payload inode, that it is regular and singly linked.
2. Compare `metadata.len()` with `expected_size: u64` exactly, including zero. No narrowing integer cast of the expected size occurs. Sync the payload before publication.
3. Atomically save a `Publishing` journal with the candidate basename **before** publication, then revalidate/sync the payload.
4. Call safe `rustix::fs::renameat_with(stage_fd, payload_name, destination_fd, candidate, RenameFlags::NOREPLACE)`. Payload and destination are on the same filesystem because the stage is a child of the destination. This syscall—not an existence precheck—determines the winner. Existing files, symlinks (including dangling symlinks), and directories cannot be replaced.
5. Skip returns `TransferError::Skipped(path)` on `EEXIST` and saves `Skipped`. KeepBoth initially tries the requested name, then fresh `mtp-<UUIDv4>[.extension]` candidates on `EEXIST`. No user stem is reused for the fallback. Only short ASCII-alphanumeric extensions are preserved.
6. On syscall success set `publication_confirmed = true` in live memory, sync destination and stage directories, recheck the binding, then atomically save `Committed`. The payload is moved, not left as a writable hard-link alias in the stage.

The source APIs were checked in the fixed cached `rustix 1.1.6`: `renameat_with` is available on Apple/Linux; Apple's `RenameFlags::NOREPLACE` maps to `RENAME_EXCL` and uses `renameatx_np` (macOS 10.12+). An unavailable syscall or unsupported filesystem returns an error and retains the stage; there is **no unsafe code, replace fallback or silent downgrade to ordinary rename for final publication**. The crate intentionally targets macOS/Linux only.

`sync_all` is required; failure is not reported as completed. Atomic visibility and durability are different: publication can already have succeeded when directory sync, pathname recheck or journal save fails. The live journal becomes `NeedsReview`; `publication_confirmed` and `published_name` help the caller report an ambiguous result. Never retry the same stage, remove the final target, or claim remote/local transfer completion because of such an error.

## Upload snapshots

- Source ancestors and final basename are opened with the same no-symlink rules. The source FD is read-only, `NOFOLLOW | NONBLOCK`; only regular, singly-linked files are accepted. NONBLOCK prevents opening a FIFO from waiting indefinitely. The original source is never renamed, modified or deleted.
- The local copy uses bounded streaming reads limited to the recorded `u64` size, with an extra-byte check for growth. FD metadata before/after copy compares identity, size, modified time and Unix ctime. A fresh FD-relative source-name open additionally detects path/inode replacement. Snapshot ownership/size is verified and synced before the journal becomes `Ready`.
- `staged_path()` has a fresh portable `mtp-<UUIDv4>` basename, optionally retaining a short alphanumeric extension. Different stages have different names even for the same source. The caller may pass that basename as a **remote unique temporary name**, but the crate creates no remote file and promises no remote no-clobber behavior.
- `check_source_unchanged()` optionally rechecks the original source FD, source-name binding and source-parent physical identity after transport work. A source valid at construction that is later unlinked (`nlink=0` on its held FD), hard-linked, replaced, missing, turned into a symlink, or has a non-directory ancestor returns `SourceChanged` and retains `NeedsReview`; initial invalid/nonregular/hardlinked sources are still rejected as `NotRegular`/I/O errors. Unrelated I/O errors such as permission denial remain I/O errors, not false unchanged results. This is metadata-based change detection, **not a content hash, lock, filesystem snapshot, or guarantee against malicious same-size writes that evade/restored timestamps**. The snapshot bytes remain local even when a later source change is detected.
- Transport code must treat the upload snapshot as read-only. `validate_staged()` detects identity, size and hard-link changes, but is not a cryptographic digest check of same-size local snapshot mutations.

## Journal and cleanup authority

The serde JSON journal includes format version, UUID ownership/task identifier, stage kind, task state, parent/stage/payload/marker names and identities, expected size, requested name, publication candidate/confirmation and failure reason. Journal names are randomized to avoid conflicting with any valid download basename.

Atomic save creates a fresh exclusive temporary JSON file in the bound stage FD, writes and syncs it, then renames it into place and syncs the directory. Initial journal publication uses NOREPLACE. Subsequent ordinary rename replaces **only the live stage's proved-owned prior journal inode**, never a final destination or arbitrary user file. Journal ownership is checked on every save.

Backend failure/cancellation, validation/I/O failure, and dropping an unfinished live stage retain payload and journal with `NeedsReview` when possible. Skip is an intentional conflict outcome (`Skipped`), not successful transfer. No destructor recursively cleans anything. A failed journal write, disk-full error, lost directory binding, or ownership tampering may prevent even `NeedsReview` from reaching disk; the previous durable document/temporary JSON files must remain for manual review. Failure during initial construction may leave a private UUID stage without a complete journal. No startup scavenger is provided.

`cleanup_owned()` is an **explicit live-handle operation**. It verifies:

- current physical parent binding and opened parent/stage device/inode identities;
- stage name and random marker identity/token;
- the current journal inode equals the latest atomic save's inode;
- any remaining payload inode is the original created inode;
- an FD-based directory listing contains only that payload, marker and journal (plus `.`/`..`). Unknown children cause refusal **before removing ownership evidence**.

Only those exact owned children are unlinked using safe `unlinkat`; the stage directory is removed non-recursively using `REMOVEDIR`. The published target is never considered for cleanup, even after a commit error or Skip. Cleanup is idempotent after success. Failure may leave a partially cleaned stage; it is not repaired by guessing or deleting user files. A new object cannot adopt an existing stage, and deserializing/forging JSON grants **no cleanup authority**. Recovery must classify leftover journals/stages as needing review, not auto-delete them.

### Threat and concurrency boundary

No-clobber publication remains atomic against concurrent final-name creators, including other tasks using this crate. Symlinks and traversal supplied at construction or swapped before validation are refused. Private `0700` stage directories plus held inode identities prevent ordinary unrelated tasks/users from being cleanup authority.

This is **not a sandbox against a malicious process running as the same UID/root**. Such a process can mutate an inode through an already-open FD or swap a private-stage child after an identity check and before a name-based syscall. Neither portable POSIX `unlinkat` nor macOS NOREPLACE supplies conditional “unlink only this inode” semantics. Callers must exclusively own each stage and must stop/close every writer before commit/cleanup; don't expose stage directory write authority to unrelated code. A directory moved after binding validation can cause publication into the still-bound original FD directory followed by a `NeedsReview` error; never interpret a potentially rebound result pathname as proof of success. These limitations are stated rather than bypassed with unsafe FFI.

## Tests and verification handoff

The crate includes over 20 unit tests for exact bytes plus pre/post hash preservation of existing targets, construction not touching targets, unique stages, zero bytes, short/oversized/`u64::MAX` lengths, Skip/KeepBoth, invalid names, Unicode names, destination/source/cache and ancestor symlinks, dangling final symlinks, existing directories, physical directory rebinding, payload symlink/hard-link substitution, owner/journal tampering, unknown-child cleanup refusal, non-recursive/idempotent cleanup, retained NeedsReview journals, atomic journal replacement, simultaneous NOREPLACE publication races, and upload source size/mtime/inode changes.

Temporary fixtures only remove their generated test directory after checking its absolute canonical temp prefix, exact UUID-generated basename, non-symlink directory and marker ownership. Tests replacing/deleting individual fixture files check absolute owned paths and generated stage/source identities first.

The delegated implementation did **not run Cargo**, update the root workspace, or commit. Standalone rustfmt checks syntax/formatting only. After the parent adds the workspace member, the parent must run the serialized `cargo test -p mtp-transfer` and platform checks. Dependency pins: workspace serde/serde_json/thiserror, `rustix = '=1.1.6'` with `fs`, `uuid = '=1.29.2'` with `v4`.
