//! Real, capability-gated Kalam owner. It is isolated in this process, never in GPUI.
use mtp_backend_api::*;
use mtp_backend_kalam::{
    Kalam, KalamError, SafeDownloadRequest, SafeUploadPolicy, SafeUploadRequest, WalkRequest,
};
use mtp_domain::{DeviceKey, RemoteFilename};
use mtp_transfer::{
    ConflictPolicy, DownloadStage, RemoteUploadReceipt, TaskState, TransferError, UploadStage,
};
use serde_json::Value as Json;
use std::{
    collections::{HashMap, HashSet},
    io::{Read, Write},
    path::Path,
};

#[derive(Clone)]
struct CachedObject {
    path: String,
    directory: bool,
    name: String,
    byte_size: Option<u64>,
    parent_id: Option<u32>,
}
struct KalamSession {
    load_error: Option<KalamError>,
    native: Option<Kalam>,
    generation: u64,
    connected: bool,
    initialized: bool,
    storages: HashSet<u32>,
    objects: HashMap<(u32, u32), CachedObject>,
}
impl KalamSession {
    fn new(path: &Path) -> Self {
        let (native, load_error) = match Kalam::load(path) {
            Ok(native) => (Some(native), None),
            Err(error) => (None, Some(error)),
        };
        Self {
            load_error,
            native,
            generation: 0,
            connected: false,
            initialized: false,
            storages: HashSet::new(),
            objects: HashMap::new(),
        }
    }
    fn error(&self, code: ErrorCode, message: impl Into<String>) -> BackendError {
        BackendError {
            code,
            message: message.into(),
            session_usable: self.connected,
            requires_review: false,
            journal_path: None,
        }
    }
    fn invalidate(&mut self) {
        self.connected = false;
        self.storages.clear();
        self.objects.clear();
        self.generation = self.generation.saturating_add(1);
    }
    fn native_error(&mut self, error: KalamError) -> BackendError {
        self.invalidate();
        let code = match &error {
            KalamError::Native { error_type, .. } if error_type == "ErrorMtpDetectFailed" => {
                ErrorCode::NotConnected
            }
            KalamError::Native { error_type, .. } if error_type == "ErrorNeedsReview" => {
                ErrorCode::UnknownOutcome
            }
            _ => ErrorCode::BackendFailure,
        };
        let message = match &error {
            KalamError::Native { error_type, .. } => safe_native_message(error_type).to_owned(),
            _ => error.to_string(),
        };
        let mut output = self.error(code, message);
        output.requires_review = code == ErrorCode::UnknownOutcome;
        output
    }
    fn invalid_data(&mut self, message: &str) -> BackendError {
        self.invalidate();
        self.error(ErrorCode::InvalidData, message)
    }
    fn native(&mut self) -> Result<&mut Kalam, BackendError> {
        if self.native.is_none() {
            return Err(self.error(ErrorCode::NotConnected, "Kalam is not initialized"));
        }
        self.native.as_mut().ok_or_else(|| BackendError {
            code: ErrorCode::NotConnected,
            message: "Kalam is not initialized".into(),
            session_usable: false,
            requires_review: false,
            journal_path: None,
        })
    }
    fn execute(&mut self, command: &Command) -> Outcome {
        match self.perform(command) {
            Ok(value) => Outcome::Success { value },
            Err(error) => Outcome::Failure { error },
        }
    }
    fn local_error(&self, error: &TransferError) -> BackendError {
        let code = match error {
            TransferError::Skipped(_) => ErrorCode::Skipped,
            TransferError::SourceChanged => ErrorCode::SourceChanged,
            TransferError::UnsafeName(_) | TransferError::NotRegular => ErrorCode::InvalidData,
            TransferError::Io(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                ErrorCode::PermissionDenied
            }
            _ => ErrorCode::BackendFailure,
        };
        self.error(
            code,
            "local staging or validation failed; no USB write was dispatched",
        )
    }

    fn review_error(&mut self, journal_path: &Path) -> BackendError {
        let journal_path = LocalPath::from_path(journal_path).ok();
        self.invalidate();
        BackendError {
            code: ErrorCode::UnknownOutcome,
            message: "transfer requires review; do not retry or remove files".into(),
            session_usable: false,
            requires_review: true,
            journal_path,
        }
    }

