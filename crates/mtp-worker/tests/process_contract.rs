#![allow(clippy::unwrap_used, clippy::expect_used)]
use mtp_backend_api::*;
use std::{
    io,
    process::{Child, ChildStdin, Command as ProcessCommand, Stdio},
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};
const DEADLINE: Duration = Duration::from_secs(3);
struct Worker {
    child: Child,
    input: ChildStdin,
    messages: Receiver<Result<Option<ServerMessage>, ProtocolError>>,
}
impl Worker {
    fn start() -> Self {
        let mut child = ProcessCommand::new(env!("CARGO_BIN_EXE_mtp-worker"))
            .args(["--backend", "fake"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let mut output = child.stdout.take().unwrap();
        let (tx, messages) = mpsc::channel();
        std::thread::spawn(move || {
            loop {
                let frame = read_frame(&mut output);
                let end = !matches!(&frame, Ok(Some(_)));
                if tx.send(frame).is_err() || end {
                    break;
                }
            }
        });
        let worker = Self {
            child,
            input,
            messages,
        };
        assert!(matches!(
            worker.receive(),
            ServerMessage::Hello {
                hello: Hello {
                    protocol_version: 1,
                    capabilities: Capabilities {
                        simulated: true,
                        upload: false,
                        download: false,
                        ..
                    },
                    ..
                }
            }
        ));
        worker
    }
    fn receive(&self) -> ServerMessage {
        self.messages
            .recv_timeout(DEADLINE)
            .unwrap()
            .unwrap()
            .unwrap()
    }
    fn send(&mut self, id: u64, generation: Option<u64>, operation: Operation) {
        write_frame(
            &mut self.input,
            &Command {
                protocol_version: 1,
                request_id: id,
                generation,
                operation,
            },
        )
        .unwrap();
    }
    fn result(&self, id: u64) -> (u64, Outcome) {
        match self.receive() {
            ServerMessage::Result {
                request_id,
                generation,
                outcome,
            } => {
                assert_eq!(request_id, id);
                (generation, outcome)
            }
            _ => panic!("unexpected hello"),
        }
    }
    fn wait(&mut self) -> io::Result<std::process::ExitStatus> {
        let deadline = Instant::now() + DEADLINE;
        loop {
            if let Some(status) = self.child.try_wait()? {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "worker did not terminate",
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn process_connect_browse_and_shutdown() {
    let mut worker = Worker::start();
    worker.send(1, None, Operation::Connect);
    let (generation, outcome) = worker.result(1);
    assert!(matches!(
        outcome,
        Outcome::Success {
            value: Value::Connected { .. }
        }
    ));
    worker.send(
        2,
        Some(generation),
        Operation::List {
            storage_id: 1,
            parent_id: None,
        },
    );
    let (_, outcome) = worker.result(2);
    assert!(
        matches!(outcome,Outcome::Success {value:Value::Entries {entries}} if entries.len()==2 && entries[1].byte_size==Some(5*1024*1024*1024))
    );
    worker.send(3, None, Operation::Shutdown);
    assert!(matches!(
        worker.result(3).1,
        Outcome::Success {
            value: Value::ShuttingDown
        }
    ));
    assert!(worker.wait().unwrap().success());
    assert!(
        worker
            .messages
            .recv_timeout(DEADLINE)
            .unwrap()
            .unwrap()
            .is_none()
    );
}
#[test]
fn stale_request_after_reconnect_is_not_executed() {
    let mut worker = Worker::start();
    worker.send(1, None, Operation::Connect);
    let (old, _) = worker.result(1);
    worker.send(2, Some(old), Operation::Disconnect);
    worker.result(2);
    worker.send(3, None, Operation::Connect);
    let (new, _) = worker.result(3);
    assert_ne!(old, new);
    worker.send(4, Some(old), Operation::Storages);
    assert!(matches!(
        worker.result(4).1,
        Outcome::Failure {
            error: BackendError {
                code: ErrorCode::StaleSession,
                ..
            }
        }
    ));
}
#[test]
fn duplicate_request_closes_transport_without_second_terminal() {
    let mut worker = Worker::start();
    worker.send(1, None, Operation::Connect);
    worker.result(1);
    worker.send(1, None, Operation::Connect);
    assert!(
        worker
            .messages
            .recv_timeout(DEADLINE)
            .unwrap()
            .unwrap()
            .is_none()
    );
    assert!(!worker.wait().unwrap().success());
}
#[test]
fn wrong_protocol_is_rejected_before_execution() {
    let mut worker = Worker::start();
    write_frame(
        &mut worker.input,
        &Command {
            protocol_version: 2,
            request_id: 1,
            generation: None,
            operation: Operation::Connect,
        },
    )
    .unwrap();
    assert!(
        worker
            .messages
            .recv_timeout(DEADLINE)
            .unwrap()
            .unwrap()
            .is_none()
    );
    assert!(!worker.wait().unwrap().success());
}
#[test]
fn oversized_input_terminates_worker_without_allocating_payload() {
    use std::io::Write;
    let mut worker = Worker::start();
    worker
        .input
        .write_all(&((MAX_FRAME_BYTES + 1) as u32).to_be_bytes())
        .unwrap();
    worker.input.flush().unwrap();
    assert!(
        worker
            .messages
            .recv_timeout(DEADLINE)
            .unwrap()
            .unwrap()
            .is_none()
    );
    assert!(!worker.wait().unwrap().success());
}
#[test]
fn uncertain_outcome_is_not_a_success_and_invalidates_session() {
    let mut worker = Worker::start();
    worker.send(1, None, Operation::Connect);
    let (generation, _) = worker.result(1);
    worker.send(
        2,
        Some(generation),
        Operation::InjectFailure {
            fault: Fault::UnknownOutcome,
        },
    );
    assert!(matches!(
        worker.result(2).1,
        Outcome::Failure {
            error: BackendError {
                code: ErrorCode::UnknownOutcome,
                requires_review: true,
                session_usable: false,
                ..
            }
        }
    ));
    worker.send(3, Some(generation), Operation::Storages);
    assert!(matches!(
        worker.result(3).1,
        Outcome::Failure {
            error: BackendError {
                code: ErrorCode::NotConnected,
                ..
            }
        }
    ));
}
#[test]
fn worker_kill_is_contained_in_child_process() {
    let mut worker = Worker::start();
    worker.child.kill().unwrap();
    assert!(!worker.wait().unwrap().success());
    assert!(
        worker
            .messages
            .recv_timeout(DEADLINE)
            .unwrap()
            .unwrap()
            .is_none()
    );
    // The test parent is still alive and can spawn a fresh independent owner.
    let mut replacement = Worker::start();
    replacement.send(1, None, Operation::Connect);
    assert!(matches!(replacement.result(1).1, Outcome::Success { .. }));
}
#[test]
fn missing_backend_flag_is_not_silently_fake_or_real() {
    let status = ProcessCommand::new(env!("CARGO_BIN_EXE_mtp-worker"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(2));
}
