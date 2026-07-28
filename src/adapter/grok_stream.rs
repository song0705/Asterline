//! Grok Build ACP adapter.
//!
//! Grok's headless `streaming-json` format carries text and thought chunks but
//! does not carry tool calls. The product path therefore uses the official ACP
//! stdio server (`grok agent stdio`) so text, reasoning, tools, and resumable
//! sessions all come from one structured protocol.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

use crate::adapter::parser::{str_field, summarize, tool_detail, tool_value};
use crate::adapter::{MemberRunner, RunRequest};
use crate::domain::event::{AgentEvent, AgentSessionId};
use crate::domain::team::{BackendKind, Effort, PermissionMode, SandboxPolicy, TeamMember};

const TOOL_SUMMARY_MAX: usize = 160;
const TOOL_OUTPUT_MAX: usize = 32_000;

#[derive(Clone, Debug)]
pub struct GrokAcpRunner {
    binary: String,
    cwd: PathBuf,
    sandbox: SandboxPolicy,
    model: Option<String>,
    permission_mode: Option<PermissionMode>,
    allowed_tools: Vec<String>,
    system_prompt: Option<String>,
}

impl GrokAcpRunner {
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

    /// Global Grok flags must precede `agent`; agent flags must precede `stdio`.
    fn command_args(&self, effort: Option<Effort>) -> Vec<String> {
        let mut args = vec!["--sandbox".to_string(), self.sandbox.grok_arg().to_string()];
        if let Some(mode) = self.permission_mode {
            args.push("--permission-mode".to_string());
            args.push(mode.grok_arg().to_string());
        }
        args.push("agent".to_string());
        // Asterline owns the process lifecycle. Avoid silently attaching to a
        // shared leader with different confinement or model settings.
        args.push("--no-leader".to_string());
        if let Some(model) = &self.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }
        if let Some(effort) = effort {
            args.push("--reasoning-effort".to_string());
            args.push(effort.as_str().to_string());
        }
        if self.permission_mode == Some(PermissionMode::BypassPermissions) {
            args.push("--always-approve".to_string());
        }
        args.push("stdio".to_string());
        args
    }

    fn session_meta(&self) -> Value {
        let mut meta = serde_json::Map::new();
        if let Some(rules) = self.effective_rules() {
            meta.insert("rules".to_string(), Value::String(rules));
        }
        match self.permission_mode {
            Some(PermissionMode::BypassPermissions) => {
                meta.insert("yoloMode".to_string(), Value::Bool(true));
            }
            Some(PermissionMode::Auto) => {
                meta.insert("autoMode".to_string(), Value::Bool(true));
            }
            _ => {}
        }
        Value::Object(meta)
    }

    fn effective_rules(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(system_prompt) = self
            .system_prompt
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            parts.push(system_prompt.to_string());
        }
        // ACP currently has no built-in-tool allowlist field. Keep the member
        // constraint visible to the model while sandbox and permission policy
        // remain the enforcement boundary.
        if !self.allowed_tools.is_empty() {
            parts.push(format!(
                "Asterline tool constraint: use only these built-in tools: {}.",
                self.allowed_tools.join(", ")
            ));
        }
        (!parts.is_empty()).then(|| parts.join("\n\n"))
    }
}

impl MemberRunner for GrokAcpRunner {
    fn backend(&self) -> BackendKind {
        BackendKind::Grok
    }

    fn run(&self, req: RunRequest, events: Sender<AgentEvent>) {
        run_acp(self, req, events);
    }
}

