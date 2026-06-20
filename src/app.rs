//! Application bootstrap: parse CLI args, resolve a team (config file or a
//! default roster from detected backends), open the store, spawn the runtime,
//! and run the chat-first TUI. Exiting shuts the runtime down gracefully.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::thread::JoinHandle;

use crate::adapter::{FakeRunner, MemberRunner, runner_for};
use crate::domain::config::{detect_backends, load_team_config};
use crate::domain::event::{ChatItem, RuntimeEvent};
use crate::domain::team::TeamConfig;
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

    let prepared = match prepare(&config, cwd.as_ref())? {
        Some(prepared) => prepared,
        None => {
            eprintln!(
                "Asterline: no team config and neither `codex` nor `claude` was found on PATH.\n\
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
    } = prepared;

    println!(
        "\x1b[1;36m      _       _             _ _\n\
     / \\   __| |_ ___  _ __| (_)_ __   ___\n\
    / _ \\ / _` \\ __/ _ \\| '__| | | '_ \\ / _ \\\n\
   / ___ \\ (_| | ||  __/| |  | | | | | |  __/\n\
  /_/   \\_\\__,_|\\__\\___||_|  |_|_|_| |_|\\___|\x1b[0m\n\
  Multi-Agent Coding Console\n"
    );

    tui::run(handle, events, state)?;
    let _ = join.join();
    Ok(())
}

/// Everything needed to run the TUI, wired but not yet started.
struct Prepared {
    handle: RuntimeHandle,
    join: JoinHandle<()>,
    events: mpsc::Receiver<RuntimeEvent>,
    state: AppState,
}

/// Build the team, store, runners, and runtime. Returns `None` if no team can
/// be resolved (no config and no detected backends).
fn prepare(config: &AppConfig, cwd: &Path) -> io::Result<Option<Prepared>> {
    let workspace = config
        .workspace
        .clone()
        .unwrap_or_else(|| cwd.to_path_buf());

    let saved_team = workspace.join(".asterline").join("team.json");
    let mut team = match &config.team_path {
        Some(path) => load_team_config(path)?,
        // Reuse a previously-built roster so the builder doesn't nag every
        // launch; `--pick-team` forces re-selection.
        None if !config.pick_team && saved_team.is_file() => load_team_config(&saved_team)?,
        None => {
            let detected = detect_backends();
            if !detected.any() {
                return Ok(None);
            }
            // Let the user choose the roster from the detected backends instead
            // of silently applying a fixed default (falls back to the default
            // roster when headless / on cancel).
            match crate::tui::team_builder::run(detected, &workspace)? {
                Some(team) => {
                    // Persist the choice for next time (before protocol injection).
                    if let Some(parent) = saved_team.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    if let Ok(json) = serde_json::to_string_pretty(&team) {
                        let _ = std::fs::write(&saved_team, json);
                    }
                    team
                }
                None => return Ok(None),
            }
        }
    };
    inject_team_protocol(&mut team);

    let db_path = config
        .db_path
        .clone()
        .unwrap_or_else(|| workspace.join(".asterline").join("asterline.sqlite3"));
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let store = SqliteStore::open(&db_path).map_err(|err| io::Error::other(err.to_string()))?;

    let runners = build_runners(&team, config.fake);
    let (chat, logs) = if config.no_restore {
        (Vec::new(), Vec::new())
    } else {
        // Replay only the current conversation (the latest, or a fresh one).
        if let Ok(conversation) = store.current_conversation() {
            store.set_conversation(conversation);
        }
        // A replay failure must be visible, not a silently-blank transcript:
        // surface it as the first chat item so a schema/store problem is
        // obvious in-app instead of looking like "history was lost".
        let chat = match store.replay_chat() {
            Ok(chat) => chat,
            Err(err) => vec![ChatItem::Notice {
                text: format!("could not replay history: {err}"),
            }],
        };
        // Logs are persisted too; replay the recent tail so the logs drawer
        // isn't empty after a restart.
        let logs = store.recent_logs(4000).unwrap_or_default();
        (chat, logs)
    };
    let mut state = AppState::new(chat);
    state.seed_logs(logs);

    let (events_tx, events_rx) = mpsc::channel();
    let (handle, join) = runtime::spawn(team, store, runners, events_tx, !config.debug);

    Ok(Some(Prepared {
        handle,
        join,
        events: events_rx,
        state,
    }))
}

fn build_runners(team: &TeamConfig, fake: bool) -> Runners {
    let mut runners: Runners = HashMap::new();
    for member in &team.members {
        let runner: Arc<dyn MemberRunner> = if fake {
            Arc::new(FakeRunner::echo(member.backend))
        } else {
            Arc::from(runner_for(member, &team.workspace))
        };
        runners.insert(member.id.clone(), runner);
    }
    runners
}

/// Prepend the Asterline team protocol to each member's system prompt so agents
/// know how to message teammates with `@@team_message`.
fn inject_team_protocol(team: &mut TeamConfig) {
    let protocols: Vec<String> = team
        .members
        .iter()
        .map(|me| {
            let teammates: Vec<String> = team
                .members
                .iter()
                .filter(|other| other.id != me.id)
                .map(|other| format!("{} [{}]", other.id, other.role))
                .collect();
            build_protocol(me.id.as_str(), &teammates)
        })
        .collect();

    for (member, protocol) in team.members.iter_mut().zip(protocols) {
        member.system_prompt = Some(match member.system_prompt.take() {
            Some(existing) => format!("{protocol}\n\n{existing}"),
            None => protocol,
        });
    }
}

fn build_protocol(me: &str, teammates: &[String]) -> String {
    let mut protocol = format!(
        "You are \"{me}\", a member of an Asterline multi-agent team.\n\
         To send a message to a teammate, output a line by itself:\n\
         @@team_message {{\"to\":\"<member-id or all>\",\"body\":\"<your message>\"}}\n"
    );
    if teammates.is_empty() {
        protocol.push_str("You are the only member; there are no teammates to message.\n");
    } else {
        protocol.push_str(&format!(
            "Teammates you can message: {}.\n",
            teammates.join(", ")
        ));
    }
    protocol.push_str("All other text you write is shown to the user.");
    protocol
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
        ])
        .unwrap();
        assert_eq!(config.team_path, Some(PathBuf::from("/tmp/t.json")));
        assert_eq!(config.workspace, Some(PathBuf::from("/tmp/ws")));
        assert!(config.no_restore);
        assert!(config.fake);
    }

    #[test]
    fn parses_equals_form_and_help() {
        let config = AppConfig::parse(["--db=/tmp/x.sqlite3", "--help"]).unwrap();
        assert_eq!(config.db_path, Some(PathBuf::from("/tmp/x.sqlite3")));
        assert!(config.show_help);
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
    fn inject_protocol_lists_teammates() {
        let mut team = crate::domain::config::default_team(
            "/tmp/ws",
            crate::domain::config::DetectedBackends {
                codex: true,
                claude: true,
                gemini: false,
            },
        )
        .unwrap();
        inject_team_protocol(&mut team);
        let builder = team
            .member(&crate::domain::team::MemberId::new("builder"))
            .unwrap();
        let prompt = builder.system_prompt.as_ref().unwrap();
        assert!(prompt.contains("@@team_message"));
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
                gemini: false,
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
}
