//! Antigravity CLI (`agy`) stream adapter.
//!
//! Drives print mode with `--output-format stream-json`. The structured stream
//! exposes the conversation id, agent response chunks, tool lifecycle, and
//! final status directly; no log scraping or cache guessing is required.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::adapter::parser::{
    MAX_MESSAGE_TEXT_BYTES, append_bounded_text, bounded_text, str_field, summarize, tool_detail,
    tool_value,
};
use crate::adapter::process::{AdapterCommand, LineParser, StreamAdapter};
use crate::domain::event::{AgentEvent, AgentSessionId};
use crate::domain::team::{BackendKind, Effort, PermissionMode, SandboxPolicy, TeamMember};

const TOOL_SUMMARY_MAX: usize = 160;
const TOOL_OUTPUT_MAX: usize = 32_000;

#[derive(Clone, Debug)]
pub struct AgyStreamAdapter {
    binary: String,
    cwd: PathBuf,
    member_id: String,
    log_dir: PathBuf,
    model: Option<String>,
    system_prompt: Option<String>,
    sandbox: SandboxPolicy,
    permission_mode: Option<PermissionMode>,
}

impl AgyStreamAdapter {
    pub fn from_member(member: &TeamMember, workspace: &Path) -> Self {
        let workspace = workspace.to_path_buf();
        Self {
            binary: "agy".to_string(),
            cwd: member.resolved_cwd(&workspace),
            log_dir: workspace.join(".asterline").join("agy"),
            member_id: member.id.as_str().to_string(),
            model: member.model.clone(),
            system_prompt: member.system_prompt.clone(),
            sandbox: member.sandbox,
            permission_mode: member.permission_mode,
        }
    }

    pub fn with_binary(mut self, binary: impl Into<String>) -> Self {
        self.binary = binary.into();
        self
    }

    fn log_path(&self) -> PathBuf {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let safe_member = self
            .member_id
            .chars()
            .map(|ch| {
                if ch.is_alphanumeric() || matches!(ch, '-' | '_') {
                    ch
                } else {
                    '_'
                }
            })
            .collect::<String>();
        self.log_dir.join(format!("{safe_member}-{millis}.log"))
    }

    fn prompt_with_system(&self, prompt: &str) -> String {
        let workspace_hint = format!(
            "The project workspace is `{}`. Work in that directory rather than the CLI scratch directory.",
            self.cwd.display()
        );
        match &self.system_prompt {
            Some(system_prompt) if !system_prompt.trim().is_empty() => {
                format!(
                    "System instructions:\n{system_prompt}\n\n{workspace_hint}\n\nUser message:\n{prompt}"
                )
            }
            _ => format!("{workspace_hint}\n\nUser message:\n{prompt}"),
        }
    }
}

impl StreamAdapter for AgyStreamAdapter {
    fn backend(&self) -> BackendKind {
        BackendKind::Agy
    }

    fn build_command(
        &self,
        prompt: &str,
        session: Option<&AgentSessionId>,
        effort: Option<Effort>,
    ) -> AdapterCommand {
        let _ = std::fs::create_dir_all(&self.log_dir);
        let mut args = vec![
            "--print-timeout".to_string(),
            "5m0s".to_string(),
            "--log-file".to_string(),
            self.log_path().display().to_string(),
            "--output-format".to_string(),
            "stream-json".to_string(),
            "--add-dir".to_string(),
            self.cwd.display().to_string(),
        ];
        if let Some(session) = session {
            args.push("--conversation".to_string());
            args.push(session.as_str().to_string());
        }
        if let Some(model) = &self.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }
        if let Some(effort) = effort {
            args.push("--effort".to_string());
            args.push(effort.as_str().to_string());
        }
        if self.sandbox != SandboxPolicy::DangerFullAccess {
            args.push("--sandbox".to_string());
        }
        match self.permission_mode {
            Some(PermissionMode::BypassPermissions) => {
                args.push("--dangerously-skip-permissions".to_string());
            }
            Some(PermissionMode::AcceptEdits) => {
                args.push("--mode".to_string());
                args.push("accept-edits".to_string());
            }
            Some(PermissionMode::Plan) => {
                args.push("--mode".to_string());
                args.push("plan".to_string());
            }
            _ => {}
        }
        args.push("--print".to_string());
        args.push(self.prompt_with_system(prompt));
        AdapterCommand {
            program: self.binary.clone(),
            args,
            cwd: self.cwd.clone(),
            stdin: None,
        }
    }

    fn parser(&self) -> Box<dyn LineParser> {
        Box::new(AgyLineParser::default())
    }
}

