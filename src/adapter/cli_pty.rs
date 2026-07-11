use std::{
    fmt,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use portable_pty::{Child, CommandBuilder, ExitStatus, MasterPty, PtySize, native_pty_system};

const DEFAULT_PTY_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CliPtySize {
    pub rows: u16,
    pub cols: u16,
}

impl Default for CliPtySize {
    fn default() -> Self {
        Self { rows: 24, cols: 80 }
    }
}

impl From<CliPtySize> for PtySize {
    fn from(size: CliPtySize) -> Self {
        Self {
            rows: size.rows,
            cols: size.cols,
            pixel_width: 0,
            pixel_height: 0,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CliPtyRun {
    pub raw_output: String,
    pub exit_code: u32,
    pub success: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CliPtySessionStatus {
    pub exit_code: u32,
    pub success: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CliPtyPromptMode {
    Argument,
    InitialInput,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CliPtyError {
    OpenPty(String),
    Spawn(String),
    Read(String),
    Write(String),
    Resize(String),
    Wait(String),
    TimedOut { after_ms: u64, raw_output: String },
    ReaderPanicked,
}

impl fmt::Display for CliPtyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OpenPty(message) => write!(f, "PTY could not be opened: {message}"),
            Self::Spawn(message) => write!(f, "PTY command could not start: {message}"),
            Self::Read(message) => write!(f, "PTY output could not be read: {message}"),
            Self::Write(message) => write!(f, "PTY input could not be written: {message}"),
            Self::Resize(message) => write!(f, "PTY could not be resized: {message}"),
            Self::Wait(message) => write!(f, "PTY command wait failed: {message}"),
            Self::TimedOut { after_ms, .. } => {
                write!(f, "PTY command timed out after {after_ms}ms")
            }
            Self::ReaderPanicked => f.write_str("PTY reader thread panicked"),
        }
    }
}

impl std::error::Error for CliPtyError {}

#[derive(Debug, Default)]
struct SessionOutput {
    bytes: Vec<u8>,
    read_error: Option<String>,
}

pub struct CliPtySession {
    master: Option<Box<dyn MasterPty + Send>>,
    child: Box<dyn Child + Send + Sync>,
    writer: Option<Box<dyn Write + Send>>,
    output: Arc<Mutex<SessionOutput>>,
    reader_thread: Option<thread::JoinHandle<()>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CliPtyAdapter {
    program: String,
    args: Vec<String>,
    cwd: PathBuf,
    size: CliPtySize,
    initial_input: Option<String>,
    prompt_mode: CliPtyPromptMode,
    timeout: Duration,
}

pub trait CliPtyRunner: Send + Sync {
    fn run_prompt(&self, prompt: &str) -> Result<CliPtyRun, CliPtyError>;
}

impl CliPtyAdapter {
    pub fn new(program: impl Into<String>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: cwd.into(),
            size: CliPtySize::default(),
            initial_input: None,
            prompt_mode: CliPtyPromptMode::Argument,
            timeout: DEFAULT_PTY_TIMEOUT,
        }
    }

    pub fn codex_interactive(cwd: impl Into<PathBuf>) -> Self {
        Self::new("codex", cwd)
    }

    pub fn claude_interactive(cwd: impl Into<PathBuf>) -> Self {
        Self::new("claude", cwd)
    }

    pub fn with_args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_size(mut self, size: CliPtySize) -> Self {
        self.size = size;
        self
    }

    pub fn with_initial_input(mut self, input: impl Into<String>) -> Self {
        self.initial_input = Some(input.into());
        self
    }

    pub fn with_prompt_mode(mut self, mode: CliPtyPromptMode) -> Self {
        self.prompt_mode = mode;
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn program(&self) -> &str {
        &self.program
    }

    pub fn args(&self) -> &[String] {
        &self.args
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn command_line(&self) -> String {
        std::iter::once(self.program.as_str())
            .chain(self.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub fn run_prompt_to_exit(&self, prompt: &str) -> Result<CliPtyRun, CliPtyError> {
        let mut command = self.clone();
        match self.prompt_mode {
            CliPtyPromptMode::Argument => command.args.push(prompt.to_string()),
            CliPtyPromptMode::InitialInput => {
                command.initial_input = Some(format!("{prompt}\n"));
            }
        }

        command.run_to_exit()
    }

    pub fn run_to_exit(&self) -> Result<CliPtyRun, CliPtyError> {
        self.spawn_session()?.wait_for_exit(self.timeout)
    }

    pub fn spawn_session(&self) -> Result<CliPtySession, CliPtyError> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(self.size.into())
            .map_err(|err| CliPtyError::OpenPty(err.to_string()))?;

        let mut command = CommandBuilder::new(&self.program);
        for arg in &self.args {
            command.arg(arg);
        }
        command.cwd(&self.cwd);

        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|err| CliPtyError::Spawn(err.to_string()))?;
        drop(pair.slave);

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|err| CliPtyError::Read(err.to_string()))?;
        let output = Arc::new(Mutex::new(SessionOutput::default()));
        let reader_output = Arc::clone(&output);
        let reader_thread = thread::spawn(move || read_pty_into_buffer(reader, reader_output));

        let mut session = CliPtySession {
            master: Some(pair.master),
            child,
            writer: None,
            output,
            reader_thread: Some(reader_thread),
        };

        let writer = session
            .master
            .as_ref()
            .expect("master should exist while session starts")
            .take_writer()
            .map_err(|err| CliPtyError::Write(err.to_string()))?;
        session.writer = Some(writer);

        if let Some(input) = &self.initial_input {
            session.send_input(input)?;
        }

        Ok(session)
    }
}

impl CliPtySession {
    pub fn send_input(&mut self, input: &str) -> Result<(), CliPtyError> {
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| CliPtyError::Write("PTY input stream is closed".to_string()))?;
        writer
            .write_all(input.as_bytes())
            .and_then(|_| writer.flush())
            .map_err(|err| CliPtyError::Write(err.to_string()))
    }

    pub fn send_line(&mut self, line: &str) -> Result<(), CliPtyError> {
        self.send_input(&format!("{line}\n"))
    }

    pub fn resize(&self, size: CliPtySize) -> Result<(), CliPtyError> {
        let Some(master) = &self.master else {
            return Err(CliPtyError::Resize("PTY master is closed".to_string()));
        };
        master
            .resize(size.into())
            .map_err(|err| CliPtyError::Resize(err.to_string()))
    }

    pub fn drain_output(&self) -> Result<String, CliPtyError> {
        let mut output = self
            .output
            .lock()
            .map_err(|_| CliPtyError::Read("PTY output buffer lock poisoned".to_string()))?;
        if let Some(error) = &output.read_error {
            return Err(CliPtyError::Read(error.clone()));
        }

        let drained = String::from_utf8_lossy(&output.bytes).to_string();
        output.bytes.clear();
        Ok(drained)
    }

    pub fn try_wait(&mut self) -> Result<Option<CliPtySessionStatus>, CliPtyError> {
        self.child
            .try_wait()
            .map(|status| status.map(session_status))
            .map_err(|err| CliPtyError::Wait(err.to_string()))
    }

    pub fn wait_for_exit(&mut self, timeout: Duration) -> Result<CliPtyRun, CliPtyError> {
        let status = match wait_with_timeout(self.child.as_mut(), timeout) {
            Ok(status) => status,
            Err(CliPtyError::TimedOut { after_ms, .. }) => {
                let _ = self.child.kill();
                let _ = self.child.wait();
                let raw_output = self.close_and_take_output()?;

                return Err(CliPtyError::TimedOut {
                    after_ms,
                    raw_output,
                });
            }
            Err(err) => return Err(err),
        };

        let raw_output = self.close_and_take_output()?;

        Ok(CliPtyRun {
            raw_output,
            exit_code: status.exit_code(),
            success: status.success(),
        })
    }

    pub fn stop(&mut self) -> Result<CliPtyRun, CliPtyError> {
        let _ = self.child.kill();
        let status = self
            .child
            .wait()
            .map_err(|err| CliPtyError::Wait(err.to_string()))?;
        let raw_output = self.close_and_take_output()?;

        Ok(CliPtyRun {
            raw_output,
            exit_code: status.exit_code(),
            success: status.success(),
        })
    }

    fn close_and_take_output(&mut self) -> Result<String, CliPtyError> {
        self.writer.take();
        self.master.take();
        if let Some(reader_thread) = self.reader_thread.take() {
            reader_thread
                .join()
                .map_err(|_| CliPtyError::ReaderPanicked)?;
        }
        self.drain_output()
    }
}

impl Drop for CliPtySession {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.writer.take();
        self.master.take();
        if let Some(reader_thread) = self.reader_thread.take() {
            let _ = reader_thread.join();
        }
    }
}

impl CliPtyRunner for CliPtyAdapter {
    fn run_prompt(&self, prompt: &str) -> Result<CliPtyRun, CliPtyError> {
        self.run_prompt_to_exit(prompt)
    }
}

fn wait_with_timeout(
    child: &mut (dyn Child + Send + Sync),
    timeout: Duration,
) -> Result<ExitStatus, CliPtyError> {
    let started = Instant::now();

    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|err| CliPtyError::Wait(err.to_string()))?
        {
            return Ok(status);
        }

        if started.elapsed() >= timeout {
            return Err(CliPtyError::TimedOut {
                after_ms: timeout.as_millis().try_into().unwrap_or(u64::MAX),
                raw_output: String::new(),
            });
        }

        thread::sleep(Duration::from_millis(10));
    }
}

fn read_pty_into_buffer(mut reader: Box<dyn Read + Send>, output: Arc<Mutex<SessionOutput>>) {
    let mut buffer = [0_u8; 8192];

    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                if let Ok(mut output) = output.lock() {
                    output.bytes.extend_from_slice(&buffer[..n]);
                } else {
                    break;
                }
            }
            Err(err) if err.raw_os_error() == Some(5) => break,
            Err(err) => {
                if let Ok(mut output) = output.lock() {
                    output.read_error = Some(err.to_string());
                }
                break;
            }
        }
    }
}

