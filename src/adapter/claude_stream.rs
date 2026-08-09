//! Claude Code streaming adapter.
//!
//! Drives `claude -p --output-format stream-json --verbose --include-partial-messages`
//! and translates the NDJSON stream into [`AgentEvent`]s. Sessions are resumable
//! (`--resume <id>`); `--no-session-persistence` is never used.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde_json::Value;

use crate::adapter::parser::{
    MAX_MESSAGE_TEXT_BYTES, append_bounded_text, str_field, summarize, tool_value,
};
use crate::adapter::process::{AdapterCommand, LineParser, StreamAdapter};
use crate::domain::event::{AgentEvent, AgentSessionId};
use crate::domain::team::{BackendKind, Effort, PermissionMode, TeamMember};

const TOOL_OUTPUT_MAX: usize = 32_000;

#[derive(Clone, Debug)]
pub struct ClaudeStreamAdapter {
    binary: String,
    cwd: std::path::PathBuf,
    model: Option<String>,
    permission_mode: Option<PermissionMode>,
    allowed_tools: Vec<String>,
    system_prompt: Option<String>,
}

impl ClaudeStreamAdapter {
    pub fn from_member(member: &TeamMember, workspace: &Path) -> Self {
        Self {
            binary: "claude".to_string(),
            cwd: member.resolved_cwd(workspace),
            model: member.model.clone(),
            permission_mode: member.permission_mode,
            allowed_tools: member.allowed_tools.clone(),
            system_prompt: member.system_prompt.clone(),
        }
    }

    pub fn with_binary(mut self, binary: impl Into<String>) -> Self {
        self.binary = binary.into();
        self
    }
}

impl StreamAdapter for ClaudeStreamAdapter {
    fn backend(&self) -> BackendKind {
        BackendKind::Claude
    }

    fn build_command(
        &self,
        prompt: &str,
        session: Option<&AgentSessionId>,
        effort: Option<Effort>,
    ) -> AdapterCommand {
        let mut args = vec![
            "--print".to_string(),
            "--output-format".to_string(),
            "stream-json".to_string(),
            "--verbose".to_string(),
            "--include-partial-messages".to_string(),
            "--forward-subagent-text".to_string(),
        ];
        if let Some(session) = session {
            args.push("--resume".to_string());
            args.push(session.as_str().to_string());
        }
        if let Some(model) = &self.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }
        // Recent Claude CLIs (e.g. v2.1.207) list acceptEdits/auto/plan/… but not
        // `default`; omit the flag so the CLI's own default applies.
        if let Some(mode) = self.permission_mode
            && mode != PermissionMode::Default
        {
            args.push("--permission-mode".to_string());
            args.push(mode.claude_arg().to_string());
        }
        if !self.allowed_tools.is_empty() {
            args.push("--allowed-tools".to_string());
            args.push(self.allowed_tools.join(","));
        }
        if let Some(system_prompt) = &self.system_prompt {
            args.push("--append-system-prompt".to_string());
            args.push(system_prompt.clone());
        }
        if let Some(effort) = effort {
            args.push("--effort".to_string());
            args.push(effort.claude_arg().to_string());
        }
        AdapterCommand {
            program: self.binary.clone(),
            args,
            cwd: self.cwd.clone(),
            // Claude officially accepts print-mode prompts on stdin. Keeping
            // user text out of argv prevents prompts such as `--version` from
            // being reinterpreted as CLI options.
            stdin: Some(prompt.to_string()),
        }
    }

    fn parser(&self) -> Box<dyn LineParser> {
        Box::new(ClaudeLineParser::default())
    }
}

/// Streaming parser for the Claude stream-json envelope.
#[derive(Default)]
pub struct ClaudeLineParser {
    message_open: bool,
    text_acc: String,
    tool_blocks: HashMap<u64, String>,
    tool_input_started: HashSet<u64>,
    result_seen: bool,
}

impl ClaudeLineParser {
    fn open_message(&mut self, out: &mut Vec<AgentEvent>) {
        if !self.message_open {
            self.message_open = true;
            self.text_acc.clear();
            out.push(AgentEvent::MessageStarted);
        }
    }

    fn close_message(&mut self, out: &mut Vec<AgentEvent>) {
        if self.message_open {
            self.message_open = false;
            out.push(AgentEvent::MessageCompleted(std::mem::take(
                &mut self.text_acc,
            )));
        }
    }

