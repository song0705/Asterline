//! Transport: dispatch a member run on a worker thread and forward its
//! [`AgentEvent`]s back to the runtime loop, tagged with the member.

use std::sync::Arc;
use std::sync::mpsc::{self, Sender};
use std::thread;

use crate::adapter::{MemberRunner, RunRequest};
use crate::domain::event::AgentEvent;
use crate::runtime::RuntimeInput;
use crate::runtime::team_runtime::RunAction;

/// Start `action` on a detached worker thread. The runner streams `AgentEvent`s
/// into a per-run channel; a forwarder relays them to the runtime loop as
/// [`RuntimeInput::Agent`].
pub fn dispatch(runner: Arc<dyn MemberRunner>, action: RunAction, input_tx: Sender<RuntimeInput>) {
    let RunAction {
        member,
        prompt,
        session,
        cancel,
    } = action;

    thread::spawn(move || {
        let (ev_tx, ev_rx) = mpsc::channel::<AgentEvent>();
        let forward_member = member.clone();
        let forwarder = thread::spawn(move || {
            while let Ok(event) = ev_rx.recv() {
                if input_tx
                    .send(RuntimeInput::Agent(forward_member.clone(), event))
                    .is_err()
                {
                    break;
                }
            }
        });

        runner.run(
            RunRequest {
                prompt,
                session,
                cancel,
            },
            ev_tx,
        );
        let _ = forwarder.join();
    });
}
