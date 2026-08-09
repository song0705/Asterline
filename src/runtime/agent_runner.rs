//! Transport: dispatch a member run on a worker thread and forward its
//! [`AgentEvent`]s back to the runtime loop, tagged with the member.

use std::sync::Arc;
use std::sync::mpsc::{self, SyncSender};
use std::thread;

use crate::adapter::{MemberRunner, RunRequest};
use crate::domain::event::AgentEvent;
use crate::runtime::RuntimeInput;
use crate::runtime::team_runtime::RunAction;

const AGENT_EVENT_QUEUE_CAPACITY: usize = 256;

/// Start `action` on a detached worker thread. The runner streams `AgentEvent`s
/// into a per-run channel; a forwarder relays them to the runtime loop as
/// [`RuntimeInput::Agent`].
pub fn dispatch(
    runner: Arc<dyn MemberRunner>,
    action: RunAction,
    input_tx: SyncSender<RuntimeInput>,
) -> thread::JoinHandle<()> {
    let RunAction {
        member,
        prompt,
        session,
        cancel,
        effort,
    } = action;

    thread::spawn(move || {
        // Bound the per-run queue so a backend that floods many individually
        // valid small events is backpressured through to its stdout pipe.
        let (ev_tx, ev_rx) = mpsc::sync_channel::<AgentEvent>(AGENT_EVENT_QUEUE_CAPACITY);
        let forward_member = member.clone();
        let forward_cancel = Arc::clone(&cancel);
        let forwarder = thread::spawn(move || {
            let mut saw_exit = false;
            let mut saw_fatal = false;
            while let Ok(event) = ev_rx.recv() {
                saw_exit |= matches!(event, AgentEvent::Exited { .. });
                saw_fatal |= matches!(event, AgentEvent::Fatal(_));
                if input_tx
                    .send(RuntimeInput::Agent(forward_member.clone(), event))
                    .is_err()
                {
                    return;
                }
            }
            if !saw_exit {
                if !saw_fatal && !forward_cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    let _ = input_tx.send(RuntimeInput::Agent(
                        forward_member.clone(),
                        AgentEvent::Fatal(
                            "backend runner stopped without an exit event".to_string(),
                        ),
                    ));
                }
                let _ = input_tx.send(RuntimeInput::Agent(
                    forward_member,
                    AgentEvent::Exited {
                        code: None,
                        ok: false,
                    },
                ));
            }
        });

        runner.run(
            RunRequest {
                prompt,
                session,
                cancel,
                effort,
            },
            ev_tx,
        );
        let _ = forwarder.join();
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    use crate::domain::team::BackendKind;

    struct SilentRunner;

    impl MemberRunner for SilentRunner {
        fn backend(&self) -> BackendKind {
            BackendKind::Codex
        }

        fn run(&self, _req: RunRequest, _events: SyncSender<AgentEvent>) {}
    }

    #[test]
    fn runner_return_without_exit_is_closed_as_failure() {
        let (input_tx, input_rx) = mpsc::sync_channel(8);
        let worker = dispatch(
            Arc::new(SilentRunner),
            RunAction {
                member: "builder".into(),
                prompt: "test".to_string(),
                session: None,
                cancel: Arc::new(AtomicBool::new(false)),
                effort: None,
            },
            input_tx,
        );

        let first = input_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(matches!(
            first,
            RuntimeInput::Agent(_, AgentEvent::Fatal(message))
                if message.contains("without an exit event")
        ));
        let second = input_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(matches!(
            second,
            RuntimeInput::Agent(_, AgentEvent::Exited { ok: false, .. })
        ));
        worker.join().unwrap();
    }
}
