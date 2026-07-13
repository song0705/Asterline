//! Deterministic fake runner for tests and offline use.
//!
//! Emits a scripted sequence of [`AgentEvent`]s without spawning a process, so
//! the runtime and TUI can be exercised without real backends or usage.

use std::sync::mpsc::Sender;

use crate::adapter::{MemberRunner, RunRequest};
use crate::domain::event::{AgentEvent, AgentSessionId};
use crate::domain::team::BackendKind;
use crate::runtime::mode_prompts::{
    LEAD_PLAN_HINT, MODERATOR_HINT, REVIEW_PROTOCOL_HINT, ROUNDTABLE_HINT,
};

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

    /// Scripted teammate for `--fake`: recognizes mode prompts by their template
    /// markers (same constants the engine uses) and plays along; anything else echoes.
    pub fn team(backend: BackendKind) -> Self {
        Self::new(backend, move |req| {
            let session = AgentEvent::SessionDiscovered(AgentSessionId(format!(
                "fake-{}-session",
                backend.as_str()
            )));
            let text = team_response(backend, &req.prompt);
            vec![session, AgentEvent::MessageCompleted(text)]
        })
    }

    /// Emits a fixed event sequence regardless of the prompt.
    pub fn scripted(backend: BackendKind, events: Vec<AgentEvent>) -> Self {
        Self::new(backend, move |_| events.clone())
    }
}

fn team_response(backend: BackendKind, prompt: &str) -> String {
    if prompt.contains(REVIEW_PROTOCOL_HINT) {
        return "Reviewed the work.\n@@review {\"verdict\":\"approve\",\"summary\":\"fake approve\"}"
            .to_string();
    }
    if prompt.contains(LEAD_PLAN_HINT) {
        return lead_plan_response(prompt);
    }
    if prompt.contains("step #") {
        return step_done_response(prompt);
    }
    if prompt.contains(MODERATOR_HINT) {
        return "Fake synthesis: converge on option A.".to_string();
    }
    if prompt.contains(ROUNDTABLE_HINT) {
        return format!("Fake perspective from {backend}.");
    }
    format!("[{backend} fake] {prompt}")
}

fn lead_plan_response(prompt: &str) -> String {
    let mut lines = Vec::new();
    lines.push("Planned the work.".to_string());
    let teammates = prompt.lines().find_map(|line| {
        line.strip_prefix("Teammates: ")
            .map(|rest| rest.split(", ").collect::<Vec<_>>())
    });
    if let Some(ids) = teammates {
        if ids.is_empty() {
            lines.push(
                "@@workflow_step {\"action\":\"add\",\"title\":\"Fake step (no owners)\"}"
                    .to_string(),
            );
        } else {
            for id in ids {
                let id = id.trim();
                if id.is_empty() {
                    continue;
                }
                lines.push(format!(
                    "@@workflow_step {{\"action\":\"add\",\"owner\":\"{id}\",\"title\":\"Fake step for {id}\"}}"
                ));
            }
        }
    } else {
        lines.push(
            "@@workflow_step {\"action\":\"add\",\"title\":\"Fake step (no owners)\"}".to_string(),
        );
    }
    lines.join("\n")
}

fn step_done_response(prompt: &str) -> String {
    let mut numbers = Vec::new();
    let mut rest = prompt;
    while let Some(idx) = rest.find("step #") {
        rest = &rest[idx + "step #".len()..];
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(n) = digits.parse::<u32>() {
            numbers.push(n);
        }
    }
    let mut lines = Vec::new();
    lines.push("did the work".to_string());
    for n in numbers {
        lines.push(format!(
            "@@workflow_step {{\"action\":\"done\",\"step\":{n}}}"
        ));
    }
    lines.join("\n")
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

    fn completed_text(events: &[AgentEvent]) -> &str {
        events
            .iter()
            .find_map(|e| match e {
                AgentEvent::MessageCompleted(text) => Some(text.as_str()),
                _ => None,
            })
            .expect("MessageCompleted")
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

    #[test]
    fn team_review_hint_approves() {
        let events = run(
            &FakeRunner::team(BackendKind::Claude),
            &format!("please review\n\n{REVIEW_PROTOCOL_HINT}"),
        );
        let text = completed_text(&events);
        assert!(text.contains("@@review"));
        assert!(text.contains("approve"));
        assert!(matches!(events[0], AgentEvent::SessionDiscovered(_)));
    }

    #[test]
    fn team_step_hash_marks_done() {
        let events = run(
            &FakeRunner::team(BackendKind::Codex),
            "You own step #2: wire the parser. Also step #5 maybe.",
        );
        let text = completed_text(&events);
        assert!(text.contains("\"action\":\"done\""));
        assert!(text.contains("\"step\":2"));
        assert!(text.contains("\"step\":5"));
        assert!(text.contains("did the work"));
    }

    #[test]
    fn team_plain_prompt_echoes() {
        let events = run(&FakeRunner::team(BackendKind::Grok), "hello");
        let text = completed_text(&events);
        assert_eq!(text, "[grok fake] hello");
    }
}