    fn handle_stream_event(&mut self, event: &Value, out: &mut Vec<AgentEvent>) {
        match str_field(event, "type") {
            Some("message_start") => {
                self.message_open = false;
                self.text_acc.clear();
            }
            Some("content_block_start") => {
                let block = &event["content_block"];
                if str_field(block, "type") == Some("tool_use") {
                    let id = str_field(block, "id").unwrap_or_default().to_string();
                    let name = str_field(block, "name").unwrap_or("tool").to_string();
                    let index = event.get("index").and_then(Value::as_u64).unwrap_or(0);
                    self.tool_blocks.insert(index, id.clone());
                    out.push(AgentEvent::ToolStarted {
                        id: id.clone(),
                        summary: name.clone(),
                        name,
                    });
                    if let Some(input) = block.get("input") {
                        let input = tool_value(input, TOOL_OUTPUT_MAX);
                        if !input.is_empty() && input != "{}" {
                            self.tool_input_started.insert(index);
                            out.push(AgentEvent::ToolProgress {
                                id,
                                delta: format!("input:\n{input}\n"),
                            });
                        }
                    }
                }
            }
            Some("content_block_delta") => {
                let delta = &event["delta"];
                if str_field(delta, "type") == Some("text_delta")
                    && let Some(text) = str_field(delta, "text")
                {
                    self.open_message(out);
                    if let Some(delta) =
                        append_bounded_text(&mut self.text_acc, text, MAX_MESSAGE_TEXT_BYTES)
                    {
                        out.push(AgentEvent::TextDelta(delta));
                    }
                } else if str_field(delta, "type") == Some("input_json_delta")
                    && let Some(partial) = str_field(delta, "partial_json")
                    && let Some(index) = event.get("index").and_then(Value::as_u64)
                    && let Some(id) = self.tool_blocks.get(&index)
                {
                    let prefix = if self.tool_input_started.insert(index) {
                        "input:\n"
                    } else {
                        ""
                    };
                    out.push(AgentEvent::ToolProgress {
                        id: id.clone(),
                        delta: format!("{prefix}{partial}"),
                    });
                } else if str_field(delta, "type") == Some("thinking_delta")
                    && let Some(thinking) = str_field(delta, "thinking")
                    && !thinking.is_empty()
                {
                    out.push(AgentEvent::Reasoning(thinking.to_string()));
                }
            }
            Some("content_block_stop") => {
                if let Some(index) = event.get("index").and_then(Value::as_u64) {
                    let input_started = self.tool_input_started.remove(&index);
                    if let Some(id) = self.tool_blocks.remove(&index)
                        && input_started
                    {
                        out.push(AgentEvent::ToolProgress {
                            id,
                            delta: "\n".to_string(),
                        });
                    }
                }
            }
            Some("message_stop") => self.close_message(out),
            _ => {}
        }
    }

    fn handle_user_message(&self, message: &Value, out: &mut Vec<AgentEvent>) {
        let Some(content) = message.get("content").and_then(Value::as_array) else {
            return;
        };
        for block in content {
            if str_field(block, "type") == Some("tool_result") {
                let id = str_field(block, "tool_use_id")
                    .unwrap_or_default()
                    .to_string();
                let is_error = block
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let summary = tool_value(&block["content"], TOOL_OUTPUT_MAX);
                out.push(AgentEvent::ToolCompleted {
                    id,
                    ok: !is_error,
                    summary,
                });
            }
        }
    }
}

