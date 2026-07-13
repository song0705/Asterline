//! Team runtime: turns `UiCommand`s into `RuntimeEvent`s, orchestrating per-member
//! runs, routing, approvals, the relay guard, and persistence.
//!
//! The core ([`team_runtime`]) is pure and synchronous. This module adds the
//! transport: a single merged input channel (UI commands + agent events), a
//! background loop, and the worker threads that drive member runs.

pub mod agent_runner;
pub mod approval;
pub mod mode_prompts;
pub mod session_registry;
pub mod team_runtime;

use std::collections::HashMap;
use std::io;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::adapter::{FakeRunner, MemberRunner, runner_for};
use crate::domain::event::{AgentEvent, RuntimeEvent, UiCommand, WorkflowRunId};
use crate::domain::team::{MemberId, TeamConfig, TeamMember};
use crate::store::sqlite::SqliteStore;

pub use team_runtime::{
    RunAction, RunnerChange, RuntimeStep, TeamRuntime, VerifyAction, VerifyOutput,
};

/// Everything the runtime loop consumes: UI commands and tagged agent events.
pub enum RuntimeInput {
    Ui(UiCommand),
    Agent(MemberId, AgentEvent),
    Verification(VerifyOutput),
}

/// Handle the TUI uses to send commands into the runtime.
#[derive(Clone)]
pub struct RuntimeHandle {
    tx: Sender<RuntimeInput>,
}

impl RuntimeHandle {
    /// Send a command; returns false if the runtime loop has stopped.
    pub fn send(&self, command: UiCommand) -> bool {
        self.tx.send(RuntimeInput::Ui(command)).is_ok()
    }
}

/// Per-member runners (real CLI or fake), keyed by member id.
pub type Runners = HashMap<MemberId, Arc<dyn MemberRunner>>;

struct RuntimeLoopOptions {
    approvals: bool,
    fake: bool,
    team_save_path: Option<PathBuf>,
}

/// Spawn the runtime on its own thread. `events` receives every [`RuntimeEvent`]
/// (starting with [`RuntimeEvent::Ready`]). Returns a handle for sending
/// commands and the thread's join handle.
pub fn spawn(
    config: TeamConfig,
    store: SqliteStore,
    runners: Runners,
    events: Sender<RuntimeEvent>,
    approvals: bool,
    fake: bool,
    team_save_path: Option<PathBuf>,
) -> (RuntimeHandle, JoinHandle<()>) {
    let (input_tx, input_rx) = mpsc::channel();
    let handle = RuntimeHandle {
        tx: input_tx.clone(),
    };
    let join = thread::spawn(move || {
        let options = RuntimeLoopOptions {
            approvals,
            fake,
            team_save_path,
        };
        run_loop(config, store, runners, events, options, input_tx, input_rx);
    });
    (handle, join)
}

fn run_loop(
    config: TeamConfig,
    store: SqliteStore,
    mut runners: Runners,
    events: Sender<RuntimeEvent>,
    options: RuntimeLoopOptions,
    input_tx: Sender<RuntimeInput>,
    input_rx: Receiver<RuntimeInput>,
) {
    let mut runtime = TeamRuntime::new(config, store).with_approvals(options.approvals);
    let mut active_verifications: HashMap<WorkflowRunId, Arc<AtomicBool>> = HashMap::new();
    let _ = events.send(runtime.ready_event());

    while let Ok(input) = input_rx.recv() {
        let shutdown = matches!(&input, RuntimeInput::Ui(UiCommand::Shutdown));
        let cancel_verifications = match &input {
            RuntimeInput::Ui(UiCommand::Cancel { member }) => member.is_none(),
            RuntimeInput::Ui(UiCommand::Shutdown) => true,
            _ => false,
        };
        let mut step = match input {
            RuntimeInput::Ui(command) => runtime.on_ui_command(command),
            RuntimeInput::Agent(member, event) => runtime.on_agent_event(&member, event),
            RuntimeInput::Verification(output) => {
                active_verifications.remove(&output.run_id);
                runtime.on_verify_output(output)
            }
        };

        if cancel_verifications {
            for cancel in active_verifications.values() {
                cancel.store(true, Ordering::Relaxed);
            }
        }

        for change in step.runner_changes {
            match change {
                RunnerChange::Upsert { member, workspace } => {
                    runners.insert(
                        member.id.clone(),
                        build_runner(&member, &workspace, options.fake),
                    );
                }
                RunnerChange::Remove(member) => {
                    runners.remove(&member);
                }
            }
        }

        if let Some(config) = step.persist_team.take()
            && let Some(path) = &options.team_save_path
            && let Err(err) = save_team_config(path, &config)
        {
            step.events.push(RuntimeEvent::Notice(format!(
                "could not save team config {}: {err}",
                path.display()
            )));
        }

        for event in step.events {
            if events.send(event).is_err() {
                return;
            }
        }
        for action in step.actions {
            if let Some(runner) = runners.get(&action.member) {
                agent_runner::dispatch(Arc::clone(runner), action, input_tx.clone());
            }
        }
        for action in step.verify_actions {
            active_verifications.insert(action.run_id, Arc::clone(&action.cancel));
            dispatch_verification(action, input_tx.clone());
        }

        if shutdown {
            break;
        }
    }
}

