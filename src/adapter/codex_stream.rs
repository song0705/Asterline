//! Codex streaming adapter.
//!
//! Drives `codex exec --json` (and `codex exec --json resume <id>`) and
//! translates the JSONL thread events into [`AgentEvent`]s. Sessions are
//! resumable via the thread id; `--ephemeral` is never used.

use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;

use crate::adapter::parser::{
    MAX_MESSAGE_TEXT_BYTES, bounded_text, str_field, summarize, tool_detail, tool_value,
};
use crate::adapter::process::{AdapterCommand, LineParser, StreamAdapter};
use crate::domain::event::{AgentEvent, AgentSessionId};
use crate::domain::team::{BackendKind, Effort, SandboxPolicy, TeamMember};

const TOOL_SUMMARY_MAX: usize = 160;
const TOOL_OUTPUT_MAX: usize = 32_000;

#[derive(Clone, Debug)]
pub struct CodexStreamAdapter {
    binary: String,
    cwd: std::path::PathBuf,
    sandbox: SandboxPolicy,
    model: Option<String>,
    system_prompt: Option<String>,
}

impl CodexStreamAdapter {
    pub fn from_member(member: &TeamMember, workspace: &Path) -> Self {
        Self {
            binary: "codex".to_string(),
            cwd: member.resolved_cwd(workspace),
            sandbox: member.sandbox,
            model: member.model.clone(),
            system_prompt: member.system_prompt.clone(),
        }
    }

    pub fn with_binary(mut self, binary: impl Into<String>) -> Self {
        self.binary = binary.into();
        self
    }

    /// Exec-level flags that must precede the optional `resume` subcommand.
    fn common_flags(&self) -> Vec<String> {
        let mut flags = vec!["--json".to_string(), "--skip-git-repo-check".to_string()];
        if let Some(model) = &self.model {
            flags.push("-m".to_string());
            flags.push(model.clone());
        }
        if let Some(instructions) = self
            .system_prompt
            .as_deref()
            .map(str::trim)
            .filter(|instructions| !instructions.is_empty())
        {
            flags.push("-c".to_string());
            flags.push(format!(
                "developer_instructions={}",
                serde_json::to_string(instructions)
                    .expect("serializing a Rust string to JSON cannot fail")
            ));
        }
        flags
    }
}

impl StreamAdapter for CodexStreamAdapter {
    fn backend(&self) -> BackendKind {
        BackendKind::Codex
    }

    fn build_command(
        &self,
        prompt: &str,
        session: Option<&AgentSessionId>,
        effort: Option<Effort>,
    ) -> AdapterCommand {
        let mut args = vec!["exec".to_string()];
        args.extend(self.common_flags());
        // `-C` and `-s` are exec-level options. Current Codex accepts them for
        // resumed runs only before the `resume` subcommand; placing them here
        // also makes member confinement changes override persisted session state.
        args.push("-C".to_string());
        args.push(self.cwd.display().to_string());
        args.push("-s".to_string());
        args.push(self.sandbox.codex_arg().to_string());
        if let Some(effort) = effort {
            args.push("-c".to_string());
            args.push(format!("model_reasoning_effort={}", effort.codex_value()));
        }
        if let Some(session) = session {
            args.push("resume".to_string());
            args.push(session.as_str().to_string());
        }
        // Codex documents `-` as the prompt sentinel for both fresh and
        // resumed exec runs. Keeping user text on stdin also avoids exposing it
        // to Windows batch-wrapper command-line parsing.
        args.push("-".to_string());

        AdapterCommand {
            program: self.binary.clone(),
            args,
            cwd: self.cwd.clone(),
            stdin: Some(prompt.to_string()),
        }
    }

