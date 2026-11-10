//! Blocking application services. Run on a background executor, never GPUI Render.
//! Worker generations are translated into process-wide application tokens, so a
//! restarted worker cannot accidentally make old object references valid again.
use mtp_backend_api::*;
use mtp_domain::{
    DeviceKey, DomainError, ObjectId, RemoteDirectoryRef, RemoteRef, SessionGeneration, StorageId,
};
use std::{
    io,
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command as ProcessCommand, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver},
    },
    time::{Duration, Instant},
};
use thiserror::Error;

static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);
const DEFAULT_DEADLINE: Duration = Duration::from_secs(20);

#[derive(Debug, Clone)]
pub enum BackendConfig {
    Fake,
    Kalam { library_path: PathBuf },
}
impl BackendConfig {
    fn name(&self) -> &'static str {
        match self {
            Self::Fake => "fake",
            Self::Kalam { .. } => "kalam",
        }
    }
}
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub device_key: DeviceKey,
    pub display_name: String,
    pub generation: SessionGeneration,
    pub capabilities: Capabilities,
}
impl SessionInfo {
    pub fn root(&self, storage: StorageId) -> RemoteDirectoryRef {
        RemoteDirectoryRef::new(self.device_key.clone(), self.generation, storage, None)
    }
}
#[derive(Debug, Clone)]
pub struct RemoteEntry {
    pub reference: RemoteRef,
    pub name: String,
    pub directory: bool,
    pub byte_size: Option<u64>,
}

