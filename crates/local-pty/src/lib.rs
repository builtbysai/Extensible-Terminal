#![forbid(unsafe_code)]

mod spec;
mod ssh;
mod tmux;

use std::io::{Read, Write};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use terminal_core::{
    Endpoint, EndpointCapabilities, EndpointError, EndpointEvent, EndpointId, EndpointKind,
    EndpointLocality, EndpointSize,
};

pub use spec::{LocalProcessSpec, LocalProcessSpecError};
pub use ssh::{SshPtyEndpoint, SshPtySpec, SshPtySpecError};
pub use tmux::{TmuxPtyEndpoint, TmuxPtySpec, TmuxPtySpecError};

const READER_CHUNK_BYTES: usize = 16 * 1024;
const READER_EVENT_CAPACITY: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalPtyState {
    Created,
    Running,
    Exited,
}

enum ReaderEvent {
    Output(Vec<u8>),
    Closed,
    Failed(String),
}

struct Runtime {
    master: Option<Box<dyn MasterPty + Send>>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    reader_rx: Receiver<ReaderEvent>,
    reader_closed: bool,
    pending_exit: Option<(Option<i32>, Option<String>)>,
    exit_reported: bool,
}

/// A local child process attached to the platform's native pseudo terminal.
///
/// This provider owns process lifecycle and raw byte transport only. It does
/// not parse shell prompts, commands, OSC metadata, or terminal escape
/// sequences. Those belong to higher layers.
pub struct LocalPtyEndpoint {
    id: EndpointId,
    spec: LocalProcessSpec,
    locality: EndpointLocality,
    state: LocalPtyState,
    runtime: Option<Runtime>,
}

impl LocalPtyEndpoint {
    #[must_use]
    pub fn new(id: EndpointId, spec: LocalProcessSpec) -> Self {
        Self {
            id,
            spec,
            locality: EndpointLocality::Local,
            state: LocalPtyState::Created,
            runtime: None,
        }
    }

    #[must_use]
    pub fn state(&self) -> LocalPtyState {
        self.state
    }

    #[must_use]
    pub fn process_id(&self) -> Option<u32> {
        self.runtime.as_ref().and_then(|runtime| runtime.child.process_id())
    }

    fn runtime_mut(&mut self, operation: &'static str) -> Result<&mut Runtime, EndpointError> {
        self.runtime.as_mut().ok_or_else(|| {
            EndpointError::Transport(format!("cannot {operation}: local PTY is not running"))
        })
    }

    fn spawn_reader(
        mut reader: Box<dyn Read + Send>,
    ) -> Result<Receiver<ReaderEvent>, EndpointError> {
        // A synchronous channel puts a hard bound on heap growth if the app
        // event loop stops draining PTY output. Once full, the reader thread
        // blocks and the OS PTY pipe provides the next level of backpressure.
        let (tx, rx) = mpsc::sync_channel(READER_EVENT_CAPACITY);
        thread::Builder::new()
            .name("terminal-local-pty-reader".to_owned())
            .spawn(move || {
                let mut buffer = vec![0_u8; READER_CHUNK_BYTES];
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) => {
                            let _ = tx.send(ReaderEvent::Closed);
                            break;
                        }
                        Ok(count) => {
                            if tx.send(ReaderEvent::Output(buffer[..count].to_vec())).is_err() {
                                break;
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                        // Windows anonymous pipes signal normal writer closure as
                        // ERROR_BROKEN_PIPE, which Rust exposes as BrokenPipe.
                        // ConPTY therefore reaches EOF through this error path
                        // after the child closes its output handles.
                        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => {
                            let _ = tx.send(ReaderEvent::Closed);
                            break;
                        }
                        Err(error) => {
                            let _ = tx.send(ReaderEvent::Failed(error.to_string()));
                            break;
                        }
                    }
                }
            })
            .map_err(|error| EndpointError::Transport(error.to_string()))?;
        Ok(rx)
    }

    fn pty_size(size: EndpointSize) -> PtySize {
        PtySize {
            rows: size.rows,
            cols: size.cols,
            pixel_width: 0,
            pixel_height: 0,
        }
    }

    fn kill_after_setup_failure(
        child: &mut Box<dyn Child + Send + Sync>,
        error: impl std::fmt::Display,
    ) -> EndpointError {
        let _ = child.kill();
        let _ = child.wait();
        EndpointError::Transport(error.to_string())
    }

    fn exit_parts(status: &portable_pty::ExitStatus) -> (Option<i32>, Option<String>) {
        let code = i32::try_from(status.exit_code()).ok();
        let signal = status.signal().map(ToOwned::to_owned);
        (code, signal)
    }
}

impl Endpoint for LocalPtyEndpoint {
    fn id(&self) -> &EndpointId {
        &self.id
    }

    fn kind(&self) -> EndpointKind {
        EndpointKind::LocalPty
    }

    fn locality(&self) -> &EndpointLocality {
        &self.locality
    }

    fn capabilities(&self) -> EndpointCapabilities {
        EndpointCapabilities {
            detachable: false,
            persistent: false,
            remote: false,
            authoritative_scrollback: false,
            resize: true,
        }
    }