/// Parser for Agy's newline-delimited `stream-json` output.
#[derive(Default)]
pub struct AgyLineParser {
    message_open: bool,
    text_acc: String,
    session_id: Option<String>,
    active_tools: HashMap<u64, String>,
    completed_tools: HashSet<u64>,
    result_seen: bool,
}

impl AgyLineParser {
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

    fn discover_session(&mut self, id: &str, out: &mut Vec<AgentEvent>) {
        if self.session_id.as_deref() == Some(id) {
            return;
        }
        self.session_id = Some(id.to_string());
        out.push(AgentEvent::SessionDiscovered(AgentSessionId(
            id.to_string(),
        )));
    }

    fn handle_step(&mut self, step: &Value, out: &mut Vec<AgentEvent>) {
        if let Some(thought) = ["thought_delta", "raw_thought", "thinking_delta"]
            .into_iter()
            .find_map(|field| str_field(step, field))
            .filter(|value| !value.is_empty())
        {
            out.push(AgentEvent::Reasoning(thought.to_string()));
        }

        if str_field(step, "step_type") == Some("agent_response") {
            if let Some(text) = str_field(step, "text_delta").filter(|value| !value.is_empty()) {
                self.open_message(out);
                if let Some(delta) =
                    append_bounded_text(&mut self.text_acc, text, MAX_MESSAGE_TEXT_BYTES)
                {
                    out.push(AgentEvent::TextDelta(delta));
                }
            }
            return;
        }

        if str_field(step, "step_type") != Some("tool") {
            return;
        }
        let index = step.get("step_index").and_then(Value::as_u64).unwrap_or(0);
        let id = format!("agy-step-{index}");
        let tool = &step["tool_info"];
        let name = str_field(step, "tool_name")
            .or_else(|| str_field(tool, "name"))
            .unwrap_or("tool")
            .to_string();
        let state = str_field(step, "state").unwrap_or_default();

        if state == "ACTIVE" && !self.active_tools.contains_key(&index) {
            self.active_tools.insert(index, name.clone());
            out.push(AgentEvent::ToolStarted {
                id,
                name: name.clone(),
                summary: tool
                    .get("parameters")
                    .map(|value| tool_value(value, TOOL_SUMMARY_MAX))
                    .filter(|value| !value.is_empty())
                    .map(|parameters| format!("{name}: {parameters}"))
                    .map(|value| summarize(&value, TOOL_SUMMARY_MAX))
                    .unwrap_or(name),
            });
            return;
        }

        if matches!(state, "DONE" | "ERROR" | "INTERRUPTED" | "CANCELLED")
            && self.completed_tools.insert(index)
        {
            if !self.active_tools.contains_key(&index) {
                out.push(AgentEvent::ToolStarted {
                    id: id.clone(),
                    name: name.clone(),
                    summary: name.clone(),
                });
            }
            self.active_tools.remove(&index);
            let summary = ["output", "error"]
                .into_iter()
                .find_map(|field| tool.get(field))
                .or_else(|| step.get("error_details"))
                .map(|value| tool_value(value, TOOL_OUTPUT_MAX))
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| name.clone());
            out.push(AgentEvent::ToolCompleted {
                id,
                ok: state == "DONE",
                summary: tool_detail(&summary, TOOL_OUTPUT_MAX),
            });
        }
    }
}

