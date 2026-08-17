//! Generic child-process streaming used by the real CLI adapters.
//!
//! A [`StreamAdapter`] knows how to build the command for a backend and how to
//! parse its stdout lines into [`AgentEvent`]s. [`run_streaming`] does the
//! backend-agnostic work: spawn the child, stream stdout (each raw line is also
//! emitted as [`AgentEvent::Raw`] for persistence), forward stderr, support
//! cancellation, and report the exit status.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::adapter::{MemberRunner, RunRequest};
use crate::domain::config::resolve_binary_on_path;
use crate::domain::event::{AgentEvent, AgentSessionId};
use crate::domain::team::{BackendKind, Effort};

pub(crate) const MAX_PROTOCOL_LINE_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const MAX_STDERR_LINE_BYTES: usize = 1024 * 1024;
const OUTPUT_DRAIN_GRACE: Duration = Duration::from_millis(500);
const CANCEL_GRACE: Duration = Duration::from_millis(400);

pub(crate) struct BoundedLines<R> {
    reader: R,
    max_bytes: usize,
    finished: bool,
}

impl<R: BufRead> Iterator for BoundedLines<R> {
    type Item = std::io::Result<String>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        match read_bounded_line(&mut self.reader, self.max_bytes) {
            Ok(Some(line)) => Some(Ok(line)),
            Ok(None) => {
                self.finished = true;
                None
            }
            Err(err) => {
                self.finished = true;
                Some(Err(err))
            }
        }
    }
}

pub(crate) fn bounded_lines<R: BufRead>(reader: R, max_bytes: usize) -> BoundedLines<R> {
    BoundedLines {
        reader,
        max_bytes,
        finished: false,
    }
}

fn read_bounded_line(
    reader: &mut impl BufRead,
    max_bytes: usize,
) -> std::io::Result<Option<String>> {
    let mut line = Vec::with_capacity(max_bytes.min(8192));
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            if line.is_empty() {
                return Ok(None);
            }
            break;
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let content_len = newline.unwrap_or(available.len());
        if line.len().saturating_add(content_len) > max_bytes {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("line exceeded {max_bytes} bytes"),
            ));
        }
        line.extend_from_slice(&available[..content_len]);
        let consumed = content_len + usize::from(newline.is_some());
        reader.consume(consumed);
        if newline.is_some() {
            break;
        }
    }
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    String::from_utf8(line)
        .map(Some)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))
}

/// Prepare a child for race-free process-tree ownership where the platform
/// supports it. Windows children start suspended and are resumed only after
/// assignment to a Job Object; Unix children start in their own process group.
pub(crate) fn configure_process_tree(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::CREATE_SUSPENDED;
        command.creation_flags(CREATE_SUSPENDED);
    }
    #[cfg(not(any(unix, windows)))]
    let _ = command;
}

/// Owns the platform object that contains a spawned process and its descendants.
pub(crate) struct ChildProcessTree {
    #[cfg(unix)]
    process_group: u32,
    #[cfg(windows)]
    job: WindowsJob,
}

impl ChildProcessTree {
    /// Attach a standard child prepared by [`configure_process_tree`].
    pub(crate) fn attach(child: &mut Child) -> std::io::Result<Self> {
        #[cfg(unix)]
        {
            Ok(Self {
                process_group: child.id(),
            })
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;

            let tree = Self {
                job: WindowsJob::assign(child.as_raw_handle())?,
            };
            resume_suspended_process(child.id())?;
            Ok(tree)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = child;
            Ok(Self {})
        }
    }

    /// Attach a child created by portable-pty. Its Unix implementation already
    /// calls `setsid`; on Windows the process is assigned immediately after
    /// ConPTY returns its process handle.
    pub(crate) fn attach_pty(child: &dyn portable_pty::Child) -> std::io::Result<Self> {
        #[cfg(unix)]
        {
            let process_group = child.process_id().ok_or_else(|| {
                std::io::Error::other("PTY child did not expose a process identifier")
            })?;
            Ok(Self { process_group })
        }
        #[cfg(windows)]
        {
            let process = child.as_raw_handle().ok_or_else(|| {
                std::io::Error::other("PTY child did not expose a process handle")
            })?;
            Ok(Self {
                job: WindowsJob::assign(process)?,
            })
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = child;
            Ok(Self {})
        }
    }

    /// Ask the tree to exit. Unix sends SIGTERM so the CLI can drop in-flight
    /// HTTP; Windows job termination is already immediate.
    pub(crate) fn request_stop(&self) -> std::io::Result<()> {
        #[cfg(unix)]
        return signal_process_group(self.process_group, libc::SIGTERM);
        #[cfg(windows)]
        return self.job.terminate();
        #[cfg(not(any(unix, windows)))]
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "process-tree termination is unavailable on this platform",
        ))
    }

    /// Terminate the whole owned tree without locking or borrowing the child.
    pub(crate) fn terminate(&self) -> std::io::Result<()> {
        #[cfg(unix)]
        return signal_process_group(self.process_group, libc::SIGKILL);
        #[cfg(windows)]
        return self.job.terminate();
        #[cfg(not(any(unix, windows)))]
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "process-tree termination is unavailable on this platform",
        ))
    }

    /// Terminate the tree, falling back to the direct child if the platform
    /// primitive is unavailable or has already gone away.
    pub(crate) fn terminate_with_fallback(&self, child: &mut Child) -> std::io::Result<()> {
        match self.terminate() {
            Ok(()) => Ok(()),
            Err(tree_error) => child.kill().map_err(|_| tree_error),
        }
    }
}