impl LineParser for ClaudeLineParser {
    fn parse_line(&mut self, line: &str) -> Vec<AgentEvent> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Vec::new();
        }
        let value: Value = match serde_json::from_str(trimmed) {
            Ok(value) => value,
            Err(err) => {
                return vec![AgentEvent::ParseWarning(format!(
                    "claude: invalid JSON line: {err}"
                ))];
            }
        };

        let mut out = Vec::new();
        match str_field(&value, "type") {
            Some("system") => {
                if let Some(session) = str_field(&value, "session_id") {
                    out.push(AgentEvent::SessionDiscovered(AgentSessionId(
                        session.to_string(),
                    )));
                }
                // `thinking_tokens` progress rows arrive many times per turn
                // (observed on claude 2.1.207); logging each would flood the
                // logs drawer.
                let subtype = str_field(&value, "subtype");
                if subtype != Some("init") && subtype != Some("thinking_tokens") {
                    out.push(AgentEvent::Log(format!(
                        "claude system/{}",
                        subtype.unwrap_or("event")
                    )));
                }
            }
            Some("stream_event") => self.handle_stream_event(&value["event"], &mut out),
            Some("user") => self.handle_user_message(&value["message"], &mut out),
            Some("assistant") => {
                // Main-agent content is already represented by `stream_event`
                // partials. `--forward-subagent-text` marks forwarded child
                // blocks so their text/thinking can be surfaced without
                // duplicating the main answer.
                if value.get("parent_tool_use_id").is_some()
                    || value["message"].get("parent_tool_use_id").is_some()
                {
                    let content = value["message"]["content"].as_array();
                    for block in content.into_iter().flatten() {
                        match str_field(block, "type") {
                            Some("thinking") => {
                                if let Some(thinking) = str_field(block, "thinking") {
                                    out.push(AgentEvent::Reasoning(format!(
                                        "[subagent] {thinking}"
                                    )));
                                }
                            }
                            Some("text") => {
                                if let Some(text) = str_field(block, "text") {
                                    out.push(AgentEvent::Log(format!("claude subagent: {text}")));
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            Some("result") => {
                self.result_seen = true;
                if let Some(session) = str_field(&value, "session_id") {
                    out.push(AgentEvent::SessionDiscovered(AgentSessionId(
                        session.to_string(),
                    )));
                }
                self.close_message(&mut out);
                if value.get("is_error").and_then(Value::as_bool) == Some(true) {
                    let message = str_field(&value, "result").unwrap_or("claude reported an error");
                    out.push(AgentEvent::Fatal(message.to_string()));
                }
            }
            Some(other) => out.push(AgentEvent::Log(format!("claude event: {other}"))),
            None => out.push(AgentEvent::ParseWarning(format!(
                "claude: event without type: {}",
                summarize(trimmed, 120)
            ))),
        }
        out
    }

    fn finish(&mut self) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        self.close_message(&mut out);
        out
    }

    fn finish_after_exit(&mut self, ok: bool) -> Vec<AgentEvent> {
        if ok && !self.result_seen {
            vec![AgentEvent::Fatal(
                "claude exited without a terminal result event".to_string(),
            )]
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::team::TeamMember;

    fn parse_all(lines: &[&str]) -> Vec<AgentEvent> {
        let mut parser = ClaudeLineParser::default();
        let mut out = Vec::new();
        for line in lines {
            out.extend(parser.parse_line(line));
        }
        out.extend(parser.finish());
        out
    }

    #[test]
    fn command_uses_stream_json_and_resume() {
        let mut member = TeamMember::new("reviewer", "Reviewer", BackendKind::Claude, "review");
        member.permission_mode = Some(PermissionMode::Plan);
        member.allowed_tools = vec!["Read".to_string(), "Bash".to_string()];
        let adapter = ClaudeStreamAdapter::from_member(&member, Path::new("/tmp/ws"));

        let command = adapter.build_command(
            "hello",
            Some(&AgentSessionId("sess-1".to_string())),
            Some(Effort::High),
        );
        assert_eq!(command.program, "claude");
        assert!(command.args.contains(&"--output-format".to_string()));
        assert!(command.args.contains(&"stream-json".to_string()));
        assert!(
            command
                .args
                .contains(&"--include-partial-messages".to_string())
        );
        assert!(
            command
                .args
                .contains(&"--forward-subagent-text".to_string())
        );
        assert!(command.args.windows(2).any(|w| w == ["--resume", "sess-1"]));
        assert!(command.args.windows(2).any(|w| w == ["--effort", "high"]));
        assert!(
            command
                .args
                .windows(2)
                .any(|w| w == ["--permission-mode", "plan"])
        );
        assert!(
            command
                .args
                .windows(2)
                .any(|w| w == ["--allowed-tools", "Read,Bash"])
        );
        assert_eq!(command.stdin.as_deref(), Some("hello"));
        assert!(!command.args.iter().any(|arg| arg == "hello"));
        // The product path never disables session persistence.
        assert!(!command.args.iter().any(|a| a == "--no-session-persistence"));
    }

    #[test]
    fn command_omits_permission_mode_default() {
        let mut member = TeamMember::new("reviewer", "Reviewer", BackendKind::Claude, "review");
        member.permission_mode = Some(PermissionMode::Default);
        let adapter = ClaudeStreamAdapter::from_member(&member, Path::new("/tmp/ws"));
        let command = adapter.build_command("hello", None, None);
        assert!(!command.args.iter().any(|a| a == "--permission-mode"));
        assert!(!command.args.iter().any(|a| a == "default"));
    }

    #[test]
    fn option_shaped_prompt_is_sent_on_stdin() {
        let member = TeamMember::new("reviewer", "Reviewer", BackendKind::Claude, "review");
        let adapter = ClaudeStreamAdapter::from_member(&member, Path::new("/tmp/ws"));
        let command = adapter.build_command("--version", None, None);

        assert_eq!(command.stdin.as_deref(), Some("--version"));
        assert!(!command.args.iter().any(|arg| arg == "--version"));
    }

    #[test]
    fn captures_session_from_init() {
        let events = parse_all(&[
            r#"{"type":"system","subtype":"init","session_id":"sess-abc","model":"claude"}"#,
        ]);
        assert_eq!(
            events,
            vec![AgentEvent::SessionDiscovered(AgentSessionId(
                "sess-abc".to_string()
            ))]
        );
    }

    #[test]
    fn thinking_tokens_progress_rows_are_not_logged() {
        let events = parse_all(&[
            r#"{"type":"system","subtype":"thinking_tokens","estimated_tokens":15,"session_id":"sess-abc"}"#,
        ]);
        // Session id still tracked (deduped downstream), but no Log spam.
        assert_eq!(
            events,
            vec![AgentEvent::SessionDiscovered(AgentSessionId(
                "sess-abc".to_string()
            ))]
        );
        let events = parse_all(&[r#"{"type":"system","subtype":"compact_boundary"}"#]);
        assert!(
            matches!(events.as_slice(), [AgentEvent::Log(msg)] if msg.contains("compact_boundary"))
        );
    }

    #[test]
    fn streams_text_deltas_then_completes() {
        let events = parse_all(&[
            r#"{"type":"stream_event","event":{"type":"message_start","message":{}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello "}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"world"}}}"#,
            r#"{"type":"stream_event","event":{"type":"message_stop"}}"#,
        ]);
        assert_eq!(
            events,
            vec![
                AgentEvent::MessageStarted,
                AgentEvent::TextDelta("Hello ".to_string()),
                AgentEvent::TextDelta("world".to_string()),
                AgentEvent::MessageCompleted("Hello world".to_string()),
            ]
        );
    }

    #[test]
    fn tool_use_start_and_result() {
        let events = parse_all(&[
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"Bash","input":{}}}}"#,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"done","is_error":false}]}}"#,
        ]);
        assert_eq!(
            events,
            vec![
                AgentEvent::ToolStarted {
                    id: "toolu_1".to_string(),
                    name: "Bash".to_string(),
                    summary: "Bash".to_string(),
                },
                AgentEvent::ToolCompleted {
                    id: "toolu_1".to_string(),
                    ok: true,
                    summary: "done".to_string(),
                },
            ]
        );
    }

    #[test]
    fn thinking_delta_is_forwarded_as_reasoning() {
        let events = parse_all(&[
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"checking invariants"}}}"#,
        ]);
        assert!(events.contains(&AgentEvent::Reasoning("checking invariants".to_string())));
    }

    #[test]
    fn forwarded_subagent_thinking_is_not_dropped() {
        let events = parse_all(&[
            r#"{"type":"assistant","parent_tool_use_id":"toolu_parent","message":{"content":[{"type":"thinking","thinking":"child analysis"},{"type":"text","text":"child result"}]}}"#,
        ]);
        assert!(events.contains(&AgentEvent::Reasoning(
            "[subagent] child analysis".to_string()
        )));
        assert!(events.iter().any(
            |event| matches!(event, AgentEvent::Log(message) if message.contains("child result"))
        ));
    }

    #[test]
    fn tool_input_and_multiline_result_are_preserved() {
        let events = parse_all(&[
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"Bash","input":{}}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"command\":\"printf hi\""}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"}"}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_stop","index":1}}"#,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":[{"type":"text","text":"line one\n  line two"}],"is_error":false}]}}"#,
        ]);

        assert!(events.contains(&AgentEvent::ToolProgress {
            id: "toolu_1".to_string(),
            delta: "input:\n{\"command\":\"printf hi\"".to_string(),
        }));
        assert!(events.contains(&AgentEvent::ToolCompleted {
            id: "toolu_1".to_string(),
            ok: true,
            summary: "line one\n  line two".to_string(),
        }));
    }

    #[test]
    fn result_captures_session_and_flushes_open_message() {
        let events = parse_all(&[
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"partial"}}}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"result":"partial","session_id":"sess-z"}"#,
        ]);
        assert!(
            events.contains(&AgentEvent::SessionDiscovered(AgentSessionId(
                "sess-z".to_string()
            )))
        );
        assert!(events.contains(&AgentEvent::MessageCompleted("partial".to_string())));
    }

    #[test]
    fn invalid_json_is_a_parse_warning() {
        let events = parse_all(&[r#"{"type":"system""#]);
        assert!(matches!(events.as_slice(), [AgentEvent::ParseWarning(_)]));
    }

    #[test]
    fn error_result_is_fatal() {
        let events = parse_all(&[
            r#"{"type":"result","is_error":true,"result":"rate limited","session_id":"s"}"#,
        ]);
        assert!(events.contains(&AgentEvent::Fatal("rate limited".to_string())));
    }

    #[test]
    fn successful_exit_requires_a_terminal_result_event() {
        let mut parser = ClaudeLineParser::default();
        assert!(matches!(
            parser.finish_after_exit(true).as_slice(),
            [AgentEvent::Fatal(message)] if message.contains("terminal result")
        ));

        parser.parse_line(r#"{"type":"result","is_error":false}"#);
        assert!(parser.finish_after_exit(true).is_empty());
    }
}
