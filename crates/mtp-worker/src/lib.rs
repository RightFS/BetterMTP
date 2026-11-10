//! Single-owner fake session. No USB, filesystem writes or implicit backend fallback.
use mtp_backend_api::*;
use std::io::{Read, Write};

pub mod kalam;

#[derive(Default)]
pub struct FakeSession {
    generation: u64,
    connected: bool,
}

impl FakeSession {
    pub fn generation(&self) -> u64 {
        self.generation
    }
    fn advance(&mut self) -> Result<(), BackendError> {
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or_else(|| self.error(ErrorCode::StaleSession, "generation exhausted", true))?;
        Ok(())
    }
    fn error(&self, code: ErrorCode, message: &str, requires_review: bool) -> BackendError {
        BackendError {
            code,
            message: message.into(),
            session_usable: self.connected,
            requires_review,
        }
    }
    fn require_session(&self, generation: Option<u64>) -> Result<(), BackendError> {
        if !self.connected {
            return Err(self.error(ErrorCode::NotConnected, "connect before browsing", false));
        }
        if generation != Some(self.generation) {
            return Err(self.error(
                ErrorCode::StaleSession,
                "remote reference belongs to another session",
                false,
            ));
        }
        Ok(())
    }
    pub fn execute(&mut self, command: &Command) -> Outcome {
        match self.perform(command) {
            Ok(value) => Outcome::Success { value },
            Err(error) => Outcome::Failure { error },
        }
    }
    fn perform(&mut self, command: &Command) -> Result<Value, BackendError> {
        match &command.operation {
            Operation::Connect => {
                if command.generation.is_some() {
                    return Err(self.error(
                        ErrorCode::StaleSession,
                        "connect must not carry a previous session",
                        false,
                    ));
                }
                if self.connected {
                    return Err(self.error(
                        ErrorCode::AlreadyConnected,
                        "disconnect explicitly before reconnecting",
                        false,
                    ));
                }
                self.advance()?;
                self.connected = true;
                Ok(Value::Connected {
                    device_key: "fake-device".into(),
                    display_name: "Simulated Android — no USB".into(),
                })
            }
            Operation::Shutdown => {
                self.connected = false;
                Ok(Value::ShuttingDown)
            }
            operation => {
                self.require_session(command.generation)?;
                match operation {
                    Operation::Storages => Ok(Value::Storages {
                        storages: vec![Storage {
                            id: 1,
                            name: "Simulated internal storage".into(),
                            capacity: Some(128 * 1024 * 1024 * 1024),
                        }],
                    }),
                    Operation::List {
                        storage_id,
                        parent_id,
                    } => {
                        if *storage_id != 1 {
                            return Err(self.error(
                                ErrorCode::NotFound,
                                "unknown simulated storage",
                                false,
                            ));
                        }
                        let entries = match parent_id {
                            None => vec![
                                Entry {
                                    object_id: 1,
                                    parent_id: None,
                                    name: "照片".into(),
                                    directory: true,
                                    byte_size: None,
                                },
                                Entry {
                                    object_id: 2,
                                    parent_id: None,
                                    name: "large-fixture.bin".into(),
                                    directory: false,
                                    byte_size: Some(5 * 1024 * 1024 * 1024),
                                },
                            ],
                            Some(1) => vec![Entry {
                                object_id: 3,
                                parent_id: Some(1),
                                name: "空文件.txt".into(),
                                directory: false,
                                byte_size: Some(0),
                            }],
                            _ => {
                                return Err(self.error(
                                    ErrorCode::NotFound,
                                    "unknown simulated directory",
                                    false,
                                ));
                            }
                        };
                        Ok(Value::Entries { entries })
                    }
                    Operation::Disconnect => {
                        self.connected = false;
                        self.advance()?;
                        Ok(Value::Disconnected)
                    }
                    Operation::InjectFailure { fault } => {
                        let (code, message, review) = match fault {
                            Fault::PermissionDenied => (
                                ErrorCode::PermissionDenied,
                                "injected permission failure",
                                false,
                            ),
                            Fault::NoSpace => {
                                (ErrorCode::NoSpace, "injected capacity failure", false)
                            }
                            Fault::UnknownOutcome => (
                                ErrorCode::UnknownOutcome,
                                "injected lost-response result; no real write occurred",
                                true,
                            ),
                        };
                        if review {
                            self.connected = false;
                            // Exhaustion must not change an uncertain result or make
                            // its session usable; reconnect still fails without wrap.
                            let _ = self.advance();
                        }
                        Err(self.error(code, message, review))
                    }
                    Operation::Connect | Operation::Shutdown => unreachable!("handled above"),
                }
            }
        }
    }
}

