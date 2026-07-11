//! Grok streaming adapter.
//!
//! Drives `grok --single <prompt> --output-format streaming-json` and translates
//! the JSONL events into [`AgentEvent`]s. Sessions are resumable with
//! `--resume <id>`.

use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;

use crate::adapter::parser::{str_field, summarize};
use crate::adapter::process::{AdapterCommand, LineParser, StreamAdapter};
use crate::domain::event::{AgentEvent, AgentSessionId};
use crate::domain::team::{BackendKind, Effort, PermissionMode, SandboxPolicy, TeamMember};

const TOOL_SUMMARY_MAX: usize = 160;
const TOOL_OUTPUT_MAX: usize = 4000;

#[derive(Clone, Debug)]
pub struct GrokStreamAdapter {
    binary: String,
    cwd: std::path::PathBuf,
    sandbox: SandboxPolicy,
    model: Option<String>,
    permission_mode: Option<PermissionMode>,
    allowed_tools: Vec<String>,
    system_prompt: Option<String>,
}

impl GrokStreamAdapter {
    pub fn from_member(member: &TeamMember, workspace: &Path) -> Self {
        Self {
            binary: "grok".to_string(),
            cwd: member.resolved_cwd(workspace),
            sandbox: member.sandbox,
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

impl StreamAdapter for GrokStreamAdapter {
    fn backend(&self) -> BackendKind {
        BackendKind::Grok
    }

    fn build_command(
        &self,
        prompt: &str,
        session: Option<&AgentSessionId>,
        effort: Option<Effort>,
    ) -> AdapterCommand {
        let mut args = vec![
            "--single".to_string(),
            prompt.to_string(),
            "--output-format".to_string(),
            "streaming-json".to_string(),
            "--sandbox".to_string(),
            self.sandbox.grok_arg().to_string(),
        ];
        if let Some(session) = session {
            args.push("--resume".to_string());
            args.push(session.as_str().to_string());
        }
        if let Some(model) = &self.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }
        if let Some(mode) = self.permission_mode {
            args.push("--permission-mode".to_string());
            args.push(mode.grok_arg().to_string());
        }
        if !self.allowed_tools.is_empty() {
            args.push("--tools".to_string());
            args.push(self.allowed_tools.join(","));
        }
        if let Some(system_prompt) = &self.system_prompt {
            args.push("--rules".to_string());
            args.push(system_prompt.clone());
        }
        if let Some(effort) = effort {
            args.push("--reasoning-effort".to_string());
            args.push(effort.as_str().to_string());
        }

        AdapterCommand {
            program: self.binary.clone(),
            args,
            cwd: self.cwd.clone(),
            stdin: None,
        }
    }

    fn parser(&self) -> Box<dyn LineParser> {
        Box::new(GrokLineParser::default())
    }
}

/// Parser for Grok's `streaming-json` event stream.
#[derive(Default)]
pub struct GrokLineParser {
    message_open: bool,
    text_acc: String,
    next_tool_id: u64,
    active_tools: HashMap<String, Vec<String>>,
}

impl GrokLineParser {
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

    fn event_tool_id(&mut self, value: &Value, name: &str) -> String {
        for field in ["tool_id", "toolId", "id"] {
            if let Some(id) = str_field(value, field) {
                return id.to_string();
            }
        }
        self.next_tool_id += 1;
        format!("grok-tool-{}-{name}", self.next_tool_id)
    }

    fn start_tool(&mut self, value: &Value) -> AgentEvent {
        let name = str_field(value, "tool_name").unwrap_or("tool").to_string();
        let id = self.event_tool_id(value, &name);
        self.active_tools
            .entry(name.clone())
            .or_default()
            .push(id.clone());
        AgentEvent::ToolStarted {
            id,
            name: name.clone(),
            summary: event_summary(value, &name, TOOL_SUMMARY_MAX),
        }
    }

    fn complete_tool(&mut self, value: &Value) -> AgentEvent {
        let name = str_field(value, "tool_name").unwrap_or("tool").to_string();
        let explicit_id = ["tool_id", "toolId", "id"]
            .into_iter()
            .find_map(|field| str_field(value, field).map(str::to_string));
        let id = match explicit_id {
            Some(id) => {
                if let Some(ids) = self.active_tools.get_mut(&name)
                    && let Some(index) = ids.iter().position(|active| active == &id)
                {
                    ids.remove(index);
                }
                id
            }
            None => self
                .active_tools
                .get_mut(&name)
                .and_then(|ids| (!ids.is_empty()).then(|| ids.remove(0)))
                .unwrap_or_else(|| self.event_tool_id(value, &name)),
        };
        let outcome = str_field(value, "outcome").unwrap_or_default();
        let is_error = value
            .get("isError")
            .or_else(|| value.get("is_error"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        AgentEvent::ToolCompleted {
            id,
            ok: !is_error && !matches!(outcome, "error" | "failed" | "failure"),
            summary: event_summary(value, &name, TOOL_OUTPUT_MAX),
        }
    }
}

impl LineParser for GrokLineParser {
    fn parse_line(&mut self, line: &str) -> Vec<AgentEvent> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Vec::new();
        }
        let value: Value = match serde_json::from_str(trimmed) {
            Ok(value) => value,
            Err(err) => {
                return vec![AgentEvent::ParseWarning(format!(
                    "grok: invalid JSON line: {err}"
                ))];
            }
        };

        let mut out = Vec::new();
        match str_field(&value, "type") {
            Some("thought") => {
                if let Some(data) = str_field(&value, "data")
                    && !data.is_empty()
                {
                    out.push(AgentEvent::Reasoning(data.to_string()));
                }
            }
            Some("text") => {
                if let Some(data) = str_field(&value, "data")
                    && !data.is_empty()
                {
                    self.open_message(&mut out);
                    self.text_acc.push_str(data);
                    out.push(AgentEvent::TextDelta(data.to_string()));
                }
            }
            Some("tool_started") => out.push(self.start_tool(&value)),
            Some("tool_completed") => out.push(self.complete_tool(&value)),
            Some("end") => {
                self.close_message(&mut out);
                if let Some(id) = str_field(&value, "sessionId")
                    && !id.is_empty()
                {
                    out.push(AgentEvent::SessionDiscovered(AgentSessionId(
                        id.to_string(),
                    )));
                }
                if value
                    .get("isError")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    out.push(AgentEvent::Fatal(
                        str_field(&value, "data")
                            .unwrap_or("grok turn failed")
                            .to_string(),
                    ));
                }
            }
            Some("error") => out.push(AgentEvent::Fatal(
                str_field(&value, "data")
                    .unwrap_or("grok stream error")
                    .to_string(),
            )),
            Some(other) => out.push(AgentEvent::Log(format!("grok event: {other}"))),
            None => out.push(AgentEvent::ParseWarning(format!(
                "grok: event without type: {}",
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
}

fn event_summary(value: &Value, fallback: &str, max: usize) -> String {
    let data = str_field(value, "data").unwrap_or(fallback);
    let outcome = str_field(value, "outcome").unwrap_or_default();
    let summary = if outcome.is_empty() || data.contains(outcome) {
        data.to_string()
    } else {
        format!("{data} ({outcome})")
    };
    summarize(&summary, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_all(lines: &[&str]) -> Vec<AgentEvent> {
        let mut parser = GrokLineParser::default();
        let mut out = Vec::new();
        for line in lines {
            out.extend(parser.parse_line(line));
        }
        out
    }

    #[test]
    fn fresh_command_uses_headless_streaming_json() {
        let mut member = TeamMember::new("grok", "Grok", BackendKind::Grok, "implementation");
        member.sandbox = SandboxPolicy::WorkspaceWrite;
        member.model = Some("grok-build".to_string());
        member.permission_mode = Some(PermissionMode::Auto);
        member.allowed_tools = vec!["shell".to_string(), "read_file".to_string()];
        member.system_prompt = Some("team rules".to_string());
        let adapter = GrokStreamAdapter::from_member(&member, Path::new("/tmp/ws"));

        let command = adapter.build_command("do it", None, Some(Effort::Xhigh));

        assert_eq!(command.program, "grok");
        assert!(command.args.windows(2).any(|w| w == ["--single", "do it"]));
        assert!(
            command
                .args
                .windows(2)
                .any(|w| w == ["--output-format", "streaming-json"])
        );
        assert!(
            command
                .args
                .windows(2)
                .any(|w| w == ["--sandbox", "workspace"])
        );
        assert!(
            command
                .args
                .windows(2)
                .any(|w| w == ["--model", "grok-build"])
        );
        assert!(
            command
                .args
                .windows(2)
                .any(|w| w == ["--permission-mode", "auto"])
        );
        assert!(
            command
                .args
                .windows(2)
                .any(|w| w == ["--tools", "shell,read_file"])
        );
        assert!(
            command
                .args
                .windows(2)
                .any(|w| w == ["--rules", "team rules"])
        );
        assert!(
            command
                .args
                .windows(2)
                .any(|w| w == ["--reasoning-effort", "xhigh"])
        );
    }

    #[test]
    fn resume_command_passes_session_id() {
        let member = TeamMember::new("grok", "Grok", BackendKind::Grok, "implementation");
        let adapter = GrokStreamAdapter::from_member(&member, Path::new("/tmp/ws"));

        let command = adapter.build_command(
            "continue",
            Some(&AgentSessionId("session-9".to_string())),
            None,
        );

        assert!(
            command
                .args
                .windows(2)
                .any(|w| w == ["--resume", "session-9"])
        );
    }

    #[test]
    fn text_reasoning_and_end_form_a_completed_message() {
        let events = parse_all(&[
            r#"{"type":"thought","data":"checking"}"#,
            r#"{"type":"text","data":"Hello "}"#,
            r#"{"type":"text","data":"world"}"#,
            r#"{"type":"end","sessionId":"session-1","stopReason":"end_turn"}"#,
        ]);

        assert_eq!(
            events,
            vec![
                AgentEvent::Reasoning("checking".to_string()),
                AgentEvent::MessageStarted,
                AgentEvent::TextDelta("Hello ".to_string()),
                AgentEvent::TextDelta("world".to_string()),
                AgentEvent::MessageCompleted("Hello world".to_string()),
                AgentEvent::SessionDiscovered(AgentSessionId("session-1".to_string())),
            ]
        );
    }

    #[test]
    fn tool_events_keep_a_stable_generated_id() {
        let events = parse_all(&[
            r#"{"type":"tool_started","tool_name":"shell","data":"cargo test"}"#,
            r#"{"type":"tool_completed","tool_name":"shell","data":"cargo test","outcome":"success","duration_ms":20}"#,
        ]);

        assert_eq!(
            events,
            vec![
                AgentEvent::ToolStarted {
                    id: "grok-tool-1-shell".to_string(),
                    name: "shell".to_string(),
                    summary: "cargo test".to_string(),
                },
                AgentEvent::ToolCompleted {
                    id: "grok-tool-1-shell".to_string(),
                    ok: true,
                    summary: "cargo test (success)".to_string(),
                },
            ]
        );
    }

    #[test]
    fn invalid_json_is_a_parse_warning() {
        assert!(matches!(
            parse_all(&["not json"]).as_slice(),
            [AgentEvent::ParseWarning(message)] if message.contains("grok")
        ));
    }
}
