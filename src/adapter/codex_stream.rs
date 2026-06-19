//! Codex streaming adapter.
//!
//! Drives `codex exec --json` (and `codex exec resume <id> --json`) and
//! translates the JSONL thread events into [`AgentEvent`]s. Sessions are
//! resumable via the thread id; `--ephemeral` is never used.

use std::path::Path;

use serde_json::Value;

use crate::adapter::parser::{str_field, summarize};
use crate::adapter::process::{AdapterCommand, LineParser, StreamAdapter};
use crate::domain::event::{AgentEvent, AgentSessionId};
use crate::domain::team::{BackendKind, SandboxPolicy, TeamMember};

const TOOL_SUMMARY_MAX: usize = 160;

#[derive(Clone, Debug)]
pub struct CodexStreamAdapter {
    binary: String,
    cwd: std::path::PathBuf,
    sandbox: SandboxPolicy,
    model: Option<String>,
}

impl CodexStreamAdapter {
    pub fn from_member(member: &TeamMember, workspace: &Path) -> Self {
        Self {
            binary: "codex".to_string(),
            cwd: member.resolved_cwd(workspace),
            sandbox: member.sandbox,
            model: member.model.clone(),
        }
    }

    pub fn with_binary(mut self, binary: impl Into<String>) -> Self {
        self.binary = binary.into();
        self
    }

    fn base_flags(&self) -> Vec<String> {
        let mut flags = vec![
            "--json".to_string(),
            "--color".to_string(),
            "never".to_string(),
            "--skip-git-repo-check".to_string(),
            "-C".to_string(),
            self.cwd.display().to_string(),
            "-s".to_string(),
            self.sandbox.codex_arg().to_string(),
        ];
        if let Some(model) = &self.model {
            flags.push("-m".to_string());
            flags.push(model.clone());
        }
        flags
    }
}

impl StreamAdapter for CodexStreamAdapter {
    fn backend(&self) -> BackendKind {
        BackendKind::Codex
    }

    fn build_command(&self, prompt: &str, session: Option<&AgentSessionId>) -> AdapterCommand {
        let mut args = vec!["exec".to_string()];
        match session {
            Some(session) => {
                args.push("resume".to_string());
                args.extend(self.base_flags());
                args.push(session.as_str().to_string());
            }
            None => args.extend(self.base_flags()),
        }
        args.push(prompt.to_string());

        AdapterCommand {
            program: self.binary.clone(),
            args,
            cwd: self.cwd.clone(),
            stdin: None,
        }
    }

