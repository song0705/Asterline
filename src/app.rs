//! Application bootstrap: parse CLI args, resolve a team (config file or a
//! default roster from detected backends), open the store, spawn the runtime,
//! and run the chat-first TUI. Exiting shuts the runtime down gracefully.

use std::collections::HashMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::thread::JoinHandle;

use crate::adapter::{FakeRunner, MemberRunner, runner_for};
use crate::domain::config::{
    detect_backends, ensure_brainstorm_skill, ensure_team_skill, inject_team_protocol,
    load_team_config,
};
use crate::domain::event::{ChatItem, RuntimeEvent};
use crate::domain::team::TeamConfig;
use crate::fs_safety;
use crate::runtime::{self, Runners, RuntimeHandle};
use crate::store::sqlite::SqliteStore;
use crate::tui;
use crate::tui::app_state::AppState;

/// Entry point invoked from `main`.
pub fn run() -> io::Result<()> {
    run_with_args(std::env::args().skip(1), std::env::current_dir()?)
}

pub fn run_with_args<I, S>(args: I, cwd: impl AsRef<Path>) -> io::Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let config = AppConfig::parse(args)?;
    if config.show_help {
        println!("{}", AppConfig::help());
        return Ok(());
    }
    if config.update {
        #[cfg(windows)]
        println!("{}", crate::update::update_now().map_err(io::Error::other)?);
        #[cfg(not(windows))]
        println!("automatic updates are currently available for the Windows Setup installation");
        return Ok(());
    }

    let prepared = match prepare(&config, cwd.as_ref())? {
        Some(prepared) => prepared,
        None => {
            eprintln!(
                "Asterline: no team config and no supported backend CLI was found on PATH.\n\
                 Install a backend CLI, or pass --team <config.json>."
            );
            return Ok(());
        }
    };

    let Prepared {
        handle,
        join,
        events,
        state,
        instance_lock: _instance_lock,
    } = prepared;

    if config.banner {
        print_startup_banner();
    }

    // Keep an independent shutdown handle so terminal-initialization errors
    // inside `tui::run` cannot leave the runtime waiting forever. The TUI also
    // sends Shutdown on its normal cleanup path; a duplicate send is harmless.
    let shutdown_handle = handle.clone();
    let tui_result = tui::run(handle, events, state);
    shutdown_handle.shutdown();
    let runtime_result = join_runtime(join);
    tui_result.and(runtime_result)
}

fn join_runtime(join: JoinHandle<()>) -> io::Result<()> {
    join.join()
        .map_err(|_| io::Error::other("Asterline runtime thread panicked"))
}

fn print_startup_banner() {
    println!("\x1b[1;36mAsterline\x1b[0m · Multi-Agent Coding Console");
}

/// Everything needed to run the TUI, wired but not yet started.
struct Prepared {
    handle: RuntimeHandle,
    join: JoinHandle<()>,
    events: mpsc::Receiver<RuntimeEvent>,
    state: AppState,
    instance_lock: InstanceLock,
}

/// An OS-backed exclusive lock for one SQLite store. The marker file remains
/// after exit, but the lock itself is released automatically with the handle.
struct InstanceLock {
    _file: std::fs::File,
}

impl InstanceLock {
    fn acquire(db_path: &Path) -> io::Result<Self> {
        let mut lock_name = db_path.as_os_str().to_os_string();
        lock_name.push(".lock");
        let lock_path = PathBuf::from(lock_name);
        fs_safety::ensure_private_regular_file(&lock_path, "instance lock")?;
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(0);
        }
        let mut file = options.open(&lock_path).map_err(|err| {
            io::Error::new(
                err.kind(),
                format!(
                    "could not lock {} (another Asterline instance may be using this store): \
                     {err}",
                    db_path.display()
                ),
            )
        })?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                let err = io::Error::last_os_error();
                return Err(io::Error::new(
                    err.kind(),
                    format!(
                        "another Asterline instance is already using {}: {err}",
                        db_path.display()
                    ),
                ));
            }
        }
        file.set_len(0)?;
        writeln!(file, "pid={}", std::process::id())?;
        Ok(Self { _file: file })
    }
}

