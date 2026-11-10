//! Real, read-only Kalam owner. It is isolated in this process, never in GPUI.
use mtp_backend_api::*;
use mtp_backend_kalam::{Kalam, KalamError, WalkRequest};
use mtp_domain::{DeviceKey, RemoteFilename};
use serde_json::Value as Json;
use std::{
    collections::{HashMap, HashSet},
    io::{Read, Write},
    path::{Path, PathBuf},
};

struct CachedObject {
    path: String,
    directory: bool,
}
struct KalamSession {
    library_path: PathBuf,
    native: Option<Kalam>,
    generation: u64,
    connected: bool,
    initialized: bool,
    storages: HashSet<u32>,
    objects: HashMap<(u32, u32), CachedObject>,
}
impl KalamSession {
    fn new(path: &Path) -> Self {
        Self {
            library_path: path.to_owned(),
            native: None,
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
        self.error(ErrorCode::BackendFailure, error.to_string())
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
        })
    }
    fn execute(&mut self, command: &Command) -> Outcome {
        match self.perform(command) {
            Ok(value) => Outcome::Success { value },
            Err(error) => Outcome::Failure { error },
        }
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
            self.native =
                Some(Kalam::load(&self.library_path).map_err(|error| self.native_error(error))?);
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
                    cache.push(((*storage_id, object_id), CachedObject { path, directory }));
                }
                for (key, value) in cache {
                    self.objects.insert(key, value);
                }
                Ok(Value::Entries { entries })
            }
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
    write_frame(
        writer,
        &ServerMessage::Hello {
            hello: Hello {
                protocol_version: PROTOCOL_VERSION,
                backend: "kalam".into(),
                backend_version: "OpenMTP-5da350b-C-ABI-readonly".into(),
                build_id: option_env!("MTP_BUILD_ID")
                    .unwrap_or("development-unversioned")
                    .into(),
                capabilities: Capabilities {
                    simulated: false,
                    browse: true,
                    upload: false,
                    download: false,
                    cancel_current: false,
                },
            },
        },
    )?;
    let mut session = KalamSession::new(path);
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
    fn go_nil_slice_is_an_empty_directory() {
        assert_eq!(native_list(&Json::Null), Ok([].as_slice()));
    }
    #[test]
    fn invalid_native_data_is_not_silently_empty() {
        assert!(native_list(&serde_json::json!({"unexpected":true})).is_err());
    }
}
