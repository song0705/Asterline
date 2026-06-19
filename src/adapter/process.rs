//! Generic child-process streaming used by the real CLI adapters.
//!
//! A [`StreamAdapter`] knows how to build the command for a backend and how to
//! parse its stdout lines into [`AgentEvent`]s. [`run_streaming`] does the
//! backend-agnostic work: spawn the child, stream stdout (each raw line is also
//! emitted as [`AgentEvent::Raw`] for persistence), forward stderr, support
//! cancellation, and report the exit status.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::adapter::{MemberRunner, RunRequest};
use crate::domain::event::{AgentEvent, AgentSessionId};
use crate::domain::team::BackendKind;

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
    fn build_command(&self, prompt: &str, session: Option<&AgentSessionId>) -> AdapterCommand;
    fn parser(&self) -> Box<dyn LineParser>;
}

/// Parses one backend stdout line into zero or more events. One parser instance
/// is created per run and may hold streaming state.
pub trait LineParser: Send {
    fn parse_line(&mut self, line: &str) -> Vec<AgentEvent>;
    /// Flush any trailing state when stdout closes.
    fn finish(&mut self) -> Vec<AgentEvent> {
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

    fn run(&self, req: RunRequest, events: Sender<AgentEvent>) {
        let command = self.adapter.build_command(&req.prompt, req.session.as_ref());
        let parser = self.adapter.parser();
        run_streaming(command, parser, req.cancel, events);
    }
}

/// Spawn `command`, stream its output through `parser`, and report completion.
pub fn run_streaming(
    command: AdapterCommand,
    mut parser: Box<dyn LineParser>,
    cancel: Arc<AtomicBool>,
    events: Sender<AgentEvent>,
) {
    let mut builder = Command::new(&command.program);
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

    let mut child = match builder.spawn() {
        Ok(child) => child,
        Err(err) => {
            let _ = events.send(AgentEvent::Fatal(format!(
                "failed to start {}: {err}",
                command.program
            )));
            return;
        }
    };

    if let (Some(input), Some(mut stdin)) = (command.stdin.as_ref(), child.stdin.take()) {
        let _ = stdin.write_all(input.as_bytes());
        // Dropping `stdin` closes the pipe so the child sees EOF.
    }

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let child = Arc::new(Mutex::new(child));
    let done = Arc::new(AtomicBool::new(false));

    // Watcher: kill the child if cancellation is requested.
    let watcher = {
        let child = Arc::clone(&child);
        let done = Arc::clone(&done);
        let cancel = Arc::clone(&cancel);
        thread::spawn(move || {
            loop {
                if done.load(Ordering::Relaxed) {
                    break;
                }
                if cancel.load(Ordering::Relaxed) {
                    if let Ok(mut child) = child.lock() {
                        let _ = child.kill();
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
        thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(result_ok) {
                let _ = events.send(AgentEvent::Stderr(line));
            }
        })
    });

    // Stream stdout on this thread.
    if let Some(stdout) = stdout {
        for line in BufReader::new(stdout).lines().map_while(result_ok) {
            let _ = events.send(AgentEvent::Raw(line.clone()));
            for event in parser.parse_line(&line) {
                let _ = events.send(event);
            }
        }
    }
    for event in parser.finish() {
        let _ = events.send(event);
    }

    done.store(true, Ordering::Relaxed);
    let status = child.lock().ok().and_then(|mut child| child.wait().ok());
    if let Some(stderr_thread) = stderr_thread {
        let _ = stderr_thread.join();
    }
    let _ = watcher.join();

    match status {
        Some(status) => {
            let _ = events.send(AgentEvent::Exited {
                code: status.code(),
                ok: status.success(),
            });
        }
        None => {
            let _ = events.send(AgentEvent::Fatal("failed to wait for process".to_string()));
        }
    }
}

fn result_ok<T, E>(result: Result<T, E>) -> Option<T> {
    result.ok()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// A trivial adapter that runs `/bin/sh -c <script>` and turns each stdout
    /// line into a `TextDelta`, to exercise the streaming machinery.
    struct ShAdapter {
        script: String,
    }

    struct LineToDelta;

    impl LineParser for LineToDelta {
        fn parse_line(&mut self, line: &str) -> Vec<AgentEvent> {
            vec![AgentEvent::TextDelta(line.to_string())]
        }
    }

    impl StreamAdapter for ShAdapter {
        fn backend(&self) -> BackendKind {
            BackendKind::Codex
        }
        fn build_command(&self, _prompt: &str, _session: Option<&AgentSessionId>) -> AdapterCommand {
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

    fn collect(rx: mpsc::Receiver<AgentEvent>) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        while let Ok(event) = rx.recv() {
            out.push(event);
        }
        out
    }

    #[test]
    fn streams_lines_then_reports_exit() {
        let runner = ProcessRunner::new(ShAdapter {
            script: "printf 'one\\ntwo\\n'; exit 0".to_string(),
        });
        let (tx, rx) = mpsc::channel();
        runner.run(
            RunRequest {
                prompt: "hi".to_string(),
                session: None,
                cancel: Arc::new(AtomicBool::new(false)),
            },
            tx,
        );
        let events = collect(rx);

        assert!(events.contains(&AgentEvent::TextDelta("one".to_string())));
        assert!(events.contains(&AgentEvent::TextDelta("two".to_string())));
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::Exited { ok: true, code: Some(0) }
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
        let (tx, rx) = mpsc::channel();
        runner.run(
            RunRequest {
                prompt: "x".to_string(),
                session: None,
                cancel: Arc::new(AtomicBool::new(false)),
            },
            tx,
        );
        let events = collect(rx);

        assert!(events.contains(&AgentEvent::Stderr("boom".to_string())));
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::Exited { ok: false, code: Some(3) }
        )));
    }

    #[test]
    fn missing_binary_is_fatal() {
        let runner = ProcessRunner::new(ShAdapter {
            script: String::new(),
        });
        // Override program by using a non-existent binary through a custom command.
        let (tx, rx) = mpsc::channel();
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
    }

    #[test]
    fn cancellation_kills_long_running_process() {
        let runner = ProcessRunner::new(ShAdapter {
            script: "printf 'start\\n'; sleep 30".to_string(),
        });
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let cancel_for_thread = Arc::clone(&cancel);
        let handle = thread::spawn(move || {
            runner.run(
                RunRequest {
                    prompt: "x".to_string(),
                    session: None,
                    cancel: cancel_for_thread,
                },
                tx,
            );
        });
        // Wait for first output, then cancel.
        let first = rx.recv().expect("first event");
        assert!(matches!(first, AgentEvent::Raw(_) | AgentEvent::TextDelta(_)));
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
}