fn dispatch_verification(action: VerifyAction, input_tx: Sender<RuntimeInput>) {
    thread::spawn(move || {
        let output = run_verification(action);
        let _ = input_tx.send(RuntimeInput::Verification(output));
    });
}

fn run_verification(action: VerifyAction) -> VerifyOutput {
    let VerifyAction {
        run_id,
        command,
        workspace,
        cancel,
    } = action;

    let mut child = match Command::new("sh")
        .arg("-lc")
        .arg(&command)
        .current_dir(&workspace)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            return VerifyOutput {
                run_id,
                command,
                ok: false,
                stdout: Vec::new(),
                stderr: Vec::new(),
                start_error: Some(err.to_string()),
                cancelled: false,
            };
        }
    };

    let stdout = child.stdout.take().map(read_pipe);
    let stderr = child.stderr.take().map(read_pipe);
    let mut cancelled = false;
    let (ok, start_error) = loop {
        if cancel.load(Ordering::Relaxed) && !cancelled {
            cancelled = true;
            let _ = child.kill();
        }
        match child.try_wait() {
            Ok(Some(status)) => break (status.success() && !cancelled, None),
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(err) => {
                break (
                    false,
                    Some(format!("could not wait for verification: {err}")),
                );
            }
        }
    };

    let stdout = stdout
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();
    let stderr = stderr
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();

    VerifyOutput {
        run_id,
        command,
        ok,
        stdout,
        stderr,
        start_error,
        cancelled,
    }
}

fn read_pipe<R: Read + Send + 'static>(mut pipe: R) -> JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = pipe.read_to_end(&mut bytes);
        bytes
    })
}

fn build_runner(member: &TeamMember, workspace: &Path, fake: bool) -> Arc<dyn MemberRunner> {
    if fake {
        Arc::new(FakeRunner::team(member.backend))
    } else {
        Arc::from(runner_for(member, workspace))
    }
}