fn run_acp(runner: &GrokAcpRunner, req: RunRequest, events: Sender<AgentEvent>) {
    let mut builder = Command::new(&runner.binary);
    builder
        .args(runner.command_args(req.effort))
        .current_dir(&runner.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match builder.spawn() {
        Ok(child) => child,
        Err(err) => {
            let _ = events.send(AgentEvent::Fatal(format!(
                "failed to start {} ACP server: {err}",
                runner.binary
            )));
            let _ = events.send(AgentEvent::Exited {
                code: None,
                ok: false,
            });
            return;
        }
    };

    let Some(stdin) = child.stdin.take() else {
        let _ = events.send(AgentEvent::Fatal(
            "grok ACP server did not expose stdin".to_string(),
        ));
        let _ = child.kill();
        let _ = child.wait();
        let _ = events.send(AgentEvent::Exited {
            code: None,
            ok: false,
        });
        return;
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = events.send(AgentEvent::Fatal(
            "grok ACP server did not expose stdout".to_string(),
        ));
        let _ = child.kill();
        let _ = child.wait();
        let _ = events.send(AgentEvent::Exited {
            code: None,
            ok: false,
        });
        return;
    };
    let stderr = child.stderr.take();
    let child = Arc::new(Mutex::new(child));
    let done = Arc::new(AtomicBool::new(false));

    let watcher = {
        let child = Arc::clone(&child);
        let done = Arc::clone(&done);
        let cancel = Arc::clone(&req.cancel);
        thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                if cancel.load(Ordering::Relaxed) {
                    if let Ok(mut child) = child.lock() {
                        let _ = child.kill();
                    }
                    break;
                }
                thread::sleep(Duration::from_millis(50));
            }
        })
    };

    let stderr_thread = stderr.map(|stderr| {
        let events = events.clone();
        thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let _ = events.send(AgentEvent::Stderr(line));
            }
        })
    });

    let mut input = BufWriter::new(stdin);
    let mut output = BufReader::new(stdout).lines();
    let mut protocol_ok = true;

    protocol_ok &= send_request(
        &mut input,
        1,
        "initialize",
        json!({
            "protocolVersion": 1,
            "clientCapabilities": {
                "fs": {"readTextFile": false, "writeTextFile": false},
                "terminal": false
            },
            "_meta": {"clientIdentifier": "asterline"}
        }),
        &events,
    );
    if protocol_ok {
        protocol_ok &= wait_for_response(&mut output, 1, &events).is_some();
    }

    let meta = runner.session_meta();
    let session_id = if protocol_ok {
        match req.session.as_ref() {
            Some(session) => {
                let mut load_meta = meta;
                if let Some(object) = load_meta.as_object_mut() {
                    object.insert("noReplay".to_string(), Value::Bool(true));
                }
                protocol_ok &= send_request(
                    &mut input,
                    2,
                    "session/load",
                    json!({
                        "sessionId": session.as_str(),
                        "cwd": runner.cwd,
                        "mcpServers": [],
                        "_meta": load_meta
                    }),
                    &events,
                );
                if protocol_ok {
                    protocol_ok &= wait_for_response(&mut output, 2, &events).is_some();
                }
                protocol_ok.then(|| session.as_str().to_string())
            }
            None => {
                protocol_ok &= send_request(
                    &mut input,
                    2,
                    "session/new",
                    json!({
                        "cwd": runner.cwd,
                        "mcpServers": [],
                        "_meta": meta
                    }),
                    &events,
                );
                let response = if protocol_ok {
                    wait_for_response(&mut output, 2, &events)
                } else {
                    None
                };
                response
                    .as_ref()
                    .and_then(|value| str_field(&value["result"], "sessionId"))
                    .map(str::to_string)
            }
        }
    } else {
        None
    };

    if protocol_ok && session_id.is_none() {
        let _ = events.send(AgentEvent::Fatal(
            "grok ACP session response did not include a sessionId".to_string(),
        ));
        protocol_ok = false;
    }

    if let Some(session_id) = session_id {
        let _ = events.send(AgentEvent::SessionDiscovered(AgentSessionId(
            session_id.clone(),
        )));
        protocol_ok &= send_request(
            &mut input,
            3,
            "session/prompt",
            json!({
                "sessionId": session_id,
                "prompt": [{"type": "text", "text": req.prompt}]
            }),
            &events,
        );
        if protocol_ok {
            let mut parser = GrokAcpParser::default();
            protocol_ok = process_prompt(
                &mut output,
                &mut input,
                &mut parser,
                runner.permission_mode,
                &events,
            );
            for event in parser.finish() {
                let _ = events.send(event);
            }
        }
    }

    // ACP stdio exits on EOF after the request has completed.
    drop(input);
    done.store(true, Ordering::Relaxed);
    let status = child.lock().ok().and_then(|mut child| child.wait().ok());
    if let Some(stderr_thread) = stderr_thread {
        let _ = stderr_thread.join();
    }
    let _ = watcher.join();

    match status {
        Some(status) => {
            let _ = events.send(AgentEvent::Exited {
                code: status.code(),
                ok: protocol_ok && status.success(),
            });
        }
        None => {
            let _ = events.send(AgentEvent::Fatal(
                "failed to wait for grok ACP server".to_string(),
            ));
            let _ = events.send(AgentEvent::Exited {
                code: None,
                ok: false,
            });
        }
    }
}