impl LineParser for AgyLineParser {
    fn parse_line(&mut self, line: &str) -> Vec<AgentEvent> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Vec::new();
        }
        let value: Value = match serde_json::from_str(trimmed) {
            Ok(value) => value,
            Err(err) => {
                return vec![AgentEvent::ParseWarning(format!(
                    "agy: invalid JSON line: {err}"
                ))];
            }
        };
        let mut out = Vec::new();
        match str_field(&value, "event") {
            Some("init") => {
                if let Some(id) = str_field(&value, "conversation_id") {
                    self.discover_session(id, &mut out);
                }
            }
            Some("step_update") => self.handle_step(&value["step_update"], &mut out),
            Some("result") => {
                self.result_seen = true;
                let result = &value["result"];
                if let Some(id) = str_field(result, "conversation_id") {
                    self.discover_session(id, &mut out);
                }
                if let Some(response) =
                    str_field(result, "response").filter(|value| !value.is_empty())
                {
                    if !self.message_open {
                        self.open_message(&mut out);
                        out.push(AgentEvent::TextDelta(bounded_text(
                            response,
                            MAX_MESSAGE_TEXT_BYTES,
                        )));
                    }
                    // Agy's incremental text_delta may split a multi-byte UTF-8
                    // character and contain U+FFFD. Its final response is the
                    // authoritative, reassembled message, so always use it when
                    // finalizing the cell.
                    self.text_acc = bounded_text(response, MAX_MESSAGE_TEXT_BYTES);
                }
                self.close_message(&mut out);
                if str_field(result, "status") != Some("SUCCESS") {
                    out.push(AgentEvent::Fatal(
                        str_field(result, "error")
                            .or_else(|| str_field(result, "status"))
                            .unwrap_or("agy run failed")
                            .to_string(),
                    ));
                }
            }
            Some(other) => out.push(AgentEvent::Log(format!("agy event: {other}"))),
            None => out.push(AgentEvent::ParseWarning(format!(
                "agy: event without type: {}",
                summarize(trimmed, 120)
            ))),
        }
        out
    }

    fn finish(&mut self) -> Vec<AgentEvent> {
        if self.result_seen {
            Vec::new()
        } else {
            let mut out = Vec::new();
            self.close_message(&mut out);
            out
        }
    }

    fn finish_after_exit(&mut self, ok: bool) -> Vec<AgentEvent> {
        if ok && !self.result_seen {
            vec![AgentEvent::Fatal(
                "agy exited without a terminal result event".to_string(),
            )]
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_uses_stream_json_effort_workspace_and_resume() {
        let mut member = TeamMember::new("a", "Agy", BackendKind::Agy, "research");
        member.model = Some("model-x".to_string());
        let adapter = AgyStreamAdapter::from_member(&member, Path::new("/tmp/ws"));
        let session = AgentSessionId("1ddde77f-dcaf-47cf-97e8-b3e6a3f4e43d".to_string());
        let command = adapter.build_command("hi there", Some(&session), Some(Effort::High));

        assert_eq!(command.program, "agy");
        assert!(
            command
                .args
                .windows(2)
                .any(|w| w == ["--output-format", "stream-json"])
        );
        assert!(command.args.windows(2).any(|w| w == ["--effort", "high"]));
        assert!(
            command
                .args
                .windows(2)
                .any(|w| w == ["--add-dir", "/tmp/ws"])
        );
        assert!(command.args.windows(2).any(|w| w == ["--model", "model-x"]));
        assert!(
            command
                .args
                .windows(2)
                .any(|w| { w == ["--conversation", "1ddde77f-dcaf-47cf-97e8-b3e6a3f4e43d",] })
        );
        assert!(command.args.contains(&"--sandbox".to_string()));
        assert!(command.args[command.args.len() - 1].contains("hi there"));
        assert_eq!(command.stdin, None);
    }

    #[test]
    fn member_id_cannot_escape_the_log_directory() {
        let member = TeamMember::new("../../escape", "Agy", BackendKind::Agy, "research");
        let adapter = AgyStreamAdapter::from_member(&member, Path::new("/tmp/ws"));
        let path = adapter.log_path();

        assert_eq!(path.parent(), Some(adapter.log_dir.as_path()));
        assert!(!path.file_name().unwrap().to_string_lossy().contains('/'));
    }

    #[test]
    fn exact_agy_modes_map_to_cli_flags() {
        let mut member = TeamMember::new("a", "Agy", BackendKind::Agy, "research");
        member.permission_mode = Some(PermissionMode::AcceptEdits);
        let adapter = AgyStreamAdapter::from_member(&member, Path::new("/tmp/ws"));
        let command = adapter.build_command("hi", None, None);
        assert!(
            command
                .args
                .windows(2)
                .any(|w| w == ["--mode", "accept-edits"])
        );
    }

    #[test]
    fn parser_transmits_session_text_and_tool_events() {
        let mut parser = AgyLineParser::default();
        let mut events = Vec::new();
        for line in [
            r#"{"event":"init","conversation_id":"2e7d0c67-f359-4cc6-ba40-dfeef04d80f8","init":{"cwd":"/tmp"}}"#,
            r#"{"event":"step_update","step_update":{"step_index":3,"state":"ACTIVE","step_type":"tool","tool_name":"run_command","tool_info":{"name":"run_command","parameters":{"CommandLine":"pwd"}}}}"#,
            r#"{"event":"step_update","step_update":{"step_index":3,"state":"DONE","step_type":"tool","tool_name":"run_command","tool_info":{"name":"run_command","parameters":{"CommandLine":"pwd"},"output":"/tmp\n"}}}"#,
            r#"{"event":"step_update","step_update":{"step_index":4,"state":"DONE","step_type":"agent_response","text_delta":"OK\n"}}"#,
            r#"{"event":"result","result":{"conversation_id":"2e7d0c67-f359-4cc6-ba40-dfeef04d80f8","status":"SUCCESS","response":"OK\n"}}"#,
        ] {
            events.extend(parser.parse_line(line));
        }

        assert!(
            events.contains(&AgentEvent::SessionDiscovered(AgentSessionId(
                "2e7d0c67-f359-4cc6-ba40-dfeef04d80f8".to_string()
            )))
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, AgentEvent::SessionDiscovered(_)))
                .count(),
            1
        );
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolStarted { id, name, .. }
                if id == "agy-step-3" && name == "run_command"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolCompleted { id, ok: true, summary }
                if id == "agy-step-3" && summary.contains("/tmp")
        )));
        assert!(events.contains(&AgentEvent::MessageCompleted("OK\n".to_string())));
    }

    #[test]
    fn failed_result_is_fatal() {
        let mut parser = AgyLineParser::default();
        let events = parser.parse_line(
            r#"{"event":"result","result":{"conversation_id":"x","status":"ERROR","error":"rate limited"}}"#,
        );
        assert!(events.contains(&AgentEvent::Fatal("rate limited".to_string())));
    }

    #[test]
    fn successful_exit_requires_a_terminal_result_event() {
        let mut parser = AgyLineParser::default();
        assert!(matches!(
            parser.finish_after_exit(true).as_slice(),
            [AgentEvent::Fatal(message)] if message.contains("terminal result")
        ));

        parser.parse_line(r#"{"event":"result","result":{"status":"SUCCESS"}}"#);
        assert!(parser.finish_after_exit(true).is_empty());
    }

    #[test]
    fn final_response_repairs_replacement_characters_in_stream_deltas() {
        let mut parser = AgyLineParser::default();
        let mut events = parser.parse_line(
            r#"{"event":"step_update","step_update":{"step_index":1,"state":"ACTIVE","step_type":"agent_response","text_delta":"拓扑���索"}}"#,
        );
        events.extend(parser.parse_line(
            r#"{"event":"result","result":{"conversation_id":"x","status":"SUCCESS","response":"拓扑检索"}}"#,
        ));

        assert!(events.contains(&AgentEvent::TextDelta("拓扑���索".to_string())));
        assert!(events.contains(&AgentEvent::MessageCompleted("拓扑检索".to_string())));
    }
}
