//! Deterministic fake runner for tests and offline use.
//!
//! Emits a scripted sequence of [`AgentEvent`]s without spawning a process, so
//! the runtime and TUI can be exercised without real backends or usage.

use std::sync::mpsc::Sender;

use crate::adapter::{MemberRunner, RunRequest};
use crate::domain::event::{AgentEvent, AgentSessionId};
use crate::domain::team::BackendKind;

type Responder = Box<dyn Fn(&RunRequest) -> Vec<AgentEvent> + Send + Sync>;

pub struct FakeRunner {
    backend: BackendKind,
    responder: Responder,
}

impl FakeRunner {
    pub fn new(
        backend: BackendKind,
        responder: impl Fn(&RunRequest) -> Vec<AgentEvent> + Send + Sync + 'static,
    ) -> Self {
        Self {
            backend,
            responder: Box::new(responder),
        }
    }

    /// Echoes the prompt back as a completed message and reports a stable
    /// session id so resume bookkeeping has something to persist.
    pub fn echo(backend: BackendKind) -> Self {
        Self::new(backend, move |req| {
            vec![
                AgentEvent::SessionDiscovered(AgentSessionId(format!(
                    "fake-{}-session",
                    backend.as_str()
                ))),
                AgentEvent::MessageCompleted(format!("[{backend} fake] {}", req.prompt)),
            ]
        })
    }

    /// Emits a fixed event sequence regardless of the prompt.
    pub fn scripted(backend: BackendKind, events: Vec<AgentEvent>) -> Self {
        Self::new(backend, move |_| events.clone())
    }
}

impl MemberRunner for FakeRunner {
    fn backend(&self) -> BackendKind {
        self.backend
    }

    fn run(&self, req: RunRequest, events: Sender<AgentEvent>) {
        for event in (self.responder)(&req) {
            let _ = events.send(event);
        }
        let _ = events.send(AgentEvent::Exited {
            code: Some(0),
            ok: true,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc;

    fn run(runner: &FakeRunner, prompt: &str) -> Vec<AgentEvent> {
        let (tx, rx) = mpsc::channel();
        runner.run(
            RunRequest {
                prompt: prompt.to_string(),
                session: None,
                cancel: Arc::new(AtomicBool::new(false)),
                effort: None,
            },
            tx,
        );
        rx.iter().collect()
    }

    #[test]
    fn echo_reports_session_message_and_exit() {
        let events = run(&FakeRunner::echo(BackendKind::Codex), "build it");
        assert!(matches!(events[0], AgentEvent::SessionDiscovered(_)));
        assert!(matches!(
            &events[1],
            AgentEvent::MessageCompleted(text) if text.contains("build it")
        ));
        assert!(matches!(
            events.last().unwrap(),
            AgentEvent::Exited { ok: true, .. }
        ));
    }

    #[test]
    fn scripted_emits_fixed_sequence_then_exit() {
        let runner = FakeRunner::scripted(
            BackendKind::Claude,
            vec![AgentEvent::MessageCompleted("hi".to_string())],
        );
        let events = run(&runner, "anything");
        assert_eq!(
            events,
            vec![
                AgentEvent::MessageCompleted("hi".to_string()),
                AgentEvent::Exited {
                    code: Some(0),
                    ok: true
                },
            ]
        );
    }
}