fn send_request(
    input: &mut BufWriter<impl Write>,
    id: u64,
    method: &str,
    params: Value,
    events: &Sender<AgentEvent>,
) -> bool {
    let message = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params
    });
    if let Err(err) = writeln!(input, "{message}").and_then(|_| input.flush()) {
        let _ = events.send(AgentEvent::Fatal(format!(
            "grok ACP {method} request failed: {err}"
        )));
        false
    } else {
        true
    }
}

fn wait_for_response(
    output: &mut impl Iterator<Item = std::io::Result<String>>,
    wanted_id: u64,
    events: &Sender<AgentEvent>,
) -> Option<Value> {
    for line in output {
        let line = match line {
            Ok(line) => line,
            Err(err) => {
                let _ = events.send(AgentEvent::Fatal(format!(
                    "failed to read grok ACP output: {err}"
                )));
                return None;
            }
        };
        let _ = events.send(AgentEvent::Raw(line.clone()));
        let value: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(err) => {
                let _ = events.send(AgentEvent::ParseWarning(format!(
                    "grok ACP: invalid JSON line: {err}"
                )));
                continue;
            }
        };
        if value.get("id").and_then(Value::as_u64) != Some(wanted_id) {
            continue;
        }
        if let Some(error) = value.get("error") {
            let message = str_field(error, "message")
                .unwrap_or("grok ACP request failed")
                .to_string();
            let _ = events.send(AgentEvent::Fatal(message));
            return None;
        }
        return Some(value);
    }
    let _ = events.send(AgentEvent::Fatal(format!(
        "grok ACP closed before response {wanted_id}"
    )));
    None
}

fn process_prompt(
    output: &mut impl Iterator<Item = std::io::Result<String>>,
    input: &mut BufWriter<impl Write>,
    parser: &mut GrokAcpParser,
    permission_mode: Option<PermissionMode>,
    events: &Sender<AgentEvent>,
) -> bool {
    for line in output {
        let line = match line {
            Ok(line) => line,
            Err(err) => {
                let _ = events.send(AgentEvent::Fatal(format!(
                    "failed to read grok ACP output: {err}"
                )));
                return false;
            }
        };
        let _ = events.send(AgentEvent::Raw(line.clone()));
        let value: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(err) => {
                let _ = events.send(AgentEvent::ParseWarning(format!(
                    "grok ACP: invalid JSON line: {err}"
                )));
                continue;
            }
        };

        match str_field(&value, "method") {
            Some("session/update") | Some("x.ai/session/update") => {
                for event in parser.parse_update(&value["params"]["update"]) {
                    let _ = events.send(event);
                }
            }
            Some("session/request_permission") => {
                respond_to_permission(input, &value, permission_mode, events);
            }
            Some(method) if value.get("id").is_some() => {
                respond_method_not_found(input, &value, method, events);
            }
            _ => {}
        }

        if value.get("id").and_then(Value::as_u64) == Some(3) {
            if let Some(error) = value.get("error") {
                let _ = events.send(AgentEvent::Fatal(
                    str_field(error, "message")
                        .unwrap_or("grok prompt failed")
                        .to_string(),
                ));
                return false;
            }
            return true;
        }
    }
    let _ = events.send(AgentEvent::Fatal(
        "grok ACP closed before the prompt completed".to_string(),
    ));
    false
}

fn respond_to_permission(
    input: &mut BufWriter<impl Write>,
    request: &Value,
    mode: Option<PermissionMode>,
    events: &Sender<AgentEvent>,
) {
    let Some(id) = request.get("id").cloned() else {
        return;
    };
    let tool_kind = str_field(&request["params"]["toolCall"], "kind").unwrap_or_default();
    let allow = match mode.unwrap_or(PermissionMode::Default) {
        PermissionMode::BypassPermissions | PermissionMode::Auto => true,
        PermissionMode::AcceptEdits => matches!(tool_kind, "edit" | "delete" | "move"),
        PermissionMode::Default | PermissionMode::DontAsk | PermissionMode::Plan => false,
    };
    let preferred = if allow {
        ["allow_once", "allow_always"]
    } else {
        ["reject_once", "reject_always"]
    };
    let option = request["params"]["options"]
        .as_array()
        .and_then(|options| {
            preferred.iter().find_map(|kind| {
                options
                    .iter()
                    .find(|option| str_field(option, "kind") == Some(*kind))
            })
        })
        .and_then(|option| str_field(option, "optionId"));
    let outcome = match option {
        Some(option_id) => json!({"outcome": "selected", "optionId": option_id}),
        None => json!({"outcome": "cancelled"}),
    };
    let response = json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {"outcome": outcome}
    });
    if let Err(err) = writeln!(input, "{response}").and_then(|_| input.flush()) {
        let _ = events.send(AgentEvent::Fatal(format!(
            "failed to answer grok permission request: {err}"
        )));
    }
}