    fn parser(&self) -> Box<dyn LineParser> {
        Box::new(CodexLineParser)
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Phase {
    Started,
    Completed,
}

/// Parser for the `codex exec --json` thread-event stream.
pub struct CodexLineParser;

impl CodexLineParser {
    fn handle_item(&self, item: &Value, phase: Phase) -> Vec<AgentEvent> {
        let id = str_field(item, "id").unwrap_or_default().to_string();
        let status = str_field(item, "status").unwrap_or_default();
        match str_field(item, "type") {
            Some("agent_message") if phase == Phase::Completed => {
                vec![AgentEvent::MessageCompleted(
                    str_field(item, "text").unwrap_or_default().to_string(),
                )]
            }
            Some("reasoning") if phase == Phase::Completed => {
                vec![AgentEvent::Reasoning(
                    str_field(item, "text").unwrap_or_default().to_string(),
                )]
            }
            Some("command_execution") => {
                let summary = summarize(str_field(item, "command").unwrap_or_default(), TOOL_SUMMARY_MAX);
                match phase {
                    Phase::Started => vec![AgentEvent::ToolStarted {
                        id,
                        name: "shell".to_string(),
                        summary,
                    }],
                    Phase::Completed => {
                        let exit_ok = item.get("exit_code").and_then(Value::as_i64).unwrap_or(0) == 0;
                        vec![AgentEvent::ToolCompleted {
                            id,
                            ok: status == "completed" && exit_ok,
                            summary,
                        }]
                    }
                }
            }
            Some("file_change") => {
                let summary = file_change_summary(item);
                let ok = status == "completed";
                vec![
                    AgentEvent::ToolStarted {
                        id: id.clone(),
                        name: "apply_patch".to_string(),
                        summary: summary.clone(),
                    },
                    AgentEvent::ToolCompleted { id, ok, summary },
                ]
            }
            Some("mcp_tool_call") => {
                let name = format!(
                    "{}/{}",
                    str_field(item, "server").unwrap_or("mcp"),
                    str_field(item, "tool").unwrap_or("tool")
                );
                match phase {
                    Phase::Started => vec![AgentEvent::ToolStarted {
                        id,
                        summary: name.clone(),
                        name,
                    }],
                    Phase::Completed => vec![AgentEvent::ToolCompleted {
                        id,
                        ok: status == "completed",
                        summary: name,
                    }],
                }
            }
            Some("web_search") => {
                let query = summarize(str_field(item, "query").unwrap_or_default(), TOOL_SUMMARY_MAX);
                match phase {
                    Phase::Started => vec![AgentEvent::ToolStarted {
                        id,
                        name: "web_search".to_string(),
                        summary: query,
                    }],
                    Phase::Completed => vec![AgentEvent::ToolCompleted {
                        id,
                        ok: true,
                        summary: query,
                    }],
                }
            }
            Some("error") => vec![AgentEvent::Log(format!(
                "codex item error: {}",
                str_field(item, "message").unwrap_or_default()
            ))],
            _ => Vec::new(),
        }
    }
}

impl LineParser for CodexLineParser {
    fn parse_line(&mut self, line: &str) -> Vec<AgentEvent> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Vec::new();
        }
        let value: Value = match serde_json::from_str(trimmed) {
            Ok(value) => value,
            Err(err) => {
                return vec![AgentEvent::ParseWarning(format!(
                    "codex: invalid JSON line: {err}"
                ))];
            }
        };

        match str_field(&value, "type") {
            Some("thread.started") => match str_field(&value, "thread_id") {
                Some(id) => vec![AgentEvent::SessionDiscovered(AgentSessionId(id.to_string()))],
                None => Vec::new(),
            },
            Some("turn.started") | Some("turn.completed") => Vec::new(),
            Some("turn.failed") => vec![AgentEvent::Fatal(
                str_field(&value["error"], "message")
                    .unwrap_or("codex turn failed")
                    .to_string(),
            )],
            Some("error") => vec![AgentEvent::Fatal(
                str_field(&value, "message")
                    .unwrap_or("codex stream error")
                    .to_string(),
            )],
            Some("item.started") => self.handle_item(&value["item"], Phase::Started),
            Some("item.completed") => self.handle_item(&value["item"], Phase::Completed),
            Some("item.updated") => Vec::new(),
            Some(other) => vec![AgentEvent::Log(format!("codex event: {other}"))],
            None => vec![AgentEvent::ParseWarning(format!(
                "codex: event without type: {}",
                summarize(trimmed, 120)
            ))],
        }
    }
}

