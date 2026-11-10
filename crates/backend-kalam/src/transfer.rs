//! SafetyVersion 2 synchronous FD transfer boundary.
use crate::{
    Kalam, KalamError, Result, State, begin_transfer_callback, callback_address,
    end_transfer_callback, input_json, preprocess_callback, progress_callback,
    validate_remote_path,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::File;
#[cfg(unix)]
use std::os::fd::AsRawFd;

/// The only supported upload policy. No overwrite/Replace variant exists.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SafeUploadPolicy {
    KeepBoth,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SafeUploadRequest {
    pub storage_id: u32,
    /// Required JSON field: None serializes null and means root, not omission.
    pub parent_id: Option<u32>,
    pub destination: String,
    pub requested_name: String,
    pub policy: SafeUploadPolicy,
    pub expected_size: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SafeDownloadRequest {
    pub storage_id: u32,
    pub object_id: u32,
    /// None omits parentId: native then DOES NOT CHECK the parent identity.
    /// Domain root must explicitly pass Some(u32::MAX), not None.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<u32>,
    pub expected_size: u64,
}

impl Kalam {
    pub fn supports_upload(&self) -> bool {
        self.symbols.safety_version == Some(2) && self.symbols.safe_upload.is_some()
    }

    pub fn supports_download(&self) -> bool {
        self.symbols.safety_version == Some(2) && self.symbols.safe_download.is_some()
    }

    /// Borrow an exclusively held immutable regular upload snapshot until return.
    /// Never opens a source pathname. Native owns/closes only its dup; dup shares
    /// offsets, so caller must not read/write/seek/clone-for-concurrent-use.
    pub fn upload(&mut self, request: &SafeUploadRequest, source: &File) -> Result<Value> {
        self.require("SafeUploadSingle", State::Ready)?;
        if !self.supports_upload() {
            return Err(KalamError::TransferUnavailable {
                operation: "upload",
            });
        }
        validate_upload_request(request)?;
        validate_file(source, request.expected_size, false)?;
        let input = input_json(request)?;
        #[cfg(unix)]
        {
            // Checked support above: no optional pointer is called unchecked.
            let function = self
                .symbols
                .safe_upload
                .ok_or(KalamError::TransferUnavailable {
                    operation: "upload",
                })?;
            let result = (|| {
                begin_transfer_callback()?;
                // SAFETY: trusted marker==2 selects exactly the inspected FD ABI.
                // Rust owns input/source throughout this synchronous call. All
                // callbacks are independent function addresses, not ptr slots.
                unsafe {
                    function(
                        input.as_ptr().cast_mut(),
                        source.as_raw_fd(),
                        preprocess_callback as *const () as *mut std::ffi::c_void,
                        progress_callback as *const () as *mut std::ffi::c_void,
                        callback_address(),
                    )
                };
                end_transfer_callback().and_then(|value| validate_upload_done(value, request))
            })();
            result.inspect_err(|_| self.state = State::Failed)
        }
        #[cfg(not(unix))]
        {
            let _ = input;
            Err(KalamError::UnsupportedPlatform)
        }
    }

    /// Borrow an exclusively owned, private, empty, non-append download stage.
    /// committed=true completes ONLY this stage, not publication to a user path.
    /// Caller must create it via a trusted directory FD (EXCL/NOFOLLOW) and retain
    /// ownership for inode-checked no-clobber commit. No target pathname is used.
    pub fn download(&mut self, request: &SafeDownloadRequest, output: &File) -> Result<Value> {
        self.require("SafeDownloadSingle", State::Ready)?;
        if !self.supports_download() {
            return Err(KalamError::TransferUnavailable {
                operation: "download",
            });
        }
        validate_download_request(request)?;
        validate_file(output, request.expected_size, true)?;
        let input = input_json(request)?;
        #[cfg(unix)]
        {
            let function = self
                .symbols
                .safe_download
                .ok_or(KalamError::TransferUnavailable {
                    operation: "download",
                })?;
            let result = (|| {
                begin_transfer_callback()?;
                // SAFETY: exact trusted v2 FD ABI; borrowed File remains open
                // until native returns after closing only its own duplicate.
                unsafe {
                    function(
                        input.as_ptr().cast_mut(),
                        output.as_raw_fd(),
                        progress_callback as *const () as *mut std::ffi::c_void,
                        callback_address(),
                    )
                };
                end_transfer_callback().and_then(|value| {
                    validate_download_done(&value, request)?;
                    let length = output
                        .metadata()
                        .map_err(|_| {
                            KalamError::Envelope("cannot verify completed stage descriptor".into())
                        })?
                        .len();
                    if length != request.expected_size {
                        return Err(KalamError::Envelope(
                            "completed stage descriptor length mismatch".into(),
                        ));
                    }
                    Ok(value)
                })
            })();
            result.inspect_err(|_| self.state = State::Failed)
        }
        #[cfg(not(unix))]
        {
            let _ = input;
            Err(KalamError::UnsupportedPlatform)
        }
    }
}

fn validate_size_storage(storage: u32, expected: u64) -> Result<()> {
    if storage == 0 || storage == u32::MAX {
        return Err(KalamError::Input("reserved storage ID".into()));
    }
    if expected > i64::MAX as u64 {
        return Err(KalamError::Input(
            "expected size exceeds signed native byte count".into(),
        ));
    }
    Ok(())
}

fn is_root(parent: Option<u32>) -> bool {
    parent.is_none_or(|id| id == 0 || id == u32::MAX)
}

fn validate_upload_request(request: &SafeUploadRequest) -> Result<()> {
    validate_size_storage(request.storage_id, request.expected_size)?;
    validate_remote_path(&request.destination)?;
    validate_basename(&request.requested_name)?;
    if is_root(request.parent_id) != (request.destination == "/") {
        return Err(KalamError::Input(
            "upload parent ID does not match root/non-root destination".into(),
        ));
    }
    Ok(())
}

fn validate_download_request(request: &SafeDownloadRequest) -> Result<()> {
    validate_size_storage(request.storage_id, request.expected_size)?;
    if request.object_id == 0 || request.object_id == u32::MAX {
        return Err(KalamError::Input("reserved download object ID".into()));
    }
    // All u32 parent values are either real IDs or explicit equivalent root
    // sentinels 0/MAX. None intentionally means no parent check, not root.
    Ok(())
}

fn validate_basename(name: &str) -> Result<()> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains(['/', '\\', '\0'])
        || name.chars().any(char::is_control)
    {
        return Err(KalamError::Input("unsafe transfer basename".into()));
    }
    Ok(())
}

fn validate_file(file: &File, expected: u64, download: bool) -> Result<()> {
    let metadata = file
        .metadata()
        .map_err(|_| KalamError::Input("cannot inspect transfer descriptor".into()))?;
    if !metadata.is_file() {
        return Err(KalamError::Input(
            "transfer descriptor must be a regular file".into(),
        ));
    }
    if metadata.len() != if download { 0 } else { expected } {
        return Err(KalamError::Input(
            "transfer descriptor length mismatch".into(),
        ));
    }
    #[cfg(unix)]
    {
        // SAFETY: F_GETFL only inspects the borrowed live File descriptor.
        let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
        if flags < 0 {
            return Err(KalamError::Input(
                "cannot inspect transfer descriptor flags".into(),
            ));
        }
        let access = flags & libc::O_ACCMODE;
        if download && (flags & libc::O_APPEND != 0 || access == libc::O_RDONLY) {
            return Err(KalamError::Input(
                "download stage must be writable and not append".into(),
            ));
        }
        if !download && access == libc::O_WRONLY {
            return Err(KalamError::Input("upload snapshot must be readable".into()));
        }
    }
    Ok(())
}

fn validate_commit(value: &Value, expected: u64) -> Result<u32> {
    let data = value
        .get("data")
        .and_then(Value::as_object)
        .ok_or_else(|| KalamError::Envelope("transfer data must be an object".into()))?;
    if data.get("committed").and_then(Value::as_bool) != Some(true)
        || data
            .get("needsReview")
            .is_some_and(|review| review.as_bool() != Some(false))
    {
        return Err(KalamError::Envelope(
            "transfer is not a verified commit".into(),
        ));
    }
    if data.get("bytes").and_then(Value::as_u64) != Some(expected) {
        return Err(KalamError::Envelope(
            "committed transfer byte count mismatch".into(),
        ));
    }
    let id = data
        .get("objectId")
        .and_then(Value::as_u64)
        .and_then(|id| u32::try_from(id).ok())
        .filter(|id| *id != 0 && *id != u32::MAX)
        .ok_or_else(|| {
            KalamError::Envelope("committed transfer has a reserved/invalid object ID".into())
        })?;
    Ok(id)
}

fn validate_upload_done(value: Value, request: &SafeUploadRequest) -> Result<Value> {
    validate_commit(&value, request.expected_size)?;
    let data = &value["data"];
    if data.get("needsReview").and_then(Value::as_bool) != Some(false) {
        return Err(KalamError::Envelope(
            "upload commit lacks needsReview=false".into(),
        ));
    }
    let name = data
        .get("actualName")
        .and_then(Value::as_str)
        .ok_or_else(|| KalamError::Envelope("upload commit lacks actualName".into()))?;
    if validate_basename(name).is_err() || name == request.requested_name || name.len() > 181 {
        return Err(KalamError::Envelope(
            "unsafe KeepBoth committed name".into(),
        ));
    }
    // v2 promises a crypto-random final KeepBoth name, never original-name rename.
    let Some((stem, suffix)) = name.rsplit_once("-mtp-") else {
        return Err(KalamError::Envelope(
            "upload committed name is not a v2 KeepBoth name".into(),
        ));
    };
    if stem.is_empty()
        || suffix.len() < 32
        || !suffix.as_bytes()[..32]
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
        || (!suffix[32..].is_empty() && (!suffix[32..].starts_with('.') || suffix[32..].len() > 24))
    {
        return Err(KalamError::Envelope(
            "upload committed name is not a v2 KeepBoth name".into(),
        ));
    }
    let expected_path = if request.destination == "/" {
        format!("/{name}")
    } else {
        format!("{}/{name}", request.destination)
    };
    if data.get("path").and_then(Value::as_str) != Some(expected_path.as_str()) {
        return Err(KalamError::Envelope(
            "upload committed path does not match destination/name".into(),
        ));
    }
    Ok(value)
}

fn validate_download_done(value: &Value, request: &SafeDownloadRequest) -> Result<()> {
    if validate_commit(value, request.expected_size)? != request.object_id {
        return Err(KalamError::Envelope(
            "download committed object ID mismatch".into(),
        ));
    }
    Ok(())
}