/// Build the team, store, runners, and runtime. Returns `None` if no team can
/// be resolved (no config and no detected backends).
fn prepare(config: &AppConfig, cwd: &Path) -> io::Result<Option<Prepared>> {
    let requested_workspace = config
        .workspace
        .clone()
        .unwrap_or_else(|| cwd.to_path_buf());

    let requested_state_dir =
        fs_safety::ensure_workspace_directory(&requested_workspace, &[".asterline"], true)?;
    let saved_team = requested_state_dir.join("team.json");
    // Only an implicitly reused saved roster participates in conversation
    // snapshot restoration. An explicit --team file or a freshly picked team
    // is a launch-time choice and must not be overwritten by an older DB.
    let restore_saved_roster = config.team_path.is_none()
        && !config.pick_team
        && fs_safety::regular_file_exists(&saved_team, "saved team config")?;
    let mut team = match &config.team_path {
        Some(path) => load_team_config(path)?,
        // Reuse a previously-built roster so the builder doesn't nag every
        // launch; `--pick-team` forces re-selection.
        None if restore_saved_roster => load_team_config(&saved_team)?,
        None => {
            let detected = detect_backends();
            if !detected.any() {
                return Ok(None);
            }
            // Let the user choose the roster from the detected backends instead
            // of silently applying a fixed default (falls back to the default
            // roster when headless / on cancel).
            match crate::tui::team_builder::run(detected, &requested_workspace)? {
                Some(team) => {
                    // Persist the choice for next time (before protocol injection).
                    runtime::save_team_config(&saved_team, &team)?;
                    team
                }
                None => return Ok(None),
            }
        }
    };
    // A CLI workspace is an explicit launch-time override. Without one, the
    // team file's workspace is canonical for runners, skills, and the default
    // database location.
    if let Some(workspace) = &config.workspace {
        team.workspace = workspace.clone();
    }
    let workspace = team.workspace.clone();
    ensure_team_skill(&team.workspace)?;
    ensure_brainstorm_skill(&team.workspace)?;

    let db_path = match &config.db_path {
        Some(path) => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            path.clone()
        }
        None => fs_safety::ensure_workspace_directory(&workspace, &[".asterline"], true)?
            .join("asterline.sqlite3"),
    };
    fs_safety::ensure_private_regular_file(&db_path, "SQLite database")?;
    let instance_lock = InstanceLock::acquire(&db_path)?;
    let store = SqliteStore::open(&db_path).map_err(|err| io::Error::other(err.to_string()))?;

    if !config.no_restore && restore_saved_roster {
        team = store
            .restore_active_team_config(&team)
            .map_err(|err| io::Error::other(err.to_string()))?;
    }
    inject_team_protocol(&mut team);

    let runners = build_runners(&team, config.fake);
    let (chat, logs) = if config.no_restore {
        (Vec::new(), Vec::new())
    } else {
        // Replay only the current conversation (the latest, or a fresh one).
        if let Ok(conversation) = store.current_conversation() {
            store
                .set_conversation(conversation)
                .map_err(|err| io::Error::other(err.to_string()))?;
        }
        // A replay failure must be visible, not a silently-blank transcript:
        // surface it as the first chat item so a schema/store problem is
        // obvious in-app instead of looking like "history was lost".
        let mut chat = match store.replay_chat() {
            Ok(chat) => chat,
            Err(err) => vec![ChatItem::Notice {
                text: format!("could not replay history: {err}"),
            }],
        };
        // Logs are persisted too; replay the recent tail so the logs drawer
        // isn't empty after a restart.
        let logs = match store.recent_logs(4000) {
            Ok(logs) => logs,
            Err(err) => {
                chat.push(ChatItem::Notice {
                    text: format!("could not replay logs: {err}"),
                });
                Vec::new()
            }
        };
        (chat, logs)
    };
    let mut state = AppState::new(chat);
    state.seed_logs(logs);

    // Bound the runtime-to-TUI stream so a fast or malformed backend cannot
    // turn a slow terminal renderer into an unbounded in-memory queue.
    let (events_tx, events_rx) = mpsc::sync_channel(2_048);
    #[cfg(windows)]
    if !config.no_auto_update {
        crate::update::spawn_auto_update(events_tx.clone());
    }
    let team_save_path = config.team_path.clone().unwrap_or(saved_team);
    let (handle, join) = runtime::spawn_bounded(
        team,
        store,
        runners,
        events_tx,
        !config.debug,
        config.fake,
        Some(team_save_path),
    );

    Ok(Some(Prepared {
        handle,
        join,
        events: events_rx,
        state,
        instance_lock,
    }))
}