    fn attach(&mut self) -> Result<(), EndpointError> {
        if self.state != LocalPtyState::Created {
            return Err(EndpointError::Transport(
                "local PTY endpoints can only be attached once".to_owned(),
            ));
        }

        let pair = native_pty_system()
            .openpty(Self::pty_size(self.spec.initial_size()))
            .map_err(|error| EndpointError::Transport(error.to_string()))?;

        let mut command = CommandBuilder::new(self.spec.program());
        command.args(self.spec.args());
        if let Some(cwd) = self.spec.cwd() {
            command.cwd(cwd);
        }
        for (name, value) in self.spec.env() {
            command.env(name, value);
        }

        // Spawn first, then acquire master-side I/O. This ordering matches the
        // portable-pty contract and is required for ConPTY to wire its pipes.
        let mut child = pair
            .slave
            .spawn_command(command)
            .map_err(|error| EndpointError::Transport(error.to_string()))?;
        drop(pair.slave);

        let reader = match pair.master.try_clone_reader() {
            Ok(reader) => reader,
            Err(error) => return Err(Self::kill_after_setup_failure(&mut child, error)),
        };
        let writer = match pair.master.take_writer() {
            Ok(writer) => writer,
            Err(error) => return Err(Self::kill_after_setup_failure(&mut child, error)),
        };
        let reader_rx = match Self::spawn_reader(reader) {
            Ok(receiver) => receiver,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };

        self.runtime = Some(Runtime {
            master: Some(pair.master),
            writer,
            child,
            reader_rx,
            reader_closed: false,
            pending_exit: None,
            exit_reported: false,
        });
        self.state = LocalPtyState::Running;
        Ok(())
    }

    fn detach(&mut self) -> Result<(), EndpointError> {
        Err(EndpointError::Unsupported("detach for local PTY endpoints"))
    }

    fn resize(&mut self, size: EndpointSize) -> Result<(), EndpointError> {
        let runtime = self.runtime_mut("resize")?;
        let master = runtime.master.as_ref().ok_or_else(|| {
            EndpointError::Transport("cannot resize: local PTY transport is closed".to_owned())
        })?;
        master
            .resize(Self::pty_size(size))
            .map_err(|error| EndpointError::Transport(error.to_string()))
    }

    fn send(&mut self, input: &[u8]) -> Result<(), EndpointError> {
        let runtime = self.runtime_mut("send input")?;
        runtime
            .writer
            .write_all(input)
            .and_then(|()| runtime.writer.flush())
            .map_err(|error| EndpointError::Transport(error.to_string()))
    }

    fn poll_event(&mut self) -> Result<Option<EndpointEvent>, EndpointError> {
        let Some(runtime) = self.runtime.as_mut() else {
            return Ok(None);
        };

        match runtime.reader_rx.try_recv() {
            Ok(ReaderEvent::Output(bytes)) => return Ok(Some(EndpointEvent::Output(bytes))),
            Ok(ReaderEvent::Failed(message)) => {
                return Ok(Some(EndpointEvent::Disconnected { reason: message }));
            }
            Ok(ReaderEvent::Closed) | Err(TryRecvError::Disconnected) => {
                runtime.reader_closed = true;
            }
            Err(TryRecvError::Empty) => {}
        }

        #[cfg(windows)]
        if !runtime.reader_closed && runtime.pending_exit.is_none() {
            // ConPTY owns the output-pipe write end through the pseudoconsole
            // handle. Waiting for pipe EOF before releasing that handle creates
            // an ownership loop on Windows: the host waits for EOF while the
            // pseudoconsole stays alive because the host still owns HPCON.
            // Once the tracked shell exits, release only the master/HPCON here.
            // The dedicated reader thread keeps its cloned output handle and
            // continues draining every trailing byte until ConPTY closes it.
            if let Some(status) = runtime
                .child
                .try_wait()
                .map_err(|error| EndpointError::Transport(error.to_string()))?
            {
                runtime.pending_exit = Some(Self::exit_parts(&status));
                runtime.master.take();
            }
        }

        // Process termination and PTY EOF are observed independently. A fast
        // child can exit before the reader thread has copied its final bytes
        // into the bounded channel, so reporting Exited before reader EOF can
        // make the frontend drop valid trailing output. Preserve transport
        // ordering by waiting for the PTY reader to close before surfacing exit.
        if runtime.reader_closed && !runtime.exit_reported {
            let exit = if let Some(exit) = runtime.pending_exit.take() {
                Some(exit)
            } else {
                runtime
                    .child
                    .try_wait()
                    .map_err(|error| EndpointError::Transport(error.to_string()))?
                    .map(|status| Self::exit_parts(&status))
            };

            if let Some((code, signal)) = exit {
                runtime.exit_reported = true;
                self.state = LocalPtyState::Exited;
                return Ok(Some(EndpointEvent::Exited { code, signal }));
            }
        }

        Ok(None)
    }
}

impl Drop for LocalPtyEndpoint {
    fn drop(&mut self) {
        if self.state == LocalPtyState::Running {
            if let Some(runtime) = self.runtime.as_mut() {
                let _ = runtime.child.kill();
                let _ = runtime.child.wait();
            }
        }
    }
}