    fn parser(&self) -> Box<dyn LineParser> {
        Box::new(CodexLineParser::default())
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Phase {
    Started,
    Completed,
}

/// Parser for the `codex exec --json` thread-event stream.
#[derive(Default)]
pub struct CodexLineParser {
    command_output: HashMap<String, String>,
    pending_message: Option<String>,
    turn_completed: bool,
}

impl CodexLineParser {
    fn handle_item(&mut self, item: &Value, phase: Phase) -> Vec<AgentEvent> {
        let id = str_field(item, "id").unwrap_or_default().to_string();
        let status = str_field(item, "status").unwrap_or_default();
        match str_field(item, "type") {
            Some("agent_message") if phase == Phase::Completed => {
                // `codex exec --json` currently omits the message phase, so an
                // item may be commentary rather than the final answer. Keep the
                // latest candidate and commit it only at `turn.completed`.
                self.pending_message = Some(bounded_text(
                    str_field(item, "text").unwrap_or_default(),
                    MAX_MESSAGE_TEXT_BYTES,
                ));
                Vec::new()
            }
            Some("reasoning") if phase == Phase::Completed => {
                vec![AgentEvent::Reasoning(
                    str_field(item, "text").unwrap_or_default().to_string(),
                )]
            }
            Some("command_execution") => {
                let summary = summarize(
                    str_field(item, "command").unwrap_or_default(),
                    TOOL_SUMMARY_MAX,
                );
                match phase {
                    Phase::Started => {
                        self.command_output.insert(id.clone(), String::new());
                        vec![AgentEvent::ToolStarted {
                            id,
                            name: "shell".to_string(),
                            summary,
                        }]
                    }
                    Phase::Completed => {
                        let exit_ok =
                            item.get("exit_code").and_then(Value::as_i64).unwrap_or(0) == 0;
                        let mut output = tool_detail(
                            str_field(item, "aggregated_output").unwrap_or_default(),
                            TOOL_OUTPUT_MAX,
                        );
                        if output.is_empty() && (!exit_ok || status != "completed") {
                            output = item
                                .get("exit_code")
                                .and_then(Value::as_i64)
                                .map(|code| format!("command failed with exit code {code}"))
                                .unwrap_or_else(|| "command failed".to_string());
                        }
                        self.command_output.remove(&id);
                        vec![AgentEvent::ToolCompleted {
                            id,
                            ok: status == "completed" && exit_ok,
                            summary: output,
                        }]
                    }
                }
            }
            Some("file_change") if phase == Phase::Completed => {
                let ok = status == "completed";
                vec![AgentEvent::FileChange {
                    files: file_change_files(item),
                    ok,
                }]
            }
            Some("mcp_tool_call") => {
                let name = format!(
                    "{}/{}",
                    str_field(item, "server").unwrap_or("mcp"),
                    str_field(item, "tool").unwrap_or("tool")
                );
                match phase {
                    Phase::Started => {
                        let input = item
                            .get("arguments")
                            .or_else(|| item.get("input"))
                            .map(|value| {
                                summarize(&tool_value(value, TOOL_SUMMARY_MAX), TOOL_SUMMARY_MAX)
                            })
                            .filter(|value| !value.is_empty())
                            .unwrap_or_else(|| name.clone());
                        vec![AgentEvent::ToolStarted {
                            id,
                            summary: input,
                            name,
                        }]
                    }
                    Phase::Completed => vec![AgentEvent::ToolCompleted {
                        id,
                        ok: status == "completed",
                        summary: item
                            .get("result")
                            .map(|result| tool_value(result, TOOL_OUTPUT_MAX))
                            .filter(|result| !result.is_empty())
                            .or_else(|| str_field(&item["error"], "message").map(str::to_string))
                            .unwrap_or(name),
                    }],
                }
            }
            Some("collab_tool_call") => {
                let name = str_field(item, "tool").unwrap_or("collab").to_string();
                match phase {
                    Phase::Started => {
                        let summary = str_field(item, "prompt")
                            .map(|prompt| summarize(prompt, TOOL_SUMMARY_MAX))
                            .filter(|prompt| !prompt.is_empty())
                            .unwrap_or_else(|| name.clone());
                        vec![AgentEvent::ToolStarted { id, name, summary }]
                    }
                    Phase::Completed => vec![AgentEvent::ToolCompleted {
                        id,
                        ok: status == "completed",
                        summary: item
                            .get("agents_states")
                            .map(|states| tool_value(states, TOOL_OUTPUT_MAX))
                            .filter(|states| !states.is_empty())
                            .unwrap_or(name),
                    }],
                }
            }
            Some("web_search") => {
                let query = summarize(
                    str_field(item, "query").unwrap_or_default(),
                    TOOL_SUMMARY_MAX,
                );
                match phase {
                    Phase::Started => vec![AgentEvent::ToolStarted {
                        id,
                        name: "web_search".to_string(),
                        summary: query,
                    }],
                    Phase::Completed => vec![AgentEvent::ToolCompleted {
                        id,
                        ok: status != "failed",
                        summary: item
                            .get("results")
                            .or_else(|| item.get("result"))
                            .map(|result| tool_value(result, TOOL_OUTPUT_MAX))
                            .filter(|result| !result.is_empty())
                            .unwrap_or(query),
                    }],
                }
            }
            Some("todo_list") => vec![AgentEvent::Log(format!(
                "codex plan: {}",
                summarize(
                    &tool_value(&item["items"], TOOL_OUTPUT_MAX),
                    TOOL_SUMMARY_MAX
                )
            ))],
            Some("error") => {
                let message = str_field(item, "message").unwrap_or("codex item failed");
                let id = if id.is_empty() {
                    "codex-item-error".to_string()
                } else {
                    id
                };
                let summary = format!("codex item error: {message}");
                vec![
                    AgentEvent::ToolStarted {
                        id: id.clone(),
                        name: "codex error".to_string(),
                        summary: "backend reported a failed item".to_string(),
                    },
                    AgentEvent::ToolCompleted {
                        id,
                        ok: false,
                        summary,
                    },
                ]
            }
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
                Some(id) => vec![AgentEvent::SessionDiscovered(AgentSessionId(
                    id.to_string(),
                ))],
                None => Vec::new(),
            },
            Some("turn.started") => {
                self.pending_message = None;
                self.turn_completed = false;
                Vec::new()
            }
            Some("turn.completed") => {
                self.turn_completed = true;
                self.pending_message
                    .take()
                    .map(AgentEvent::MessageCompleted)
                    .into_iter()
                    .collect()
            }
            Some("turn.failed") => {
                self.pending_message = None;
                vec![AgentEvent::Fatal(
                    str_field(&value["error"], "message")
                        .unwrap_or("codex turn failed")
                        .to_string(),
                )]
            }
            Some("error") => {
                let message = str_field(&value, "message")
                    .unwrap_or("codex stream error")
                    .to_string();
                if is_recoverable_stream_error(&message) {
                    vec![AgentEvent::ParseWarning(format!(
                        "codex transient stream error: {message}"
                    ))]
                } else {
                    self.pending_message = None;
                    vec![AgentEvent::Fatal(message)]
                }
            }
            Some("item.started") => self.handle_item(&value["item"], Phase::Started),
            Some("item.completed") => self.handle_item(&value["item"], Phase::Completed),
            Some("item.updated") => {
                let item = &value["item"];
                if str_field(item, "type") == Some("todo_list") {
                    return self.handle_item(item, Phase::Started);
                }
                if str_field(item, "type") != Some("command_execution") {
                    return Vec::new();
                }
                let id = str_field(item, "id").unwrap_or_default().to_string();
                let output = str_field(item, "aggregated_output").unwrap_or_default();
                let previous = self.command_output.entry(id.clone()).or_default();
                let delta = output.strip_prefix(previous.as_str()).unwrap_or(output);
                *previous = output.to_string();
                if delta.is_empty() {
                    Vec::new()
                } else {
                    vec![AgentEvent::ToolProgress {
                        id,
                        delta: delta.to_string(),
                    }]
                }
            }
            Some(other) => vec![AgentEvent::Log(format!("codex event: {other}"))],
            None => vec![AgentEvent::ParseWarning(format!(
                "codex: event without type: {}",
                summarize(trimmed, 120)
            ))],
        }
    }

    fn finish_after_exit(&mut self, ok: bool) -> Vec<AgentEvent> {
        if ok && !self.turn_completed {
            vec![AgentEvent::Fatal(
                "codex exited without a terminal turn.completed event".to_string(),
            )]
        } else {
            Vec::new()
        }
    }
}

fn is_recoverable_stream_error(message: &str) -> bool {
    message
        .trim_start()
        .to_ascii_lowercase()
        .starts_with("reconnecting")
}

fn file_change_files(item: &Value) -> Vec<(String, String)> {
    item.get("changes")
        .and_then(Value::as_array)
        .map(|changes| {
            changes
                .iter()
                .map(|change| {
                    (
                        str_field(change, "path").unwrap_or_default().to_string(),
                        str_field(change, "kind").unwrap_or("update").to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_all(lines: &[&str]) -> Vec<AgentEvent> {
        let mut parser = CodexLineParser::default();
        let mut out = Vec::new();
        for line in lines {
            out.extend(parser.parse_line(line));
        }
        out
    }

    #[test]
    fn fresh_command_targets_exec_json() {
        let mut member = TeamMember::new("builder", "Builder", BackendKind::Codex, "impl");
        member.system_prompt = Some("coordinate with the team".to_string());
        let adapter = CodexStreamAdapter::from_member(&member, Path::new("/tmp/ws"));
        let command = adapter.build_command("do it", None, Some(Effort::Max));

        assert_eq!(command.program, "codex");
        assert_eq!(command.args[0], "exec");
        assert!(command.args.contains(&"--json".to_string()));
        assert!(command.args.windows(2).any(|w| w == ["-C", "/tmp/ws"]));
        assert!(command.args.windows(2).any(|w| w == ["-s", "read-only"]));
        assert_eq!(command.args.last().unwrap(), "-");
        assert_eq!(command.stdin.as_deref(), Some("do it"));
        assert!(
            command
                .args
                .windows(2)
                .any(|w| w == ["-c", "model_reasoning_effort=max"])
        );
        assert!(
            command
                .args
                .windows(2)
                .any(|w| { w == ["-c", r#"developer_instructions="coordinate with the team""#,] })
        );
        // Never ephemeral on the product path.
        assert!(!command.args.iter().any(|a| a == "--ephemeral"));
    }

    #[test]
    fn resume_command_uses_resume_subcommand_with_session() {
        let member = TeamMember::new("builder", "Builder", BackendKind::Codex, "impl");
        let adapter = CodexStreamAdapter::from_member(&member, Path::new("/tmp/ws"));
        let command =
            adapter.build_command("again", Some(&AgentSessionId("thread-9".to_string())), None);

        assert_eq!(command.args.first().map(String::as_str), Some("exec"));
        let resume = command.args.iter().position(|arg| arg == "resume").unwrap();
        let cwd = command.args.iter().position(|arg| arg == "-C").unwrap();
        let sandbox = command.args.iter().position(|arg| arg == "-s").unwrap();
        assert!(cwd < resume, "-C must be an exec option before resume");
        assert!(sandbox < resume, "-s must be an exec option before resume");
        assert_eq!(
            command.args.get(resume + 1).map(String::as_str),
            Some("thread-9")
        );
        assert!(command.args.windows(2).any(|w| w == ["-C", "/tmp/ws"]));
        assert!(command.args.windows(2).any(|w| w == ["-s", "read-only"]));
        assert_eq!(command.args.last().unwrap(), "-");
        assert_eq!(command.stdin.as_deref(), Some("again"));
        assert!(command.args.contains(&"--json".to_string()));
        assert!(!command.args.iter().any(|a| a == "--color"));
    }

    #[test]
    fn user_prompt_is_never_part_of_the_command_line() {
        let member = TeamMember::new("builder", "Builder", BackendKind::Codex, "impl");
        let adapter = CodexStreamAdapter::from_member(&member, Path::new("/tmp/ws"));

        for session in [None, Some(AgentSessionId("thread-9".to_string()))] {
            for prompt in ["& whoami", "--version"] {
                let command = adapter.build_command(prompt, session.as_ref(), None);
                assert!(!command.args.iter().any(|arg| arg == prompt));
                assert_eq!(command.args.last().map(String::as_str), Some("-"));
                assert_eq!(command.stdin.as_deref(), Some(prompt));
            }
        }
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
    fn only_the_last_agent_message_is_committed_at_turn_completed() {
        let mut parser = CodexLineParser::default();
        assert!(parser
            .parse_line(
                r#"{"type":"item.started","item":{"id":"i1","type":"agent_message","text":"partial"}}"#,
            )
            .is_empty());
        assert!(parser
            .parse_line(
                r#"{"type":"item.completed","item":{"id":"i1","type":"agent_message","text":"@@team_message {\"to\":\"reviewer\",\"body\":\"premature\"}"}}"#,
            )
            .is_empty());
        assert!(parser
            .parse_line(
                r#"{"type":"item.completed","item":{"id":"i2","type":"agent_message","text":"Done."}}"#,
            )
            .is_empty());

        let completed = parser.parse_line(r#"{"type":"turn.completed"}"#);
        assert_eq!(
            completed,
            vec![AgentEvent::MessageCompleted("Done.".to_string())]
        );
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
                    summary: "ok".to_string(),
                },
            ]
        );
    }

    #[test]
    fn collab_tool_call_is_transmitted() {
        let events = parse_all(&[
            r#"{"type":"item.started","item":{"id":"a1","type":"collab_tool_call","tool":"spawn_agent","prompt":"audit adapters","status":"in_progress"}}"#,
            r#"{"type":"item.completed","item":{"id":"a1","type":"collab_tool_call","tool":"spawn_agent","agents_states":{"thread-1":{"status":"completed","message":"done"}},"status":"completed"}}"#,
        ]);
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolStarted { id, name, summary }
                if id == "a1" && name == "spawn_agent" && summary == "audit adapters"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolCompleted { id, ok: true, summary }
                if id == "a1" && summary.contains("thread-1")
        )));
    }

    #[test]
    fn failed_mcp_call_preserves_error_message() {
        let events = parse_all(&[
            r#"{"type":"item.completed","item":{"id":"m1","type":"mcp_tool_call","server":"docs","tool":"fetch","status":"failed","error":{"message":"not found"}}}"#,
        ]);
        assert!(events.contains(&AgentEvent::ToolCompleted {
            id: "m1".to_string(),
            ok: false,
            summary: "not found".to_string(),
        }));
    }

    #[test]
    fn command_output_streams_and_preserves_formatting() {
        let events = parse_all(&[
            r#"{"type":"item.started","item":{"id":"c1","type":"command_execution","command":"cargo test","status":"in_progress"}}"#,
            r#"{"type":"item.updated","item":{"id":"c1","type":"command_execution","aggregated_output":"first\n  second\n"}}"#,
            r#"{"type":"item.updated","item":{"id":"c1","type":"command_execution","aggregated_output":"first\n  second\nthird\n"}}"#,
            r#"{"type":"item.completed","item":{"id":"c1","type":"command_execution","command":"cargo test","aggregated_output":"first\n  second\nthird\n","exit_code":0,"status":"completed"}}"#,
        ]);

        assert!(events.contains(&AgentEvent::ToolProgress {
            id: "c1".to_string(),
            delta: "first\n  second\n".to_string(),
        }));
        assert!(events.contains(&AgentEvent::ToolProgress {
            id: "c1".to_string(),
            delta: "third\n".to_string(),
        }));
        assert!(events.contains(&AgentEvent::ToolCompleted {
            id: "c1".to_string(),
            ok: true,
            summary: "first\n  second\nthird".to_string(),
        }));
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
        let events = parse_all(&[r#"{"type":"turn.failed","error":{"message":"model error"}}"#]);
        assert_eq!(events, vec![AgentEvent::Fatal("model error".to_string())]);
    }

    #[test]
    fn item_error_is_visible_as_a_failed_operation() {
        let events = parse_all(&[
            r#"{"type":"item.completed","item":{"id":"e1","type":"error","message":"permission denied"}}"#,
        ]);

        assert_eq!(
            events,
            vec![
                AgentEvent::ToolStarted {
                    id: "e1".to_string(),
                    name: "codex error".to_string(),
                    summary: "backend reported a failed item".to_string(),
                },
                AgentEvent::ToolCompleted {
                    id: "e1".to_string(),
                    ok: false,
                    summary: "codex item error: permission denied".to_string(),
                },
            ]
        );
    }

    #[test]
    fn successful_exit_requires_turn_completed() {
        let mut parser = CodexLineParser::default();
        assert!(matches!(
            parser.finish_after_exit(true).as_slice(),
            [AgentEvent::Fatal(message)] if message.contains("turn.completed")
        ));

        parser.parse_line(r#"{"type":"turn.completed"}"#);
        assert!(parser.finish_after_exit(true).is_empty());
    }

    #[test]
    fn reasoning_is_emitted_on_completion() {
        let events = parse_all(&[
            r#"{"type":"item.completed","item":{"id":"r1","type":"reasoning","text":"thinking"}}"#,
        ]);
        assert_eq!(events, vec![AgentEvent::Reasoning("thinking".to_string())]);
    }

    #[test]
    fn todo_updates_are_transmitted_as_plan_logs() {
        let events = parse_all(&[
            r#"{"type":"item.updated","item":{"id":"p1","type":"todo_list","items":[{"text":"audit adapters","completed":false}]}}"#,
        ]);
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::Log(text)
                if text.contains("codex plan") && text.contains("audit adapters")
        )));
    }

    #[test]
    fn invalid_json_warns() {
        let events = parse_all(&[r#"not json"#]);
        assert!(matches!(events.as_slice(), [AgentEvent::ParseWarning(_)]));
    }

    #[test]
    fn file_change_emits_a_diff_event() {
        let events = parse_all(&[
            r#"{"type":"item.completed","item":{"id":"f1","type":"file_change","status":"completed","changes":[{"path":"src/a.rs","kind":"update"},{"path":"src/b.rs","kind":"add"}]}}"#,
        ]);
        assert_eq!(
            events,
            vec![AgentEvent::FileChange {
                files: vec![
                    ("src/a.rs".to_string(), "update".to_string()),
                    ("src/b.rs".to_string(), "add".to_string()),
                ],
                ok: true,
            }]
        );
    }

    #[test]
    fn file_change_waits_for_item_completion() {
        let events = parse_all(&[
            r#"{"type":"item.started","item":{"id":"f1","type":"file_change","status":"in_progress","changes":[{"path":"src/a.rs","kind":"update"}]}}"#,
            r#"{"type":"item.completed","item":{"id":"f1","type":"file_change","status":"completed","changes":[{"path":"src/a.rs","kind":"update"}]}}"#,
        ]);

        assert_eq!(
            events,
            vec![AgentEvent::FileChange {
                files: vec![("src/a.rs".to_string(), "update".to_string())],
                ok: true,
            }]
        );
    }

    #[test]
    fn reconnecting_stream_error_warns_without_failing_a_completed_turn() {
        let events = parse_all(&[
            r#"{"type":"turn.started"}"#,
            r#"{"type":"error","message":"Reconnecting to Codex…"}"#,
            r#"{"type":"item.completed","item":{"id":"m1","type":"agent_message","text":"Done."}}"#,
            r#"{"type":"turn.completed"}"#,
        ]);

        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::ParseWarning(message) if message.contains("Reconnecting")
        )));
        assert!(events.contains(&AgentEvent::MessageCompleted("Done.".to_string())));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, AgentEvent::Fatal(_)))
        );
    }
}