fn build_runners(team: &TeamConfig, fake: bool) -> Runners {
    let mut runners: Runners = HashMap::new();
    for member in &team.members {
        let runner: Arc<dyn MemberRunner> = if fake {
            Arc::new(FakeRunner::team(member.backend))
        } else {
            Arc::from(runner_for(member, &team.workspace))
        };
        runners.insert(member.id.clone(), runner);
    }
    runners
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AppConfig {
    team_path: Option<PathBuf>,
    workspace: Option<PathBuf>,
    db_path: Option<PathBuf>,
    no_restore: bool,
    debug: bool,
    fake: bool,
    pick_team: bool,
    banner: bool,
    no_auto_update: bool,
    update: bool,
    show_help: bool,
}

impl AppConfig {
    pub fn parse<I, S>(args: I) -> io::Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut config = AppConfig::default();
        let args: Vec<String> = args.into_iter().map(|a| a.as_ref().to_string()).collect();
        let mut index = 0;
        while index < args.len() {
            let arg = args[index].as_str();
            match arg {
                "--team" => {
                    config.team_path = Some(Self::value(&args, &mut index, "--team")?.into())
                }
                "--workspace" => {
                    config.workspace = Some(Self::value(&args, &mut index, "--workspace")?.into())
                }
                "--db" => config.db_path = Some(Self::value(&args, &mut index, "--db")?.into()),
                "--no-restore" => config.no_restore = true,
                "--debug" => config.debug = true,
                "--fake" => config.fake = true,
                "--pick-team" => config.pick_team = true,
                "--banner" => config.banner = true,
                "--no-auto-update" => config.no_auto_update = true,
                "--update" => config.update = true,
                "-h" | "--help" => config.show_help = true,
                _ if arg.starts_with("--team=") => {
                    config.team_path = Some(arg["--team=".len()..].into())
                }
                _ if arg.starts_with("--workspace=") => {
                    config.workspace = Some(arg["--workspace=".len()..].into())
                }
                _ if arg.starts_with("--db=") => config.db_path = Some(arg["--db=".len()..].into()),
                unknown => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("unknown argument: {unknown}"),
                    ));
                }
            }
            index += 1;
        }
        Ok(config)
    }

    fn value(args: &[String], index: &mut usize, flag: &str) -> io::Result<String> {
        *index += 1;
        args.get(*index).cloned().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{flag} requires a value"),
            )
        })
    }

    pub fn help() -> &'static str {
        "Asterline — a chat-first multi-agent coding console.\n\
         \n\
         Usage: asterline [OPTIONS]\n\
         \n\
         Options:\n\
         \x20 --team <PATH>       Load a team config (JSON). Skips the team builder.\n\
         \x20 --pick-team         Re-open the interactive team builder (ignore the saved team).\n\
         \x20 --workspace <PATH>  Working directory for members. Default: current directory.\n\
         \x20 --db <PATH>         SQLite path. Default: <workspace>/.asterline/asterline.sqlite3.\n\
         \x20 --no-restore        Do not replay persisted chat history on startup.\n\
         \x20 --debug             Disable the approval gate (developer mode).\n\
         \x20 --fake              Use offline fake agents instead of real CLIs.\n\
         \x20 --banner            Print a compact startup banner before the TUI.\n\
         \x20 --update            Check now and schedule a Windows installer update.\n\
         \x20 --no-auto-update    Skip the Windows installer update check.\n\
         \x20 -h, --help          Show this help.\n\
         \n\
         With no --team, Asterline opens a team builder from the detected backends\n\
         and remembers your choice in <workspace>/.asterline/team.json."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::event::{MessageTarget, UiCommand};
    use std::time::Duration;

    #[test]
    fn parses_flags() {
        let config = AppConfig::parse([
            "--team",
            "/tmp/t.json",
            "--workspace",
            "/tmp/ws",
            "--no-restore",
            "--fake",
            "--banner",
            "--no-auto-update",
        ])
        .unwrap();
        assert_eq!(config.team_path, Some(PathBuf::from("/tmp/t.json")));
        assert_eq!(config.workspace, Some(PathBuf::from("/tmp/ws")));
        assert!(config.no_restore);
        assert!(config.fake);
        assert!(config.banner);
        assert!(config.no_auto_update);
    }

    #[test]
    fn parses_equals_form_and_help() {
        let config = AppConfig::parse(["--db=/tmp/x.sqlite3", "--help"]).unwrap();
        assert_eq!(config.db_path, Some(PathBuf::from("/tmp/x.sqlite3")));
        assert!(config.show_help);
        assert!(!config.banner);
        assert!(!config.update);
    }

    #[test]
    fn help_mentions_compact_banner_flag() {
        assert!(AppConfig::help().contains("--banner"));
        assert!(AppConfig::help().contains("compact startup banner"));
        assert!(AppConfig::help().contains("--update"));
        assert!(AppConfig::help().contains("--no-auto-update"));
    }

    #[test]
    fn parses_manual_update_flag() {
        let config = AppConfig::parse(["--update"]).unwrap();
        assert!(config.update);
        assert!(!config.no_auto_update);
    }

    #[test]
    fn unknown_arg_rejected() {
        assert!(AppConfig::parse(["--nope"]).is_err());
    }

    #[test]
    fn missing_value_rejected() {
        assert!(AppConfig::parse(["--team"]).is_err());
    }

    #[test]
    fn runtime_thread_panic_is_reported_as_an_io_error() {
        let join = std::thread::spawn(|| panic!("runtime failure"));
        let result = join_runtime(join);

        assert_eq!(
            result.expect_err("panic must be visible").to_string(),
            "Asterline runtime thread panicked"
        );
    }

    #[test]
    fn store_instance_lock_is_exclusive_and_released_on_drop() {
        let dir =
            std::env::temp_dir().join(format!("asterline-instance-lock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("state.sqlite3");

        let first = InstanceLock::acquire(&db).unwrap();
        let error = InstanceLock::acquire(&db)
            .err()
            .expect("a second instance must be rejected");
        assert!(error.to_string().contains("another Asterline instance"));

        drop(first);
        InstanceLock::acquire(&db).expect("lock must be released when the owner exits");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn instance_lock_rejects_a_symlink_without_touching_its_target() {
        use std::os::unix::fs::symlink;

        let dir = std::env::temp_dir().join(format!(
            "asterline-instance-lock-symlink-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("state.sqlite3");
        let victim = dir.join("victim.txt");
        std::fs::write(&victim, "do not truncate").unwrap();
        symlink(&victim, format!("{}.lock", db.display())).unwrap();

        assert!(InstanceLock::acquire(&db).is_err());
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "do not truncate");

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn inject_protocol_lists_teammates() {
        let mut team = crate::domain::config::default_team(
            "/tmp/ws",
            crate::domain::config::DetectedBackends {
                codex: true,
                claude: true,
                grok: false,
                agy: false,
            },
        )
        .unwrap();
        inject_team_protocol(&mut team);
        let builder = team
            .member(&crate::domain::team::MemberId::new("builder"))
            .unwrap();
        let prompt = builder.system_prompt.as_ref().unwrap();
        assert!(prompt.contains("$asterline-team"));
        assert!(prompt.contains("reviewer"));
    }

    #[test]
    fn prepare_with_fake_backend_runs_a_turn() {
        let dir = std::env::temp_dir().join(format!("asterline-app-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Force a single-codex default team via a written config so the test is
        // independent of what is installed on PATH.
        let team = crate::domain::config::default_team(
            &dir,
            crate::domain::config::DetectedBackends {
                codex: true,
                claude: false,
                grok: false,
                agy: false,
            },
        )
        .unwrap();
        let team_path = dir.join("team.json");
        std::fs::write(&team_path, serde_json::to_string(&team).unwrap()).unwrap();

        let config = AppConfig::parse([
            "--team",
            team_path.to_str().unwrap(),
            "--db",
            dir.join("db.sqlite3").to_str().unwrap(),
            "--fake",
        ])
        .unwrap();

        let prepared = prepare(&config, &dir).unwrap().expect("prepared");
        assert!(
            dir.join(crate::domain::config::ASTERLINE_TEAM_SKILL_PATH)
                .is_file()
        );
        let Prepared {
            handle,
            join,
            events,
            ..
        } = prepared;

        // Drain the Ready event.
        let ready = events.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(matches!(ready, RuntimeEvent::Ready { .. }));

        handle.send(UiCommand::UserMessage {
            target: MessageTarget::Default,
            body: "hello".to_string(),
        });

        let mut saw_completed = false;
        while let Ok(event) = events.recv_timeout(Duration::from_secs(2)) {
            if let RuntimeEvent::MessageCompleted { text, .. } = &event
                && text.contains("hello")
            {
                saw_completed = true;
            }
            if matches!(event, RuntimeEvent::TurnFinished { .. }) {
                break;
            }
        }
        assert!(saw_completed);

        handle.send(UiCommand::Shutdown);
        let _ = join.join();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn prepare_restores_conversation_effort_before_building_runners() {
        let dir = std::env::temp_dir().join(format!(
            "asterline-app-restore-effort-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut launch = crate::domain::config::default_team(
            &dir,
            crate::domain::config::DetectedBackends {
                codex: true,
                claude: false,
                grok: false,
                agy: false,
            },
        )
        .unwrap();
        launch.members[0].effort = Some(crate::domain::team::Effort::Low);
        let team_path = dir.join(".asterline").join("team.json");
        std::fs::create_dir_all(team_path.parent().unwrap()).unwrap();
        runtime::save_team_config(&team_path, &launch).unwrap();

        let db_path = dir.join("db.sqlite3");
        let store = SqliteStore::open(&db_path).unwrap();
        store.current_conversation().unwrap();
        let mut saved = launch.clone();
        saved.members[0].effort = Some(crate::domain::team::Effort::High);
        store
            .save_conversation_snapshot(&saved, &[], crate::domain::TerminalMode::Normal)
            .unwrap();
        drop(store);

        let config = AppConfig::parse([
            "--workspace",
            dir.to_str().unwrap(),
            "--db",
            db_path.to_str().unwrap(),
            "--fake",
        ])
        .unwrap();
        let prepared = prepare(&config, &dir).unwrap().expect("prepared");
        let ready = prepared
            .events
            .recv_timeout(Duration::from_secs(2))
            .expect("ready");
        assert!(matches!(
            ready,
            RuntimeEvent::Ready { members, .. }
                if members.first().and_then(|member| member.effort)
                    == Some(crate::domain::team::Effort::High)
        ));

        prepared.handle.send(UiCommand::Shutdown);
        prepared.join.join().unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn explicit_team_is_not_overwritten_by_active_conversation_snapshot() {
        let dir = std::env::temp_dir().join(format!(
            "asterline-app-explicit-team-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut explicit = crate::domain::config::default_team(
            &dir,
            crate::domain::config::DetectedBackends {
                codex: true,
                claude: false,
                grok: false,
                agy: false,
            },
        )
        .unwrap();
        explicit.members[0].effort = Some(crate::domain::team::Effort::Low);
        let team_path = dir.join("explicit-team.json");
        runtime::save_team_config(&team_path, &explicit).unwrap();

        let db_path = dir.join("db.sqlite3");
        let store = SqliteStore::open(&db_path).unwrap();
        store.current_conversation().unwrap();
        let mut stale = explicit.clone();
        stale.members[0].effort = Some(crate::domain::team::Effort::High);
        stale.members[0].display_name = "Stale snapshot member".to_string();
        store
            .save_conversation_snapshot(&stale, &[], crate::domain::TerminalMode::Normal)
            .unwrap();
        drop(store);

        let config = AppConfig::parse([
            "--team",
            team_path.to_str().unwrap(),
            "--db",
            db_path.to_str().unwrap(),
            "--fake",
        ])
        .unwrap();
        let prepared = prepare(&config, &dir).unwrap().expect("prepared");
        let ready = prepared
            .events
            .recv_timeout(Duration::from_secs(2))
            .expect("ready");
        assert!(matches!(
            ready,
            RuntimeEvent::Ready { members, .. }
                if members.first().is_some_and(|member|
                    member.effort == Some(crate::domain::team::Effort::Low)
                        && member.display_name != "Stale snapshot member")
        ));

        prepared.handle.send(UiCommand::Shutdown);
        prepared.join.join().unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn team_workspace_is_canonical_for_the_default_database() {
        let root = std::env::temp_dir().join(format!(
            "asterline-app-team-workspace-{}",
            std::process::id()
        ));
        let invocation = root.join("invocation");
        let team_workspace = root.join("team-workspace");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&invocation).unwrap();
        std::fs::create_dir_all(&team_workspace).unwrap();

        let team = crate::domain::config::default_team(
            &team_workspace,
            crate::domain::config::DetectedBackends {
                codex: true,
                claude: false,
                grok: false,
                agy: false,
            },
        )
        .unwrap();
        let team_path = root.join("team.json");
        std::fs::write(&team_path, serde_json::to_string(&team).unwrap()).unwrap();
        let config = AppConfig::parse([
            "--team",
            team_path.to_str().unwrap(),
            "--fake",
            "--no-restore",
        ])
        .unwrap();

        let prepared = prepare(&config, &invocation).unwrap().expect("prepared");
        assert!(
            team_workspace
                .join(".asterline/asterline.sqlite3")
                .is_file()
        );
        assert!(!invocation.join(".asterline/asterline.sqlite3").exists());
        let ready = prepared
            .events
            .recv_timeout(Duration::from_secs(2))
            .expect("ready");
        assert!(matches!(
            ready,
            RuntimeEvent::Ready { workspace, .. }
                if workspace == team_workspace.display().to_string()
        ));

        prepared.handle.send(UiCommand::Shutdown);
        prepared.join.join().unwrap();
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn cli_workspace_overrides_the_team_file_workspace() {
        let root = std::env::temp_dir().join(format!(
            "asterline-app-cli-workspace-{}",
            std::process::id()
        ));
        let declared_workspace = root.join("declared-workspace");
        let cli_workspace = root.join("cli-workspace");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&declared_workspace).unwrap();
        std::fs::create_dir_all(&cli_workspace).unwrap();

        let team = crate::domain::config::default_team(
            &declared_workspace,
            crate::domain::config::DetectedBackends {
                codex: true,
                claude: false,
                grok: false,
                agy: false,
            },
        )
        .unwrap();
        let team_path = root.join("team.json");
        std::fs::write(&team_path, serde_json::to_string(&team).unwrap()).unwrap();
        let config = AppConfig::parse([
            "--team",
            team_path.to_str().unwrap(),
            "--workspace",
            cli_workspace.to_str().unwrap(),
            "--fake",
            "--no-restore",
        ])
        .unwrap();

        let prepared = prepare(&config, &root).unwrap().expect("prepared");
        assert!(cli_workspace.join(".asterline/asterline.sqlite3").is_file());
        assert!(!declared_workspace.join(".asterline").exists());
        let ready = prepared
            .events
            .recv_timeout(Duration::from_secs(2))
            .expect("ready");
        assert!(matches!(
            ready,
            RuntimeEvent::Ready { workspace, .. }
                if workspace == cli_workspace.display().to_string()
        ));

        prepared.handle.send(UiCommand::Shutdown);
        prepared.join.join().unwrap();
        std::fs::remove_dir_all(&root).ok();
    }
}
