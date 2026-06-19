use std::io;
use std::path::{Path, PathBuf};

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

    let workflow = config.build_workflow(cwd)?;
    crate::tui::run(crate::tui::TuiState::with_workflow(
        workflow,
        config.backend_label(),
    ))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppConfig {
    codex: BackendMode,
    claude: BackendMode,
    db_path: Option<PathBuf>,
    show_help: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BackendMode {
    Fake,
    Real,
    Pty,
}

impl AppConfig {
    pub fn parse<I, S>(args: I) -> io::Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut config = Self {
            codex: BackendMode::Fake,
            claude: BackendMode::Fake,
            db_path: None,
            show_help: false,
        };

        let args = args
            .into_iter()
            .map(|arg| arg.as_ref().to_string())
            .collect::<Vec<_>>();
        let mut index = 0;
        while index < args.len() {
            match args[index].as_str() {
                "--real-codex" => config.codex = BackendMode::Real,
                "--real-claude" => config.claude = BackendMode::Real,
                "--real-agents" => {
                    config.codex = BackendMode::Real;
                    config.claude = BackendMode::Real;
                }
                "--pty-codex" => config.codex = BackendMode::Pty,
                "--pty-claude" => config.claude = BackendMode::Pty,
                "--pty-agents" => {
                    config.codex = BackendMode::Pty;
                    config.claude = BackendMode::Pty;
                }
                "--db" => {
                    index += 1;
                    let Some(path) = args.get(index) else {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "--db requires a path",
                        ));
                    };
                    config.db_path = Some(PathBuf::from(path));
                }
                arg if arg.starts_with("--db=") => {
                    config.db_path = Some(PathBuf::from(&arg["--db=".len()..]));
                }
                "-h" | "--help" => config.show_help = true,
                unknown => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("unknown argument: {unknown}"),
                    ));
                }
            };
            index += 1;
        }

        Ok(config)
    }

    pub fn help() -> &'static str {
        "Usage: asterline [--real-codex] [--real-claude] [--real-agents] [--pty-codex] [--pty-claude] [--pty-agents] [--db PATH]\n\
         \n\
         Defaults to fake Codex and fake Claude. Real backends use codex exec --json and claude -p --output-format json.\n\
         PTY backends launch codex/claude through a pseudo-terminal and capture raw terminal output.\n\
         The SQLite event log defaults to .asterline/asterline.sqlite3 under the current directory."
    }

    fn build_workflow(
        &self,
        cwd: impl AsRef<Path>,
    ) -> io::Result<crate::runtime::workflow::FakeWorkflow> {
        use crate::{
            adapter::{
                claude_print::ClaudePrintAdapter,
                cli_pty::CliPtyAdapter,
                codex_exec::{CodexExecAdapter, CodexSandbox},
            },
            runtime::workflow::{WorkflowClaudeBackend, WorkflowCodexBackend},
            store::sqlite::SqliteStore,
        };

        let cwd = cwd.as_ref();
        let codex = match self.codex {
            BackendMode::Fake => WorkflowCodexBackend::fake(),
            BackendMode::Real => WorkflowCodexBackend::exec(
                CodexExecAdapter::new(cwd).with_sandbox(CodexSandbox::ReadOnly),
            ),
            BackendMode::Pty => WorkflowCodexBackend::pty(
                CliPtyAdapter::codex_interactive(cwd)
                    .with_timeout(std::time::Duration::from_secs(120)),
            ),
        };
        let claude = match self.claude {
            BackendMode::Fake => WorkflowClaudeBackend::fake(),
            BackendMode::Real => WorkflowClaudeBackend::print(ClaudePrintAdapter::new(cwd)),
            BackendMode::Pty => WorkflowClaudeBackend::pty(
                CliPtyAdapter::claude_interactive(cwd)
                    .with_timeout(std::time::Duration::from_secs(120)),
            ),
        };
        let db_path = self.database_path(cwd);
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let store = SqliteStore::open(&db_path).map_err(|err| io::Error::other(err.to_string()))?;

        crate::runtime::workflow::FakeWorkflow::with_store_and_backends(
            store,
            crate::router::relay::DEFAULT_MAX_AUTO_RELAYS,
            codex,
            claude,
        )
        .map_err(|err| io::Error::other(err.to_string()))
    }

    fn backend_label(self) -> String {
        format!(
            "codex={}, claude={}",
            self.codex.as_label(),
            self.claude.as_label()
        )
    }

    fn database_path(&self, cwd: &Path) -> PathBuf {
        self.db_path
            .clone()
            .unwrap_or_else(|| cwd.join(".asterline").join("asterline.sqlite3"))
    }
}

impl BackendMode {
    fn as_label(self) -> &'static str {
        match self {
            Self::Fake => "fake",
            Self::Real => "real",
            Self::Pty => "pty",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_uses_fake_backends() {
        let config = AppConfig::parse([] as [&str; 0]).expect("config should parse");

        assert_eq!(
            config,
            AppConfig {
                codex: BackendMode::Fake,
                claude: BackendMode::Fake,
                db_path: None,
                show_help: false,
            }
        );
        assert_eq!(config.backend_label(), "codex=fake, claude=fake");
    }

    #[test]
    fn real_agents_flag_enables_both_real_backends() {
        let config = AppConfig::parse(["--real-agents"]).expect("config should parse");

        assert_eq!(
            config,
            AppConfig {
                codex: BackendMode::Real,
                claude: BackendMode::Real,
                db_path: None,
                show_help: false,
            }
        );
        assert_eq!(config.backend_label(), "codex=real, claude=real");
    }

    #[test]
    fn pty_agents_flag_enables_both_pty_backends() {
        let config = AppConfig::parse(["--pty-agents"]).expect("config should parse");

        assert_eq!(
            config,
            AppConfig {
                codex: BackendMode::Pty,
                claude: BackendMode::Pty,
                db_path: None,
                show_help: false,
            }
        );
        assert_eq!(config.backend_label(), "codex=pty, claude=pty");
    }

    #[test]
    fn pty_flags_can_enable_one_agent_at_a_time() {
        let config =
            AppConfig::parse(["--pty-codex", "--real-claude"]).expect("config should parse");

        assert_eq!(config.backend_label(), "codex=pty, claude=real");
    }

    #[test]
    fn db_flag_sets_database_path() {
        let config =
            AppConfig::parse(["--db", "/tmp/asterline.sqlite3"]).expect("config should parse");

        assert_eq!(
            config.db_path,
            Some(PathBuf::from("/tmp/asterline.sqlite3"))
        );
    }

    #[test]
    fn db_equals_flag_sets_database_path() {
        let config =
            AppConfig::parse(["--db=/tmp/asterline.sqlite3"]).expect("config should parse");

        assert_eq!(
            config.db_path,
            Some(PathBuf::from("/tmp/asterline.sqlite3"))
        );
    }

    #[test]
    fn build_workflow_creates_sqlite_file() {
        let test_dir = std::env::temp_dir().join(format!(
            "asterline-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let db_path = test_dir.join("nested").join("events.sqlite3");
        let config =
            AppConfig::parse(["--db", db_path.to_str().unwrap()]).expect("config should parse");

        let _workflow = config
            .build_workflow(&test_dir)
            .expect("workflow should initialize");

        assert!(db_path.exists());
    }

    #[test]
    fn unknown_arg_is_rejected() {
        let err = AppConfig::parse(["--wat"]).expect_err("config should reject unknown args");

        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
}