    fn target_path(&self, storage_id: u32, parent_id: Option<u32>) -> Result<String, BackendError> {
        if !self.storages.contains(&storage_id) {
            return Err(self.error(ErrorCode::NotFound, "select an enumerated current storage"));
        }
        match parent_id {
            None => Ok("/".into()),
            Some(id) => self
                .objects
                .get(&(storage_id, id))
                .filter(|object| object.directory)
                .map(|object| object.path.clone())
                .ok_or_else(|| {
                    self.error(
                        ErrorCode::NotFound,
                        "directory is not in this session cache",
                    )
                }),
        }
    }

    fn upload_file(
        &mut self,
        storage_id: u32,
        parent_id: Option<u32>,
        source: &LocalPath,
        cache: &LocalPath,
        requested_name: &str,
    ) -> Result<Value, BackendError> {
        if !self.native.as_ref().is_some_and(Kalam::supports_upload) {
            return Err(self.error(
                ErrorCode::Unsupported,
                "verified v2 FD upload is unavailable",
            ));
        }
        if RemoteFilename::new(requested_name).is_err()
            || mtp_transfer::validate_basename(requested_name).is_err()
        {
            return Err(self.error(
                ErrorCode::InvalidData,
                "requested upload filename is unsafe",
            ));
        }
        let destination = self.target_path(storage_id, parent_id)?;
        let mut stage = UploadStage::new(&source.to_path_buf(), &cache.to_path_buf())
            .map_err(|error| self.local_error(&error))?;
        let journal = stage.journal_path().to_owned();
        stage
            .check_source_unchanged()
            .and_then(|_| stage.validate_staged())
            .map_err(|error| self.local_error(&error))?;
        let bytes = stage.file_size();
        if bytes > i64::MAX as u64 {
            return Err(self.error(
                ErrorCode::Unsupported,
                "file exceeds signed native transfer size",
            ));
        }
        // Persist native_dispatched=true while retaining Ready before USB.
        // Save failure returns here; native.upload below must not be invoked.
        stage
            .mark_dispatched()
            .map_err(|error| self.local_error(&error))?;
        let request = SafeUploadRequest {
            storage_id,
            parent_id,
            destination,
            requested_name: requested_name.into(),
            policy: SafeUploadPolicy::KeepBoth,
            expected_size: bytes,
        };
        let native = self.native()?.upload(&request, stage.staged_file());
        let result = match native {
            Ok(result) => result,
            Err(error) => {
                let _ = stage.mark_needs_review(&native_review_record(&error));
                return Err(self.review_error(&journal));
            }
        };
        let data = &result["data"];
        let actual_name = data["actualName"]
            .as_str()
            .filter(|name| RemoteFilename::new(*name).is_ok());
        let object_id = data["objectId"]
            .as_u64()
            .and_then(|id| u32::try_from(id).ok())
            .filter(|id| *id != 0 && *id != u32::MAX);
        let path = data["path"].as_str();
        let (Some(actual_name), Some(object_id), Some(path)) = (actual_name, object_id, path)
        else {
            let _ = stage.mark_needs_review("native receipt metadata is incomplete");
            return Err(self.review_error(&journal));
        };
        if data["committed"].as_bool() != Some(true)
            || data["bytes"].as_u64() != Some(bytes)
            || data["needsReview"].as_bool() != Some(false)
        {
            let _ = stage.mark_needs_review("native upload receipt is not a verified commit");
            return Err(self.review_error(&journal));
        }
        let receipt = RemoteUploadReceipt {
            storage_id,
            object_id,
            actual_name: actual_name.into(),
            bytes,
            path: Some(path.into()),
        };
        if stage.mark_uploaded(receipt).is_err() {
            // Remote publication may already have happened: never delete/retry.
            let _ = stage.mark_needs_review("remote upload succeeded; durable receipt save failed");
            return Err(self.review_error(&journal));
        }
        // The transfer published the verified immutable snapshot. A later
        // change to the original source cannot make that target outcome unknown.
        Ok(Value::Transferred {
            direction: TransferDirection::Upload,
            actual_name: actual_name.into(),
            bytes,
            object_id: Some(object_id),
            local_path: None,
            journal_path: LocalPath::from_path(&journal)
                .map_err(|_| self.review_error(&journal))?,
        })
    }

