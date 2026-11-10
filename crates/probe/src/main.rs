use mtp_backend_api::*;
use std::{
    error::Error,
    io,
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command as ProcessCommand, Stdio},
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const DEADLINE: Duration = Duration::from_secs(3);

struct Worker {
    child: Child,
    stdin: Option<ChildStdin>,
    messages: Receiver<std::result::Result<Option<ServerMessage>, ProtocolError>>,
    next_id: u64,
}
impl Worker {
    fn start(path: &Path) -> Result<Self> {
        let mut child = ProcessCommand::new(path)
            .args(["--backend", "fake"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("worker stdin unavailable"))?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("worker stdout unavailable"))?;
        let (tx, messages) = mpsc::sync_channel(8);
        std::thread::spawn(move || {
            loop {
                let frame = read_frame::<ServerMessage>(&mut stdout);
                let terminal = !matches!(&frame, Ok(Some(_)));
                if tx.send(frame).is_err() || terminal {
                    break;
                }
            }
        });
        let worker = Self {
            child,
            stdin: Some(stdin),
            messages,
            next_id: 1,
        };
        match worker.receive()? {
            ServerMessage::Hello { hello }
                if hello.protocol_version == PROTOCOL_VERSION
                    && hello.backend == "fake"
                    && hello.capabilities.simulated
                    && !hello.capabilities.upload
                    && !hello.capabilities.download => {}
            _ => {
                return Err(
                    io::Error::other("unexpected worker hello or unsafe capabilities").into(),
                );
            }
        }
        Ok(worker)
    }
    fn receive(&self) -> Result<ServerMessage> {
        self.messages
            .recv_timeout(DEADLINE)??
            .ok_or_else(|| io::Error::other("worker closed before result").into())
    }
    fn call(&mut self, generation: Option<u64>, operation: Operation) -> Result<(u64, Outcome)> {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| io::Error::other("request ID exhausted"))?;
        let command = mtp_backend_api::Command {
            protocol_version: PROTOCOL_VERSION,
            request_id: id,
            generation,
            operation,
        };
        write_frame(
            self.stdin
                .as_mut()
                .ok_or_else(|| io::Error::other("worker stdin closed"))?,
            &command,
        )?;
        match self.receive()? {
            ServerMessage::Result {
                request_id,
                generation,
                outcome,
            } if request_id == id => Ok((generation, outcome)),
            _ => Err(io::Error::other("duplicate, stale or unexpected worker result").into()),
        }
    }
    fn shutdown(mut self) -> Result<()> {
        let (_, outcome) = self.call(None, Operation::Shutdown)?;
        require(
            matches!(
                outcome,
                Outcome::Success {
                    value: Value::ShuttingDown
                }
            ),
            "shutdown failed",
        )?;
        self.stdin.take();
        let deadline = Instant::now() + DEADLINE;
        loop {
            if let Some(status) = self.child.try_wait()? {
                require(status.success(), "worker exited unsuccessfully")?;
                break;
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "worker shutdown deadline exceeded",
                )
                .into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn require(condition: bool, message: &str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(io::Error::other(message).into())
    }
}

fn fake_probe(path: &Path) -> Result<()> {
    let mut worker = Worker::start(path)?;
    let (first, outcome) = worker.call(None, Operation::Connect)?;
    require(
        matches!(
            outcome,
            Outcome::Success {
                value: Value::Connected { .. }
            }
        ),
        "connect failed",
    )?;
    let (_, outcome) = worker.call(Some(first), Operation::Storages)?;
    require(
        matches!(outcome,Outcome::Success {value:Value::Storages { storages }} if storages.len()==1 && storages[0].id==1),
        "storage contract failed",
    )?;
    let (_, outcome) = worker.call(
        Some(first),
        Operation::List {
            storage_id: 1,
            parent_id: None,
        },
    )?;
    require(
        matches!(outcome,Outcome::Success {value:Value::Entries { entries }} if entries.iter().any(|e|e.name=="照片") && entries.iter().any(|e|e.byte_size.is_some_and(|n|n>u32::MAX as u64))),
        "Unicode/u64 fixture contract failed",
    )?;
    worker.call(Some(first), Operation::Disconnect)?;
    let (second, outcome) = worker.call(None, Operation::Connect)?;
    require(
        second != first && matches!(outcome, Outcome::Success { .. }),
        "reconnect did not replace generation",
    )?;
    let (_, outcome) = worker.call(Some(first), Operation::Storages)?;
    require(
        matches!(
            outcome,
            Outcome::Failure {
                error: BackendError {
                    code: ErrorCode::StaleSession,
                    ..
                }
            }
        ),
        "old session accepted",
    )?;
    let (_, outcome) = worker.call(
        Some(second),
        Operation::InjectFailure {
            fault: Fault::UnknownOutcome,
        },
    )?;
    require(
        matches!(
            outcome,
            Outcome::Failure {
                error: BackendError {
                    code: ErrorCode::UnknownOutcome,
                    requires_review: true,
                    session_usable: false,
                    ..
                }
            }
        ),
        "unknown outcome was treated as success/retryable",
    )?;
    let (_, outcome) = worker.call(Some(second), Operation::Storages)?;
    require(
        matches!(
            outcome,
            Outcome::Failure {
                error: BackendError {
                    code: ErrorCode::NotConnected,
                    ..
                }
            }
        ),
        "uncertain session remained usable",
    )?;
    worker.shutdown()?;
    println!(
        "PASS: fake worker handshake, storage, Unicode/u64 metadata, reconnect, stale-session rejection, review-required failure and bounded shutdown"
    );
    println!("No USB device was accessed and no file was transferred.");
    Ok(())
}

fn run() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    match args.next().as_deref() {
        Some(mode) if mode == "fake" => {
            let path = match args.next() {
                Some(path) => PathBuf::from(path),
                None => std::env::current_exe()?
                    .parent()
                    .ok_or_else(|| io::Error::other("probe executable has no parent"))?
                    .join("mtp-worker"),
            };
            require(args.next().is_none(), "too many arguments")?;
            fake_probe(&path)
        }
        Some(mode) if mode == "local" => {
            let path = args
                .next()
                .map(PathBuf::from)
                .ok_or_else(|| io::Error::other("local requires a directory path"))?;
            require(args.next().is_none(), "too many arguments")?;
            for entry in mtp_local_fs::read_directory(&path, false)? {
                println!(
                    "{:?}\t{}\t{}",
                    entry.kind,
                    entry.byte_size.map_or("—".into(), |n| n.to_string()),
                    entry.name.to_string_lossy()
                );
                if let Some(error) = entry.metadata_error {
                    eprintln!("metadata unavailable: {error}");
                }
            }
            Ok(())
        }
        _ => {
            Err(io::Error::other("Usage: mtp-probe fake [worker-path] | local <directory>").into())
        }
    }
}
fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("probe failed: {error}");
            std::process::ExitCode::from(1)
        }
    }
}
