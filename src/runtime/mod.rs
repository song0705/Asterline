//! Team runtime: turns `UiCommand`s into `RuntimeEvent`s, orchestrating per-member
//! runs, routing, approvals, the relay guard, and persistence.
//!
//! The core ([`team_runtime`]) is pure and synchronous. This module adds the
//! transport: a priority UI-control channel, a bounded worker-event channel,
//! a background loop, and the worker threads that drive member runs.

pub mod agent_runner;
pub mod approval;
pub mod mode_prompts;
pub mod session_registry;
pub mod team_runtime;

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsString;
use std::io;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{
    self, Receiver, RecvTimeoutError, Sender, SyncSender, TryRecvError, TrySendError,
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::adapter::process::{ChildProcessTree, configure_process_tree};
use crate::adapter::{FakeRunner, MemberRunner, runner_for};
use crate::domain::event::{AgentEvent, ImportedMessage, LogLevel, RunId, RuntimeEvent, UiCommand};
use crate::domain::mode::TerminalMode;
use crate::domain::team::{MemberId, TeamConfig, TeamMember};
use crate::store::sqlite::SqliteStore;

pub use team_runtime::{
    RunAction, RunnerChange, RunnerControl, RuntimeStep, TeamRuntime, VerifyAction, VerifyOutput,
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
    tx: SyncSender<UiCommand>,
    control_tx: SyncSender<UiCommand>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeCommandSend {
    Sent,
    Full,
    Disconnected,
}

impl RuntimeHandle {
    /// Attempt to enqueue a command without blocking the caller. Cancellation
    /// and shutdown use an independent low-volume control lane, so renderer
    /// backpressure cannot starve them.
    pub fn try_send(&self, command: UiCommand) -> RuntimeCommandSend {
        if matches!(
            &command,
            UiCommand::Cancel { .. }
                | UiCommand::AttachFinished { .. }
                | UiCommand::Approve { .. }
                | UiCommand::Shutdown
        ) {
            match self.control_tx.try_send(command) {
                Ok(()) => RuntimeCommandSend::Sent,
                Err(TrySendError::Full(_)) => RuntimeCommandSend::Full,
                Err(TrySendError::Disconnected(_)) => RuntimeCommandSend::Disconnected,
            }
        } else {
            match self.tx.try_send(command) {
                Ok(()) => RuntimeCommandSend::Sent,
                Err(TrySendError::Full(_)) => RuntimeCommandSend::Full,
                Err(TrySendError::Disconnected(_)) => RuntimeCommandSend::Disconnected,
            }
        }
    }

    /// Backwards-compatible reliable enqueue. Product UI code uses
    /// [`Self::try_send`] so a full queue is visible without blocking; legacy
    /// callers retain the original contract that `false` means disconnected
    /// and a live, temporarily full queue waits rather than dropping work.
    pub fn send(&self, command: UiCommand) -> bool {
        if matches!(
            &command,
            UiCommand::Cancel { .. }
                | UiCommand::AttachFinished { .. }
                | UiCommand::Approve { .. }
                | UiCommand::Shutdown
        ) {
            self.control_tx.send(command).is_ok()
        } else {
            self.tx.send(command).is_ok()
        }
    }

    /// Reliably release the runtime-side attach reservation. This is used only
    /// while the TUI has stopped consuming ordinary work, so waiting briefly
    /// for the small control lane is preferable to leaking a reservation when
    /// that lane is momentarily full.
    pub fn finish_attach(&self, member: MemberId, items: Vec<ImportedMessage>) -> bool {
        self.finish_attach_with_session(member, None, items)
    }

    /// Finish an attach while atomically carrying a session identity recovered
    /// from the native transcript. The runtime accepts it only for the member
    /// that currently owns the attach reservation.
    pub fn finish_attach_with_session(
        &self,
        member: MemberId,
        session: Option<crate::domain::event::AgentSessionId>,
        items: Vec<ImportedMessage>,
    ) -> bool {
        self.control_tx
            .send(UiCommand::AttachFinished {
                member,
                session,
                items,
            })
            .is_ok()
    }

    /// Reliably enqueue the terminal shutdown barrier. Cleanup paths must not
    /// mistake a momentarily full control lane for a stopped runtime.
    pub fn shutdown(&self) -> bool {
        self.control_tx.send(UiCommand::Shutdown).is_ok()
    }
}

/// Per-member runners (real CLI or fake), keyed by member id.
pub type Runners = HashMap<MemberId, Arc<dyn MemberRunner>>;

struct RuntimeLoopOptions {
    approvals: bool,
    fake: bool,
    team_save_path: Option<PathBuf>,
}

struct ActiveVerification {
    cancel: Arc<AtomicBool>,
    command: String,
    worker: JoinHandle<()>,
}

static TEAM_SAVE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const VERIFY_OUTPUT_LIMIT: usize = 1024 * 1024;
const RUNTIME_INPUT_QUEUE_CAPACITY: usize = 256;
const RUNTIME_UI_QUEUE_CAPACITY: usize = 256;
const RUNTIME_CONTROL_QUEUE_CAPACITY: usize = 32;
const RUNTIME_INPUT_POLL_INTERVAL: Duration = Duration::from_millis(10);

enum RuntimeEventSender {
    Unbounded(Sender<RuntimeEvent>),
    Bounded(SyncSender<RuntimeEvent>),
}

enum RuntimeEventSend {
    Sent,
    Full(Box<RuntimeEvent>),
    Disconnected,
}

impl RuntimeEventSender {
    fn try_send(&self, event: RuntimeEvent) -> RuntimeEventSend {
        match self {
            Self::Unbounded(sender) => match sender.send(event) {
                Ok(()) => RuntimeEventSend::Sent,
                Err(_) => RuntimeEventSend::Disconnected,
            },
            Self::Bounded(sender) => match sender.try_send(event) {
                Ok(()) => RuntimeEventSend::Sent,
                Err(TrySendError::Full(event)) => RuntimeEventSend::Full(Box::new(event)),
                Err(TrySendError::Disconnected(_)) => RuntimeEventSend::Disconnected,
            },
        }
    }
}

/// Advisory stream-only events can be omitted while the renderer is behind.
/// Message/tool deltas are superseded by canonical completion events, and
/// debug and info logs normally have a durable copy. Reasoning snapshots are
/// retained separately and coalesced per member so the latest live thought is
/// eventually visible without allowing an unbounded stream to build up.
fn is_transient_runtime_event(event: &RuntimeEvent) -> bool {
    matches!(
        event,
        RuntimeEvent::MessageDelta { .. }
            | RuntimeEvent::ToolProgress { .. }
            | RuntimeEvent::Log(crate::domain::event::LogEntry {
                level: LogLevel::Debug | LogLevel::Info,
                ..
            })
    )
}

fn queue_runtime_event(pending: &mut VecDeque<RuntimeEvent>, event: RuntimeEvent) {
    if let RuntimeEvent::Reasoning { member, text } = &event
        && let Some(RuntimeEvent::Reasoning {
            text: queued_text, ..
        }) = pending.iter_mut().rev().find(|queued| {
            matches!(queued, RuntimeEvent::Reasoning { member: queued_member, .. } if queued_member == member)
        })
    {
        *queued_text = text.clone();
        return;
    }
    pending.push_back(event);
}

fn flush_runtime_events(sender: &RuntimeEventSender, pending: &mut VecDeque<RuntimeEvent>) -> bool {
    while let Some(event) = pending.pop_front() {
        match sender.try_send(event) {
            RuntimeEventSend::Sent => {}
            RuntimeEventSend::Full(event) => {
                pending.push_front(*event);
                return true;
            }
            RuntimeEventSend::Disconnected => return false,
        }
    }
    true
}

fn enqueue_runtime_events(
    sender: &RuntimeEventSender,
    pending: &mut VecDeque<RuntimeEvent>,
    events: impl IntoIterator<Item = RuntimeEvent>,
) -> bool {
    if !flush_runtime_events(sender, pending) {
        return false;
    }
    for event in events {
        if !pending.is_empty() {
            if !is_transient_runtime_event(&event) {
                queue_runtime_event(pending, event);
            }
            continue;
        }
        match sender.try_send(event) {
            RuntimeEventSend::Sent => {}
            RuntimeEventSend::Full(event) => {
                let event = *event;
                if !is_transient_runtime_event(&event) {
                    queue_runtime_event(pending, event);
                }
            }
            RuntimeEventSend::Disconnected => return false,
        }
    }
    true
}

struct RuntimeChannels {
    events: RuntimeEventSender,
    worker_tx: SyncSender<RuntimeInput>,
    worker_rx: Receiver<RuntimeInput>,
    ui_rx: Receiver<UiCommand>,
    control_rx: Receiver<UiCommand>,
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
    spawn_inner(
        config,
        store,
        runners,
        RuntimeEventSender::Unbounded(events),
        approvals,
        fake,
        team_save_path,
    )
}

/// Spawn the runtime with a bounded event sink. This is the preferred product
/// path: canonical state events wait in order while redundant stream-only
/// updates are shed under renderer backpressure.
pub fn spawn_bounded(
    config: TeamConfig,
    store: SqliteStore,
    runners: Runners,
    events: SyncSender<RuntimeEvent>,
    approvals: bool,
    fake: bool,
    team_save_path: Option<PathBuf>,
) -> (RuntimeHandle, JoinHandle<()>) {
    spawn_inner(
        config,
        store,
        runners,
        RuntimeEventSender::Bounded(events),
        approvals,
        fake,
        team_save_path,
    )
}

fn spawn_inner(
    config: TeamConfig,
    store: SqliteStore,
    runners: Runners,
    events: RuntimeEventSender,
    approvals: bool,
    fake: bool,
    team_save_path: Option<PathBuf>,
) -> (RuntimeHandle, JoinHandle<()>) {
    // UI commands are low-volume control traffic and must remain enqueueable
    // while workers are backpressured. In particular, cancellation and Shutdown
    // cannot share a full token/event queue with the work they must cancel.
    let (ui_tx, ui_rx) = mpsc::sync_channel(RUNTIME_UI_QUEUE_CAPACITY);
    let (control_tx, control_rx) = mpsc::sync_channel(RUNTIME_CONTROL_QUEUE_CAPACITY);
    let (input_tx, input_rx) = mpsc::sync_channel(RUNTIME_INPUT_QUEUE_CAPACITY);
    let handle = RuntimeHandle {
        tx: ui_tx,
        control_tx,
    };
    let join = thread::spawn(move || {
        let options = RuntimeLoopOptions {
            approvals,
            fake,
            team_save_path,
        };
        let channels = RuntimeChannels {
            events,
            worker_tx: input_tx,
            worker_rx: input_rx,
            ui_rx,
            control_rx,
        };
        run_loop(config, store, runners, options, channels);
    });
    (handle, join)
}

fn run_loop(
    config: TeamConfig,
    store: SqliteStore,
    mut runners: Runners,
    options: RuntimeLoopOptions,
    channels: RuntimeChannels,
) {
    let RuntimeChannels {
        events,
        worker_tx: input_tx,
        worker_rx: input_rx,
        ui_rx,
        control_rx,
    } = channels;
    let mut cancel_member_ids: HashSet<MemberId> = config.all_member_ids().into_iter().collect();
    let mut runtime = TeamRuntime::new(config, store).with_approvals(options.approvals);
    let mut active_verifications: HashMap<RunId, ActiveVerification> = HashMap::new();
    let mut agent_workers: Vec<JoinHandle<()>> = Vec::new();
    let mut pending_events = VecDeque::new();
    let mut startup_events = vec![runtime.ready_event()];
    if runtime.active_mode() != TerminalMode::Normal {
        startup_events.push(RuntimeEvent::ModeChanged {
            mode: runtime.active_mode(),
        });
    }
    startup_events.extend(runtime.take_startup_events());
    if !enqueue_runtime_events(&events, &mut pending_events, startup_events) {
        let _ = prepare_shutdown(&mut runtime, &mut active_verifications, &mut agent_workers);
        return;
    }

    let mut deferred_inputs = VecDeque::new();
    let mut attach_in_progress: Option<MemberId> = None;
    let mut globally_cancelled_during_backpressure = false;
    let mut cancelled_members_during_backpressure = HashSet::new();
    let mut stray_attach_finished_noticed_during_backpressure = false;
    loop {
        if !flush_runtime_events(&events, &mut pending_events) {
            let _ = prepare_shutdown(&mut runtime, &mut active_verifications, &mut agent_workers);
            join_workers_bounded(&mut agent_workers, Duration::from_secs(5));
            return;
        }
        if pending_events.is_empty() {
            globally_cancelled_during_backpressure = false;
            cancelled_members_during_backpressure.clear();
            stray_attach_finished_noticed_during_backpressure = false;
        }
        // Give cancellation/shutdown/user commands priority over worker
        // deltas. Polling the bounded worker queue with a short timeout avoids
        // needing a crossbeam-style select while keeping UI latency low.
        let input = match control_rx.try_recv() {
            Ok(command) => RuntimeInput::Ui(command),
            Err(TryRecvError::Disconnected) => RuntimeInput::Ui(UiCommand::Shutdown),
            Err(TryRecvError::Empty)
                if attach_in_progress.is_some() || !pending_events.is_empty() =>
            {
                // Do not keep turning worker events into more canonical output
                // or consume ordinary UI work while the sink is full or a
                // native interactive attach owns the session. Their channels
                // now own backpressure; only control traffic may bypass it.
                match control_rx.recv_timeout(RUNTIME_INPUT_POLL_INTERVAL) {
                    Ok(command) => RuntimeInput::Ui(command),
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => RuntimeInput::Ui(UiCommand::Shutdown),
                }
            }
            Err(TryRecvError::Empty) => match ui_rx.try_recv() {
                Ok(command) => RuntimeInput::Ui(command),
                Err(TryRecvError::Disconnected) => RuntimeInput::Ui(UiCommand::Shutdown),
                Err(TryRecvError::Empty) => match deferred_inputs.pop_front() {
                    Some(input) => input,
                    None => match input_rx.recv_timeout(RUNTIME_INPUT_POLL_INTERVAL) {
                        Ok(input) => input,
                        Err(RecvTimeoutError::Timeout) => continue,
                        Err(RecvTimeoutError::Disconnected) => break,
                    },
                },
            },
        };
        if !pending_events.is_empty()
            && let RuntimeInput::Ui(UiCommand::Cancel { member }) = &input
        {
            match member {
                None if globally_cancelled_during_backpressure => continue,
                None => {
                    globally_cancelled_during_backpressure = true;
                    cancelled_members_during_backpressure.clear();
                }
                Some(_) if globally_cancelled_during_backpressure => continue,
                Some(member)
                    if cancel_member_ids.contains(member)
                        && !cancelled_members_during_backpressure.insert(member.clone()) =>
                {
                    continue;
                }
                Some(_) => {}
            }
        }
        if !pending_events.is_empty()
            && stray_attach_finished_noticed_during_backpressure
            && let RuntimeInput::Ui(UiCommand::AttachFinished { member, .. }) = &input
            && attach_in_progress.as_ref() != Some(member)
        {
            // A full renderer must not let a producer grow `pending_events`
            // without bound by continuously refilling the bounded control
            // lane with stray completions. Keep the first diagnostic for the
            // current backpressure episode and discard equivalent noise after
            // that; the correctly reserved owner is never suppressed.
            continue;
        }
        let shutdown = matches!(&input, RuntimeInput::Ui(UiCommand::Shutdown));
        let global_cancel = matches!(&input, RuntimeInput::Ui(UiCommand::Cancel { member: None }));
        if global_cancel {
            let mut discarded = 0_usize;
            while let Ok(command) = ui_rx.try_recv() {
                discarded = discarded.saturating_add(1);
                if let UiCommand::RequestAttach { member } = command {
                    pending_events.push_back(RuntimeEvent::AttachDenied {
                        member,
                        reason: "attach request was cancelled before it started".to_string(),
                    });
                }
            }
            if discarded > 0 {
                pending_events.push_back(RuntimeEvent::Notice(format!(
                    "cancelled {discarded} queued command(s) that had not started"
                )));
            }
        }
        let cancel_verifications = match &input {
            RuntimeInput::Ui(UiCommand::Cancel { member }) => member.is_none(),
            RuntimeInput::Ui(UiCommand::Shutdown) => true,
            _ => false,
        };
        let mut release_attach_after_step = false;
        let mut step = match input {
            RuntimeInput::Ui(UiCommand::NewSession) if !active_verifications.is_empty() => {
                RuntimeStep {
                    events: vec![RuntimeEvent::Notice(
                        "cannot start a new chat while verification is active; press Esc to cancel it first"
                            .to_string(),
                    )],
                    ..RuntimeStep::default()
                }
            }
            RuntimeInput::Ui(UiCommand::ResumeConversation { .. })
                if !active_verifications.is_empty() =>
            {
                RuntimeStep {
                    events: vec![RuntimeEvent::Notice(
                        "cannot resume another chat while verification is active; press Esc to cancel it first"
                            .to_string(),
                    )],
                    ..RuntimeStep::default()
                }
            }
            RuntimeInput::Ui(UiCommand::RequestAttach { member }) => {
                reap_finished_workers(&mut agent_workers);
                if !active_verifications.is_empty()
                    || agent_workers.iter().any(|worker| !worker.is_finished())
                {
                    RuntimeStep {
                        events: vec![RuntimeEvent::AttachDenied {
                            member,
                            reason: "cannot attach while a runtime worker or verification is active; press Esc to cancel it first"
                                .to_string(),
                        }],
                        ..RuntimeStep::default()
                    }
                } else {
                    runtime.on_ui_command(UiCommand::RequestAttach { member })
                }
            }
            RuntimeInput::Ui(UiCommand::ImportTranscript { .. }) => RuntimeStep {
                events: vec![RuntimeEvent::Notice(
                    "ignored direct transcript import without an attach reservation".to_string(),
                )],
                ..RuntimeStep::default()
            },
            RuntimeInput::Ui(UiCommand::AttachFinished {
                member,
                session,
                items,
            }) => {
                match attach_in_progress.as_ref() {
                    Some(reserved) if reserved == &member => {
                        // Execute transcript persistence and build its events
                        // while ordinary/worker intake is still paused. The
                        // reservation is released only after those events are
                        // enqueued below.
                        release_attach_after_step = true;
                        runtime.import_attached_transcript(member, session, items)
                    }
                    Some(reserved) => {
                        stray_attach_finished_noticed_during_backpressure = true;
                        RuntimeStep {
                            events: vec![RuntimeEvent::Notice(format!(
                                "ignored attach completion for {member}; {reserved} owns the reservation"
                            ))],
                            ..RuntimeStep::default()
                        }
                    }
                    None => {
                        stray_attach_finished_noticed_during_backpressure = true;
                        RuntimeStep {
                            events: vec![RuntimeEvent::Notice(format!(
                                "ignored attach completion for {member}; no attach is reserved"
                            ))],
                            ..RuntimeStep::default()
                        }
                    }
                }
            }
            RuntimeInput::Ui(UiCommand::Shutdown) => {
                prepare_shutdown(&mut runtime, &mut active_verifications, &mut agent_workers)
            }
            RuntimeInput::Ui(command) => runtime.on_ui_command(command),
            RuntimeInput::Agent(member, event) => runtime.on_agent_event(&member, event),
            RuntimeInput::Verification(output) => {
                if let Some(active) = active_verifications.remove(&output.run_id) {
                    let _ = active.worker.join();
                }
                runtime.on_verify_output(output)
            }
        };

        if cancel_verifications {
            for active in active_verifications.values() {
                active.cancel.store(true, Ordering::Relaxed);
            }
        }
        if let Some(config) = step.persist_team.take()
            && let Some(path) = &options.team_save_path
            && let Err(err) = save_team_config(path, &config)
        {
            // TeamRuntime has already committed its matching SQLite state.
            // Do not run with a different on-disk boot config: withhold runner
            // changes/actions, cancel owned workers, and stop this runtime.
            let cleanup =
                prepare_shutdown(&mut runtime, &mut active_verifications, &mut agent_workers);
            step.events.clear();
            step.events.push(RuntimeEvent::Notice(format!(
                "could not save team config {}; runtime stopped to avoid inconsistent state: {err}",
                path.display()
            )));
            step.events.extend(cleanup.events);
            let _ = enqueue_runtime_events(&events, &mut pending_events, step.events);
            join_workers_bounded(&mut agent_workers, Duration::from_secs(5));
            return;
        }

        for change in step.runner_changes {
            match change {
                RunnerChange::Upsert { member, workspace } => {
                    cancel_member_ids.insert(member.id.clone());
                    runners.insert(
                        member.id.clone(),
                        build_runner(&member, &workspace, options.fake),
                    );
                }
                RunnerChange::Remove(member) => {
                    cancel_member_ids.remove(&member);
                    runners.remove(&member);
                }
            }
        }

        for control in step.runner_controls.drain(..) {
            match control {
                RunnerControl::ResolveNativeApproval {
                    member,
                    request_id,
                    decision,
                } => {
                    let delivered = runners
                        .get(&member)
                        .is_some_and(|runner| runner.resolve_native_approval(request_id, decision));
                    if !delivered {
                        step.events.push(RuntimeEvent::Notice(format!(
                            "could not deliver approval {} to {member}; its native session ended",
                            decision.as_str()
                        )));
                    }
                }
            }
        }

        if let Some(member) = step.events.iter().find_map(|event| match event {
            RuntimeEvent::AttachGranted { member } => Some(member.clone()),
            _ => None,
        }) {
            attach_in_progress = Some(member);
        }

        if !enqueue_runtime_events(&events, &mut pending_events, step.events) {
            let _ = prepare_shutdown(&mut runtime, &mut active_verifications, &mut agent_workers);
            join_workers_bounded(&mut agent_workers, Duration::from_secs(5));
            return;
        }
        if release_attach_after_step {
            attach_in_progress = None;
        }
        for action in step.actions {
            if let Some(runner) = runners.get(&action.member) {
                agent_workers.push(agent_runner::dispatch(
                    Arc::clone(runner),
                    action,
                    input_tx.clone(),
                ));
            } else {
                let member = action.member;
                // Never send synthetic events into our own bounded channel:
                // another worker could fill it first and deadlock the sole
                // consumer. Process these terminal events on the next loop.
                deferred_inputs.push_back(RuntimeInput::Agent(
                    member.clone(),
                    AgentEvent::Fatal(format!("no backend runner is configured for {member}")),
                ));
                deferred_inputs.push_back(RuntimeInput::Agent(
                    member,
                    AgentEvent::Exited {
                        code: None,
                        ok: false,
                    },
                ));
            }
        }
        for action in step.verify_actions {
            let run_id = action.run_id;
            let cancel = Arc::clone(&action.cancel);
            let command = action.command.clone();
            let worker = dispatch_verification(action, input_tx.clone());
            active_verifications.insert(
                run_id,
                ActiveVerification {
                    cancel,
                    command,
                    worker,
                },
            );
        }

        if shutdown {
            join_workers_bounded(&mut agent_workers, Duration::from_secs(5));
            break;
        }
        reap_finished_workers(&mut agent_workers);
    }
}

fn prepare_shutdown(
    runtime: &mut TeamRuntime,
    active_verifications: &mut HashMap<RunId, ActiveVerification>,
    agent_workers: &mut Vec<JoinHandle<()>>,
) -> RuntimeStep {
    let mut step = runtime.on_ui_command(UiCommand::Shutdown);
    // The worker's channel result will not be handled after the loop exits.
    // Persist cancellation synchronously so a plain run cannot remain in
    // Verifying forever across restart, then retain the handle for bounded
    // cleanup below.
    for active in active_verifications.values() {
        active.cancel.store(true, Ordering::Relaxed);
    }
    for (run_id, active) in active_verifications.drain() {
        let cancelled = runtime.on_verify_output(VerifyOutput {
            run_id,
            command: active.command,
            ok: false,
            stdout: Vec::new(),
            stderr: Vec::new(),
            start_error: None,
            cancelled: true,
        });
        merge_runtime_step(&mut step, cancelled);
        agent_workers.push(active.worker);
    }
    step
}

fn merge_runtime_step(target: &mut RuntimeStep, mut source: RuntimeStep) {
    target.events.append(&mut source.events);
    target.actions.append(&mut source.actions);
    target.verify_actions.append(&mut source.verify_actions);
    target.runner_changes.append(&mut source.runner_changes);
    target.runner_controls.append(&mut source.runner_controls);
    if source.persist_team.is_some() {
        target.persist_team = source.persist_team;
    }
}

fn dispatch_verification(
    action: VerifyAction,
    input_tx: SyncSender<RuntimeInput>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let output = run_verification(action);
        let _ = input_tx.send(RuntimeInput::Verification(output));
    })
}