    fn download_file(
        &mut self,
        storage_id: u32,
        object_id: u32,
        destination: &LocalPath,
        policy: FileConflictPolicy,
    ) -> Result<Value, BackendError> {
        if !self.native.as_ref().is_some_and(Kalam::supports_download) {
            return Err(self.error(
                ErrorCode::Unsupported,
                "verified v2 FD download is unavailable",
            ));
        }
        if !self.storages.contains(&storage_id) {
            return Err(self.error(ErrorCode::NotFound, "select an enumerated current storage"));
        }
        let object = self
            .objects
            .get(&(storage_id, object_id))
            .cloned()
            .ok_or_else(|| self.error(ErrorCode::NotFound, "file is not in this session cache"))?;
        if object.directory || object.byte_size.is_none() || object.parent_id.is_none() {
            return Err(self.error(
                ErrorCode::Unsupported,
                "single-file download requires known size and native parent metadata",
            ));
        }
        let bytes = object
            .byte_size
            .ok_or_else(|| self.error(ErrorCode::Unsupported, "unknown file size"))?;
        if bytes > i64::MAX as u64 {
            return Err(self.error(
                ErrorCode::Unsupported,
                "file exceeds signed native transfer size",
            ));
        }
        let policy = match policy {
            FileConflictPolicy::Skip => ConflictPolicy::Skip,
            FileConflictPolicy::KeepBoth => ConflictPolicy::KeepBoth,
        };
        let mut stage =
            DownloadStage::with_policy(&destination.to_path_buf(), &object.name, bytes, policy)
                .map_err(|error| self.local_error(&error))?;
        let journal = stage.journal_path().to_owned();
        // Remain Staging so commit is legal; persist dispatched=true before USB.
        stage
            .mark_dispatched()
            .map_err(|error| self.local_error(&error))?;
        let request = SafeDownloadRequest {
            storage_id,
            object_id,
            parent_id: object.parent_id,
            expected_size: bytes,
        };
        let native = self.native()?.download(&request, stage.payload_file());
        let result = match native {
            Ok(result) => result,
            Err(error) => {
                let _ = stage.mark_needs_review(&native_review_record(&error));
                return Err(self.review_error(&journal));
            }
        };
        let data = &result["data"];
        if data["committed"].as_bool() != Some(true)
            || data["bytes"].as_u64() != Some(bytes)
            || data["objectId"].as_u64() != Some(u64::from(object_id))
        {
            let _ = stage
                .mark_needs_review("native download receipt is not a verified stage completion");
            return Err(self.review_error(&journal));
        }
        let published = match stage.commit() {
            Ok(path) => path,
            Err(error @ TransferError::Skipped(_))
                if stage.journal().state == TaskState::Skipped =>
            {
                let mut error = self.local_error(&error);
                error.message = "destination exists; skipped without overwriting".into();
                error.journal_path = LocalPath::from_path(&journal).ok();
                return Err(error);
            }
            Err(_) => return Err(self.review_error(&journal)),
        };
        let actual_name = published
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .ok_or_else(|| self.review_error(&journal))?
            .to_owned();
        Ok(Value::Transferred {
            direction: TransferDirection::Download,
            actual_name,
            bytes,
            object_id: Some(object_id),
            local_path: Some(
                LocalPath::from_path(&published).map_err(|_| self.review_error(&journal))?,
            ),
            journal_path: LocalPath::from_path(&journal)
                .map_err(|_| self.review_error(&journal))?,
        })
    }