fn respond_method_not_found(
    input: &mut BufWriter<impl Write>,
    request: &Value,
    method: &str,
    events: &Sender<AgentEvent>,
) {
    let response = json!({
        "jsonrpc": "2.0",
        "id": request["id"].clone(),
        "error": {"code": -32601, "message": format!("unsupported ACP client method: {method}")}
    });
    if let Err(err) = writeln!(input, "{response}").and_then(|_| input.flush()) {
        let _ = events.send(AgentEvent::Fatal(format!(
            "failed to reject unsupported grok ACP method: {err}"
        )));
    }
}

#[derive(Default)]
pub struct GrokAcpParser {
    message_open: bool,
    text_acc: String,
    active_tools: HashMap<String, String>,
    completed_tools: HashSet<String>,
}

impl GrokAcpParser {
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

    pub fn parse_update(&mut self, update: &Value) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        match str_field(update, "sessionUpdate") {
            Some("agent_message_chunk") => {
                if let Some(text) = content_text(&update["content"])
                    && !text.is_empty()
                {
                    self.open_message(&mut out);
                    self.text_acc.push_str(text);
                    out.push(AgentEvent::TextDelta(text.to_string()));
                }
            }
            Some("agent_thought_chunk") => {
                if let Some(text) = content_text(&update["content"])
                    && !text.is_empty()
                {
                    out.push(AgentEvent::Reasoning(text.to_string()));
                }
            }
            Some("tool_call") => {
                let id = str_field(update, "toolCallId")
                    .unwrap_or("grok-tool")
                    .to_string();
                let title = str_field(update, "title").unwrap_or("tool").to_string();
                let name = str_field(update, "kind").unwrap_or(&title).to_string();
                self.active_tools.insert(id.clone(), title.clone());
                let summary = update
                    .get("rawInput")
                    .map(|value| tool_value(value, TOOL_SUMMARY_MAX))
                    .filter(|value| !value.is_empty())
                    .map(|input| format!("{title}: {input}"))
                    .unwrap_or(title);
                out.push(AgentEvent::ToolStarted {
                    id: id.clone(),
                    name,
                    summary: summarize(&summary, TOOL_SUMMARY_MAX),
                });
                if terminal_tool_status(str_field(update, "status")) {
                    out.extend(self.complete_tool(update, id));
                }
            }
            Some("tool_call_update") => {
                let id = str_field(update, "toolCallId")
                    .unwrap_or("grok-tool")
                    .to_string();
                if terminal_tool_status(str_field(update, "status")) {
                    out.extend(self.complete_tool(update, id));
                } else if let Some(detail) = tool_update_detail(update)
                    && !detail.is_empty()
                {
                    out.push(AgentEvent::ToolProgress { id, delta: detail });
                }
            }
            Some("plan") => {
                if let Some(entries) = update.get("entries") {
                    out.push(AgentEvent::Log(format!(
                        "grok plan: {}",
                        summarize(&tool_value(entries, TOOL_OUTPUT_MAX), TOOL_SUMMARY_MAX)
                    )));
                }
            }
            Some(other) => out.push(AgentEvent::Log(format!("grok ACP update: {other}"))),
            None => out.push(AgentEvent::ParseWarning(
                "grok ACP update without sessionUpdate".to_string(),
            )),
        }
        out
    }

    fn complete_tool(&mut self, update: &Value, id: String) -> Vec<AgentEvent> {
        if !self.completed_tools.insert(id.clone()) {
            return Vec::new();
        }
        let status = str_field(update, "status").unwrap_or("completed");
        let fallback = self
            .active_tools
            .remove(&id)
            .unwrap_or_else(|| "tool".to_string());
        let summary = tool_update_detail(update)
            .filter(|value| !value.is_empty())
            .unwrap_or(fallback);
        let mut out = vec![AgentEvent::ToolCompleted {
            id,
            ok: status == "completed",
            summary: tool_detail(&summary, TOOL_OUTPUT_MAX),
        }];
        let files = tool_diff_files(update);
        if !files.is_empty() {
            out.push(AgentEvent::FileChange {
                files,
                ok: status == "completed",
            });
        }
        out
    }

    pub fn finish(&mut self) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        self.close_message(&mut out);
        out
    }
}