fn session_status(status: ExitStatus) -> CliPtySessionStatus {
    CliPtySessionStatus {
        exit_code: status.exit_code(),
        success: status.success(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interactive_command_builders_target_real_cli_binaries() {
        let codex = CliPtyAdapter::codex_interactive("/tmp/project");
        let claude = CliPtyAdapter::claude_interactive("/tmp/project");

        assert_eq!(codex.program(), "codex");
        assert_eq!(codex.args(), Vec::<String>::new());
        assert_eq!(codex.cwd(), Path::new("/tmp/project"));
        assert_eq!(claude.program(), "claude");
    }

    #[cfg(unix)]
    #[test]
    fn pty_command_captures_output_and_exit_status() {
        let run = CliPtyAdapter::new("/bin/sh", "/tmp")
            .with_args(["-lc", "printf 'pty-ready\\n'; exit 7"])
            .run_to_exit()
            .expect("PTY command should run");

        assert!(run.raw_output.contains("pty-ready"));
        assert_eq!(run.exit_code, 7);
        assert!(!run.success);
    }

    #[cfg(unix)]
    #[test]
    fn pty_command_can_send_initial_input() {
        let run = CliPtyAdapter::new("/bin/sh", "/tmp")
            .with_args(["-lc", "read line; printf 'got:%s\\n' \"$line\""])
            .with_initial_input("hello from pty\n")
            .run_to_exit()
            .expect("PTY command should run");

        assert!(run.raw_output.contains("got:hello from pty"));
        assert_eq!(run.exit_code, 0);
        assert!(run.success);
    }

    #[cfg(unix)]
    #[test]
    fn pty_prompt_can_be_passed_as_argument() {
        let run = CliPtyAdapter::new("/bin/sh", "/tmp")
            .with_args(["-lc", "printf 'arg:%s\\n' \"$1\"", "sh"])
            .run_prompt("hello from prompt")
            .expect("PTY command should run");

        assert!(run.raw_output.contains("arg:hello from prompt"));
        assert!(run.success);
    }

    #[cfg(unix)]
    #[test]
    fn pty_prompt_can_be_passed_as_initial_input() {
        let run = CliPtyAdapter::new("/bin/sh", "/tmp")
            .with_args(["-lc", "read line; printf 'input:%s\\n' \"$line\""])
            .with_prompt_mode(CliPtyPromptMode::InitialInput)
            .run_prompt("hello from stdin")
            .expect("PTY command should run");

        assert!(run.raw_output.contains("input:hello from stdin"));
        assert!(run.success);
    }

    #[cfg(unix)]
    #[test]
    fn pty_command_times_out_and_returns_partial_output() {
        let mut session = CliPtyAdapter::new("/bin/sh", "/tmp")
            .with_args(["-lc", "printf 'before-sleep\\n'; sleep 2"])
            .spawn_session()
            .expect("PTY session should start");

        wait_for_buffered_session_output(&session, "before-sleep");
        let error = session
            .wait_for_exit(Duration::from_millis(50))
            .expect_err("PTY command should time out");

        assert!(matches!(
            error,
            CliPtyError::TimedOut {
                raw_output,
                ..
            } if raw_output.contains("before-sleep")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn pty_session_can_inject_multiple_inputs() {
        let mut session = CliPtyAdapter::new("/bin/sh", "/tmp")
            .with_args([
                "-lc",
                "while IFS= read -r line; do if [ \"$line\" = quit ]; then printf 'bye\\n'; exit 0; fi; printf 'seen:%s\\n' \"$line\"; done",
            ])
            .spawn_session()
            .expect("PTY session should start");

        session.send_line("first").expect("first line should write");
        assert!(wait_for_session_output(&session, "seen:first").contains("seen:first"));

        session
            .send_line("second")
            .expect("second line should write");
        assert!(wait_for_session_output(&session, "seen:second").contains("seen:second"));
        assert_eq!(session.try_wait().expect("status should poll"), None);

        session
            .resize(CliPtySize {
                rows: 30,
                cols: 100,
            })
            .unwrap();
        session.send_line("quit").expect("quit line should write");
        let run = session
            .wait_for_exit(Duration::from_secs(1))
            .expect("session should exit after quit");

        assert!(run.success);
        assert!(run.raw_output.contains("bye"));
    }

    #[cfg(unix)]
    #[test]
    fn pty_session_stop_terminates_running_process() {
        let mut session = CliPtyAdapter::new("/bin/sh", "/tmp")
            .with_args(["-lc", "printf 'ready\\n'; while true; do sleep 1; done"])
            .spawn_session()
            .expect("PTY session should start");

        assert!(wait_for_session_output(&session, "ready").contains("ready"));
        let run = session.stop().expect("session should stop");

        assert!(!run.success);
    }

    #[cfg(unix)]
    fn wait_for_session_output(session: &CliPtySession, needle: &str) -> String {
        let mut output = String::new();
        let started = Instant::now();

        loop {
            output.push_str(&session.drain_output().expect("session output should drain"));
            if output.contains(needle) {
                return output;
            }
            if started.elapsed() > Duration::from_secs(1) {
                panic!("session output did not contain {needle:?}; output was {output:?}");
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(unix)]
    fn wait_for_buffered_session_output(session: &CliPtySession, needle: &str) {
        let started = Instant::now();

        loop {
            let contains_needle = {
                let output = session.output.lock().expect("session output should lock");
                String::from_utf8_lossy(&output.bytes).contains(needle)
            };
            if contains_needle {
                return;
            }
            if started.elapsed() > Duration::from_secs(1) {
                panic!("session output did not contain {needle:?} before timeout testing");
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}
