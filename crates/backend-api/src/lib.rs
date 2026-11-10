//! Versioned, bounded messages. File contents never travel through this protocol.
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
use thiserror::Error;

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hello {
    pub protocol_version: u32,
    pub backend: String,
    pub backend_version: String,
    pub build_id: String,
    pub capabilities: Capabilities,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    pub simulated: bool,
    pub browse: bool,
    pub upload: bool,
    pub download: bool,
    pub cancel_current: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Command {
    pub protocol_version: u32,
    /// Strictly increasing within one transport, including failed requests.
    pub request_id: u64,
    /// Wire integer; application services convert to domain SessionGeneration.
    pub generation: Option<u64>,
    pub operation: Operation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Connect,
    Storages,
    List {
        storage_id: u32,
        parent_id: Option<u32>,
    },
    Disconnect,
    InjectFailure {
        fault: Fault,
    },
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fault {
    PermissionDenied,
    NoSpace,
    UnknownOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ServerMessage {
    Hello {
        hello: Hello,
    },
    Result {
        request_id: u64,
        generation: u64,
        outcome: Outcome,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Outcome {
    Success { value: Value },
    Failure { error: BackendError },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Value {
    Connected {
        device_key: String,
        display_name: String,
    },
    Storages {
        storages: Vec<Storage>,
    },
    Entries {
        entries: Vec<Entry>,
    },
    Disconnected,
    ShuttingDown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Storage {
    pub id: u32,
    pub name: String,
    pub capacity: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub object_id: u32,
    pub parent_id: Option<u32>,
    pub name: String,
    pub directory: bool,
    pub byte_size: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackendError {
    pub code: ErrorCode,
    pub message: String,
    pub session_usable: bool,
    /// Never inferred from the fact that an IPC operation completed.
    pub requires_review: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    BackendFailure,
    InvalidData,
    Unsupported,
    NotConnected,
    AlreadyConnected,
    StaleSession,
    NotFound,
    PermissionDenied,
    NoSpace,
    UnknownOutcome,
}

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("transport IO: {0}")]
    Io(#[from] io::Error),
    #[error("invalid protocol JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("frame length {0} exceeds allowed bounds")]
    FrameLength(usize),
    #[error("truncated frame")]
    Truncated,
    #[error("incompatible protocol version {0}")]
    Version(u32),
    #[error("request IDs must be positive and strictly increasing")]
    RequestOrder,
}

/// Four-byte big-endian length plus UTF-8 JSON; EOF is only valid between frames.
pub fn read_frame<T: serde::de::DeserializeOwned>(
    reader: &mut impl Read,
) -> Result<Option<T>, ProtocolError> {
    let mut prefix = [0_u8; 4];
    loop {
        match reader.read(&mut prefix[..1]) {
            Ok(0) => return Ok(None),
            Ok(_) => break,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        }
    }
    read_exact(reader, &mut prefix[1..])?;
    let length = u32::from_be_bytes(prefix) as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(ProtocolError::FrameLength(length));
    }
    let mut payload = vec![0; length];
    read_exact(reader, &mut payload)?;
    Ok(Some(serde_json::from_slice(&payload)?))
}

fn read_exact(reader: &mut impl Read, bytes: &mut [u8]) -> Result<(), ProtocolError> {
    reader.read_exact(bytes).map_err(|e| {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            ProtocolError::Truncated
        } else {
            ProtocolError::Io(e)
        }
    })
}

pub fn write_frame<T: Serialize>(
    writer: &mut impl Write,
    message: &T,
) -> Result<(), ProtocolError> {
    // Bound serialization itself, not just the subsequent transport write.
    struct Bounded(Vec<u8>);
    impl Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > MAX_FRAME_BYTES.saturating_sub(self.0.len()) {
                return Err(io::Error::other("serialized frame exceeds limit"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut payload = Bounded(Vec::new());
    serde_json::to_writer(&mut payload, message)?;
    writer.write_all(&(payload.0.len() as u32).to_be_bytes())?;
    writer.write_all(&payload.0)?;
    writer.flush()?;
    Ok(())
}

pub fn validate_command(command: &Command, previous_id: u64) -> Result<(), ProtocolError> {
    if command.protocol_version != PROTOCOL_VERSION {
        return Err(ProtocolError::Version(command.protocol_version));
    }
    if command.request_id == 0 || command.request_id <= previous_id {
        return Err(ProtocolError::RequestOrder);
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::io::Cursor;
    fn command() -> Command {
        Command {
            protocol_version: 1,
            request_id: 1,
            generation: None,
            operation: Operation::Connect,
        }
    }
    #[test]
    fn round_trip() {
        let mut data = Vec::new();
        write_frame(&mut data, &command()).unwrap();
        let mut reader = Cursor::new(data);
        assert_eq!(read_frame::<Command>(&mut reader).unwrap(), Some(command()));
        assert_eq!(read_frame::<Command>(&mut reader).unwrap(), None);
    }
    #[test]
    fn two_frames() {
        let mut data = Vec::new();
        write_frame(&mut data, &command()).unwrap();
        write_frame(&mut data, &command()).unwrap();
        let mut reader = Cursor::new(data);
        assert!(read_frame::<Command>(&mut reader).unwrap().is_some());
        assert!(read_frame::<Command>(&mut reader).unwrap().is_some());
    }
    #[test]
    fn oversized_before_allocation() {
        let mut data = Cursor::new(((MAX_FRAME_BYTES + 1) as u32).to_be_bytes());
        assert!(matches!(
            read_frame::<Command>(&mut data),
            Err(ProtocolError::FrameLength(_))
        ));
    }
    #[test]
    fn zero_length_rejected() {
        assert!(matches!(
            read_frame::<Command>(&mut Cursor::new([0_u8; 4])),
            Err(ProtocolError::FrameLength(0))
        ));
    }
    #[test]
    fn truncated_prefix() {
        assert!(matches!(
            read_frame::<Command>(&mut Cursor::new([0, 0])),
            Err(ProtocolError::Truncated)
        ));
    }
    #[test]
    fn truncated_payload() {
        assert!(matches!(
            read_frame::<Command>(&mut Cursor::new([0, 0, 0, 4, b'{'])),
            Err(ProtocolError::Truncated)
        ));
    }
    #[test]
    fn invalid_json() {
        assert!(matches!(
            read_frame::<Command>(&mut Cursor::new([0, 0, 0, 1, b'?'])),
            Err(ProtocolError::Json(_))
        ));
    }
    #[test]
    fn unknown_fields_rejected() {
        assert!(serde_json::from_str::<Command>(r#"{"protocol_version":1,"request_id":1,"generation":null,"operation":{"kind":"connect"},"shell":"rm"}"#).is_err());
    }
    #[test]
    fn oversized_serialization_rejected() {
        assert!(write_frame(&mut Vec::new(), &"x".repeat(MAX_FRAME_BYTES + 1)).is_err());
    }
    #[test]
    fn version_rejected() {
        let mut c = command();
        c.protocol_version = 2;
        assert!(matches!(
            validate_command(&c, 0),
            Err(ProtocolError::Version(2))
        ));
    }
    #[test]
    fn repeated_and_zero_ids_rejected() {
        assert!(validate_command(&command(), 1).is_err());
        let mut c = command();
        c.request_id = 0;
        assert!(validate_command(&c, 0).is_err());
    }
    #[test]
    fn u64_not_rounded() {
        let s = Storage {
            id: 1,
            name: "SD".into(),
            capacity: Some(u64::MAX),
        };
        let bytes = serde_json::to_vec(&s).unwrap();
        assert_eq!(serde_json::from_slice::<Storage>(&bytes).unwrap(), s);
    }
}