pub fn serve(reader: &mut impl Read, writer: &mut impl Write) -> Result<(), ProtocolError> {
    let hello = Hello {
        protocol_version: PROTOCOL_VERSION,
        backend: "fake".into(),
        backend_version: env!("CARGO_PKG_VERSION").into(),
        build_id: option_env!("MTP_BUILD_ID")
            .unwrap_or("development-unversioned")
            .into(),
        capabilities: Capabilities {
            simulated: true,
            browse: true,
            upload: false,
            download: false,
            cancel_current: false,
        },
    };
    write_frame(writer, &ServerMessage::Hello { hello })?;
    let mut owner = FakeSession::default();
    let mut previous_id = 0;
    while let Some(command) = read_frame::<Command>(reader)? {
        // A malformed/replayed request closes the transport, rather than emitting
        // a second terminal result under the already-used request ID.
        validate_command(&command, previous_id)?;
        previous_id = command.request_id;
        let shutdown = matches!(command.operation, Operation::Shutdown);
        let outcome = owner.execute(&command);
        write_frame(
            writer,
            &ServerMessage::Result {
                request_id: command.request_id,
                generation: owner.generation(),
                outcome,
            },
        )?;
        if shutdown {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    fn command(id: u64, generation: Option<u64>, operation: Operation) -> Command {
        Command {
            protocol_version: PROTOCOL_VERSION,
            request_id: id,
            generation,
            operation,
        }
    }
    #[test]
    fn no_session_is_explicit_error() {
        assert!(matches!(
            FakeSession::default().execute(&command(1, None, Operation::Storages)),
            Outcome::Failure {
                error: BackendError {
                    code: ErrorCode::NotConnected,
                    ..
                }
            }
        ));
    }
    #[test]
    fn reconnect_rejects_old_generation() {
        let mut s = FakeSession::default();
        s.execute(&command(1, None, Operation::Connect));
        let old = s.generation();
        s.execute(&command(2, Some(old), Operation::Disconnect));
        s.execute(&command(3, None, Operation::Connect));
        assert_ne!(s.generation(), old);
        assert!(matches!(
            s.execute(&command(4, Some(old), Operation::Storages)),
            Outcome::Failure {
                error: BackendError {
                    code: ErrorCode::StaleSession,
                    ..
                }
            }
        ));
    }
    #[test]
    fn unknown_result_requires_review_and_invalidates_session() {
        let mut s = FakeSession::default();
        s.execute(&command(1, None, Operation::Connect));
        assert!(matches!(
            s.execute(&command(
                2,
                Some(s.generation()),
                Operation::InjectFailure {
                    fault: Fault::UnknownOutcome
                }
            )),
            Outcome::Failure {
                error: BackendError {
                    requires_review: true,
                    session_usable: false,
                    ..
                }
            }
        ));
        assert!(!s.connected);
    }
    #[test]
    fn duplicate_connect_does_not_replace_owner() {
        let mut s = FakeSession::default();
        s.execute(&command(1, None, Operation::Connect));
        let generation = s.generation();
        assert!(matches!(
            s.execute(&command(2, None, Operation::Connect)),
            Outcome::Failure { .. }
        ));
        assert_eq!(s.generation(), generation);
    }
    #[test]
    fn overflow_does_not_wrap_session() {
        let mut s = FakeSession {
            generation: u64::MAX,
            connected: false,
        };
        assert!(matches!(
            s.execute(&command(1, None, Operation::Connect)),
            Outcome::Failure { .. }
        ));
        assert!(!s.connected);
    }
    #[test]
    fn exhausted_generation_still_invalidates_session() {
        for operation in [
            Operation::Disconnect,
            Operation::InjectFailure {
                fault: Fault::UnknownOutcome,
            },
        ] {
            let mut s = FakeSession {
                generation: u64::MAX,
                connected: true,
            };
            let outcome = s.execute(&command(1, Some(u64::MAX), operation.clone()));
            assert!(matches!(
                &outcome,
                Outcome::Failure {
                    error: BackendError {
                        session_usable: false,
                        ..
                    }
                }
            ));
            if matches!(operation, Operation::InjectFailure { .. }) {
                assert!(matches!(
                    outcome,
                    Outcome::Failure {
                        error: BackendError {
                            code: ErrorCode::UnknownOutcome,
                            requires_review: true,
                            ..
                        }
                    }
                ));
            }
            assert!(!s.connected);
            assert_eq!(s.generation(), u64::MAX);
            assert!(matches!(
                s.execute(&command(2, None, Operation::Connect)),
                Outcome::Failure { .. }
            ));
        }
    }
    #[test]
    fn terminal_once_and_shutdown_closes_loop() {
        let mut input = Vec::new();
        write_frame(&mut input, &command(1, None, Operation::Connect)).unwrap();
        write_frame(&mut input, &command(2, None, Operation::Shutdown)).unwrap();
        let mut output = Vec::new();
        serve(&mut input.as_slice(), &mut output).unwrap();
        let mut frames = output.as_slice();
        assert!(matches!(
            read_frame::<ServerMessage>(&mut frames).unwrap(),
            Some(ServerMessage::Hello { .. })
        ));
        for id in [1, 2] {
            assert!(
                matches!(read_frame::<ServerMessage>(&mut frames).unwrap(),Some(ServerMessage::Result { request_id,.. }) if request_id==id)
            );
        }
        assert!(read_frame::<ServerMessage>(&mut frames).unwrap().is_none());
    }
    #[test]
    fn replay_closes_without_duplicate_result() {
        let c = command(1, None, Operation::Connect);
        let mut input = Vec::new();
        write_frame(&mut input, &c).unwrap();
        write_frame(&mut input, &c).unwrap();
        let mut output = Vec::new();
        assert!(matches!(
            serve(&mut input.as_slice(), &mut output),
            Err(ProtocolError::RequestOrder)
        ));
        let mut frames = output.as_slice();
        read_frame::<ServerMessage>(&mut frames).unwrap();
        read_frame::<ServerMessage>(&mut frames).unwrap();
        assert!(read_frame::<ServerMessage>(&mut frames).unwrap().is_none());
    }
}
