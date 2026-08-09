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

use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::io;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::adapter::process::{ChildProcessTree, configure_process_tree};
use crate::adapter::{FakeRunner, MemberRunner, runner_for};
use crate::domain::event::{AgentEvent, RunId, RuntimeEvent, UiCommand};
use crate::domain::mode::TerminalMode;
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
    tx: Sender<UiCommand>,
}

impl RuntimeHandle {
    /// Send a command; returns false if the runtime loop has stopped.
    pub fn send(&self, command: UiCommand) -> bool {
        self.tx.send(command).is_ok()
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
const RUNTIME_INPUT_POLL_INTERVAL: Duration = Duration::from_millis(10);

enum RuntimeEventSender {
    Unbounded(Sender<RuntimeEvent>),
    Bounded(SyncSender<RuntimeEvent>),
}

impl RuntimeEventSender {
    fn send(&self, event: RuntimeEvent) -> Result<(), ()> {
        let sent = match self {
            Self::Unbounded(sender) => sender.send(event),
            Self::Bounded(sender) => sender.send(event),
        };
        sent.map_err(|_| ())
    }
}

struct RuntimeChannels {
    events: RuntimeEventSender,
    worker_tx: SyncSender<RuntimeInput>,
    worker_rx: Receiver<RuntimeInput>,
    ui_rx: Receiver<UiCommand>,
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
/// path: slow rendering applies backpressure instead of accumulating an
/// unbounded number of stream events in memory.
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
    // while workers are backpressured. In particular, /abort and Shutdown
    // cannot share a full token/event queue with the work they must cancel.
    let (ui_tx, ui_rx) = mpsc::channel();
    let (input_tx, input_rx) = mpsc::sync_channel(RUNTIME_INPUT_QUEUE_CAPACITY);
    let handle = RuntimeHandle { tx: ui_tx };
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
    } = channels;
    let mut runtime = TeamRuntime::new(config, store).with_approvals(options.approvals);
    let mut active_verifications: HashMap<RunId, ActiveVerification> = HashMap::new();
    let mut agent_workers: Vec<JoinHandle<()>> = Vec::new();
    let _ = events.send(runtime.ready_event());
    if runtime.active_mode() != TerminalMode::Normal {
        let _ = events.send(RuntimeEvent::ModeChanged {
            mode: runtime.active_mode(),
        });
    }
    for event in runtime.take_startup_events() {
        let _ = events.send(event);
    }

    let mut deferred_inputs = VecDeque::new();
    loop {
        // Give cancellation/shutdown/user commands priority over worker
        // deltas. Polling the bounded worker queue with a short timeout avoids
        // needing a crossbeam-style select while keeping UI latency low.
        let input = match ui_rx.try_recv() {
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
        };
        let shutdown = matches!(&input, RuntimeInput::Ui(UiCommand::Shutdown));
        let cancel_verifications = match &input {
            RuntimeInput::Ui(UiCommand::Cancel { member }) => member.is_none(),
            RuntimeInput::Ui(UiCommand::Shutdown) => true,
            _ => false,
        };
        let mut step = match input {
            RuntimeInput::Ui(UiCommand::NewSession) if !active_verifications.is_empty() => {
                RuntimeStep {
                    events: vec![RuntimeEvent::Notice(
                        "cannot start a new chat while verification is active; use /abort first"
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
                        "cannot resume another chat while verification is active; use /abort first"
                            .to_string(),
                    )],
                    ..RuntimeStep::default()
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
            for event in step.events {
                if events.send(event).is_err() {
                    break;
                }
            }
            join_workers_bounded(&mut agent_workers, Duration::from_secs(5));
            return;
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

        for event in step.events {
            if events.send(event).is_err() {
                let _ =
                    prepare_shutdown(&mut runtime, &mut active_verifications, &mut agent_workers);
                join_workers_bounded(&mut agent_workers, Duration::from_secs(5));
                return;
            }
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
    use crate::domain::event::MessageTarget;
    use crate::domain::event::RunStatus;
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
    fn bounded_output_never_blocks_abort_or_shutdown_enqueue() {
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

        // Ready fills the sole output slot. The user message makes the runtime
        // block on its next event until the TUI drains, but control traffic is
        // on a separate channel and must still enqueue immediately.
        thread::sleep(Duration::from_millis(50));
        assert!(handle.send(UiCommand::UserMessage {
            target: MessageTarget::Default,
            body: "fill output".to_string(),
        }));
        thread::sleep(Duration::from_millis(50));
        let started = std::time::Instant::now();
        assert!(handle.send(UiCommand::Cancel { member: None }));
        assert!(handle.send(UiCommand::Shutdown));
        assert!(started.elapsed() < Duration::from_millis(100));

        // Dropping the UI receiver releases the blocked event send; the
        // runtime observes disconnection, cancels owned work, and exits.
        drop(evt_rx);
        join.join().unwrap();
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