#[cfg(unix)]
fn signal_process_group(process_group: u32, signal: i32) -> std::io::Result<()> {
    // SAFETY: children are spawned with PGID equal to their PID, and `killpg`
    // does not retain the integer beyond this call.
    if unsafe { libc::killpg(process_group as i32, signal) } == 0 {
        Ok(())
    } else {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(err)
        }
    }
}

#[cfg(windows)]
struct WindowsJob(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
unsafe impl Send for WindowsJob {}
#[cfg(windows)]
unsafe impl Sync for WindowsJob {}

#[cfg(windows)]
impl WindowsJob {
    fn assign(process: std::os::windows::io::RawHandle) -> std::io::Result<Self> {
        use windows_sys::Win32::System::JobObjects::{AssignProcessToJobObject, CreateJobObjectW};

        // SAFETY: null security/name pointers create a private Job Object.
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let job = Self(job);
        // SAFETY: `process` is borrowed from a live Child/portable-pty child.
        if unsafe { AssignProcessToJobObject(job.0, process.cast()) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(job)
    }

    fn terminate(&self) -> std::io::Result<()> {
        use windows_sys::Win32::System::JobObjects::TerminateJobObject;

        // SAFETY: the Job handle remains owned by self for the call duration.
        if unsafe { TerminateJobObject(self.0, 1) } != 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
}

#[cfg(windows)]
impl Drop for WindowsJob {
    fn drop(&mut self) {
        // Deliberately do not use JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: a normal
        // CLI exit may intentionally leave a background process running, as it
        // does on Unix. Cancellation and PTY teardown call `terminate` first.
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

#[cfg(windows)]
fn resume_suspended_process(process_id: u32) -> std::io::Result<()> {
    use std::mem::size_of;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

    // The process was created suspended, so its sole thread is the primary one.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }
    let mut entry = THREADENTRY32 {
        dwSize: size_of::<THREADENTRY32>() as u32,
        ..THREADENTRY32::default()
    };
    let mut has_entry = unsafe { Thread32First(snapshot, &mut entry) } != 0;
    let result = loop {
        if !has_entry {
            break Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "could not find the suspended child primary thread",
            ));
        }
        if entry.th32OwnerProcessID == process_id {
            let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if thread.is_null() {
                break Err(std::io::Error::last_os_error());
            }
            let resumed = unsafe { ResumeThread(thread) };
            unsafe {
                CloseHandle(thread);
            }
            if resumed == u32::MAX {
                break Err(std::io::Error::last_os_error());
            }
            break Ok(());
        }
        has_entry = unsafe { Thread32Next(snapshot, &mut entry) } != 0;
    };
    unsafe {
        CloseHandle(snapshot);
    }
    result
}

/// A command line for a backend run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdapterCommand {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// Optional stdin payload; `None` means stdin is closed (the prompt is an arg).
    pub stdin: Option<String>,
}

/// Builds backend commands and stateful per-run line parsers.
pub trait StreamAdapter: Send + Sync {
    fn backend(&self) -> BackendKind;
    /// Validate backend capabilities before starting a member turn.
    fn preflight(&self) -> Result<(), String> {
        Ok(())
    }
    fn build_command(
        &self,
        prompt: &str,
        session: Option<&AgentSessionId>,
        effort: Option<Effort>,
    ) -> AdapterCommand;
    fn parser(&self) -> Box<dyn LineParser>;
    /// Wait before spawning the child. Return `false` if `cancel` fires.
    fn prepare_run(&self, cancel: &AtomicBool) -> bool {
        let _ = cancel;
        true
    }
    /// Record that a child has exited so the next spawn can be spaced.
    fn finish_run(&self) {}
    /// Backoff before retrying a failed attempt. `None` means do not retry.
    fn retry_delay(&self, _fatal: &str, _attempt: u32) -> Option<Duration> {
        None
    }
}

/// Parses one backend stdout line into zero or more events. One parser instance
/// is created per run and may hold streaming state.
pub trait LineParser: Send {
    fn parse_line(&mut self, line: &str) -> Vec<AgentEvent>;
    /// Flush any trailing state when stdout closes.
    fn finish(&mut self) -> Vec<AgentEvent> {
        Vec::new()
    }
    /// Emit any events that require the process to have fully exited.
    fn finish_after_exit(&mut self, _ok: bool) -> Vec<AgentEvent> {
        Vec::new()
    }
}

/// A [`MemberRunner`] that drives a real CLI through a [`StreamAdapter`].
pub struct ProcessRunner<A: StreamAdapter> {
    adapter: A,
}

impl<A: StreamAdapter> ProcessRunner<A> {
    pub fn new(adapter: A) -> Self {
        Self { adapter }
    }
}

impl<A: StreamAdapter> MemberRunner for ProcessRunner<A> {
    fn backend(&self) -> BackendKind {
        self.adapter.backend()
    }

    fn run(&self, req: RunRequest, events: SyncSender<AgentEvent>) {
        if let Err(message) = self.adapter.preflight() {
            let _ = events.send(AgentEvent::Fatal(message));
            let _ = events.send(AgentEvent::Exited {
                code: None,
                ok: false,
            });
            return;
        }
        let mut attempt = 0u32;
        loop {
            if req.cancel.load(Ordering::Relaxed) || !self.adapter.prepare_run(&req.cancel) {
                let _ = events.send(AgentEvent::Exited {
                    code: None,
                    ok: false,
                });
                return;
            }
            let command = self
                .adapter
                .build_command(&req.prompt, req.session.as_ref(), req.effort);
            let parser = self.adapter.parser();
            let (tx, rx) = std::sync::mpsc::sync_channel(1_024);
            let cancel = Arc::clone(&req.cancel);
            let worker = thread::spawn(move || run_streaming(command, parser, cancel, tx));
            let mut quota_hint = false;
            let mut retryable_fatal = None;
            let mut exit = None;
            while let Ok(event) = rx.recv() {
                match event {
                    AgentEvent::Stderr(line) => {
                        if self.adapter.retry_delay(&line, attempt).is_some() {
                            quota_hint = true;
                        }
                        let _ = events.send(AgentEvent::Stderr(line));
                    }
                    AgentEvent::Fatal(message) => {
                        if self.adapter.retry_delay(&message, attempt).is_some() {
                            quota_hint = true;
                            retryable_fatal = Some(message);
                        } else {
                            let _ = events.send(AgentEvent::Fatal(message));
                        }
                    }
                    AgentEvent::Exited { code, ok } => exit = Some((code, ok)),
                    other => {
                        let _ = events.send(other);
                    }
                }
            }
            let _ = worker.join();
            self.adapter.finish_run();
            let cancelled = req.cancel.load(Ordering::Relaxed);
            let retry_delay = match retryable_fatal.as_deref() {
                Some(message) => self.adapter.retry_delay(message, attempt),
                None if quota_hint && !cancelled => {
                    self.adapter.retry_delay("RESOURCE_EXHAUSTED", attempt)
                }
                None => None,
            };
            if let Some(delay) = retry_delay.filter(|_| !cancelled) {
                attempt = attempt.saturating_add(1);
                let _ = events.send(AgentEvent::Log(format!(
                    "agy quota exhausted; retrying in {}s (attempt {attempt})",
                    delay.as_secs().max(1)
                )));
                if sleep_cancelable(delay, &req.cancel) {
                    continue;
                }
            }
            if let Some(message) = retryable_fatal {
                let _ = events.send(AgentEvent::Fatal(message));
            }
            match exit {
                Some((code, ok)) => {
                    let _ = events.send(AgentEvent::Exited { code, ok });
                }
                None => {
                    let _ = events.send(AgentEvent::Exited {
                        code: None,
                        ok: false,
                    });
                }
            }
            return;
        }
    }
}

fn sleep_cancelable(duration: Duration, cancel: &AtomicBool) -> bool {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        thread::sleep(
            Duration::from_millis(50).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
    !cancel.load(Ordering::Relaxed)
}

/// Spawn `command`, stream its output through `parser`, and report completion.
pub fn run_streaming(
    command: AdapterCommand,
    mut parser: Box<dyn LineParser>,
    cancel: Arc<AtomicBool>,
    events: SyncSender<AgentEvent>,
) {
    let resolved_program =
        resolve_binary_on_path(&command.program).unwrap_or_else(|| PathBuf::from(&command.program));
    let mut builder = Command::new(resolved_program);
    builder
        .args(&command.args)
        .current_dir(&command.cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(if command.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    configure_process_tree(&mut builder);

    let mut child = match builder.spawn() {
        Ok(child) => child,
        Err(err) => {
            let _ = events.send(AgentEvent::Fatal(format!(
                "failed to start {}: {err}",
                command.program
            )));
            // Always end a run with Exited so the runtime can finalize it.
            let _ = events.send(AgentEvent::Exited {
                code: None,
                ok: false,
            });
            return;
        }
    };

    let process_tree = match ChildProcessTree::attach(&mut child) {
        Ok(process_tree) => Arc::new(process_tree),
        Err(err) => {
            let _ = child.kill();
            let _ = child.wait();
            let _ = events.send(AgentEvent::Fatal(format!(
                "failed to isolate {} process tree: {err}",
                command.program
            )));
            let _ = events.send(AgentEvent::Exited {
                code: None,
                ok: false,
            });
            return;
        }
    };

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let transport_ok = Arc::new(AtomicBool::new(true));

    let stdin_thread = if let Some(input) = command.stdin {
        let Some(mut stdin) = child.stdin.take() else {
            let _ = process_tree.terminate_with_fallback(&mut child);
            let _ = child.wait();
            let _ = events.send(AgentEvent::Fatal(format!(
                "{} did not expose stdin for the prompt",
                command.program
            )));
            let _ = events.send(AgentEvent::Exited {
                code: None,
                ok: false,
            });
            return;
        };
        let events = events.clone();
        let process_tree = Arc::clone(&process_tree);
        let transport_ok = Arc::clone(&transport_ok);
        let program = command.program.clone();
        Some(thread::spawn(move || {
            if let Err(err) = stdin.write_all(input.as_bytes()) {
                transport_ok.store(false, Ordering::Relaxed);
                let _ = events.send(AgentEvent::Fatal(format!(
                    "failed to write {program} prompt to stdin: {err}"
                )));
                let _ = process_tree.terminate();
            }
            // Dropping `stdin` closes the pipe so the child sees EOF.
        }))
    } else {
        None
    };

    let child = Arc::new(Mutex::new(child));
    let done = Arc::new(AtomicBool::new(false));

    // Watcher: kill the tree on cancellation. If the direct child has exited
    // but a descendant still owns an output/input pipe, allow a short drain
    // window and then reap the owned tree so the adapter cannot hang forever.
    let watcher = {
        let child = Arc::clone(&child);
        let process_tree = Arc::clone(&process_tree);
        let done = Arc::clone(&done);
        let cancel = Arc::clone(&cancel);
        thread::spawn(move || {
            loop {
                if done.load(Ordering::Relaxed) {
                    break;
                }
                if cancel.load(Ordering::Relaxed) {
                    let _ = process_tree.request_stop();
                    let deadline = Instant::now() + CANCEL_GRACE;
                    while Instant::now() < deadline && !done.load(Ordering::Relaxed) {
                        let parent_exited = child
                            .lock()
                            .ok()
                            .and_then(|mut child| child.try_wait().ok())
                            .flatten()
                            .is_some();
                        if parent_exited {
                            break;
                        }
                        thread::sleep(Duration::from_millis(25));
                    }
                    if !done.load(Ordering::Relaxed)
                        && process_tree.terminate().is_err()
                        && let Ok(mut child) = child.lock()
                    {
                        let _ = child.kill();
                    }
                    break;
                }
                let parent_exited = child
                    .lock()
                    .ok()
                    .and_then(|mut child| child.try_wait().ok())
                    .flatten()
                    .is_some();
                if parent_exited {
                    let deadline = Instant::now() + OUTPUT_DRAIN_GRACE;
                    while !done.load(Ordering::Relaxed) && Instant::now() < deadline {
                        if cancel.load(Ordering::Relaxed) {
                            break;
                        }
                        thread::sleep(Duration::from_millis(25));
                    }
                    if !done.load(Ordering::Relaxed) {
                        let _ = process_tree.terminate();
                    }
                    break;
                }
                thread::sleep(Duration::from_millis(50));
            }
        })
    };

    // Forward stderr lines.
    let stderr_thread = stderr.map(|stderr| {
        let events = events.clone();
        let process_tree = Arc::clone(&process_tree);
        let transport_ok = Arc::clone(&transport_ok);
        let program = command.program.clone();
        thread::spawn(move || {
            for line in bounded_lines(BufReader::new(stderr), MAX_STDERR_LINE_BYTES) {
                match line {
                    Ok(line) => {
                        let _ = events.send(AgentEvent::Stderr(line));
                    }
                    Err(err) => {
                        transport_ok.store(false, Ordering::Relaxed);
                        let _ = events.send(AgentEvent::Fatal(format!(
                            "failed to read {program} stderr: {err}"
                        )));
                        let _ = process_tree.terminate();
                        break;
                    }
                }
            }
        })
    });

    // Stream stdout on this thread. Invalid/truncated UTF-8 is a transport
    // failure: silently dropping the rest of the JSONL stream could turn a
    // backend error into an apparently successful exit.
    if let Some(stdout) = stdout {
        for line in bounded_lines(BufReader::new(stdout), MAX_PROTOCOL_LINE_BYTES) {
            match line {
                Ok(line) => {
                    let _ = events.send(AgentEvent::Raw(line.clone()));
                    for event in parser.parse_line(&line) {
                        let _ = events.send(event);
                    }
                }
                Err(err) => {
                    transport_ok.store(false, Ordering::Relaxed);
                    let _ = events.send(AgentEvent::Fatal(format!(
                        "failed to read {} output: {err}",
                        command.program
                    )));
                    let _ = process_tree.terminate();
                    break;
                }
            }
        }
    }
    for event in parser.finish() {
        let _ = events.send(event);
    }

    let status = child.lock().ok().and_then(|mut child| child.wait().ok());
    if let Some(stdin_thread) = stdin_thread {
        let _ = stdin_thread.join();
    }
    if let Some(stderr_thread) = stderr_thread {
        let _ = stderr_thread.join();
    }
    done.store(true, Ordering::Relaxed);
    let _ = watcher.join();

    let ok = transport_ok.load(Ordering::Relaxed)
        && status.as_ref().is_some_and(|status| status.success());
    for event in parser.finish_after_exit(ok) {
        let _ = events.send(event);
    }

    match status {
        Some(status) => {
            let _ = events.send(AgentEvent::Exited {
                code: status.code(),
                ok,
            });
        }
        None => {
            let _ = events.send(AgentEvent::Fatal("failed to wait for process".to_string()));
            let _ = events.send(AgentEvent::Exited {
                code: None,
                ok: false,
            });
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::mpsc;

    /// A trivial adapter that runs `/bin/sh -c <script>` and turns each stdout
    /// line into a `TextDelta`, to exercise the streaming machinery.
    struct ShAdapter {
        script: String,
    }

    struct LineToDelta;

    struct FailingPreflightAdapter;

    impl LineParser for LineToDelta {
        fn parse_line(&mut self, line: &str) -> Vec<AgentEvent> {
            vec![AgentEvent::TextDelta(line.to_string())]
        }
    }

    impl StreamAdapter for FailingPreflightAdapter {
        fn backend(&self) -> BackendKind {
            BackendKind::Agy
        }

        fn preflight(&self) -> Result<(), String> {
            Err("Agy 1.1.12 or newer is required".to_string())
        }

        fn build_command(
            &self,
            _prompt: &str,
            _session: Option<&AgentSessionId>,
            _effort: Option<Effort>,
        ) -> AdapterCommand {
            panic!("preflight failure must prevent command construction")
        }

        fn parser(&self) -> Box<dyn LineParser> {
            panic!("preflight failure must prevent parser construction")
        }
    }

    impl StreamAdapter for ShAdapter {
        fn backend(&self) -> BackendKind {
            BackendKind::Codex
        }
        fn build_command(
            &self,
            _prompt: &str,
            _session: Option<&AgentSessionId>,
            _effort: Option<Effort>,
        ) -> AdapterCommand {
            AdapterCommand {
                program: "/bin/sh".to_string(),
                args: vec!["-c".to_string(), self.script.clone()],
                cwd: PathBuf::from("/tmp"),
                stdin: None,
            }
        }
        fn parser(&self) -> Box<dyn LineParser> {
            Box::new(LineToDelta)
        }
    }

    struct QuotaRetryAdapter {
        script: String,
    }

    struct FatalOrDelta;

    impl LineParser for FatalOrDelta {
        fn parse_line(&mut self, line: &str) -> Vec<AgentEvent> {
            if let Some(message) = line.strip_prefix("FATAL:") {
                vec![AgentEvent::Fatal(message.to_string())]
            } else {
                vec![AgentEvent::TextDelta(line.to_string())]
            }
        }
    }

    impl StreamAdapter for QuotaRetryAdapter {
        fn backend(&self) -> BackendKind {
            BackendKind::Agy
        }

        fn build_command(
            &self,
            _prompt: &str,
            _session: Option<&AgentSessionId>,
            _effort: Option<Effort>,
        ) -> AdapterCommand {
            AdapterCommand {
                program: "/bin/sh".to_string(),
                args: vec!["-c".to_string(), self.script.clone()],
                cwd: PathBuf::from("/tmp"),
                stdin: None,
            }
        }

        fn parser(&self) -> Box<dyn LineParser> {
            Box::new(FatalOrDelta)
        }

        fn retry_delay(&self, fatal: &str, attempt: u32) -> Option<Duration> {
            crate::adapter::agy_stream::agy_quota_retry_delay(fatal, attempt)
        }
    }

    fn collect(rx: mpsc::Receiver<AgentEvent>) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        while let Ok(event) = rx.recv() {
            out.push(event);
        }
        out
    }

    #[test]
    fn retries_agy_eligibility_429_then_succeeds() {
        let dir =
            std::env::temp_dir().join(format!("asterline-agy-quota-retry-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let flag = dir.join("first");
        let runner = ProcessRunner::new(QuotaRetryAdapter {
            script: format!(
                "if [ ! -f '{flag}' ]; then printf '%s\\n' 'FATAL:Eligibility check failed: RESOURCE_EXHAUSTED (code 429): Resource has been exhausted (e.g. check quota).'; touch '{flag}'; exit 1; fi; printf 'ok\\n'",
                flag = flag.display()
            ),
        });
        let (tx, rx) = mpsc::sync_channel(65_536);
        runner.run(
            RunRequest {
                prompt: "x".to_string(),
                session: None,
                cancel: Arc::new(AtomicBool::new(false)),
                effort: None,
            },
            tx,
        );
        let events = collect(rx);
        let _ = std::fs::remove_dir_all(&dir);

        assert!(
            events.iter().any(|event| matches!(
                event,
                AgentEvent::Log(message) if message.contains("agy quota exhausted")
            )),
            "{events:?}"
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, AgentEvent::TextDelta(text) if text == "ok")),
            "{events:?}"
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, AgentEvent::Fatal(_))),
            "{events:?}"
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, AgentEvent::Exited { ok: true, .. }))
        );
    }

    #[test]
    fn does_not_retry_non_quota_failures() {
        let runner = ProcessRunner::new(QuotaRetryAdapter {
            script: "printf '%s\\n' 'FATAL:model not found'; exit 1".to_string(),
        });
        let (tx, rx) = mpsc::sync_channel(65_536);
        runner.run(
            RunRequest {
                prompt: "x".to_string(),
                session: None,
                cancel: Arc::new(AtomicBool::new(false)),
                effort: None,
            },
            tx,
        );
        let events = collect(rx);
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::Fatal(message) if message.contains("model not found")
        )));
        assert!(
            !events.iter().any(
                |event| matches!(event, AgentEvent::Log(message) if message.contains("retrying"))
            ),
            "{events:?}"
        );
    }

    #[test]
    fn preflight_failure_is_fatal_without_starting_the_backend() {
        let runner = ProcessRunner::new(FailingPreflightAdapter);
        let (tx, rx) = mpsc::sync_channel(8);

        runner.run(
            RunRequest {
                prompt: "x".to_string(),
                session: None,
                cancel: Arc::new(AtomicBool::new(false)),
                effort: None,
            },
            tx,
        );
        let events = collect(rx);

        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::Fatal(message) if message.contains("Agy 1.1.12")
        )));
        assert!(events.contains(&AgentEvent::Exited {
            code: None,
            ok: false,
        }));
    }

    #[test]
    fn streams_lines_then_reports_exit() {
        let runner = ProcessRunner::new(ShAdapter {
            script: "printf 'one\\ntwo\\n'; exit 0".to_string(),
        });
        let (tx, rx) = mpsc::sync_channel(65_536);
        runner.run(
            RunRequest {
                prompt: "hi".to_string(),
                session: None,
                cancel: Arc::new(AtomicBool::new(false)),
                effort: None,
            },
            tx,
        );
        let events = collect(rx);

        assert!(events.contains(&AgentEvent::TextDelta("one".to_string())));
        assert!(events.contains(&AgentEvent::TextDelta("two".to_string())));
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::Exited {
                ok: true,
                code: Some(0)
            }
        )));
        // Each stdout line is also emitted raw for persistence.
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, AgentEvent::Raw(_)))
                .count(),
            2
        );
    }

    #[test]
    fn nonzero_exit_is_reported() {
        let runner = ProcessRunner::new(ShAdapter {
            script: "printf 'boom\\n' 1>&2; exit 3".to_string(),
        });
        let (tx, rx) = mpsc::sync_channel(65_536);
        runner.run(
            RunRequest {
                prompt: "x".to_string(),
                session: None,
                cancel: Arc::new(AtomicBool::new(false)),
                effort: None,
            },
            tx,
        );
        let events = collect(rx);

        assert!(events.contains(&AgentEvent::Stderr("boom".to_string())));
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::Exited {
                ok: false,
                code: Some(3)
            }
        )));
    }

    #[test]
    fn missing_binary_is_fatal() {
        let runner = ProcessRunner::new(ShAdapter {
            script: String::new(),
        });
        // Override program by using a non-existent binary through a custom command.
        let (tx, rx) = mpsc::sync_channel(65_536);
        run_streaming(
            AdapterCommand {
                program: "asterline-no-such-binary".to_string(),
                args: vec![],
                cwd: PathBuf::from("/tmp"),
                stdin: None,
            },
            runner.adapter.parser(),
            Arc::new(AtomicBool::new(false)),
            tx,
        );
        let events = collect(rx);

        assert!(events.iter().any(|e| matches!(e, AgentEvent::Fatal(_))));
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::Exited {
                code: None,
                ok: false
            }
        )));
    }

    #[test]
    fn unreadable_stdout_marks_an_zero_exit_as_failed() {
        let runner = ProcessRunner::new(ShAdapter {
            script: "printf '\\377\\n'; exit 0".to_string(),
        });
        let (tx, rx) = mpsc::sync_channel(65_536);
        runner.run(
            RunRequest {
                prompt: "x".to_string(),
                session: None,
                cancel: Arc::new(AtomicBool::new(false)),
                effort: None,
            },
            tx,
        );
        let events = collect(rx);

        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::Fatal(message) if message.contains("failed to read")
        )));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, AgentEvent::Exited { ok: false, .. }))
        );
    }

    #[test]
    fn cancellation_kills_long_running_process() {
        let runner = ProcessRunner::new(ShAdapter {
            script: "printf 'start\\n'; sleep 30".to_string(),
        });
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::sync_channel(65_536);
        let cancel_for_thread = Arc::clone(&cancel);
        let handle = thread::spawn(move || {
            runner.run(
                RunRequest {
                    prompt: "x".to_string(),
                    session: None,
                    cancel: cancel_for_thread,
                    effort: None,
                },
                tx,
            );
        });
        // Wait for first output, then cancel.
        let first = rx.recv().expect("first event");
        assert!(matches!(
            first,
            AgentEvent::Raw(_) | AgentEvent::TextDelta(_)
        ));
        cancel.store(true, Ordering::Relaxed);
        handle.join().expect("runner finishes after cancel");

        let mut saw_exit = false;
        while let Ok(event) = rx.recv() {
            if matches!(event, AgentEvent::Exited { .. } | AgentEvent::Fatal(_)) {
                saw_exit = true;
            }
        }
        assert!(saw_exit, "cancelled run still reports completion");
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_kills_descendant_processes() {
        let dir = std::env::temp_dir().join(format!(
            "asterline-process-tree-cancel-{}",
            std::process::id()
        ));
        let survivor = dir.join("survived");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let runner = ProcessRunner::new(ShAdapter {
            script: format!(
                "(sleep 1; printf survived > '{}') & printf 'start\\n'; wait",
                survivor.display()
            ),
        });
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::sync_channel(65_536);
        let cancel_for_thread = Arc::clone(&cancel);
        let handle = thread::spawn(move || {
            runner.run(
                RunRequest {
                    prompt: "x".to_string(),
                    session: None,
                    cancel: cancel_for_thread,
                    effort: None,
                },
                tx,
            );
        });

        let _ = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        cancel.store(true, Ordering::Relaxed);
        handle.join().unwrap();
        thread::sleep(Duration::from_millis(1_100));

        assert!(
            !survivor.exists(),
            "a cancelled adapter descendant continued running"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn bounded_lines_rejects_an_oversized_unterminated_line() {
        let input = Cursor::new(b"12345".to_vec());
        let error = bounded_lines(BufReader::new(input), 4)
            .next()
            .unwrap()
            .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("exceeded 4 bytes"));
    }

    #[test]
    fn oversized_stdout_is_fatal_and_terminates_the_process_tree() {
        let (tx, rx) = mpsc::sync_channel(65_536);
        let handle = thread::spawn(move || {
            run_streaming(
                AdapterCommand {
                    program: "/bin/sh".to_string(),
                    args: vec!["-c".to_string(), "yes x | tr -d '\\n'".to_string()],
                    cwd: PathBuf::from("/tmp"),
                    stdin: None,
                },
                Box::new(LineToDelta),
                Arc::new(AtomicBool::new(false)),
                tx,
            );
        });
        let events = recv_until_exit(&rx, Duration::from_secs(3));
        handle.join().unwrap();

        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::Fatal(message) if message.contains("line exceeded")
        )));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, AgentEvent::Exited { .. }))
                .count(),
            1
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, AgentEvent::Exited { ok: false, .. }))
        );
    }

    #[test]
    fn closed_child_stdin_is_fatal_and_reports_one_exit() {
        let (tx, rx) = mpsc::sync_channel(65_536);
        run_streaming(
            AdapterCommand {
                program: "/bin/sh".to_string(),
                args: vec![
                    "-c".to_string(),
                    "exec 0<&-; sleep 0.05; exit 0".to_string(),
                ],
                cwd: PathBuf::from("/tmp"),
                stdin: Some("x".repeat(16 * 1024 * 1024)),
            },
            Box::new(LineToDelta),
            Arc::new(AtomicBool::new(false)),
            tx,
        );
        let events = collect(rx);

        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::Fatal(message) if message.contains("failed to write") && message.contains("stdin")
        )));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, AgentEvent::Exited { .. }))
                .count(),
            1
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, AgentEvent::Exited { ok: false, .. }))
        );
    }

    #[test]
    fn stdin_and_stdout_are_drained_concurrently() {
        let (tx, rx) = mpsc::sync_channel(65_536);
        let handle = thread::spawn(move || {
            run_streaming(
                AdapterCommand {
                    program: "/bin/sh".to_string(),
                    args: vec![
                        "-c".to_string(),
                        "head -c 262144 /dev/zero | tr '\\000' x; printf '\\n'; cat >/dev/null"
                            .to_string(),
                    ],
                    cwd: PathBuf::from("/tmp"),
                    stdin: Some("y".repeat(262144)),
                },
                Box::new(LineToDelta),
                Arc::new(AtomicBool::new(false)),
                tx,
            );
        });
        let events = recv_until_exit(&rx, Duration::from_secs(3));
        handle.join().unwrap();

        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::Exited {
                ok: true,
                code: Some(0)
            }
        )));
    }

    #[test]
    fn normal_parent_exit_does_not_kill_background_descendants() {
        let dir = std::env::temp_dir().join(format!(
            "asterline-process-tree-normal-exit-{}",
            std::process::id()
        ));
        let survivor = dir.join("survived");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let runner = ProcessRunner::new(ShAdapter {
            script: format!(
                "(sleep 0.2; printf survived > '{}') >/dev/null 2>&1 & exit 0",
                survivor.display()
            ),
        });
        let (tx, rx) = mpsc::sync_channel(65_536);
        runner.run(
            RunRequest {
                prompt: "x".to_string(),
                session: None,
                cancel: Arc::new(AtomicBool::new(false)),
                effort: None,
            },
            tx,
        );
        let _ = collect(rx);
        thread::sleep(Duration::from_millis(350));

        assert!(survivor.exists(), "normal exit killed a background child");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn parent_exit_reaps_descendants_that_keep_output_pipes_open() {
        let dir = std::env::temp_dir().join(format!(
            "asterline-process-tree-held-pipe-{}",
            std::process::id()
        ));
        let process_group_file = dir.join("process-group");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let runner = ProcessRunner::new(ShAdapter {
            script: format!(
                "printf '%s' \"$$\" > '{}'; sleep 30 & exit 0",
                process_group_file.display()
            ),
        });
        let (tx, rx) = mpsc::sync_channel(65_536);
        let started = Instant::now();

        runner.run(
            RunRequest {
                prompt: "x".to_string(),
                session: None,
                cancel: Arc::new(AtomicBool::new(false)),
                effort: None,
            },
            tx,
        );
        let events = recv_until_exit(&rx, Duration::from_secs(2));

        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(events.contains(&AgentEvent::Exited {
            code: Some(0),
            ok: true,
        }));
        let process_group: i32 = std::fs::read_to_string(&process_group_file)
            .unwrap()
            .parse()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        let process_group_gone = loop {
            // SAFETY: signal 0 only checks whether any process remains in the
            // isolated group; it does not alter the process.
            if unsafe { libc::killpg(process_group, 0) } != 0
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
            {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            thread::sleep(Duration::from_millis(25));
        };
        assert!(
            process_group_gone,
            "the pipe-holding adapter process group survived parent cleanup"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    fn recv_until_exit(rx: &mpsc::Receiver<AgentEvent>, timeout: Duration) -> Vec<AgentEvent> {
        let deadline = std::time::Instant::now() + timeout;
        let mut events = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let event = rx
                .recv_timeout(remaining)
                .expect("adapter did not report Exited before the deadline");
            let exited = matches!(event, AgentEvent::Exited { .. });
            events.push(event);
            if exited {
                return events;
            }
        }
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    const DESCENDANT_SENTINEL: &str = "ASTERLINE_WINDOWS_DESCENDANT_SENTINEL";
    const PARENT_READY_SENTINEL: &str = "ASTERLINE_WINDOWS_PARENT_READY_SENTINEL";
    const PARENT_MODE: &str = "ASTERLINE_WINDOWS_PARENT_MODE";

    struct LineToLog;

    impl LineParser for LineToLog {
        fn parse_line(&mut self, line: &str) -> Vec<AgentEvent> {
            vec![AgentEvent::Log(line.to_string())]
        }
    }

    #[test]
    fn windows_descendant_helper() {
        let Some(sentinel) = std::env::var_os(DESCENDANT_SENTINEL) else {
            return;
        };
        std::thread::sleep(Duration::from_secs(2));
        std::fs::write(sentinel, b"survived").unwrap();
    }

    #[test]
    fn windows_parent_helper() {
        let Some(mode) = std::env::var_os(PARENT_MODE) else {
            return;
        };
        let ready = std::env::var_os(PARENT_READY_SENTINEL)
            .expect("parent helper requires a ready sentinel");
        let mut descendant = Command::new(std::env::current_exe().unwrap());
        descendant
            .args([
                "--exact",
                "adapter::process::windows_tests::windows_descendant_helper",
                "--nocapture",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut descendant = descendant.spawn().expect("descendant should start");
        std::fs::write(ready, b"ready").unwrap();
        if mode == "wait" {
            let _ = descendant.wait();
        }
    }

    fn wait_for_file(path: &Path, timeout: Duration) {
        let started = std::time::Instant::now();
        while !path.exists() && started.elapsed() < timeout {
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(path.exists(), "fixture did not spawn its descendant");
    }

    fn parent_fixture(sentinel: &Path, ready: &Path, mode: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "adapter::process::windows_tests::windows_parent_helper",
                "--nocapture",
            ])
            .env(DESCENDANT_SENTINEL, sentinel)
            .env(PARENT_READY_SENTINEL, ready)
            .env(PARENT_MODE, mode)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }

    #[test]
    fn explicit_cmd_shim_is_launchable() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "asterline-windows-cmd-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let shim = dir.join("fake-backend.CMD");
        std::fs::write(&shim, b"@echo off\r\necho cmd-ok\r\n").unwrap();
        let (events, received) = std::sync::mpsc::sync_channel(32);

        run_streaming(
            AdapterCommand {
                program: shim.display().to_string(),
                args: Vec::new(),
                cwd: dir.clone(),
                stdin: None,
            },
            Box::new(LineToLog),
            Arc::new(AtomicBool::new(false)),
            events,
        );
        let observed = received.iter().collect::<Vec<_>>();

        assert!(
            observed
                .iter()
                .any(|event| matches!(event, AgentEvent::Log(line) if line.trim() == "cmd-ok"))
        );
        assert!(
            observed
                .iter()
                .any(|event| matches!(event, AgentEvent::Exited { ok: true, .. }))
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn job_termination_kills_descendant_processes() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "asterline-windows-job-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let sentinel = dir.join("descendant-survived");
        let ready = dir.join("descendant-ready");
        let mut command = parent_fixture(&sentinel, &ready, "wait");
        configure_process_tree(&mut command);
        let mut child = command.spawn().expect("parent fixture should start");
        let tree = ChildProcessTree::attach(&mut child).expect("fixture should enter a Job Object");
        wait_for_file(&ready, Duration::from_secs(10));

        tree.terminate_with_fallback(&mut child).unwrap();
        let _ = child.wait();
        std::thread::sleep(Duration::from_millis(2_300));

        assert!(
            !sentinel.exists(),
            "a process inside the cancelled Windows Job Object survived"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn normal_parent_exit_does_not_kill_windows_background_descendants() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "asterline-windows-job-normal-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let sentinel = dir.join("descendant-survived");
        let ready = dir.join("descendant-ready");
        let mut command = parent_fixture(&sentinel, &ready, "exit");
        configure_process_tree(&mut command);
        let mut child = command.spawn().expect("parent fixture should start");
        let tree = ChildProcessTree::attach(&mut child).expect("fixture should enter a Job Object");
        assert!(child.wait().unwrap().success());
        wait_for_file(&ready, Duration::from_secs(10));
        drop(tree);
        wait_for_file(&sentinel, Duration::from_secs(10));
        let _ = std::fs::remove_dir_all(dir);
    }
}
