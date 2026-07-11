//! Opt-in smoke tests against the real `codex` / `claude` / `grok` / `agy` CLIs. They exercise
//! the full adapter -> process -> parser path (no TUI) with one trivial turn.
//!
//! These are `#[ignore]`d because they call local CLIs and may consume usage.
//! Run explicitly:
//!
//! ```bash
//! ASTERLINE_SMOKE_CODEX=1  cargo test --test real_smoke real_codex_smoke  -- --ignored --nocapture
//! ASTERLINE_SMOKE_CLAUDE=1 cargo test --test real_smoke real_claude_smoke -- --ignored --nocapture
//! ASTERLINE_SMOKE_GROK=1   cargo test --test real_smoke real_grok_smoke   -- --ignored --nocapture
//! ASTERLINE_SMOKE_AGY=1    cargo test --test real_smoke real_agy_smoke    -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};
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
            effort: None,
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

fn assert_completed_contains(label: &str, events: &[AgentEvent], needle: &str) {
    assert!(
        events.iter().any(|event| {
            matches!(event, AgentEvent::MessageCompleted(text) if text.contains(needle))
        }),
        "{label}: expected completed message to contain {needle:?}"
    );
}

fn agy_member(test_name: &str) -> TeamMember {
    let cwd: PathBuf = std::env::temp_dir().join(format!(
        "asterline-agy-smoke-{test_name}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&cwd).unwrap();
    let mut member = TeamMember::new("agy", "Agy", BackendKind::Agy, "smoke");
    member.cwd = Some(cwd);
    member
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
#[ignore = "calls the real codex CLI twice (fresh + resume); opt in with ASTERLINE_SMOKE_CODEX=1"]
fn real_codex_resume_smoke() {
    if std::env::var("ASTERLINE_SMOKE_CODEX").as_deref() != Ok("1") {
        return;
    }
    let member = TeamMember::new("codex", "Codex", BackendKind::Codex, "smoke");
    let first = run_once(&member, "Remember the word ORANGE. Reply with: READY");
    assert_healthy_turn("codex-fresh", &first);
    let session = first
        .iter()
        .find_map(|event| match event {
            AgentEvent::SessionDiscovered(id) => Some(id.clone()),
            _ => None,
        })
        .expect("a session id to resume");

    // Resume the same session — this is the path that previously sent
    // exec-only flags to `codex exec resume` and exited with code 2.
    let runner = runner_for(&member, Path::new(env!("CARGO_MANIFEST_DIR")));
    let (tx, rx) = mpsc::channel();
    runner.run(
        RunRequest {
            prompt: "Reply with the word you were asked to remember.".to_string(),
            session: Some(session),
            cancel: Arc::new(AtomicBool::new(false)),
            effort: None,
        },
        tx,
    );
    let resumed: Vec<AgentEvent> = rx.iter().collect();
    assert_healthy_turn("codex-resume", &resumed);
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

#[test]
#[ignore = "calls the real grok CLI; opt in with ASTERLINE_SMOKE_GROK=1"]
fn real_grok_smoke() {
    if std::env::var("ASTERLINE_SMOKE_GROK").as_deref() != Ok("1") {
        return;
    }
    let member = TeamMember::new("grok", "Grok", BackendKind::Grok, "smoke");
    let events = run_once(&member, "Reply with exactly: ASTERLINE_OK");
    assert_healthy_turn("grok", &events);
    assert_completed_contains("grok", &events, "ASTERLINE_OK");
}

#[test]
#[ignore = "calls the real agy CLI; opt in with ASTERLINE_SMOKE_AGY=1"]
fn real_agy_smoke() {
    if std::env::var("ASTERLINE_SMOKE_AGY").as_deref() != Ok("1") {
        return;
    }
    let member = agy_member("fresh");
    let events = run_once(
        &member,
        "Do not inspect files or use tools. Reply with exactly: ASTERLINE_OK",
    );
    assert_healthy_turn("agy", &events);
    assert_completed_contains("agy", &events, "ASTERLINE_OK");
}

#[test]
#[ignore = "calls the real agy CLI twice (fresh + resume); opt in with ASTERLINE_SMOKE_AGY=1"]
fn real_agy_resume_smoke() {
    if std::env::var("ASTERLINE_SMOKE_AGY").as_deref() != Ok("1") {
        return;
    }
    let member = agy_member("resume");
    let first = run_once(
        &member,
        "Do not inspect files or use tools. Remember the word ORANGE. Reply with: READY",
    );
    assert_healthy_turn("agy-fresh", &first);
    assert_completed_contains("agy-fresh", &first, "READY");
    let session = first
        .iter()
        .find_map(|event| match event {
            AgentEvent::SessionDiscovered(id) => Some(id.clone()),
            _ => None,
        })
        .expect("a session id to resume");

    let runner = runner_for(&member, Path::new(env!("CARGO_MANIFEST_DIR")));
    let (tx, rx) = mpsc::channel();
    runner.run(
        RunRequest {
            prompt: "Reply with the word you were asked to remember.".to_string(),
            session: Some(session),
            cancel: Arc::new(AtomicBool::new(false)),
            effort: None,
        },
        tx,
    );
    let resumed: Vec<AgentEvent> = rx.iter().collect();
    assert_healthy_turn("agy-resume", &resumed);
    assert_completed_contains("agy-resume", &resumed, "ORANGE");
}
