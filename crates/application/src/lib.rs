//! Blocking application services. Run on a background executor, never GPUI Render.
//! Worker generations are translated into process-wide application tokens, so a
//! restarted worker cannot accidentally make old object references valid again.
use mtp_backend_api::*;
use mtp_domain::{
    DeviceKey, DomainError, ObjectId, RemoteDirectoryRef, RemoteRef, SessionGeneration, StorageId,
};
use std::{
    collections::HashMap,
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

pub use mtp_backend_api::{FileConflictPolicy, LocalPath, TransferDirection};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferReceipt {
    pub direction: TransferDirection,
    pub actual_name: String,
    pub bytes: u64,
    pub object_id: Option<u32>,
    pub local_path: Option<LocalPath>,
    pub journal_path: LocalPath,
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
    #[error("transfer outcome is unknown; RequiresReview, do not retry or delete files")]
    UnknownOutcome { journal_path: Option<LocalPath> },
    #[error(transparent)]
    Domain(#[from] DomainError),
}

/// Process-stop handle, not USB cancellation. Killing a dispatched transfer
/// yields RequiresReview; the owning client observes EOF and reaps the child.
#[derive(Clone)]
pub struct WorkerStopHandle {
    child: std::sync::Arc<std::sync::Mutex<Child>>,
}
impl WorkerStopHandle {
    pub fn request_stop(&self) -> io::Result<()> {
        let mut child = self
            .child
            .lock()
            .map_err(|_| io::Error::other("worker child lock poisoned"))?;
        if child.try_wait()?.is_none() {
            child.kill()?;
        }
        Ok(())
    }
}

pub struct WorkerClient {
    child: std::sync::Arc<std::sync::Mutex<Child>>,
    input: Option<ChildStdin>,
    messages: Receiver<Result<Option<ServerMessage>, ProtocolError>>,
    hello: Hello,
    next_request: u64,
    deadline: Duration,
    transfer_deadline_override: Option<Duration>,
    entry_sizes: HashMap<(u32, u32), u64>,
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
            child: std::sync::Arc::new(std::sync::Mutex::new(child)),
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
            transfer_deadline_override: None,
            entry_sizes: HashMap::new(),
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
    pub fn stop_handle(&self) -> WorkerStopHandle {
        WorkerStopHandle {
            child: self.child.clone(),
        }
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
        let stable_session = matches!(
            &operation,
            Operation::Storages
                | Operation::List { .. }
                | Operation::UploadFile { .. }
                | Operation::DownloadFile { .. }
        );
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
                        if error.requires_review {
                            return Err(ApplicationError::UnknownOutcome {
                                journal_path: error.journal_path,
                            });
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
            if let Some(size) = entry.byte_size {
                self.entry_sizes
                    .insert((directory.storage().0, entry.object_id), size);
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
    /// Override read and transfer deadlines for controlled tests or explicit callers.
    pub fn set_deadline(&mut self, deadline: Duration) {
        self.deadline = deadline;
        self.transfer_deadline_override = Some(deadline);
    }

    fn transfer_call(
        &mut self,
        operation: Operation,
        bytes: Option<u64>,
        direction: TransferDirection,
    ) -> Result<TransferReceipt, ApplicationError> {
        let read_deadline = self.deadline;
        self.deadline = self.transfer_deadline_override.unwrap_or_else(|| {
            Duration::from_secs(bytes.map_or(7200, |length| {
                (length / (1024 * 1024)).saturating_add(120).min(7200)
            }))
        });
        let result = self.call(self.wire_generation, operation);
        self.deadline = read_deadline;
        let value = match result {
            Ok((_, value)) => value,
            Err(error @ ApplicationError::Backend { .. }) => return Err(error),
            Err(error @ ApplicationError::UnknownOutcome { .. }) => return Err(error),
            Err(_) => {
                self.invalidate();
                return Err(ApplicationError::UnknownOutcome { journal_path: None });
            }
        };
        let Value::Transferred {
            direction: received,
            actual_name,
            bytes,
            object_id,
            local_path,
            journal_path,
        } = value
        else {
            self.invalidate();
            return Err(ApplicationError::UnknownOutcome { journal_path: None });
        };
        if received != direction
            || mtp_domain::RemoteFilename::new(&actual_name).is_err()
            || actual_name.chars().any(char::is_control)
            || object_id.is_none_or(|id| id == 0 || id == u32::MAX)
            || (direction == TransferDirection::Download) != local_path.is_some()
        {
            self.invalidate();
            return Err(ApplicationError::UnknownOutcome {
                journal_path: Some(journal_path),
            });
        }
        Ok(TransferReceipt {
            direction,
            actual_name,
            bytes,
            object_id,
            local_path,
            journal_path,
        })
    }

    /// Remote identity comes from the session reference, never a display label.
    pub fn upload_file(
        &mut self,
        source: &Path,
        requested_name: &str,
        dest: &RemoteDirectoryRef,
        cache: &Path,
    ) -> Result<TransferReceipt, ApplicationError> {
        let info = self
            .session
            .as_ref()
            .ok_or(ApplicationError::NotConnected)?;
        dest.validate_session(&info.device_key, info.generation)?;
        mtp_domain::RemoteFilename::new(requested_name)?;
        if requested_name.chars().any(char::is_control) || requested_name.len() > 255 {
            return Err(DomainError::InvalidRemoteFilename.into());
        }
        let source_path = LocalPath::from_path(source)?;
        let cache_directory = LocalPath::from_path(cache)?;
        if !info.capabilities.upload {
            return Err(ApplicationError::Backend {
                code: ErrorCode::Unsupported,
                message: "backend has no verified upload capability".into(),
                requires_review: false,
            });
        }
        // Estimate only; worker independently validates NOFOLLOW snapshot metadata.
        let size = std::fs::symlink_metadata(source)?.len();
        self.transfer_call(
            Operation::UploadFile {
                storage_id: dest.storage().0,
                parent_id: dest.parent().map(|id| id.0),
                source: source_path,
                cache_directory,
                requested_name: requested_name.into(),
            },
            Some(size),
            TransferDirection::Upload,
        )
    }

    /// Destination is an existing directory, not a reconstructed target path.
    pub fn download_file(
        &mut self,
        source: &RemoteRef,
        dest: &Path,
        policy: FileConflictPolicy,
    ) -> Result<TransferReceipt, ApplicationError> {
        let info = self
            .session
            .as_ref()
            .ok_or(ApplicationError::NotConnected)?;
        source.validate_session(&info.device_key, info.generation)?;
        let destination = LocalPath::from_path(dest)?;
        if !info.capabilities.download {
            return Err(ApplicationError::Backend {
                code: ErrorCode::Unsupported,
                message: "backend has no verified download capability".into(),
                requires_review: false,
            });
        }
        let size = self
            .entry_sizes
            .get(&(source.storage().0, source.object().0))
            .copied();
        self.transfer_call(
            Operation::DownloadFile {
                storage_id: source.storage().0,
                object_id: source.object().0,
                destination,
                policy,
            },
            size,
            TransferDirection::Download,
        )
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
            let status = self
                .child
                .lock()
                .map_err(|_| io::Error::other("worker child lock poisoned"))?
                .try_wait()?;
            if let Some(status) = status {
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
        self.entry_sizes.clear();
        self.wire_generation = None;
        self.input.take();
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
impl Drop for WorkerClient {
    fn drop(&mut self) {
        self.invalidate();
    }
}