#[derive(Debug, Error)]
pub enum ApplicationError {
    #[error("worker IO: {0}")]
    Io(#[from] io::Error),
    #[error("worker protocol: {0}")]
    Protocol(#[from] ProtocolError),
    #[error("worker did not respond within {0:?}")]
    TimedOut(Duration),
    #[error("worker disconnected before returning a result")]
    Disconnected,
    #[error("worker response does not match its request")]
    UnexpectedResponse,
    #[error("worker handshake is incompatible with requested backend")]
    IncompatibleHello,
    #[error("connect before accessing a device")]
    NotConnected,
    #[error("backend {code:?}: {message}")]
    Backend {
        code: ErrorCode,
        message: String,
        requires_review: bool,
    },
    #[error(transparent)]
    Domain(#[from] DomainError),
}

pub struct WorkerClient {
    child: Child,
    input: Option<ChildStdin>,
    messages: Receiver<Result<Option<ServerMessage>, ProtocolError>>,
    hello: Hello,
    next_request: u64,
    deadline: Duration,
    wire_generation: Option<u64>,
    session: Option<SessionInfo>,
    closed: bool,
}
impl WorkerClient {
    pub fn spawn(path: &Path, backend: &BackendConfig) -> Result<Self, ApplicationError> {
        Self::spawn_with_deadline(path, backend, DEFAULT_DEADLINE)
    }
    pub fn spawn_with_deadline(
        path: &Path,
        backend: &BackendConfig,
        deadline: Duration,
    ) -> Result<Self, ApplicationError> {
        let mut command = ProcessCommand::new(path);
        command.args(["--backend", backend.name()]);
        if let BackendConfig::Kalam { library_path } = backend {
            command.arg("--library").arg(library_path);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let input = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("worker stdin unavailable"))?;
        let mut output = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("worker stdout unavailable"))?;
        let (sender, messages) = mpsc::sync_channel(8);
        std::thread::spawn(move || {
            loop {
                let message = read_frame::<ServerMessage>(&mut output);
                let done = !matches!(&message, Ok(Some(_)));
                if sender.send(message).is_err() || done {
                    break;
                }
            }
        });
        let mut client = Self {
            child,
            input: Some(input),
            messages,
            hello: Hello {
                protocol_version: 0,
                backend: String::new(),
                backend_version: String::new(),
                build_id: String::new(),
                capabilities: Capabilities {
                    simulated: false,
                    browse: false,
                    upload: false,
                    download: false,
                    cancel_current: false,
                },
            },
            next_request: 1,
            deadline,
            wire_generation: None,
            session: None,
            closed: false,
        };
        match client.receive()? {
            ServerMessage::Hello { hello }
                if hello.protocol_version == PROTOCOL_VERSION
                    && hello.backend == backend.name()
                    && hello.capabilities.simulated == matches!(backend, BackendConfig::Fake) =>
            {
                client.hello = hello
            }
            _ => return Err(ApplicationError::IncompatibleHello),
        }
        Ok(client)
    }
    pub fn hello(&self) -> &Hello {
        &self.hello
    }
    pub fn session(&self) -> Option<&SessionInfo> {
        self.session.as_ref()
    }
    fn receive(&mut self) -> Result<ServerMessage, ApplicationError> {
        match self.messages.recv_timeout(self.deadline) {
            Ok(Ok(Some(message))) => Ok(message),
            Ok(Err(error)) => {
                self.invalidate();
                Err(error.into())
            }
            Ok(Ok(None)) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                self.invalidate();
                Err(ApplicationError::Disconnected)
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.invalidate();
                Err(ApplicationError::TimedOut(self.deadline))
            }
        }
    }
    fn call(
        &mut self,
        generation: Option<u64>,
        operation: Operation,
    ) -> Result<(u64, Value), ApplicationError> {
        if self.closed {
            return Err(ApplicationError::Disconnected);
        }
        let request_id = self.next_request;
        self.next_request = self
            .next_request
            .checked_add(1)
            .ok_or_else(|| io::Error::other("request IDs exhausted"))?;
        let stable_session = matches!(&operation, Operation::Storages | Operation::List { .. });
        let input = self.input.as_mut().ok_or(ApplicationError::Disconnected)?;
        if let Err(error) = write_frame(
            input,
            &Command {
                protocol_version: PROTOCOL_VERSION,
                request_id,
                generation,
                operation,
            },
        ) {
            self.invalidate();
            return Err(error.into());
        }
        match self.receive()? {
            ServerMessage::Result {
                request_id: received,
                generation: received_generation,
                outcome,
            } if received == request_id => {
                if stable_session
                    && Some(received_generation) != generation
                    && matches!(
                        &outcome,
                        Outcome::Success { .. }
                            | Outcome::Failure {
                                error: BackendError {
                                    session_usable: true,
                                    ..
                                }
                            }
                    )
                {
                    self.invalidate();
                    return Err(ApplicationError::UnexpectedResponse);
                }
                match outcome {
                    Outcome::Success { value } => Ok((received_generation, value)),
                    Outcome::Failure { error } => {
                        if !error.session_usable
                            || error.requires_review
                            || error.code == ErrorCode::StaleSession
                        {
                            self.invalidate();
                        }
                        Err(ApplicationError::Backend {
                            code: error.code,
                            message: error.message,
                            requires_review: error.requires_review,
                        })
                    }
                }
            }
            _ => {
                self.invalidate();
                Err(ApplicationError::UnexpectedResponse)
            }
        }
    }
    pub fn connect(&mut self) -> Result<SessionInfo, ApplicationError> {
        let (wire, value) = self.call(None, Operation::Connect)?;
        let Value::Connected {
            device_key,
            display_name,
        } = value
        else {
            self.invalidate();
            return Err(ApplicationError::UnexpectedResponse);
        };
        let key = match DeviceKey::new(device_key) {
            Ok(key) => key,
            Err(error) => {
                self.invalidate();
                return Err(error.into());
            }
        };
        let token = match NEXT_SESSION
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
        {
            Ok(token) => token,
            Err(_) => {
                self.invalidate();
                return Err(DomainError::GenerationOverflow.into());
            }
        };
        let info = SessionInfo {
            device_key: key,
            display_name,
            generation: SessionGeneration(token),
            capabilities: self.hello.capabilities.clone(),
        };
        self.wire_generation = Some(wire);
        self.session = Some(info.clone());
        Ok(info)
    }
    pub fn storages(&mut self) -> Result<Vec<Storage>, ApplicationError> {
        let generation = self.wire_generation.ok_or(ApplicationError::NotConnected)?;
        let (_, value) = self.call(Some(generation), Operation::Storages)?;
        match value {
            Value::Storages { storages } => Ok(storages),
            _ => {
                self.invalidate();
                Err(ApplicationError::UnexpectedResponse)
            }
        }
    }
    pub fn list(
        &mut self,
        directory: &RemoteDirectoryRef,
    ) -> Result<Vec<RemoteEntry>, ApplicationError> {
        let info = self
            .session
            .as_ref()
            .ok_or(ApplicationError::NotConnected)?
            .clone();
        directory.validate_session(&info.device_key, info.generation)?;
        let (_, value) = self.call(
            self.wire_generation,
            Operation::List {
                storage_id: directory.storage().0,
                parent_id: directory.parent().map(|id| id.0),
            },
        )?;
        let Value::Entries { entries } = value else {
            self.invalidate();
            return Err(ApplicationError::UnexpectedResponse);
        };
        let mut output = Vec::with_capacity(entries.len());
        for entry in entries {
            // Reject unsafe native names rather than reconstructing operation paths
            // from labels. All actions retain this session's object identity.
            if let Err(error) = mtp_domain::RemoteFilename::new(&entry.name) {
                self.invalidate();
                return Err(error.into());
            }
            output.push(RemoteEntry {
                reference: RemoteRef::new(
                    info.device_key.clone(),
                    info.generation,
                    directory.storage(),
                    ObjectId(entry.object_id),
                ),
                name: entry.name,
                directory: entry.directory,
                byte_size: entry.byte_size,
            });
        }
        Ok(output)
    }
    pub fn disconnect(&mut self) -> Result<(), ApplicationError> {
        if let Some(generation) = self.wire_generation {
            let result = self.call(Some(generation), Operation::Disconnect);
            self.invalidate();
            match result? {
                (_, Value::Disconnected) => Ok(()),
                _ => Err(ApplicationError::UnexpectedResponse),
            }
        } else {
            self.invalidate();
            Ok(())
        }
    }
    pub fn shutdown(mut self) -> Result<(), ApplicationError> {
        if self.closed {
            return Ok(());
        }
        let (_, value) = self.call(None, Operation::Shutdown)?;
        if !matches!(value, Value::ShuttingDown) {
            self.invalidate();
            return Err(ApplicationError::UnexpectedResponse);
        }
        self.input.take();
        let deadline = Instant::now() + self.deadline;
        loop {
            if let Some(status) = self.child.try_wait()? {
                self.closed = true;
                return if status.success() {
                    Ok(())
                } else {
                    Err(ApplicationError::Disconnected)
                };
            }
            if Instant::now() >= deadline {
                self.invalidate();
                return Err(ApplicationError::TimedOut(self.deadline));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn invalidate(&mut self) {
        self.closed = true;
        self.session = None;
        self.wire_generation = None;
        self.input.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Drop for WorkerClient {
    fn drop(&mut self) {
        self.invalidate();
    }
}
