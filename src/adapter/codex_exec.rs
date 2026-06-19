use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodexSandbox {
    ReadOnly,
    WorkspaceWrite,
}

impl CodexSandbox {
    fn as_arg(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CodexExecEvent {
    ThreadStarted { thread_id: String },
    TurnStarted,
    TurnCompleted,
    TurnFailed,
    AgentMessage { text: String },
    Other { event_type: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodexExecRun {
    pub events: Vec<CodexExecEvent>,
    pub final_message: Option<String>,
    pub raw_stdout: String,
    pub raw_stderr: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CodexExecError {
    InvalidJsonLine {
        line: String,
        message: String,
    },
    MissingEventType(String),
    ProcessFailed {
        status: Option<i32>,
        stdout: String,
        stderr: String,
    },
    Io(String),
}

#[derive(Clone, Debug)]
pub struct CodexExecAdapter {
    binary: String,
    cwd: PathBuf,
    sandbox: CodexSandbox,
}

pub trait CodexExecRunner: Send + Sync {
    fn run_prompt(&self, prompt: &str) -> Result<CodexExecRun, CodexExecError>;
}

impl CodexExecAdapter {
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self {
            binary: "codex".to_string(),
            cwd: cwd.into(),
            sandbox: CodexSandbox::ReadOnly,
        }
    }

    pub fn with_binary(mut self, binary: impl Into<String>) -> Self {
        self.binary = binary.into();
        self
    }

    pub fn with_sandbox(mut self, sandbox: CodexSandbox) -> Self {
        self.sandbox = sandbox;
        self
    }

    pub fn command_args(&self, prompt: &str) -> Vec<String> {
        vec![
            "exec".to_string(),
            "--json".to_string(),
            "--color".to_string(),
            "never".to_string(),
            "--ephemeral".to_string(),
            "--skip-git-repo-check".to_string(),
            "--sandbox".to_string(),
            self.sandbox.as_arg().to_string(),
            "--cd".to_string(),
            self.cwd.display().to_string(),
            prompt.to_string(),
        ]
    }

    fn execute_prompt(&self, prompt: &str) -> Result<CodexExecRun, CodexExecError> {
        let output = Command::new(&self.binary)
            .args(self.command_args(prompt))
            .current_dir(&self.cwd)
            .stdin(Stdio::null())
            .output()
            .map_err(|err| CodexExecError::Io(err.to_string()))?;

        if !output.status.success() {
            return Err(CodexExecError::ProcessFailed {
                status: output.status.code(),
                stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            });
        }

        parse_jsonl_with_raw(
            &String::from_utf8_lossy(&output.stdout),
            &String::from_utf8_lossy(&output.stderr),
        )
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }
}

impl CodexExecRunner for CodexExecAdapter {
    fn run_prompt(&self, prompt: &str) -> Result<CodexExecRun, CodexExecError> {
        self.execute_prompt(prompt)
    }
}

pub fn parse_jsonl(output: &str) -> Result<CodexExecRun, CodexExecError> {
    parse_jsonl_with_raw(output, "")
}

pub fn parse_jsonl_with_raw(output: &str, stderr: &str) -> Result<CodexExecRun, CodexExecError> {
    let mut events = Vec::new();
    let mut final_message = None;

    for line in output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let event = parse_event(line)?;
        if let CodexExecEvent::AgentMessage { text } = &event {
            final_message = Some(text.clone());
        }
        events.push(event);
    }

    Ok(CodexExecRun {
        events,
        final_message,
        raw_stdout: output.to_string(),
        raw_stderr: stderr.to_string(),
    })
}

fn parse_event(line: &str) -> Result<CodexExecEvent, CodexExecError> {
    let value: Value =
        serde_json::from_str(line).map_err(|err| CodexExecError::InvalidJsonLine {
            line: line.to_string(),
            message: err.to_string(),
        })?;

    let event_type = value
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| CodexExecError::MissingEventType(line.to_string()))?;

    match event_type {
        "thread.started" => Ok(CodexExecEvent::ThreadStarted {
            thread_id: value
                .get("thread_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        }),
        "turn.started" => Ok(CodexExecEvent::TurnStarted),
        "turn.completed" => Ok(CodexExecEvent::TurnCompleted),
        "turn.failed" => Ok(CodexExecEvent::TurnFailed),
        "item.completed" => {
            let item = value.get("item").unwrap_or(&Value::Null);
            if item.get("type").and_then(Value::as_str) == Some("agent_message") {
                Ok(CodexExecEvent::AgentMessage {
                    text: item
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                })
            } else {
                Ok(CodexExecEvent::Other {
                    event_type: event_type.to_string(),
                })
            }
        }
        _ => Ok(CodexExecEvent::Other {
            event_type: event_type.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_args_use_machine_readable_exec_mode() {
        let adapter =
            CodexExecAdapter::new("/tmp/project").with_sandbox(CodexSandbox::WorkspaceWrite);

        assert_eq!(
            adapter.command_args("summarize"),
            vec![
                "exec",
                "--json",
                "--color",
                "never",
                "--ephemeral",
                "--skip-git-repo-check",
                "--sandbox",
                "workspace-write",
                "--cd",
                "/tmp/project",
                "summarize",
            ]
        );
    }

    #[test]
    fn parses_codex_jsonl_and_tracks_final_agent_message() {
        let output = r#"{"type":"thread.started","thread_id":"0199a213-81c0"}
{"type":"turn.started"}
{"type":"item.completed","item":{"id":"item_3","type":"agent_message","text":"Repo contains docs and src."}}
{"type":"turn.completed","usage":{"input_tokens":10,"output_tokens":5}}"#;

        let run = parse_jsonl(output).expect("jsonl should parse");

        assert_eq!(
            run.events,
            vec![
                CodexExecEvent::ThreadStarted {
                    thread_id: "0199a213-81c0".to_string(),
                },
                CodexExecEvent::TurnStarted,
                CodexExecEvent::AgentMessage {
                    text: "Repo contains docs and src.".to_string(),
                },
                CodexExecEvent::TurnCompleted,
            ]
        );
        assert_eq!(
            run.final_message,
            Some("Repo contains docs and src.".to_string())
        );
        assert!(run.raw_stdout.contains("thread.started"));
        assert_eq!(run.raw_stderr, "");
    }

    #[test]
    fn ignores_non_message_item_events() {
        let output = r#"{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"ls"}}"#;

        let run = parse_jsonl(output).expect("jsonl should parse");

        assert_eq!(
            run.events,
            vec![CodexExecEvent::Other {
                event_type: "item.completed".to_string(),
            }]
        );
        assert_eq!(run.final_message, None);
        assert_eq!(run.raw_stdout, output);
    }

    #[test]
    fn rejects_non_json_stdout_line() {
        let parsed = parse_jsonl("not json");

        assert!(matches!(
            parsed,
            Err(CodexExecError::InvalidJsonLine { .. })
        ));
    }

    #[test]
    #[ignore = "runs the real Codex CLI and may consume Codex usage"]
    fn real_codex_exec_smoke_test() {
        if std::env::var("ASTERLINE_RUN_CODEX_EXEC_SMOKE").as_deref() != Ok("1") {
            return;
        }

        let adapter = CodexExecAdapter::new(env!("CARGO_MANIFEST_DIR"));
        let run = adapter
            .run_prompt("Reply exactly: ASTERLINE_CODEX_EXEC_OK")
            .expect("real codex exec should complete");

        assert_eq!(
            run.final_message.as_deref(),
            Some("ASTERLINE_CODEX_EXEC_OK")
        );
    }
}
