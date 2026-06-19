//! Opt-in smoke tests against the real `codex` / `claude` CLIs. They exercise
//! the full adapter -> process -> parser path (no TUI) with one trivial turn.
//!
//! These are `#[ignore]`d because they call local CLIs and may consume usage.
//! Run explicitly:
//!
//! ```bash
//! ASTERLINE_SMOKE_CODEX=1  cargo test --test real_smoke real_codex_smoke  -- --ignored --nocapture
//! ASTERLINE_SMOKE_CLAUDE=1 cargo test --test real_smoke real_claude_smoke -- --ignored --nocapture
//! ```

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;

use asterline::adapter::{RunRequest, runner_for};
use asterline::domain::event::AgentEvent;
use asterline::domain::team::{BackendKind, TeamMember};

fn run_once(member: &TeamMember, prompt: &str) -> Vec<AgentEvent> {
    let runner = runner_for(member, Path::new(env!("CARGO_MANIFEST_DIR")));
    let (tx, rx) = mpsc::channel();
    runner.run(
        RunRequest {
            prompt: prompt.to_string(),
            session: None,
            cancel: Arc::new(AtomicBool::new(false)),
        },
        tx,
    );
    rx.iter().collect()
}

fn report(label: &str, events: &[AgentEvent]) {
    for event in events {
        match event {
            AgentEvent::SessionDiscovered(id) => eprintln!("[{label}] session: {id}"),
            AgentEvent::MessageCompleted(text) => eprintln!("[{label}] message: {text}"),
            AgentEvent::ToolStarted { name, summary, .. } => {
                eprintln!("[{label}] tool start: {name} {summary}")
            }
            AgentEvent::ToolCompleted { ok, summary, .. } => {
                eprintln!("[{label}] tool done ({ok}): {summary}")
            }
            AgentEvent::Exited { code, ok } => eprintln!("[{label}] exit ok={ok} code={code:?}"),
            AgentEvent::Fatal(message) => eprintln!("[{label}] FATAL: {message}"),
            AgentEvent::Stderr(line) => eprintln!("[{label}] stderr: {line}"),
            _ => {}
        }
    }
}

fn assert_healthy_turn(label: &str, events: &[AgentEvent]) {
    report(label, events);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::SessionDiscovered(id) if !id.as_str().is_empty())),
        "{label}: expected a session id for resume"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::MessageCompleted(t) if !t.trim().is_empty())),
        "{label}: expected a non-empty completed message"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::Exited { ok: true, .. })),
        "{label}: expected a successful exit"
    );
}

#[test]
#[ignore = "calls the real codex CLI; opt in with ASTERLINE_SMOKE_CODEX=1"]
fn real_codex_smoke() {
    if std::env::var("ASTERLINE_SMOKE_CODEX").as_deref() != Ok("1") {
        return;
    }
    let member = TeamMember::new("codex", "Codex", BackendKind::Codex, "smoke");
    let events = run_once(&member, "Reply with exactly: ASTERLINE_OK");
    assert_healthy_turn("codex", &events);
}

#[test]
#[ignore = "calls the real claude CLI; opt in with ASTERLINE_SMOKE_CLAUDE=1"]
fn real_claude_smoke() {
    if std::env::var("ASTERLINE_SMOKE_CLAUDE").as_deref() != Ok("1") {
        return;
    }
    let member = TeamMember::new("claude", "Claude", BackendKind::Claude, "smoke");
    let events = run_once(&member, "Reply with exactly: ASTERLINE_OK");
    assert_healthy_turn("claude", &events);
}
