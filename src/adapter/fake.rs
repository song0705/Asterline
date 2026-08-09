//! Deterministic fake runner for tests and offline use.
//!
//! Emits a scripted sequence of [`AgentEvent`]s without spawning a process, so
//! the runtime and TUI can be exercised without real backends or usage.

use std::sync::mpsc::SyncSender;

use crate::adapter::{MemberRunner, RunRequest};
use crate::domain::event::{AgentEvent, AgentSessionId};
use crate::domain::team::BackendKind;
use crate::runtime::mode_prompts::{
    BRAINSTORM_BUILD_HINT, BRAINSTORM_PROPOSE_HINT, BRAINSTORM_STRETCH_HINT,
    BRAINSTORM_SYNTHESIS_HINT, BRAINSTORM_VOTE_HINT, PLAN_MODE_HINT, REVIEW_PROTOCOL_HINT,
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
    if prompt.contains(PLAN_MODE_HINT) {
        return plan_plan_response(prompt);
    }
    if prompt.contains("step #") {
        return step_done_response(prompt);
    }
    if prompt.contains(BRAINSTORM_SYNTHESIS_HINT) {
        return "## Ranked result\n\n1. R1-A#1 — 10 points\n2. R1-B#1 — 8 points\n\nPrimary recommendation: validate R1-A#1 with a small experiment."
            .to_string();
    }
    if prompt.contains(BRAINSTORM_VOTE_HINT) {
        return format!(
            "I ranked candidates for relevance and testability.\n\
             @@brainstorm_vote {{\"ranked\":[\"R1-A#1\",\"R1-B#1\",\"R2-A#2\",\"R2-B#2\",\"R3-A#3\"],\"summary\":\"{backend} ballot\"}}"
        );
    }
    if prompt.contains(BRAINSTORM_STRETCH_HINT) {
        return format!(
            "@@brainstorm_card {{\"title\":\"Invert {backend}\",\"proposal\":\"Remove the default assumption\",\"mechanism\":\"Reverse the usual dependency\",\"operation\":\"INVERT\",\"sources\":[\"R2-A#1\"]}}\n\
             @@brainstorm_card {{\"title\":\"No constraint {backend}\",\"proposal\":\"Imagine the main constraint disappears\",\"mechanism\":\"Explore the newly reachable design space\",\"operation\":\"REMOVE_CONSTRAINT\",\"sources\":[\"R2-A#2\"]}}\n\
             @@brainstorm_card {{\"title\":\"Ecology analogy {backend}\",\"proposal\":\"Borrow a mechanism from ecology\",\"mechanism\":\"Map ecological feedback onto the topic\",\"operation\":\"ANALOGY\",\"sources\":[\"R2-A#3\"]}}\n\
             @@brainstorm_card {{\"title\":\"Bridge {backend}\",\"proposal\":\"Combine two prior batches\",\"mechanism\":\"Join their complementary mechanisms\",\"operation\":\"BRIDGE\",\"sources\":[\"R2-A#1\",\"R2-B#1\"]}}"
        );
    }
    if prompt.contains(BRAINSTORM_BUILD_HINT) {
        return format!(
            "@@brainstorm_card {{\"title\":\"New {backend} direction\",\"proposal\":\"Try an independent direction\",\"mechanism\":\"Start from a separate assumption\",\"operation\":\"NEW\",\"sources\":[]}}\n\
             @@brainstorm_card {{\"title\":\"Build {backend} seed\",\"proposal\":\"Extend the shared seed\",\"mechanism\":\"Add one reinforcing capability\",\"operation\":\"BUILD\",\"sources\":[\"R1-A#1\"]}}\n\
             @@brainstorm_card {{\"title\":\"Combine {backend} seeds\",\"proposal\":\"Join two mechanisms\",\"mechanism\":\"Compose their strongest interactions\",\"operation\":\"COMBINE\",\"sources\":[\"R1-A#1\",\"R1-B#1\"]}}\n\
             @@brainstorm_card {{\"title\":\"Mutate {backend} audience\",\"proposal\":\"Change the target user\",\"mechanism\":\"Reframe the seed around another actor\",\"operation\":\"MUTATE\",\"sources\":[\"R1-A#2\"]}}"
        );
    }
    if prompt.contains(BRAINSTORM_PROPOSE_HINT) {
        return format!(
            "@@brainstorm_card {{\"title\":\"{backend} seed\",\"proposal\":\"A seed tailored to the backend\",\"mechanism\":\"Use its native strengths\",\"operation\":\"SEED\",\"sources\":[]}}\n\
             @@brainstorm_card {{\"title\":\"Low-tech {backend} seed\",\"proposal\":\"Try a contrasting low-tech path\",\"mechanism\":\"Replace automation with a simple workflow\",\"operation\":\"SEED\",\"sources\":[]}}\n\
             @@brainstorm_card {{\"title\":\"Service {backend} seed\",\"proposal\":\"Use a service model\",\"mechanism\":\"Deliver the capability as an ongoing service\",\"operation\":\"SEED\",\"sources\":[]}}\n\
             @@brainstorm_card {{\"title\":\"Wild {backend} inversion\",\"proposal\":\"Invert the usual relationship\",\"mechanism\":\"Make the receiver initiate the exchange\",\"operation\":\"SEED\",\"sources\":[]}}"
        );
    }
    format!("[{backend} fake] {prompt}")
}

fn plan_plan_response(prompt: &str) -> String {
    let mut lines = Vec::new();
    lines.push("Planned the work.".to_string());
    let teammates = prompt.lines().find_map(|line| {
        line.strip_prefix("Teammates: ")
            .map(|rest| rest.split(", ").collect::<Vec<_>>())
    });
    if let Some(ids) = teammates {
        if ids.is_empty() {
            lines.push(
                "@@run_step {\"action\":\"add\",\"title\":\"Fake step (no owners)\"}".to_string(),
            );
        } else {
            for id in ids {
                let id = id.trim();
                if id.is_empty() {
                    continue;
                }
                lines.push(format!(
                    "@@run_step {{\"action\":\"add\",\"owner\":\"{id}\",\"title\":\"Fake step for {id}\"}}"
                ));
            }
        }
    } else {
        lines.push(
            "@@run_step {\"action\":\"add\",\"title\":\"Fake step (no owners)\"}".to_string(),
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
        lines.push(format!("@@run_step {{\"action\":\"done\",\"step\":{n}}}"));
    }
    lines.join("\n")
}

impl MemberRunner for FakeRunner {
    fn backend(&self) -> BackendKind {
        self.backend
    }

    fn run(&self, req: RunRequest, events: SyncSender<AgentEvent>) {
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
        let (tx, rx) = mpsc::sync_channel(65_536);
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