fn content_text(content: &Value) -> Option<&str> {
    content
        .get("text")
        .and_then(Value::as_str)
        .or_else(|| content.as_str())
}

fn terminal_tool_status(status: Option<&str>) -> bool {
    matches!(status, Some("completed" | "failed"))
}

fn tool_update_detail(update: &Value) -> Option<String> {
    ["rawOutput", "content", "rawInput"]
        .into_iter()
        .find_map(|field| {
            update
                .get(field)
                .map(|value| tool_value(value, TOOL_OUTPUT_MAX))
                .filter(|value| !value.is_empty() && value != "null")
        })
}

fn tool_diff_files(update: &Value) -> Vec<(String, String)> {
    update
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            if str_field(item, "type") != Some("diff") {
                return None;
            }
            str_field(item, "path").map(|path| (path.to_string(), "update".to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_uses_agent_stdio_with_flags_in_their_required_positions() {
        let mut member = TeamMember::new("grok", "Grok", BackendKind::Grok, "implementation");
        member.sandbox = SandboxPolicy::WorkspaceWrite;
        member.model = Some("grok-build".to_string());
        member.permission_mode = Some(PermissionMode::Auto);
        let runner = GrokAcpRunner::from_member(&member, Path::new("/tmp/ws"));

        assert_eq!(
            runner.command_args(Some(Effort::Xhigh)),
            vec![
                "--sandbox",
                "workspace",
                "--permission-mode",
                "auto",
                "agent",
                "--no-leader",
                "--model",
                "grok-build",
                "--reasoning-effort",
                "xhigh",
                "stdio",
            ]
        );
    }

    #[test]
    fn session_meta_carries_rules_tool_constraint_and_auto_mode() {
        let mut member = TeamMember::new("grok", "Grok", BackendKind::Grok, "implementation");
        member.permission_mode = Some(PermissionMode::Auto);
        member.system_prompt = Some("team rules".to_string());
        member.allowed_tools = vec!["shell".to_string(), "read_file".to_string()];
        let runner = GrokAcpRunner::from_member(&member, Path::new("/tmp/ws"));
        let meta = runner.session_meta();

        assert_eq!(meta["autoMode"], true);
        assert!(meta["rules"].as_str().unwrap().contains("team rules"));
        assert!(meta["rules"].as_str().unwrap().contains("shell, read_file"));
    }

    #[test]
    fn parses_text_reasoning_and_tool_lifecycle() {
        let mut parser = GrokAcpParser::default();
        let mut events = Vec::new();
        events.extend(parser.parse_update(&json!({
            "sessionUpdate": "agent_thought_chunk",
            "content": {"type": "text", "text": "checking"}
        })));
        events.extend(parser.parse_update(&json!({
            "sessionUpdate": "agent_message_chunk",
            "content": {"type": "text", "text": "Done"}
        })));
        events.extend(parser.parse_update(&json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "tool-1",
            "title": "Run tests",
            "kind": "execute",
            "status": "in_progress",
            "rawInput": {"command": "cargo test"}
        })));
        events.extend(parser.parse_update(&json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "tool-1",
            "status": "completed",
            "rawOutput": {"stdout": "ok"}
        })));
        events.extend(parser.finish());

        assert!(events.contains(&AgentEvent::Reasoning("checking".to_string())));
        assert!(events.contains(&AgentEvent::TextDelta("Done".to_string())));
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolStarted { id, name, .. }
                if id == "tool-1" && name == "execute"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolCompleted { id, ok: true, summary }
                if id == "tool-1" && summary.contains("stdout")
        )));
        assert!(events.contains(&AgentEvent::MessageCompleted("Done".to_string())));
    }

    #[test]
    fn completed_diff_emits_file_change() {
        let mut parser = GrokAcpParser::default();
        let events = parser.parse_update(&json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "edit-1",
            "status": "completed",
            "content": [{"type": "diff", "path": "/tmp/ws/src/lib.rs", "newText": "x"}]
        }));
        assert!(events.contains(&AgentEvent::FileChange {
            files: vec![("/tmp/ws/src/lib.rs".to_string(), "update".to_string())],
            ok: true,
        }));
    }

    #[test]
    fn tool_title_is_used_when_optional_kind_is_absent() {
        let mut parser = GrokAcpParser::default();
        let events = parser.parse_update(&json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "read-1",
            "title": "read_file",
            "status": "in_progress",
            "rawInput": {"target_file": "fixture.txt"}
        }));
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolStarted { name, .. } if name == "read_file"
        )));
    }
}