fn file_change_summary(item: &Value) -> String {
    let changes = item.get("changes").and_then(Value::as_array);
    let count = changes.map(|c| c.len()).unwrap_or(0);
    let paths = changes
        .map(|c| {
            c.iter()
                .filter_map(|change| str_field(change, "path"))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    summarize(&format!("{count} file(s): {paths}"), TOOL_SUMMARY_MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_all(lines: &[&str]) -> Vec<AgentEvent> {
        let mut parser = CodexLineParser;
        let mut out = Vec::new();
        for line in lines {
            out.extend(parser.parse_line(line));
        }
        out
    }

    #[test]
    fn fresh_command_targets_exec_json() {
        let member = TeamMember::new("builder", "Builder", BackendKind::Codex, "impl");
        let adapter = CodexStreamAdapter::from_member(&member, Path::new("/tmp/ws"));
        let command = adapter.build_command("do it", None);

        assert_eq!(command.program, "codex");
        assert_eq!(command.args[0], "exec");
        assert!(command.args.contains(&"--json".to_string()));
        assert!(command.args.windows(2).any(|w| w == ["-C", "/tmp/ws"]));
        assert!(command.args.windows(2).any(|w| w == ["-s", "read-only"]));
        assert_eq!(command.args.last().unwrap(), "do it");
        // Never ephemeral on the product path.
        assert!(!command.args.iter().any(|a| a == "--ephemeral"));
    }

    #[test]
    fn resume_command_uses_resume_subcommand_with_session() {
        let member = TeamMember::new("builder", "Builder", BackendKind::Codex, "impl");
        let adapter = CodexStreamAdapter::from_member(&member, Path::new("/tmp/ws"));
        let command =
            adapter.build_command("again", Some(&AgentSessionId("thread-9".to_string())));

        assert_eq!(&command.args[0..2], &["exec".to_string(), "resume".to_string()]);
        assert!(command.args.contains(&"thread-9".to_string()));
        assert_eq!(command.args.last().unwrap(), "again");
    }

    #[test]
    fn thread_started_yields_session() {
        let events = parse_all(&[r#"{"type":"thread.started","thread_id":"0199-uuid"}"#]);
        assert_eq!(
            events,
            vec![AgentEvent::SessionDiscovered(AgentSessionId(
                "0199-uuid".to_string()
            ))]
        );
    }

    #[test]
    fn agent_message_completes_only_on_item_completed() {
        let started = parse_all(&[
            r#"{"type":"item.started","item":{"id":"i1","type":"agent_message","text":"partial"}}"#,
        ]);
        assert!(started.is_empty());

        let completed = parse_all(&[
            r#"{"type":"item.completed","item":{"id":"i1","type":"agent_message","text":"Done."}}"#,
        ]);
        assert_eq!(completed, vec![AgentEvent::MessageCompleted("Done.".to_string())]);
    }

    #[test]
    fn command_execution_starts_and_completes() {
        let events = parse_all(&[
            r#"{"type":"item.started","item":{"id":"c1","type":"command_execution","command":"cargo test","status":"in_progress"}}"#,
            r#"{"type":"item.completed","item":{"id":"c1","type":"command_execution","command":"cargo test","aggregated_output":"ok","exit_code":0,"status":"completed"}}"#,
        ]);
        assert_eq!(
            events,
            vec![
                AgentEvent::ToolStarted {
                    id: "c1".to_string(),
                    name: "shell".to_string(),
                    summary: "cargo test".to_string(),
                },
                AgentEvent::ToolCompleted {
                    id: "c1".to_string(),
                    ok: true,
                    summary: "cargo test".to_string(),
                },
            ]
        );
    }

    #[test]
    fn failed_command_is_not_ok() {
        let events = parse_all(&[
            r#"{"type":"item.completed","item":{"id":"c2","type":"command_execution","command":"false","exit_code":1,"status":"failed"}}"#,
        ]);
        assert!(matches!(
            events.as_slice(),
            [AgentEvent::ToolCompleted { ok: false, .. }]
        ));
    }

    #[test]
    fn turn_failed_is_fatal() {
        let events = parse_all(&[
            r#"{"type":"turn.failed","error":{"message":"model error"}}"#,
        ]);
        assert_eq!(events, vec![AgentEvent::Fatal("model error".to_string())]);
    }

    #[test]
    fn reasoning_is_emitted_on_completion() {
        let events = parse_all(&[
            r#"{"type":"item.completed","item":{"id":"r1","type":"reasoning","text":"thinking"}}"#,
        ]);
        assert_eq!(events, vec![AgentEvent::Reasoning("thinking".to_string())]);
    }

    #[test]
    fn invalid_json_warns() {
        let events = parse_all(&[r#"not json"#]);
        assert!(matches!(events.as_slice(), [AgentEvent::ParseWarning(_)]));
    }
}
