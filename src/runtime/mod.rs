//! Team runtime: turns `UiCommand`s into `RuntimeEvent`s, orchestrating per-member
//! runs, routing, approvals, the relay guard, and persistence.
//!
//! The core ([`team_runtime`]) is pure and synchronous. This module adds the
//! transport: a single merged input channel (UI commands + agent events), a
//! background loop, and the worker threads that drive member runs.

pub mod agent_runner;
pub mod approval;
pub mod session_registry;
pub mod team_runtime;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use crate::adapter::MemberRunner;
use crate::domain::event::{AgentEvent, RuntimeEvent, UiCommand};
use crate::domain::team::{MemberId, TeamConfig};
use crate::store::sqlite::SqliteStore;

pub use team_runtime::{RunAction, RuntimeStep, TeamRuntime};

/// Everything the runtime loop consumes: UI commands and tagged agent events.
pub enum RuntimeInput {
    Ui(UiCommand),
    Agent(MemberId, AgentEvent),
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

/// Spawn the runtime on its own thread. `events` receives every [`RuntimeEvent`]
/// (starting with [`RuntimeEvent::Ready`]). Returns a handle for sending
/// commands and the thread's join handle.
pub fn spawn(
    config: TeamConfig,
    store: SqliteStore,
    runners: Runners,
    events: Sender<RuntimeEvent>,
    approvals: bool,
) -> (RuntimeHandle, JoinHandle<()>) {
    let (input_tx, input_rx) = mpsc::channel();
    let handle = RuntimeHandle {
        tx: input_tx.clone(),
    };
    let join = thread::spawn(move || {
        run_loop(config, store, runners, events, approvals, input_tx, input_rx);
    });
    (handle, join)
}

fn run_loop(
    config: TeamConfig,
    store: SqliteStore,
    runners: Runners,
    events: Sender<RuntimeEvent>,
    approvals: bool,
    input_tx: Sender<RuntimeInput>,
    input_rx: Receiver<RuntimeInput>,
) {
    let mut runtime = TeamRuntime::new(config, store).with_approvals(approvals);
    let _ = events.send(runtime.ready_event());

    while let Ok(input) = input_rx.recv() {
        let shutdown = matches!(input, RuntimeInput::Ui(UiCommand::Shutdown));
        let step = match input {
            RuntimeInput::Ui(command) => runtime.on_ui_command(command),
            RuntimeInput::Agent(member, event) => runtime.on_agent_event(&member, event),
        };

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

        if shutdown {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::FakeRunner;
    use crate::domain::event::MessageTarget;
    use crate::domain::team::{BackendKind, TeamMember};
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
}