fn save_team_config(path: &PathBuf, config: &TeamConfig) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(config).map_err(io::Error::other)?;
    std::fs::write(path, json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::FakeRunner;
    use crate::domain::event::MessageTarget;
    use crate::domain::event::WorkflowRunStatus;
    use crate::domain::team::{BackendKind, TeamMember};
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    fn single_codex_team() -> TeamConfig {
        TeamConfig::new("solo", "/tmp/ws").with_member(TeamMember::new(
            "builder",
            "Builder",
            BackendKind::Codex,
            "impl",
        ))
    }

    #[test]
    fn runtime_thread_processes_a_message_end_to_end() {
        let mut runners: Runners = HashMap::new();
        runners.insert(
            MemberId::new("builder"),
            Arc::new(FakeRunner::echo(BackendKind::Codex)),
        );
        let (evt_tx, evt_rx) = mpsc::channel();
        let (handle, join) = spawn(
            single_codex_team(),
            SqliteStore::in_memory().unwrap(),
            runners,
            evt_tx,
            true,
            true,
            None,
        );

        // First event is Ready.
        let ready = evt_rx.recv_timeout(Duration::from_secs(2)).expect("ready");
        assert!(matches!(ready, RuntimeEvent::Ready { .. }));

        handle.send(UiCommand::UserMessage {
            target: MessageTarget::Default,
            body: "hi".to_string(),
        });

        let mut saw_completed = false;
        let mut saw_turn_finished = false;
        while let Ok(event) = evt_rx.recv_timeout(Duration::from_secs(2)) {
            match event {
                RuntimeEvent::MessageCompleted { text, .. } if text.contains("hi") => {
                    saw_completed = true;
                }
                RuntimeEvent::TurnFinished { .. } => {
                    saw_turn_finished = true;
                    break;
                }
                _ => {}
            }
        }
        assert!(saw_completed, "the fake reply was streamed to the TUI");
        assert!(saw_turn_finished, "the turn completed");

        handle.send(UiCommand::Shutdown);
        let _ = join.join();
    }

    #[test]
    fn runtime_thread_replaces_team_and_saves_config() {
        let dir = std::env::temp_dir().join(format!("asterline-runtime-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let save_path = dir.join("team.json");

        let mut runners: Runners = HashMap::new();
        runners.insert(
            MemberId::new("builder"),
            Arc::new(FakeRunner::echo(BackendKind::Codex)),
        );
        let (evt_tx, evt_rx) = mpsc::channel();
        let (handle, join) = spawn(
            single_codex_team(),
            SqliteStore::in_memory().unwrap(),
            runners,
            evt_tx,
            true,
            true,
            Some(save_path.clone()),
        );
        let _ = evt_rx.recv_timeout(Duration::from_secs(2)).expect("ready");

        let members = vec![
            TeamMember::new("builder", "Builder", BackendKind::Codex, "impl"),
            TeamMember::new("researcher", "Researcher", BackendKind::Agy, "research"),
        ];
        handle.send(UiCommand::ReplaceTeam {
            members,
            default_target: None,
        });

        let mut saw_ready = false;
        while let Ok(event) = evt_rx.recv_timeout(Duration::from_secs(2)) {
            if let RuntimeEvent::Ready { members, .. } = event
                && members.len() == 2
            {
                saw_ready = true;
                break;
            }
        }
        assert!(saw_ready);
        let saved = std::fs::read_to_string(&save_path).unwrap();
        let saved_config: TeamConfig = serde_json::from_str(&saved).unwrap();
        assert_eq!(saved_config.members.len(), 2);
        assert!(saved_config.member(&MemberId::new("researcher")).is_some());
        assert!(!saved.contains("\"id\""));
        assert!(!saved.contains("ASTERLINE_TEAM_PROTOCOL"));

        handle.send(UiCommand::Shutdown);
        let _ = join.join();
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn runtime_thread_runs_workflow_verification() {
        let (evt_tx, evt_rx) = mpsc::channel();
        let (handle, join) = spawn(
            single_codex_team(),
            SqliteStore::in_memory().unwrap(),
            HashMap::new(),
            evt_tx,
            true,
            true,
            None,
        );
        let _ = evt_rx.recv_timeout(Duration::from_secs(2)).expect("ready");

        handle.send(UiCommand::RunWorkflow {
            goal: "verify from runtime".to_string(),
        });
        let mut saw_run = false;
        while let Ok(event) = evt_rx.recv_timeout(Duration::from_secs(2)) {
            if matches!(event, RuntimeEvent::WorkflowRunUpdated { .. }) {
                saw_run = true;
                break;
            }
        }
        assert!(saw_run, "workflow run was created");

        handle.send(UiCommand::VerifyWorkflow {
            run_id: None,
            command: Some("printf runtime-verified".to_string()),
        });

        let mut saw_verifying = false;
        let mut saw_done = false;
        while let Ok(event) = evt_rx.recv_timeout(Duration::from_secs(2)) {
            match event {
                RuntimeEvent::WorkflowRunUpdated { run }
                    if run.status == WorkflowRunStatus::Verifying =>
                {
                    saw_verifying = true;
                }
                RuntimeEvent::WorkflowRunUpdated { run }
                    if run.status == WorkflowRunStatus::Done
                        && run.verification.as_ref().is_some_and(|v| {
                            v.ok && v.command == "printf runtime-verified"
                                && v.summary == "runtime-verified"
                        }) =>
                {
                    saw_done = true;
                    break;
                }
                _ => {}
            }
        }
        assert!(saw_verifying);
        assert!(saw_done);

        handle.send(UiCommand::Shutdown);
        let _ = join.join();
    }

    #[test]
    fn verification_worker_can_be_cancelled() {
        let cancel = Arc::new(AtomicBool::new(false));
        let action = VerifyAction {
            run_id: crate::domain::event::WorkflowRunId(1),
            command: "sleep 5; printf done".to_string(),
            workspace: std::env::temp_dir(),
            cancel: Arc::clone(&cancel),
        };

        let join = thread::spawn(move || run_verification(action));
        thread::sleep(Duration::from_millis(100));
        cancel.store(true, Ordering::Relaxed);
        let output = join.join().unwrap();

        assert!(output.cancelled);
        assert!(!output.ok);
    }
}
