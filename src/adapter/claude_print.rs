use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaudePrintRun {
    pub session_id: Option<String>,
    pub result: String,
    pub is_error: bool,
    pub raw_stdout: String,
    pub raw_stderr: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClaudePrintError {
    InvalidJson {
        message: String,
    },
    MissingResult,
    ProcessFailed {
        status: Option<i32>,
        stdout: String,
        stderr: String,
    },
    Io(String),
}

#[derive(Clone, Debug)]
pub struct ClaudePrintAdapter {
    binary: String,
    cwd: PathBuf,
}

pub trait ClaudePrintRunner: Send + Sync {
    fn run_prompt(&self, prompt: &str) -> Result<ClaudePrintRun, ClaudePrintError>;
}

impl ClaudePrintAdapter {
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self {
            binary: "claude".to_string(),
            cwd: cwd.into(),
        }
    }

    pub fn with_binary(mut self, binary: impl Into<String>) -> Self {
        self.binary = binary.into();
        self
    }

    pub fn command_args(&self, prompt: &str) -> Vec<String> {
        vec![
            "-p".to_string(),
            "--output-format".to_string(),
            "json".to_string(),
            "--no-session-persistence".to_string(),
            "--permission-mode".to_string(),
            "plan".to_string(),
            prompt.to_string(),
        ]
    }

    fn execute_prompt(&self, prompt: &str) -> Result<ClaudePrintRun, ClaudePrintError> {
        let output = Command::new(&self.binary)
            .args(self.command_args(prompt))
            .current_dir(&self.cwd)
            .stdin(Stdio::null())
            .output()
            .map_err(|err| ClaudePrintError::Io(err.to_string()))?;

        if !output.status.success() {
            return Err(ClaudePrintError::ProcessFailed {
                status: output.status.code(),
                stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            });
        }

        parse_json_with_raw(
            &String::from_utf8_lossy(&output.stdout),
            &String::from_utf8_lossy(&output.stderr),
        )
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }
}

impl ClaudePrintRunner for ClaudePrintAdapter {
    fn run_prompt(&self, prompt: &str) -> Result<ClaudePrintRun, ClaudePrintError> {
        self.execute_prompt(prompt)
    }
}

pub fn parse_json(output: &str) -> Result<ClaudePrintRun, ClaudePrintError> {
    parse_json_with_raw(output, "")
}

pub fn parse_json_with_raw(output: &str, stderr: &str) -> Result<ClaudePrintRun, ClaudePrintError> {
    let value: Value =
        serde_json::from_str(output).map_err(|err| ClaudePrintError::InvalidJson {
            message: err.to_string(),
        })?;

    let result = value
        .get("result")
        .and_then(Value::as_str)
        .ok_or(ClaudePrintError::MissingResult)?
        .to_string();

    Ok(ClaudePrintRun {
        session_id: value
            .get("session_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        result,
        is_error: value
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        raw_stdout: output.to_string(),
        raw_stderr: stderr.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_args_use_machine_readable_print_mode() {
        let adapter = ClaudePrintAdapter::new("/tmp/project");

        assert_eq!(
            adapter.command_args("summarize"),
            vec![
                "-p",
                "--output-format",
                "json",
                "--no-session-persistence",
                "--permission-mode",
                "plan",
                "summarize",
            ]
        );
    }

    #[test]
    fn parses_claude_json_result() {
        let output = r#"{
            "type": "result",
            "subtype": "success",
            "is_error": false,
            "session_id": "abc-123",
            "result": "ASTERLINE_CLAUDE_PRINT_OK",
            "total_cost_usd": 0.001
        }"#;

        let run = parse_json(output).expect("json should parse");

        assert_eq!(
            run,
            ClaudePrintRun {
                session_id: Some("abc-123".to_string()),
                result: "ASTERLINE_CLAUDE_PRINT_OK".to_string(),
                is_error: false,
                raw_stdout: output.to_string(),
                raw_stderr: "".to_string(),
            }
        );
    }

    #[test]
    fn rejects_missing_result() {
        let parsed = parse_json(r#"{"type":"result","is_error":false}"#);

        assert_eq!(parsed, Err(ClaudePrintError::MissingResult));
    }

    #[test]
    #[ignore = "runs the real Claude Code CLI and may consume Claude usage"]
    fn real_claude_print_smoke_test() {
        if std::env::var("ASTERLINE_RUN_CLAUDE_PRINT_SMOKE").as_deref() != Ok("1") {
            return;
        }

        let adapter = ClaudePrintAdapter::new(env!("CARGO_MANIFEST_DIR"));
        let run = adapter
            .run_prompt("Reply exactly: ASTERLINE_CLAUDE_PRINT_OK")
            .expect("real claude print should complete");

        assert_eq!(run.result.trim(), "ASTERLINE_CLAUDE_PRINT_OK");
    }
}