    fn perform(&mut self, command: &Command) -> Result<Value, BackendError> {
        if matches!(&command.operation, Operation::Connect) {
            if self.initialized {
                return Err(self.error(
                    ErrorCode::AlreadyConnected,
                    "start a fresh worker for each Kalam connection",
                ));
            }
            if command.generation.is_some() {
                return Err(self.error(
                    ErrorCode::StaleSession,
                    "connect cannot carry an old generation",
                ));
            }
            self.initialized = true;
            if let Some(error) = self.load_error.take() {
                return Err(self.native_error(error));
            }
            let result = self
                .native()?
                .initialize()
                .map_err(|error| self.native_error(error))?;
            let data = &result["data"]["mtpDeviceInfo"];
            let serial = data["SerialNumber"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or("unidentified-single-session");
            let key = format!("kalam:{serial}");
            if DeviceKey::new(&key).is_err() {
                return Err(self.invalid_data("device identity is invalid"));
            }
            let display = [data["Manufacturer"].as_str(), data["Model"].as_str()]
                .into_iter()
                .flatten()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            self.generation = self
                .generation
                .checked_add(1)
                .ok_or_else(|| self.error(ErrorCode::StaleSession, "generation exhausted"))?;
            self.connected = true;
            return Ok(Value::Connected {
                device_key: key,
                display_name: if display.is_empty() {
                    "Android MTP device".into()
                } else {
                    display
                },
            });
        }
        if matches!(&command.operation, Operation::Shutdown) {
            let result = if let Some(native) = self.native.as_mut() {
                native.dispose().map(|_| ())
            } else {
                Ok(())
            };
            self.invalidate();
            result.map_err(|error| self.native_error(error))?;
            return Ok(Value::ShuttingDown);
        }
        if !self.connected {
            return Err(self.error(
                ErrorCode::NotConnected,
                "device is disconnected; start a fresh worker",
            ));
        }
        if command.generation != Some(self.generation) {
            return Err(self.error(
                ErrorCode::StaleSession,
                "object belongs to an old device session",
            ));
        }
        match &command.operation {
            Operation::Storages => {
                let result = self
                    .native()?
                    .storages()
                    .map_err(|error| self.native_error(error))?;
                let items = native_list(&result["data"]).map_err(|_| {
                    self.invalid_data("storage result must be an array or Go nil slice")
                })?;
                let mut output = Vec::with_capacity(items.len());
                let mut ids = HashSet::new();
                for item in items {
                    // Sid/Info are the fixed go-mtpx StorageData fields. The aliases
                    // allow controlled C ABI fixtures, not text/CLI heuristics.
                    let id = item
                        .get("Sid")
                        .or_else(|| item.get("StorageId"))
                        .and_then(Json::as_u64)
                        .and_then(|id| u32::try_from(id).ok())
                        .ok_or_else(|| self.invalid_data("storage ID missing or out of range"))?;
                    if !ids.insert(id) {
                        return Err(self.invalid_data("duplicate storage identity"));
                    }
                    let info = item.get("Info").or_else(|| item.get("StorageInfo"));
                    let name = info
                        .and_then(|i| i.get("StorageDescription"))
                        .and_then(Json::as_str)
                        .filter(|n| !n.is_empty())
                        .map_or_else(|| format!("Storage {id}"), str::to_owned);
                    let capacity = info
                        .and_then(|i| i.get("MaxCapacity"))
                        .and_then(Json::as_u64);
                    output.push(Storage { id, name, capacity });
                }
                self.storages = ids;
                Ok(Value::Storages { storages: output })
            }
            Operation::List {
                storage_id,
                parent_id,
            } => {
                if !self.storages.contains(storage_id) {
                    return Err(self.error(
                        ErrorCode::NotFound,
                        "enumerate and select a current storage first",
                    ));
                }
                let parent_path = match parent_id {
                    None => "/".into(),
                    Some(id) => match self.objects.get(&(*storage_id, *id)) {
                        Some(object) if object.directory => object.path.clone(),
                        _ => {
                            return Err(self.error(
                                ErrorCode::NotFound,
                                "directory reference is not in this session snapshot",
                            ));
                        }
                    },
                };
                let result = self
                    .native()?
                    .walk(&WalkRequest::directory(*storage_id, parent_path.clone()))
                    .map_err(|error| self.native_error(error))?;
                let items = native_list(&result["data"]).map_err(|_| {
                    self.invalid_data("directory result must be an array or Go nil slice")
                })?;
                let mut entries = Vec::with_capacity(items.len());
                let mut ids = HashSet::new();
                let mut names = HashSet::new();
                let mut cache = Vec::new();
                for item in items {
                    let name = item["name"]
                        .as_str()
                        .ok_or_else(|| self.invalid_data("entry has no name"))?;
                    if RemoteFilename::new(name).is_err() {
                        return Err(self.invalid_data("device returned an unsafe entry name"));
                    }
                    let object_id = item["objectId"]
                        .as_u64()
                        .and_then(|id| u32::try_from(id).ok())
                        .ok_or_else(|| {
                            self.invalid_data("entry object ID missing or out of range")
                        })?;
                    if !ids.insert(object_id) || !names.insert(name.to_owned()) {
                        return Err(self.invalid_data(
                            "ambiguous duplicate directory entries; operations refused",
                        ));
                    }
                    let directory = item["isFolder"]
                        .as_bool()
                        .ok_or_else(|| self.invalid_data("entry has no directory flag"))?;
                    let path = if parent_path == "/" {
                        format!("/{name}")
                    } else {
                        format!("{parent_path}/{name}")
                    };
                    let supplied = item["path"]
                        .as_str()
                        .ok_or_else(|| self.invalid_data("entry has no native operation path"))?;
                    if supplied.trim_end_matches('/') != path {
                        return Err(
                            self.invalid_data("entry path does not match its parent and name")
                        );
                    }
                    let byte_size = if directory {
                        None
                    } else {
                        item["size"].as_u64()
                    };
                    entries.push(Entry {
                        object_id,
                        parent_id: *parent_id,
                        name: name.into(),
                        directory,
                        byte_size,
                    });
                    let native_parent = item["parentId"]
                        .as_u64()
                        .and_then(|id| u32::try_from(id).ok());
                    if native_parent.is_some_and(|id| match parent_id {
                        None => id != 0 && id != u32::MAX,
                        Some(expected) => id != *expected,
                    }) {
                        return Err(self.invalid_data(
                            "entry native parent does not match directory identity",
                        ));
                    }
                    cache.push((
                        (*storage_id, object_id),
                        CachedObject {
                            path,
                            directory,
                            name: name.into(),
                            byte_size,
                            parent_id: native_parent.map(|id| if id == 0 { u32::MAX } else { id }),
                        },
                    ));
                }
                for (key, value) in cache {
                    self.objects.insert(key, value);
                }
                Ok(Value::Entries { entries })
            }
            Operation::UploadFile {
                storage_id,
                parent_id,
                source,
                cache_directory,
                requested_name,
            } => self.upload_file(
                *storage_id,
                *parent_id,
                source,
                cache_directory,
                requested_name,
            ),
            Operation::DownloadFile {
                storage_id,
                object_id,
                destination,
                policy,
            } => self.download_file(*storage_id, *object_id, destination, *policy),
            Operation::Disconnect => {
                let result = self.native()?.dispose();
                self.invalidate();
                result.map_err(|error| self.native_error(error))?;
                Ok(Value::Disconnected)
            }
            Operation::InjectFailure { .. } => Err(self.error(
                ErrorCode::Unsupported,
                "fault injection is only available for the explicit fake backend",
            )),
            Operation::Connect | Operation::Shutdown => {
                Err(self.error(ErrorCode::Unsupported, "unsupported operation"))
            }
        }
    }
}

/// Only inspected enum literals may enter a diagnostic message. Unknown types,
/// native message text, and data never enter Display or the IPC error message.
fn safe_native_message(error_type: &str) -> &'static str {
    match error_type {
        "ErrorMtpDetectFailed" => "native failure: ErrorMtpDetectFailed",
        "ErrorMtpLockExists" => "native failure: ErrorMtpLockExists",
        "ErrorDeviceChanged" => "native failure: ErrorDeviceChanged",
        "ErrorDeviceSetup" => "native failure: ErrorDeviceSetup",
        "ErrorMultipleDevice" => "native failure: ErrorMultipleDevice",
        "ErrorAllowStorageAccess" => "native failure: ErrorAllowStorageAccess",
        "ErrorDeviceLocked" => "native failure: ErrorDeviceLocked",
        "ErrorDeviceInfo" => "native failure: ErrorDeviceInfo",
        "ErrorStorageInfo" => "native failure: ErrorStorageInfo",
        "ErrorNoStorage" => "native failure: ErrorNoStorage",
        "ErrorStorageFull" => "native failure: ErrorStorageFull",
        "ErrorListDirectory" => "native failure: ErrorListDirectory",
        "ErrorFileNotFound" => "native failure: ErrorFileNotFound",
        "ErrorFilePermission" => "native failure: ErrorFilePermission",
        "ErrorLocalFileRead" => "native failure: ErrorLocalFileRead",
        "ErrorInvalidPath" => "native failure: ErrorInvalidPath",
        "ErrorFileTransfer" => "native failure: ErrorFileTransfer",
        "ErrorFileObjectRead" => "native failure: ErrorFileObjectRead",
        "ErrorSendObject" => "native failure: ErrorSendObject",
        "ErrorGeneral" => "native failure: ErrorGeneral",
        "ErrorNeedsReview" => "native failure: ErrorNeedsReview",
        _ => "native failure: details withheld",
    }
}