fn reap_finished_workers(workers: &mut Vec<JoinHandle<()>>) {
    let mut pending = Vec::with_capacity(workers.len());
    for worker in workers.drain(..) {
        if worker.is_finished() {
            let _ = worker.join();
        } else {
            pending.push(worker);
        }
    }
    *workers = pending;
}

fn join_workers_bounded(workers: &mut Vec<JoinHandle<()>>, timeout: Duration) {
    let deadline = std::time::Instant::now() + timeout;
    while !workers.is_empty() && std::time::Instant::now() < deadline {
        reap_finished_workers(workers);
        if !workers.is_empty() {
            thread::sleep(Duration::from_millis(10));
        }
    }
    reap_finished_workers(workers);
}

fn run_verification(action: VerifyAction) -> VerifyOutput {
    let VerifyAction {
        run_id,
        command,
        workspace,
        cancel,
    } = action;

    let mut builder = verification_command(&command);
    builder
        .current_dir(&workspace)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());
    configure_process_tree(&mut builder);
    let mut child = match builder.spawn() {
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
    let process_tree = match ChildProcessTree::attach(&mut child) {
        Ok(tree) => tree,
        Err(err) => {
            let _ = child.kill();
            let _ = child.wait();
            return VerifyOutput {
                run_id,
                command,
                ok: false,
                stdout: Vec::new(),
                stderr: Vec::new(),
                start_error: Some(format!("could not own verification process tree: {err}")),
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
            let _ = process_tree.terminate_with_fallback(&mut child);
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

fn verification_command(command: &str) -> Command {
    #[cfg(windows)]
    {
        let shell = std::env::var_os("COMSPEC").unwrap_or_else(|| OsString::from("cmd.exe"));
        let mut builder = Command::new(shell);
        builder.args(["/D", "/S", "/C", command]);
        builder
    }
    #[cfg(not(windows))]
    {
        let mut builder = Command::new("sh");
        builder.args(["-lc", command]);
        builder
    }
}

fn read_pipe<R: Read + Send + 'static>(mut pipe: R) -> JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        const MARKER_RESERVE: usize = 128;
        let data_limit = VERIFY_OUTPUT_LIMIT - MARKER_RESERVE;
        let head_limit = data_limit / 2;
        let tail_limit = data_limit - head_limit;
        let mut head = Vec::with_capacity(head_limit);
        let mut tail = VecDeque::with_capacity(tail_limit);
        let mut total = 0usize;
        let mut buffer = [0u8; 8192];
        loop {
            let read = match pipe.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => read,
                Err(_) => break,
            };
            total = total.saturating_add(read);
            let mut offset = 0;
            if head.len() < head_limit {
                let take = (head_limit - head.len()).min(read);
                head.extend_from_slice(&buffer[..take]);
                offset = take;
            }
            for byte in &buffer[offset..read] {
                if tail.len() == tail_limit {
                    tail.pop_front();
                }
                tail.push_back(*byte);
            }
        }

        let truncated = total > head.len() + tail.len();
        let mut bytes = Vec::with_capacity(VERIFY_OUTPUT_LIMIT);
        bytes.extend_from_slice(&head);
        if truncated {
            bytes.extend_from_slice(
                format!("\n...[verification output truncated: {total} bytes total]...\n")
                    .as_bytes(),
            );
        }
        bytes.extend(tail);
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

pub(crate) fn save_team_config(path: &Path, config: &TeamConfig) -> io::Result<()> {
    let json = serde_json::to_string_pretty(config).map_err(io::Error::other)?;
    save_team_config_with_replace(path, json.as_bytes(), atomic_replace)
}

fn save_team_config_with_replace(
    path: &Path,
    contents: &[u8],
    replace: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    let parent = team_config_parent(path);
    std::fs::create_dir_all(parent)?;

    let file_name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "team config path must name a file",
        )
    })?;

    let mut created_temp = None;
    for _ in 0..100 {
        let mut temp_name = OsString::from(".");
        temp_name.push(file_name);
        temp_name.push(format!(
            ".asterline-save-{}-{}.tmp",
            std::process::id(),
            TEAM_SAVE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let temp_path = parent.join(temp_name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
        {
            Ok(temp) => {
                created_temp = Some((temp_path, temp));
                break;
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    let (temp_path, mut temp) = created_temp.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique temporary team config",
        )
    })?;

    let result = (|| {
        temp.write_all(contents)?;
        temp.sync_all()?;
        if let Ok(metadata) = std::fs::metadata(path) {
            std::fs::set_permissions(&temp_path, metadata.permissions())?;
        }
        drop(temp);
        replace(&temp_path, path)?;
        // The replacement already happened; a directory-sync failure cannot
        // be rolled back and must not be reported as if the old file remained.
        let _ = sync_parent_directory(parent);
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    result
}

fn team_config_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

#[cfg(not(windows))]
fn atomic_replace(source: &Path, destination: &Path) -> io::Result<()> {
    std::fs::rename(source, destination)
}

#[cfg(windows)]
fn atomic_replace(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(
            existing_file_name: *const u16,
            new_file_name: *const u16,
            flags: u32,
        ) -> i32;
    }

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    // SAFETY: both buffers are NUL-terminated and remain alive for the call.
    if unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn sync_parent_directory(parent: &Path) -> io::Result<()> {
    std::fs::File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent_directory(_parent: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::FakeRunner;
    use crate::domain::event::RunStatus;
    use crate::domain::event::{AgentSessionId, MessageTarget};
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
    fn runtime_handle_reports_full_queues_without_misreporting_disconnect() {
        let (ui_tx, _ui_rx) = mpsc::sync_channel(1);
        let (control_tx, _control_rx) = mpsc::sync_channel(1);
        let handle = RuntimeHandle {
            tx: ui_tx,
            control_tx,
        };
        let user = || UiCommand::UserMessage {
            target: MessageTarget::Default,
            body: "queued".to_string(),
        };

        assert_eq!(handle.try_send(user()), RuntimeCommandSend::Sent);
        assert_eq!(handle.try_send(user()), RuntimeCommandSend::Full);
        assert_eq!(
            handle.try_send(UiCommand::Cancel { member: None }),
            RuntimeCommandSend::Sent
        );
        assert_eq!(
            handle.try_send(UiCommand::Cancel { member: None }),
            RuntimeCommandSend::Full
        );
    }

    #[test]
    fn legacy_send_waits_for_capacity_and_delivers_instead_of_dropping() {
        let (ui_tx, ui_rx) = mpsc::sync_channel(1);
        let (control_tx, _control_rx) = mpsc::sync_channel(1);
        let handle = RuntimeHandle {
            tx: ui_tx,
            control_tx,
        };
        let user = |body: &str| UiCommand::UserMessage {
            target: MessageTarget::Default,
            body: body.to_string(),
        };

        assert!(handle.send(user("first")));
        let blocked_handle = handle.clone();
        let blocked = thread::spawn(move || blocked_handle.send(user("second")));
        thread::sleep(Duration::from_millis(25));
        assert!(
            !blocked.is_finished(),
            "legacy send must wait when its live queue is temporarily full"
        );
        assert!(matches!(
            ui_rx.recv_timeout(Duration::from_secs(1)),
            Ok(UiCommand::UserMessage { body, .. }) if body == "first"
        ));
        assert!(blocked.join().unwrap());
        assert!(matches!(
            ui_rx.recv_timeout(Duration::from_secs(1)),
            Ok(UiCommand::UserMessage { body, .. }) if body == "second"
        ));
    }

    #[test]
    fn reliable_shutdown_waits_for_a_full_control_lane_then_runtime_exits() {
        let (evt_tx, _evt_rx) = mpsc::channel();
        let (ui_tx, ui_rx) = mpsc::sync_channel(1);
        let (control_tx, control_rx) = mpsc::sync_channel(1);
        let (worker_tx, worker_rx) = mpsc::sync_channel(1);
        let handle = RuntimeHandle {
            tx: ui_tx,
            control_tx: control_tx.clone(),
        };
        control_tx
            .send(UiCommand::Cancel {
                member: Some(MemberId::new("builder")),
            })
            .unwrap();

        let shutdown_handle = handle.clone();
        let shutdown = thread::spawn(move || shutdown_handle.shutdown());
        thread::sleep(Duration::from_millis(25));
        assert!(
            !shutdown.is_finished(),
            "reliable shutdown should wait rather than drop a full control command"
        );

        let join = thread::spawn(move || {
            run_loop(
                single_codex_team(),
                SqliteStore::in_memory().unwrap(),
                HashMap::new(),
                RuntimeLoopOptions {
                    approvals: true,
                    fake: false,
                    team_save_path: None,
                },
                RuntimeChannels {
                    events: RuntimeEventSender::Unbounded(evt_tx),
                    worker_tx,
                    worker_rx,
                    ui_rx,
                    control_rx,
                },
            );
        });
        assert!(shutdown.join().unwrap());

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !join.is_finished() && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(
            join.is_finished(),
            "runtime must consume the saturated control lane and then shut down"
        );
        join.join().unwrap();
    }

    #[test]
    fn shutdown_bypasses_an_active_attach_reservation() {
        let (evt_tx, evt_rx) = mpsc::channel();
        let (handle, join) = spawn(
            single_codex_team(),
            SqliteStore::in_memory().unwrap(),
            HashMap::new(),
            evt_tx,
            true,
            false,
            None,
        );
        assert!(matches!(
            evt_rx.recv_timeout(Duration::from_secs(2)),
            Ok(RuntimeEvent::Ready { .. })
        ));
        assert_eq!(
            handle.try_send(UiCommand::RequestAttach {
                member: MemberId::new("builder"),
            }),
            RuntimeCommandSend::Sent
        );
        assert!(matches!(
            evt_rx.recv_timeout(Duration::from_secs(2)),
            Ok(RuntimeEvent::AttachGranted { .. })
        ));

        assert!(handle.shutdown());
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !join.is_finished() && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(
            join.is_finished(),
            "Shutdown is control traffic and must bypass attach pause"
        );
        join.join().unwrap();
    }

    #[test]
    fn warning_and_error_logs_are_retained_during_backpressure() {
        for level in [LogLevel::Warn, LogLevel::Error] {
            assert!(!is_transient_runtime_event(&RuntimeEvent::Log(
                crate::domain::event::LogEntry::new(level, "runtime", "important")
            )));
        }
        for level in [LogLevel::Debug, LogLevel::Info] {
            assert!(is_transient_runtime_event(&RuntimeEvent::Log(
                crate::domain::event::LogEntry::new(level, "runtime", "advisory")
            )));
        }
        assert!(!is_transient_runtime_event(&RuntimeEvent::Reasoning {
            member: MemberId::new("builder"),
            text: "latest thought".to_string(),
        }));
    }

    #[test]
    fn reasoning_is_coalesced_but_retained_during_backpressure() {
        let (tx, rx) = mpsc::sync_channel(1);
        tx.send(RuntimeEvent::Notice("occupy the output queue".to_string()))
            .unwrap();
        let sender = RuntimeEventSender::Bounded(tx);
        let mut pending = VecDeque::new();
        let member = MemberId::new("builder");

        assert!(enqueue_runtime_events(
            &sender,
            &mut pending,
            [
                RuntimeEvent::Reasoning {
                    member: member.clone(),
                    text: "first snapshot".to_string(),
                },
                RuntimeEvent::Reasoning {
                    member: member.clone(),
                    text: "latest snapshot".to_string(),
                },
            ],
        ));

        assert_eq!(pending.len(), 1);
        assert!(matches!(
            pending.front(),
            Some(RuntimeEvent::Reasoning { member: queued, text })
                if queued == &member && text == "latest snapshot"
        ));
        assert!(matches!(rx.recv().unwrap(), RuntimeEvent::Notice(_)));
        assert!(flush_runtime_events(&sender, &mut pending));
        assert!(matches!(
            rx.recv().unwrap(),
            RuntimeEvent::Reasoning { member: queued, text }
                if queued == member && text == "latest snapshot"
        ));
    }

    #[test]
    fn bounded_output_backpressure_still_processes_abort_and_shutdown() {
        struct CancelAwareRunner {
            started: SyncSender<()>,
            cancelled: SyncSender<()>,
        }

        impl MemberRunner for CancelAwareRunner {
            fn backend(&self) -> BackendKind {
                BackendKind::Codex
            }

            fn run(&self, req: crate::adapter::RunRequest, events: SyncSender<AgentEvent>) {
                let _ = self.started.send(());
                while !req.cancel.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_millis(5));
                }
                let _ = self.cancelled.send(());
                let _ = events.send(AgentEvent::Exited {
                    code: None,
                    ok: false,
                });
            }
        }

        let (evt_tx, evt_rx) = mpsc::sync_channel(1);
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (cancelled_tx, cancelled_rx) = mpsc::sync_channel(1);
        let mut runners: Runners = HashMap::new();
        runners.insert(
            MemberId::new("builder"),
            Arc::new(CancelAwareRunner {
                started: started_tx,
                cancelled: cancelled_tx,
            }),
        );
        let (handle, join) = spawn_bounded(
            single_codex_team(),
            SqliteStore::in_memory().unwrap(),
            runners,
            evt_tx,
            true,
            false,
            None,
        );

        // Ready fills the sole output slot and the receiver deliberately stays
        // live without draining it for the rest of the test.
        assert!(handle.send(UiCommand::UserMessage {
            target: MessageTarget::Default,
            body: "fill output".to_string(),
        }));
        started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("the state machine dispatched work despite output backpressure");

        assert!(handle.send(UiCommand::Cancel { member: None }));
        cancelled_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("global cancellation reached the active runner despite output backpressure");
        assert!(handle.send(UiCommand::Shutdown));

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !join.is_finished() && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(
            join.is_finished(),
            "Shutdown must be processed without waiting for the full output sink"
        );
        join.join().unwrap();
        drop(evt_rx);
    }

    #[test]
    fn global_cancel_is_a_barrier_for_ordinary_commands_queued_behind_backpressure() {
        struct CountingRunner {
            started: SyncSender<()>,
            cancelled: SyncSender<()>,
        }

        impl MemberRunner for CountingRunner {
            fn backend(&self) -> BackendKind {
                BackendKind::Codex
            }

            fn run(&self, req: crate::adapter::RunRequest, events: SyncSender<AgentEvent>) {
                let _ = self.started.send(());
                while !req.cancel.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_millis(5));
                }
                let _ = self.cancelled.send(());
                let _ = events.send(AgentEvent::Exited {
                    code: None,
                    ok: false,
                });
            }
        }

        let (evt_tx, evt_rx) = mpsc::sync_channel(1);
        let (started_tx, started_rx) = mpsc::sync_channel(2);
        let (cancelled_tx, cancelled_rx) = mpsc::sync_channel(1);
        let mut runners: Runners = HashMap::new();
        runners.insert(
            MemberId::new("builder"),
            Arc::new(CountingRunner {
                started: started_tx,
                cancelled: cancelled_tx,
            }),
        );
        let (handle, join) = spawn_bounded(
            single_codex_team(),
            SqliteStore::in_memory().unwrap(),
            runners,
            evt_tx,
            true,
            false,
            None,
        );

        assert_eq!(
            handle.try_send(UiCommand::UserMessage {
                target: MessageTarget::Default,
                body: "first".to_string(),
            }),
            RuntimeCommandSend::Sent
        );
        started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("first command started");
        assert_eq!(
            handle.try_send(UiCommand::UserMessage {
                target: MessageTarget::Default,
                body: "must be cancelled before start".to_string(),
            }),
            RuntimeCommandSend::Sent
        );
        assert_eq!(
            handle.try_send(UiCommand::Cancel { member: None }),
            RuntimeCommandSend::Sent
        );
        cancelled_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("active run was cancelled");

        // Let the sink and cancelled first turn fully catch up. The queued
        // second command must not appear or dispatch after the cancel barrier.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut saw_barrier = false;
        let mut saw_finished = false;
        while std::time::Instant::now() < deadline && !(saw_barrier && saw_finished) {
            if let Ok(event) = evt_rx.recv_timeout(Duration::from_millis(100)) {
                match event {
                    RuntimeEvent::Notice(text) if text.contains("queued command") => {
                        saw_barrier = true;
                    }
                    RuntimeEvent::UserMessage { body, .. }
                        if body == "must be cancelled before start" =>
                    {
                        panic!("the cancel barrier allowed a queued user message")
                    }
                    RuntimeEvent::TurnFinished { .. } => saw_finished = true,
                    _ => {}
                }
            }
        }
        assert!(saw_barrier && saw_finished);
        assert!(
            started_rx.recv_timeout(Duration::from_millis(250)).is_err(),
            "a pre-cancel queued command started after the cancel barrier"
        );
        assert_eq!(
            handle.try_send(UiCommand::Shutdown),
            RuntimeCommandSend::Sent
        );
        join.join().unwrap();
    }

    #[test]
    fn member_cancel_can_upgrade_to_global_cancel_during_backpressure() {
        struct MemberCancelRunner {
            member: MemberId,
            started: SyncSender<MemberId>,
            cancelled: SyncSender<MemberId>,
        }

        impl MemberRunner for MemberCancelRunner {
            fn backend(&self) -> BackendKind {
                BackendKind::Codex
            }

            fn run(&self, req: crate::adapter::RunRequest, events: SyncSender<AgentEvent>) {
                let _ = self.started.send(self.member.clone());
                while !req.cancel.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_millis(5));
                }
                let _ = self.cancelled.send(self.member.clone());
                let _ = events.send(AgentEvent::Exited {
                    code: None,
                    ok: false,
                });
            }
        }

        let builder = MemberId::new("builder");
        let reviewer = MemberId::new("reviewer");
        let config = single_codex_team().with_member(TeamMember::new(
            "reviewer",
            "Reviewer",
            BackendKind::Codex,
            "review",
        ));
        let (evt_tx, evt_rx) = mpsc::sync_channel(1);
        let (started_tx, started_rx) = mpsc::sync_channel(4);
        let (cancelled_tx, cancelled_rx) = mpsc::sync_channel(2);
        let mut runners: Runners = HashMap::new();
        for member in [&builder, &reviewer] {
            runners.insert(
                member.clone(),
                Arc::new(MemberCancelRunner {
                    member: member.clone(),
                    started: started_tx.clone(),
                    cancelled: cancelled_tx.clone(),
                }),
            );
        }
        let (handle, join) = spawn_bounded(
            config,
            SqliteStore::in_memory().unwrap(),
            runners,
            evt_tx,
            true,
            false,
            None,
        );

        assert_eq!(
            handle.try_send(UiCommand::UserMessage {
                target: MessageTarget::All,
                body: "first".to_string(),
            }),
            RuntimeCommandSend::Sent
        );
        let mut started = HashSet::new();
        for _ in 0..2 {
            started.insert(
                started_rx
                    .recv_timeout(Duration::from_secs(2))
                    .expect("both members started the first turn"),
            );
        }
        assert_eq!(started, HashSet::from([builder.clone(), reviewer.clone()]));

        assert_eq!(
            handle.try_send(UiCommand::UserMessage {
                target: MessageTarget::Default,
                body: "must be discarded by upgraded global cancel".to_string(),
            }),
            RuntimeCommandSend::Sent
        );
        assert_eq!(
            handle.try_send(UiCommand::Cancel {
                member: Some(builder.clone()),
            }),
            RuntimeCommandSend::Sent
        );
        assert_eq!(
            handle.try_send(UiCommand::Cancel { member: None }),
            RuntimeCommandSend::Sent
        );

        let mut cancelled = HashSet::new();
        for _ in 0..2 {
            cancelled.insert(
                cancelled_rx
                    .recv_timeout(Duration::from_secs(2))
                    .expect("the upgraded global cancel reached both members"),
            );
        }
        assert_eq!(
            cancelled,
            HashSet::from([builder.clone(), reviewer.clone()])
        );

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut saw_barrier = false;
        let mut saw_finished = false;
        while std::time::Instant::now() < deadline && !(saw_barrier && saw_finished) {
            if let Ok(event) = evt_rx.recv_timeout(Duration::from_millis(100)) {
                match event {
                    RuntimeEvent::Notice(text) if text.contains("queued command") => {
                        saw_barrier = true;
                    }
                    RuntimeEvent::UserMessage { body, .. }
                        if body == "must be discarded by upgraded global cancel" =>
                    {
                        panic!("member-specific cancellation swallowed the global barrier")
                    }
                    RuntimeEvent::TurnFinished { .. } => saw_finished = true,
                    _ => {}
                }
            }
        }
        assert!(saw_barrier && saw_finished);
        assert!(
            started_rx.recv_timeout(Duration::from_millis(250)).is_err(),
            "queued work started after the upgraded global cancel"
        );

        assert_eq!(
            handle.try_send(UiCommand::Shutdown),
            RuntimeCommandSend::Sent
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !join.is_finished() && std::time::Instant::now() < deadline {
            let _ = evt_rx.recv_timeout(Duration::from_millis(20));
        }
        assert!(join.is_finished());
        join.join().unwrap();
    }

    #[test]
    fn attach_grant_pauses_new_work_until_attach_finished() {
        struct StartReportingRunner {
            started: SyncSender<()>,
        }

        impl MemberRunner for StartReportingRunner {
            fn backend(&self) -> BackendKind {
                BackendKind::Codex
            }

            fn run(&self, _req: crate::adapter::RunRequest, events: SyncSender<AgentEvent>) {
                let _ = self.started.send(());
                let _ = events.send(AgentEvent::Exited {
                    code: Some(0),
                    ok: true,
                });
            }
        }

        let (evt_tx, evt_rx) = mpsc::channel();
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let mut runners: Runners = HashMap::new();
        runners.insert(
            MemberId::new("builder"),
            Arc::new(StartReportingRunner {
                started: started_tx,
            }),
        );
        let (handle, join) = spawn(
            single_codex_team(),
            SqliteStore::in_memory().unwrap(),
            runners,
            evt_tx,
            true,
            false,
            None,
        );
        assert!(matches!(
            evt_rx.recv_timeout(Duration::from_secs(2)),
            Ok(RuntimeEvent::Ready { .. })
        ));

        assert_eq!(
            handle.try_send(UiCommand::RequestAttach {
                member: MemberId::new("builder"),
            }),
            RuntimeCommandSend::Sent
        );
        assert_eq!(
            evt_rx.recv_timeout(Duration::from_secs(2)),
            Ok(RuntimeEvent::AttachGranted {
                member: MemberId::new("builder"),
            })
        );

        let queued_handle = handle.clone();
        let queued = thread::spawn(move || {
            queued_handle.try_send(UiCommand::UserMessage {
                target: MessageTarget::Default,
                body: "queued during attach".to_string(),
            })
        });
        assert_eq!(queued.join().unwrap(), RuntimeCommandSend::Sent);
        assert!(
            started_rx.recv_timeout(Duration::from_millis(250)).is_err(),
            "ordinary work started while the attach reservation was active"
        );

        assert_eq!(
            handle.try_send(UiCommand::AttachFinished {
                member: MemberId::new("ghost"),
                session: None,
                items: vec![ImportedMessage {
                    from_user: true,
                    text: "must not import".to_string(),
                }],
            }),
            RuntimeCommandSend::Sent
        );
        assert!(
            started_rx.recv_timeout(Duration::from_millis(250)).is_err(),
            "a mismatched completion released another member's reservation"
        );

        assert_eq!(
            handle.try_send(UiCommand::AttachFinished {
                member: MemberId::new("builder"),
                session: None,
                items: vec![ImportedMessage {
                    from_user: true,
                    text: "typed while attached".to_string(),
                }],
            }),
            RuntimeCommandSend::Sent
        );
        started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("queued work resumed after AttachFinished");

        let mut imported_position = None;
        let mut queued_position = None;
        let mut position = 0_usize;
        while let Ok(event) = evt_rx.recv_timeout(Duration::from_secs(2)) {
            match event {
                RuntimeEvent::UserMessage { body, .. } if body == "must not import" => {
                    panic!("a mismatched attach completion imported transcript data")
                }
                RuntimeEvent::UserMessage { body, .. } if body == "typed while attached" => {
                    imported_position = Some(position);
                }
                RuntimeEvent::UserMessage { body, .. } if body == "queued during attach" => {
                    queued_position = Some(position);
                    if imported_position.is_some() {
                        break;
                    }
                }
                _ => {}
            }
            position = position.saturating_add(1);
        }
        assert!(
            imported_position.is_some_and(|imported| {
                queued_position.is_some_and(|queued| imported < queued)
            }),
            "attached transcript must be emitted before work queued after the grant"
        );

        assert_eq!(
            handle.try_send(UiCommand::Shutdown),
            RuntimeCommandSend::Sent
        );
        join.join().unwrap();
    }

    #[test]
    fn direct_transcript_import_cannot_bypass_the_attach_reservation() {
        let path = std::env::temp_dir().join(format!(
            "asterline-runtime-direct-import-{}-{}.sqlite3",
            std::process::id(),
            TEAM_SAVE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&path);
        let store = SqliteStore::open(&path).unwrap();
        let external = rusqlite::Connection::open(&path).unwrap();
        let (evt_tx, evt_rx) = mpsc::channel();
        let (handle, join) = spawn(
            single_codex_team(),
            store,
            HashMap::new(),
            evt_tx,
            true,
            false,
            None,
        );
        assert!(matches!(
            evt_rx.recv_timeout(Duration::from_secs(2)),
            Ok(RuntimeEvent::Ready { .. })
        ));

        let builder = MemberId::new("builder");
        assert_eq!(
            handle.try_send(UiCommand::RequestAttach {
                member: builder.clone(),
            }),
            RuntimeCommandSend::Sent
        );
        assert_eq!(
            evt_rx.recv_timeout(Duration::from_secs(2)),
            Ok(RuntimeEvent::AttachGranted {
                member: builder.clone(),
            })
        );

        assert_eq!(
            handle.try_send(UiCommand::ImportTranscript {
                member: builder.clone(),
                items: vec![ImportedMessage {
                    from_user: true,
                    text: "must not bypass attach".to_string(),
                }],
            }),
            RuntimeCommandSend::Sent
        );
        let attached_session = AgentSessionId("native-attached-session".to_string());
        assert!(handle.finish_attach_with_session(
            builder.clone(),
            Some(attached_session.clone()),
            vec![ImportedMessage {
                from_user: true,
                text: "validated attached message".to_string(),
            }],
        ));

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut saw_validated = false;
        let mut saw_rejection = false;
        let mut saw_session = false;
        while std::time::Instant::now() < deadline
            && !(saw_validated && saw_rejection && saw_session)
        {
            match evt_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(RuntimeEvent::UserMessage { body, .. })
                    if body == "validated attached message" =>
                {
                    saw_validated = true;
                }
                Ok(RuntimeEvent::UserMessage { body, .. }) if body == "must not bypass attach" => {
                    panic!("a direct transcript import bypassed the reservation")
                }
                Ok(RuntimeEvent::Notice(text))
                    if text.contains("ignored direct transcript import") =>
                {
                    saw_rejection = true;
                }
                Ok(RuntimeEvent::SessionUpdated { member, session })
                    if member == builder && session == attached_session =>
                {
                    saw_session = true;
                }
                Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        assert!(saw_validated && saw_rejection && saw_session);

        let bypassed: i64 = external
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE text = ?1",
                ["must not bypass attach"],
                |row| row.get(0),
            )
            .unwrap();
        let validated: i64 = external
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE text = ?1",
                ["validated attached message"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(bypassed, 0);
        assert_eq!(validated, 1);
        let persisted_session: String = external
            .query_row(
                "SELECT session_id FROM agent_sessions WHERE member_id = ?1",
                ["builder"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(persisted_session, attached_session.0);

        assert!(handle.shutdown());
        join.join().unwrap();
        drop(external);
        let _ = std::fs::remove_file(&path);
        for suffix in ["-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    #[test]
    fn stray_attach_completion_flood_is_coalesced_and_cannot_starve_shutdown() {
        let path = std::env::temp_dir().join(format!(
            "asterline-runtime-attach-control-flood-{}-{}.sqlite3",
            std::process::id(),
            TEAM_SAVE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&path);
        let store = SqliteStore::open(&path).unwrap();
        let external = rusqlite::Connection::open(&path).unwrap();
        let (evt_tx, evt_rx) = mpsc::sync_channel(1);
        let (handle, join) = spawn_bounded(
            single_codex_team(),
            store,
            HashMap::new(),
            evt_tx,
            true,
            false,
            None,
        );
        assert!(matches!(
            evt_rx.recv_timeout(Duration::from_secs(2)),
            Ok(RuntimeEvent::Ready { .. })
        ));

        let builder = MemberId::new("builder");
        assert_eq!(
            handle.try_send(UiCommand::RequestAttach {
                member: builder.clone(),
            }),
            RuntimeCommandSend::Sent
        );
        assert_eq!(
            evt_rx.recv_timeout(Duration::from_secs(2)),
            Ok(RuntimeEvent::AttachGranted {
                member: builder.clone(),
            })
        );

        // The first stray completion fills the one-slot output channel; the
        // second becomes the single retained diagnostic. Every later unique
        // member id must be discarded rather than extending `pending_events`.
        for index in 0..512 {
            assert!(handle.finish_attach(MemberId::new(format!("ghost-{index}")), Vec::new()));
        }
        assert!(handle.finish_attach(
            builder.clone(),
            vec![ImportedMessage {
                from_user: true,
                text: "correct owner still completes".to_string(),
            }],
        ));

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let imported: i64 = external
                .query_row(
                    "SELECT COUNT(*) FROM messages WHERE kind = 'user'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            if imported == 1 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the reserved owner's completion was suppressed by stray coalescing"
            );
            thread::sleep(Duration::from_millis(5));
        }

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut ignored = 0_usize;
        let mut imported_turn_finished = false;
        while std::time::Instant::now() < deadline && !imported_turn_finished {
            match evt_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(RuntimeEvent::Notice(text)) if text.contains("ignored attach completion") => {
                    ignored = ignored.saturating_add(1);
                }
                Ok(RuntimeEvent::TurnFinished { .. }) => imported_turn_finished = true,
                Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        assert!(imported_turn_finished);
        assert_eq!(
            ignored, 2,
            "a one-slot sink may hold one diagnostic and pending output at most one more"
        );

        // Re-establish output backpressure without an attach reservation, then
        // continuously refill the control lane. Reliable Shutdown must still
        // reach the sole state-machine thread after the coalesced flood.
        assert_eq!(
            handle.try_send(UiCommand::UserMessage {
                target: MessageTarget::Default,
                body: "fill output again".to_string(),
            }),
            RuntimeCommandSend::Sent
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let users: i64 = external
                .query_row(
                    "SELECT COUNT(*) FROM messages WHERE kind = 'user'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            if users == 2 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the command used to refill the sink was not processed"
            );
            thread::sleep(Duration::from_millis(5));
        }
        for index in 0..512 {
            assert!(handle.finish_attach(MemberId::new(format!("unreserved-{index}")), Vec::new()));
        }
        assert!(handle.shutdown());
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !join.is_finished() && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(
            join.is_finished(),
            "Shutdown was starved by stray attach completion control traffic"
        );
        join.join().unwrap();
        drop(external);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn attach_request_queued_after_active_work_is_refused() {
        struct BlockingRunner {
            started: SyncSender<()>,
            cancelled: SyncSender<()>,
        }

        impl MemberRunner for BlockingRunner {
            fn backend(&self) -> BackendKind {
                BackendKind::Codex
            }

            fn run(&self, req: crate::adapter::RunRequest, events: SyncSender<AgentEvent>) {
                let _ = self.started.send(());
                while !req.cancel.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_millis(5));
                }
                let _ = self.cancelled.send(());
                let _ = events.send(AgentEvent::Exited {
                    code: None,
                    ok: false,
                });
            }
        }

        let (evt_tx, evt_rx) = mpsc::channel();
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (cancelled_tx, cancelled_rx) = mpsc::sync_channel(1);
        let mut runners: Runners = HashMap::new();
        runners.insert(
            MemberId::new("builder"),
            Arc::new(BlockingRunner {
                started: started_tx,
                cancelled: cancelled_tx,
            }),
        );
        let (handle, join) = spawn(
            single_codex_team(),
            SqliteStore::in_memory().unwrap(),
            runners,
            evt_tx,
            true,
            false,
            None,
        );
        assert!(matches!(
            evt_rx.recv_timeout(Duration::from_secs(2)),
            Ok(RuntimeEvent::Ready { .. })
        ));

        assert_eq!(
            handle.try_send(UiCommand::UserMessage {
                target: MessageTarget::Default,
                body: "prior work".to_string(),
            }),
            RuntimeCommandSend::Sent
        );
        assert_eq!(
            handle.try_send(UiCommand::RequestAttach {
                member: MemberId::new("builder"),
            }),
            RuntimeCommandSend::Sent
        );
        started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("prior work started");

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut saw_refusal = false;
        while std::time::Instant::now() < deadline && !saw_refusal {
            match evt_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(RuntimeEvent::AttachGranted { .. }) => {
                    panic!("attach was granted while prior work was still active")
                }
                Ok(RuntimeEvent::AttachDenied { member, reason })
                    if member == MemberId::new("builder") && reason.contains("cannot attach") =>
                {
                    saw_refusal = true;
                }
                Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        assert!(saw_refusal, "active work produced an attach refusal");

        assert_eq!(
            handle.try_send(UiCommand::Cancel { member: None }),
            RuntimeCommandSend::Sent
        );
        cancelled_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("cleanup cancellation reached the runner");
        assert_eq!(
            handle.try_send(UiCommand::Shutdown),
            RuntimeCommandSend::Sent
        );
        join.join().unwrap();
    }

    #[test]
    fn unknown_attach_member_is_structurally_denied() {
        let (evt_tx, evt_rx) = mpsc::channel();
        let (handle, join) = spawn(
            single_codex_team(),
            SqliteStore::in_memory().unwrap(),
            HashMap::new(),
            evt_tx,
            true,
            false,
            None,
        );
        assert!(matches!(
            evt_rx.recv_timeout(Duration::from_secs(2)),
            Ok(RuntimeEvent::Ready { .. })
        ));

        let unknown = MemberId::new("ghost");
        assert_eq!(
            handle.try_send(UiCommand::RequestAttach {
                member: unknown.clone(),
            }),
            RuntimeCommandSend::Sent
        );
        match evt_rx.recv_timeout(Duration::from_secs(2)) {
            Ok(RuntimeEvent::AttachDenied { member, reason }) => {
                assert_eq!(member, unknown);
                assert!(reason.contains("unknown member"));
            }
            other => panic!("expected structured attach denial, got {other:?}"),
        }

        assert_eq!(
            handle.try_send(UiCommand::Shutdown),
            RuntimeCommandSend::Sent
        );
        join.join().unwrap();
    }

    #[test]
    fn global_cancel_denies_a_queued_attach_request_dropped_by_its_barrier() {
        struct BlockingRunner {
            started: SyncSender<()>,
            cancelled: SyncSender<()>,
        }

        impl MemberRunner for BlockingRunner {
            fn backend(&self) -> BackendKind {
                BackendKind::Codex
            }

            fn run(&self, req: crate::adapter::RunRequest, events: SyncSender<AgentEvent>) {
                let _ = self.started.send(());
                while !req.cancel.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_millis(5));
                }
                let _ = self.cancelled.send(());
                let _ = events.send(AgentEvent::Exited {
                    code: None,
                    ok: false,
                });
            }
        }

        let (evt_tx, evt_rx) = mpsc::sync_channel(1);
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (cancelled_tx, cancelled_rx) = mpsc::sync_channel(1);
        let mut runners: Runners = HashMap::new();
        runners.insert(
            MemberId::new("builder"),
            Arc::new(BlockingRunner {
                started: started_tx,
                cancelled: cancelled_tx,
            }),
        );
        let (handle, join) = spawn_bounded(
            single_codex_team(),
            SqliteStore::in_memory().unwrap(),
            runners,
            evt_tx,
            true,
            false,
            None,
        );

        assert_eq!(
            handle.try_send(UiCommand::UserMessage {
                target: MessageTarget::Default,
                body: "active".to_string(),
            }),
            RuntimeCommandSend::Sent
        );
        started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("prior command started");
        let builder = MemberId::new("builder");
        assert_eq!(
            handle.try_send(UiCommand::RequestAttach {
                member: builder.clone(),
            }),
            RuntimeCommandSend::Sent
        );
        assert_eq!(
            handle.try_send(UiCommand::Cancel { member: None }),
            RuntimeCommandSend::Sent
        );
        cancelled_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("global cancel reached active work");

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut saw_denial = false;
        while std::time::Instant::now() < deadline && !saw_denial {
            match evt_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(RuntimeEvent::AttachGranted { .. }) => {
                    panic!("a cancelled queued attach request was granted")
                }
                Ok(RuntimeEvent::AttachDenied { member, reason }) => {
                    assert_eq!(member, builder);
                    assert!(reason.contains("cancelled before it started"));
                    saw_denial = true;
                }
                Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        assert!(saw_denial);

        assert_eq!(
            handle.try_send(UiCommand::Shutdown),
            RuntimeCommandSend::Sent
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !join.is_finished() && std::time::Instant::now() < deadline {
            let _ = evt_rx.recv_timeout(Duration::from_millis(20));
        }
        assert!(join.is_finished());
        join.join().unwrap();
    }

    #[test]
    fn oversized_attach_transcript_is_bounded_and_still_releases_reservation() {
        struct StartReportingRunner {
            started: SyncSender<()>,
        }

        impl MemberRunner for StartReportingRunner {
            fn backend(&self) -> BackendKind {
                BackendKind::Codex
            }

            fn run(&self, _req: crate::adapter::RunRequest, events: SyncSender<AgentEvent>) {
                let _ = self.started.send(());
                let _ = events.send(AgentEvent::Exited {
                    code: Some(0),
                    ok: true,
                });
            }
        }

        let (evt_tx, evt_rx) = mpsc::channel();
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let mut runners: Runners = HashMap::new();
        runners.insert(
            MemberId::new("builder"),
            Arc::new(StartReportingRunner {
                started: started_tx,
            }),
        );
        let (handle, join) = spawn(
            single_codex_team(),
            SqliteStore::in_memory().unwrap(),
            runners,
            evt_tx,
            true,
            false,
            None,
        );
        assert!(matches!(
            evt_rx.recv_timeout(Duration::from_secs(2)),
            Ok(RuntimeEvent::Ready { .. })
        ));
        let builder = MemberId::new("builder");
        assert_eq!(
            handle.try_send(UiCommand::RequestAttach {
                member: builder.clone(),
            }),
            RuntimeCommandSend::Sent
        );
        assert!(matches!(
            evt_rx.recv_timeout(Duration::from_secs(2)),
            Ok(RuntimeEvent::AttachGranted { .. })
        ));
        assert_eq!(
            handle.try_send(UiCommand::UserMessage {
                target: MessageTarget::Default,
                body: "after bounded import".to_string(),
            }),
            RuntimeCommandSend::Sent
        );

        let mut items = vec![ImportedMessage {
            from_user: true,
            text: "oversized"
                .repeat((team_runtime::MAX_IMPORTED_ITEM_BYTES / "oversized".len()) + 1),
        }];
        items.extend(
            (0..team_runtime::MAX_IMPORTED_ITEMS + 25).map(|index| ImportedMessage {
                from_user: index % 2 == 0,
                text: format!("bounded-{index}"),
            }),
        );
        assert!(handle.finish_attach(builder, items));
        started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("bounded transcript completion released the reservation");

        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let mut imported = 0_usize;
        let mut saw_oversized = false;
        let mut saw_truncated = false;
        let mut saw_after = false;
        while std::time::Instant::now() < deadline && !saw_after {
            match evt_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(RuntimeEvent::UserMessage { body, .. }) if body == "after bounded import" => {
                    saw_after = true;
                }
                Ok(RuntimeEvent::UserMessage { body, .. }) => {
                    assert!(!body.starts_with("oversized"));
                    imported = imported.saturating_add(1);
                }
                Ok(RuntimeEvent::MessageCompleted { .. }) => {
                    imported = imported.saturating_add(1);
                }
                Ok(RuntimeEvent::Notice(text)) if text.contains("was truncated") => {
                    saw_truncated = true;
                    saw_oversized = text.contains("skipped 1 oversized message");
                }
                Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        assert_eq!(imported, team_runtime::MAX_IMPORTED_ITEMS - 1);
        assert!(saw_truncated && saw_oversized && saw_after);

        assert!(handle.shutdown());
        join.join().unwrap();
    }

    #[test]
    fn attach_transcript_total_byte_limit_drops_the_tail_without_reordering() {
        let (evt_tx, evt_rx) = mpsc::channel();
        let (handle, join) = spawn(
            single_codex_team(),
            SqliteStore::in_memory().unwrap(),
            HashMap::new(),
            evt_tx,
            true,
            false,
            None,
        );
        assert!(matches!(
            evt_rx.recv_timeout(Duration::from_secs(2)),
            Ok(RuntimeEvent::Ready { .. })
        ));
        let builder = MemberId::new("builder");
        assert_eq!(
            handle.try_send(UiCommand::RequestAttach {
                member: builder.clone(),
            }),
            RuntimeCommandSend::Sent
        );
        assert!(matches!(
            evt_rx.recv_timeout(Duration::from_secs(2)),
            Ok(RuntimeEvent::AttachGranted { .. })
        ));

        let chunk = "x".repeat(team_runtime::MAX_IMPORTED_ITEM_BYTES);
        let chunks = team_runtime::MAX_IMPORTED_TOTAL_BYTES / team_runtime::MAX_IMPORTED_ITEM_BYTES;
        let mut items = (0..chunks)
            .map(|_| ImportedMessage {
                from_user: true,
                text: chunk.clone(),
            })
            .collect::<Vec<_>>();
        items.push(ImportedMessage {
            from_user: true,
            text: "tail-must-not-import".to_string(),
        });
        assert!(handle.finish_attach(builder, items));

        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let mut imported = 0_usize;
        let mut saw_truncated = false;
        while std::time::Instant::now() < deadline && !saw_truncated {
            match evt_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(RuntimeEvent::UserMessage { body, .. }) => {
                    assert_ne!(body, "tail-must-not-import");
                    imported = imported.saturating_add(1);
                }
                Ok(RuntimeEvent::Notice(text)) if text.contains("was truncated") => {
                    assert!(
                        text.contains(&format!("{} bytes", team_runtime::MAX_IMPORTED_TOTAL_BYTES))
                    );
                    saw_truncated = true;
                }
                Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        assert_eq!(imported, chunks);
        assert!(saw_truncated);

        assert!(handle.shutdown());
        join.join().unwrap();
    }

    #[test]
    fn bounded_output_retains_critical_events_until_the_tui_catches_up() {
        let (evt_tx, evt_rx) = mpsc::sync_channel(1);
        let (handle, join) = spawn_bounded(
            single_codex_team(),
            SqliteStore::in_memory().unwrap(),
            HashMap::new(),
            evt_tx,
            true,
            false,
            None,
        );

        // Ready occupies the only sink slot while the complete missing-runner
        // lifecycle is generated behind it.
        assert!(handle.send(UiCommand::UserMessage {
            target: MessageTarget::Default,
            body: "retain lifecycle".to_string(),
        }));
        thread::sleep(Duration::from_millis(50));
        assert!(matches!(
            evt_rx.recv_timeout(Duration::from_secs(2)),
            Ok(RuntimeEvent::Ready { .. })
        ));

        let mut saw_user = false;
        let mut saw_error = false;
        let mut saw_finished = false;
        while let Ok(event) = evt_rx.recv_timeout(Duration::from_secs(2)) {
            match event {
                RuntimeEvent::UserMessage { body, .. } if body == "retain lifecycle" => {
                    saw_user = true;
                }
                RuntimeEvent::MemberError { message, .. }
                    if message.contains("no backend runner") =>
                {
                    saw_error = true;
                }
                RuntimeEvent::TurnFinished { .. } => {
                    saw_finished = true;
                    break;
                }
                _ => {}
            }
        }
        assert!(saw_user && saw_error && saw_finished);

        assert!(handle.send(UiCommand::Shutdown));
        join.join().unwrap();
    }

    #[test]
    fn bounded_output_stops_ingesting_worker_flood_while_critical_events_wait() {
        let path = std::env::temp_dir().join(format!(
            "asterline-runtime-backpressure-{}-{}.sqlite3",
            std::process::id(),
            TEAM_SAVE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&path);
        let store = SqliteStore::open(&path).unwrap();
        let external = rusqlite::Connection::open(&path).unwrap();
        let (evt_tx, evt_rx) = mpsc::sync_channel(1);
        let event_sink = evt_tx.clone();
        let (ui_tx, ui_rx) = mpsc::sync_channel(64);
        let (control_tx, control_rx) = mpsc::sync_channel(4);
        let (worker_tx, worker_rx) = mpsc::sync_channel(64);
        let channels = RuntimeChannels {
            events: RuntimeEventSender::Bounded(evt_tx),
            worker_tx: worker_tx.clone(),
            worker_rx,
            ui_rx,
            control_rx,
        };
        let join = thread::spawn(move || {
            run_loop(
                single_codex_team(),
                store,
                HashMap::new(),
                RuntimeLoopOptions {
                    approvals: true,
                    fake: false,
                    team_save_path: None,
                },
                channels,
            );
        });

        // Wait until startup has emitted Ready, then put it back to keep the
        // sole output slot full. This avoids counting Windows thread startup
        // time against the command-processing assertion below.
        let ready = evt_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("runtime must emit Ready");
        assert!(matches!(ready, RuntimeEvent::Ready { .. }));
        event_sink.send(ready).unwrap();

        ui_tx
            .send(UiCommand::UserMessage {
                target: MessageTarget::Default,
                body: "hold canonical events".to_string(),
            })
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let users: i64 = external
                .query_row(
                    "SELECT COUNT(*) FROM messages WHERE kind = 'user'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            if users == 1 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the user command was not processed"
            );
            thread::sleep(Duration::from_millis(5));
        }

        for index in 0..32 {
            worker_tx
                .send(RuntimeInput::Agent(
                    MemberId::new("builder"),
                    AgentEvent::Raw(format!("raw-{index}")),
                ))
                .unwrap();
        }
        thread::sleep(Duration::from_millis(100));
        let stored: i64 = external
            .query_row("SELECT COUNT(*) FROM stream_events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            stored, 0,
            "worker events must remain in the bounded input queue while canonical output waits"
        );

        control_tx.send(UiCommand::Cancel { member: None }).unwrap();
        control_tx.send(UiCommand::Shutdown).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !join.is_finished() && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(join.is_finished(), "Shutdown was starved by worker flood");
        join.join().unwrap();
        drop(worker_tx);
        drop(external);
        let _ = std::fs::remove_file(&path);
        for suffix in ["-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    #[test]
    fn missing_runner_fails_and_finishes_the_turn() {
        let (evt_tx, evt_rx) = mpsc::channel();
        let (handle, join) = spawn(
            single_codex_team(),
            SqliteStore::in_memory().unwrap(),
            HashMap::new(),
            evt_tx,
            true,
            false,
            None,
        );
        let _ = evt_rx.recv_timeout(Duration::from_secs(2)).expect("ready");

        handle.send(UiCommand::UserMessage {
            target: MessageTarget::Default,
            body: "hi".to_string(),
        });

        let mut saw_error = false;
        let mut saw_finished = false;
        while let Ok(event) = evt_rx.recv_timeout(Duration::from_secs(2)) {
            match event {
                RuntimeEvent::MemberError { message, .. }
                    if message.contains("no backend runner") =>
                {
                    saw_error = true;
                }
                RuntimeEvent::TurnFinished { .. } => {
                    saw_finished = true;
                    break;
                }
                _ => {}
            }
        }
        assert!(saw_error);
        assert!(saw_finished);

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
    fn team_save_failure_stops_before_applying_runner_changes() {
        let dir = std::env::temp_dir().join(format!(
            "asterline-runtime-save-failure-{}-{}",
            std::process::id(),
            TEAM_SAVE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let non_directory = dir.join("not-a-directory");
        std::fs::write(&non_directory, "block parent creation").unwrap();
        let save_path = non_directory.join("team.json");
        let (evt_tx, evt_rx) = mpsc::channel();
        let (handle, join) = spawn(
            single_codex_team(),
            SqliteStore::in_memory().unwrap(),
            HashMap::new(),
            evt_tx,
            true,
            true,
            Some(save_path),
        );
        let _ = evt_rx.recv_timeout(Duration::from_secs(2)).expect("ready");

        assert!(handle.send(UiCommand::ReplaceTeam {
            members: vec![
                TeamMember::new("builder", "Builder", BackendKind::Codex, "impl"),
                TeamMember::new("researcher", "Researcher", BackendKind::Agy, "research"),
            ],
            default_target: None,
        }));
        join.join().unwrap();

        let events = evt_rx.try_iter().collect::<Vec<_>>();
        assert!(events.iter().any(|event| {
            matches!(event, RuntimeEvent::Notice(message)
                if message.contains("runtime stopped to avoid inconsistent state"))
        }));
        assert!(!events.iter().any(|event| {
            matches!(event, RuntimeEvent::Ready { members, .. } if members.len() == 2)
                || matches!(event, RuntimeEvent::Notice(message) if message.starts_with("team updated:"))
        }));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn atomic_team_save_replaces_valid_file_and_cleans_temp() {
        let dir = std::env::temp_dir().join(format!(
            "asterline-atomic-team-save-{}-{}",
            std::process::id(),
            TEAM_SAVE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("team.json");
        std::fs::write(&path, "old valid contents").unwrap();

        let config = single_codex_team();
        save_team_config(&path, &config).unwrap();

        let saved = std::fs::read_to_string(&path).unwrap();
        let decoded: TeamConfig = serde_json::from_str(&saved).unwrap();
        assert_eq!(decoded, config);
        assert!(std::fs::read_dir(&dir).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".tmp")
        }));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_atomic_team_replace_preserves_previous_file() {
        let dir = std::env::temp_dir().join(format!(
            "asterline-failed-team-save-{}-{}",
            std::process::id(),
            TEAM_SAVE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("team.json");
        std::fs::write(&path, "previous valid file").unwrap();

        let error = save_team_config_with_replace(&path, b"replacement", |temp, _| {
            assert_eq!(std::fs::read(temp).unwrap(), b"replacement");
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "injected replace failure",
            ))
        })
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "previous valid file"
        );
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn bare_team_filename_uses_current_directory_as_parent() {
        assert_eq!(team_config_parent(Path::new("team.json")), Path::new("."));
    }

    #[test]
    fn verification_shell_uses_platform_command_conventions() {
        let command = verification_command("echo checked");
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        #[cfg(windows)]
        {
            let program = command.get_program().to_string_lossy().to_ascii_lowercase();
            assert!(program.ends_with("cmd") || program.ends_with("cmd.exe"));
            assert_eq!(args, ["/D", "/S", "/C", "echo checked"]);
        }
        #[cfg(not(windows))]
        {
            assert_eq!(command.get_program(), "sh");
            assert_eq!(args, ["-lc", "echo checked"]);
        }
    }

    #[test]
    fn verification_output_capture_is_bounded_and_keeps_both_ends() {
        let mut input = vec![b'H'; VERIFY_OUTPUT_LIMIT];
        input.extend(vec![b'T'; VERIFY_OUTPUT_LIMIT]);
        let output = read_pipe(std::io::Cursor::new(input)).join().unwrap();

        assert!(output.len() <= VERIFY_OUTPUT_LIMIT);
        assert!(output.starts_with(b"HHHH"));
        assert!(output.ends_with(b"TTTT"));
        assert!(
            String::from_utf8_lossy(&output).contains("verification output truncated"),
            "large output must explicitly report truncation"
        );
    }

    #[test]
    fn shutdown_persists_active_verification_as_blocked() {
        let dir = std::env::temp_dir().join(format!(
            "asterline-shutdown-verification-{}-{}",
            std::process::id(),
            TEAM_SAVE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("state.sqlite3");
        let store = SqliteStore::open(&db_path).unwrap();
        let conversation = store.current_conversation().unwrap();
        let run = store.create_run("shutdown verification", None).unwrap();
        let run_id = run.id;
        let (evt_tx, evt_rx) = mpsc::channel();
        let (handle, join) = spawn(
            single_codex_team(),
            store,
            HashMap::new(),
            evt_tx,
            true,
            false,
            None,
        );
        let _ = evt_rx.recv_timeout(Duration::from_secs(2)).expect("ready");
        #[cfg(windows)]
        let slow_command = "ping.exe 127.0.0.1 -n 6 >NUL";
        #[cfg(not(windows))]
        let slow_command = "sleep 5";
        assert!(handle.send(UiCommand::VerifyRun {
            run_id: Some(run_id),
            command: Some(slow_command.to_string()),
        }));
        while let Ok(event) = evt_rx.recv_timeout(Duration::from_secs(2)) {
            if matches!(
                event,
                RuntimeEvent::RunUpdated { run }
                    if run.id == run_id && run.status == RunStatus::Verifying
            ) {
                break;
            }
        }

        assert!(handle.send(UiCommand::Shutdown));
        join.join().unwrap();

        let reopened = SqliteStore::open(&db_path).unwrap();
        reopened.set_conversation(conversation).unwrap();
        let run = reopened.run(run_id).unwrap();
        assert_eq!(run.status, RunStatus::Blocked);
        assert_eq!(run.verification.unwrap().summary, "verification cancelled");
        drop(reopened);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn runtime_thread_runs_run_verification() {
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
        let _ = evt_rx.recv_timeout(Duration::from_secs(2)).expect("ready");

        handle.send(UiCommand::SetMode {
            mode: crate::domain::mode::TerminalMode::Team,
        });
        handle.send(UiCommand::UserMessage {
            target: crate::domain::event::MessageTarget::Default,
            body: "verify from runtime".to_string(),
        });
        let mut saw_run = false;
        let mut saw_team_done = false;
        while let Ok(event) = evt_rx.recv_timeout(Duration::from_secs(2)) {
            if let RuntimeEvent::RunUpdated { run } = event {
                saw_run = true;
                if run.status == RunStatus::Done {
                    saw_team_done = true;
                    break;
                }
            }
        }
        assert!(saw_run, "team run was created");
        assert!(saw_team_done, "team run finished before verification");

        handle.send(UiCommand::VerifyRun {
            run_id: None,
            command: Some("echo runtime-verified".to_string()),
        });

        let mut saw_verifying = false;
        let mut saw_done = false;
        while let Ok(event) = evt_rx.recv_timeout(Duration::from_secs(2)) {
            match event {
                RuntimeEvent::RunUpdated { run } if run.status == RunStatus::Verifying => {
                    saw_verifying = true;
                }
                RuntimeEvent::RunUpdated { run }
                    if run.status == RunStatus::Done
                        && run.verification.as_ref().is_some_and(|v| {
                            v.ok && v.command == "echo runtime-verified"
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
        #[cfg(windows)]
        let slow_command = "ping.exe 127.0.0.1 -n 6 >NUL & echo done";
        #[cfg(not(windows))]
        let slow_command = "sleep 5; printf done";
        let action = VerifyAction {
            run_id: crate::domain::event::RunId(1),
            command: slow_command.to_string(),
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

    #[cfg(unix)]
    #[test]
    fn verification_cancellation_kills_descendant_processes() {
        let dir = std::env::temp_dir().join(format!(
            "asterline-verification-tree-cancel-{}",
            std::process::id()
        ));
        let started = dir.join("started");
        let survivor = dir.join("survived");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let action = VerifyAction {
            run_id: crate::domain::event::RunId(1),
            command: format!(
                "printf started > '{}'; (sleep 1; printf survived > '{}') & wait",
                started.display(),
                survivor.display()
            ),
            workspace: dir.clone(),
            cancel: Arc::clone(&cancel),
        };

        let join = thread::spawn(move || run_verification(action));
        for _ in 0..40 {
            if started.exists() {
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }
        assert!(started.exists(), "verification command did not start");
        cancel.store(true, Ordering::Relaxed);
        let output = join.join().unwrap();
        thread::sleep(Duration::from_millis(1_100));

        assert!(output.cancelled);
        assert!(
            !survivor.exists(),
            "a cancelled verification descendant continued running"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