/// Stored only inside the private local journal, never in Display/UI error text.
fn native_review_record(error: &KalamError) -> String {
    match error {
        KalamError::Native { error_type, message, data } => serde_json::json!({
            "nativeError": { "errorType": error_type, "message": message, "data": data },
            "requiresReview": true,
        }).to_string(),
        _ => serde_json::json!({ "nativeError": { "class": error.to_string() }, "requiresReview": true }).to_string(),
    }
}

fn native_list(value: &Json) -> Result<&[Json], ()> {
    match value {
        Json::Array(items) => Ok(items),
        Json::Null => Ok(&[]),
        _ => Err(()),
    }
}

pub fn serve_kalam(
    reader: &mut impl Read,
    writer: &mut impl Write,
    path: &Path,
) -> Result<(), ProtocolError> {
    let mut session = KalamSession::new(path);
    let upload = session.native.as_ref().is_some_and(Kalam::supports_upload);
    let download = session
        .native
        .as_ref()
        .is_some_and(Kalam::supports_download);
    write_frame(
        writer,
        &ServerMessage::Hello {
            hello: Hello {
                protocol_version: PROTOCOL_VERSION,
                backend: "kalam".into(),
                backend_version: "OpenMTP-5da350b-C-ABI-v2-FD-gated".into(),
                build_id: option_env!("MTP_BUILD_ID")
                    .unwrap_or("development-unversioned")
                    .into(),
                capabilities: Capabilities {
                    simulated: false,
                    browse: true,
                    upload,
                    download,
                    cancel_current: false,
                },
            },
        },
    )?;
    let mut last_id = 0;
    while let Some(command) = read_frame::<Command>(reader)? {
        validate_command(&command, last_id)?;
        last_id = command.request_id;
        let end = matches!(
            &command.operation,
            Operation::Shutdown | Operation::Disconnect
        );
        let outcome = session.execute(&command);
        write_frame(
            writer,
            &ServerMessage::Result {
                request_id: command.request_id,
                generation: session.generation,
                outcome,
            },
        )?;
        if end {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_categories_are_static_whitelist_literals_only() {
        for (kind, expected) in [
            ("ErrorDeviceSetup", "native failure: ErrorDeviceSetup"),
            ("ErrorMtpLockExists", "native failure: ErrorMtpLockExists"),
            ("ErrorListDirectory", "native failure: ErrorListDirectory"),
        ] {
            assert_eq!(safe_native_message(kind), expected);
        }
        for kind in [
            "/private/device/file",
            "ErrorDeviceSetup /private/device/file",
            "ErrorMtpLockExists\nserial=private",
            "UnknownNativeError",
        ] {
            assert_eq!(
                safe_native_message(kind),
                "native failure: details withheld"
            );
        }
    }

    #[test]
    fn native_payload_details_never_enter_backend_error_message() {
        let mut session = KalamSession {
            load_error: None,
            native: None,
            generation: 7,
            connected: true,
            initialized: true,
            storages: HashSet::new(),
            objects: HashMap::new(),
        };
        for kind in [
            "ErrorDeviceSetup",
            "ErrorListDirectory",
            "ErrorDeviceSetup /private/device/file",
        ] {
            let error = session.native_error(KalamError::Native {
                error_type: kind.into(),
                message: "/private/device/file serial=secret".into(),
                data: Some(serde_json::json!({"path":"/private/device/file", "serial":"secret"})),
            });
            assert_eq!(error.message, safe_native_message(kind));
            assert!(!error.message.contains("/private"));
            assert!(!error.message.contains("secret"));
            assert!(!error.session_usable);
        }
    }

    #[test]
    fn go_nil_slice_is_an_empty_directory() {
        assert_eq!(native_list(&Json::Null), Ok([].as_slice()));
    }
    #[test]
    fn invalid_native_data_is_not_silently_empty() {
        assert!(native_list(&serde_json::json!({"unexpected":true})).is_err());
    }
}
