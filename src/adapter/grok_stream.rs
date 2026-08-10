//! Grok Build ACP adapter.
//!
//! Grok's headless `streaming-json` format carries text and thought chunks but
//! does not carry tool calls. The product path therefore uses the official ACP
//! stdio server (`grok agent stdio`) so text, reasoning, tools, and resumable
//! sessions all come from one structured protocol.

use std::collections::{HashMap, HashSet};
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::adapter::parser::{
    MAX_MESSAGE_TEXT_BYTES, append_bounded_text, str_field, summarize, tool_detail, tool_value,
};
use crate::adapter::process::{
    ChildProcessTree, MAX_PROTOCOL_LINE_BYTES, MAX_STDERR_LINE_BYTES, bounded_lines,
    configure_process_tree,
};
use crate::adapter::{MemberRunner, RunRequest};
use crate::domain::config::resolve_binary_on_path;
use crate::domain::event::{AgentEvent, AgentSessionId};
use crate::domain::team::{BackendKind, Effort, PermissionMode, SandboxPolicy, TeamMember};

const TOOL_SUMMARY_MAX: usize = 160;
const TOOL_OUTPUT_MAX: usize = 32_000;
const CANCEL_GRACE: Duration = Duration::from_secs(2);
const ACP_OUTPUT_QUEUE_CAPACITY: usize = 128;

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
        let mut args = vec![
            "--no-auto-update".to_string(),
            "--sandbox".to_string(),
            self.sandbox.grok_arg().to_string(),
        ];
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

    fn run(&self, req: RunRequest, events: SyncSender<AgentEvent>) {
        run_acp(self, req, events);
    }
}

fn run_acp(runner: &GrokAcpRunner, req: RunRequest, events: SyncSender<AgentEvent>) {
    let resolved_binary =
        resolve_binary_on_path(&runner.binary).unwrap_or_else(|| PathBuf::from(&runner.binary));
    let mut builder = Command::new(resolved_binary);
    builder
        .args(runner.command_args(req.effort))
        .current_dir(&runner.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_process_tree(&mut builder);

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

    let process_tree = match ChildProcessTree::attach(&mut child) {
        Ok(process_tree) => Arc::new(process_tree),
        Err(err) => {
            let _ = child.kill();
            let _ = child.wait();
            let _ = events.send(AgentEvent::Fatal(format!(
                "failed to isolate {} ACP process tree: {err}",
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
        let _ = process_tree.terminate_with_fallback(&mut child);
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
        let _ = process_tree.terminate_with_fallback(&mut child);
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
    let transport_ok = Arc::new(AtomicBool::new(true));

    let watcher = {
        let child = Arc::clone(&child);
        let process_tree = Arc::clone(&process_tree);
        let done = Arc::clone(&done);
        let cancel = Arc::clone(&req.cancel);
        thread::spawn(move || {
            let mut deadline = None;
            while !done.load(Ordering::Relaxed) {
                if cancel.load(Ordering::Relaxed) {
                    let deadline = *deadline.get_or_insert_with(|| Instant::now() + CANCEL_GRACE);
                    if Instant::now() >= deadline {
                        if process_tree.terminate().is_err()
                            && let Ok(mut child) = child.lock()
                        {
                            let _ = child.kill();
                        }
                        break;
                    }
                }
                thread::sleep(Duration::from_millis(25));
            }
        })
    };

    let stderr_thread = stderr.map(|stderr| {
        let events = events.clone();
        let process_tree = Arc::clone(&process_tree);
        let transport_ok = Arc::clone(&transport_ok);
        thread::spawn(move || {
            for line in bounded_lines(BufReader::new(stderr), MAX_STDERR_LINE_BYTES) {
                match line {
                    Ok(line) => {
                        let _ = events.send(AgentEvent::Stderr(line));
                    }
                    Err(err) => {
                        transport_ok.store(false, Ordering::Relaxed);
                        let _ = events.send(AgentEvent::Fatal(format!(
                            "failed to read grok ACP stderr: {err}"
                        )));
                        let _ = process_tree.terminate();
                        break;
                    }
                }
            }
        })
    });

    let mut input = BufWriter::new(stdin);
    let mut output = bounded_lines(BufReader::new(stdout), MAX_PROTOCOL_LINE_BYTES);
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
            "clientInfo": {
                "name": "asterline",
                "version": env!("CARGO_PKG_VERSION")
            },
            "_meta": {"clientIdentifier": "asterline"}
        }),
        &events,
    );
    let initialize_response = if protocol_ok {
        wait_for_response(&mut output, 1, &events)
    } else {
        None
    };
    if protocol_ok {
        protocol_ok = initialize_response.as_ref().is_some_and(|response| {
            if response["result"]["protocolVersion"].as_u64() == Some(1) {
                true
            } else {
                let _ = events.send(AgentEvent::Fatal(
                    "grok ACP server negotiated an unsupported protocol version".to_string(),
                ));
                false
            }
        });
    }

    let load_session_supported = initialize_response
        .as_ref()
        .is_some_and(supports_load_session);
    if protocol_ok && req.session.is_some() && !load_session_supported {
        let _ = events.send(AgentEvent::Fatal(
            "grok ACP server does not support restoring sessions".to_string(),
        ));
        protocol_ok = false;
    }

    let mut next_request_id = 2;
    if protocol_ok {
        match initialize_response.as_ref().and_then(|response| {
            select_auth_method(response, std::env::var_os("XAI_API_KEY").is_some())
        }) {
            Some(method_id) => {
                protocol_ok &= send_request(
                    &mut input,
                    next_request_id,
                    "authenticate",
                    json!({
                        "methodId": method_id,
                        "_meta": {"headless": true}
                    }),
                    &events,
                );
                if protocol_ok {
                    protocol_ok &=
                        wait_for_response(&mut output, next_request_id, &events).is_some();
                }
                next_request_id += 1;
            }
            None => {
                let _ = events.send(AgentEvent::Fatal(
                    "grok ACP has no usable authentication method; run `grok login` or set XAI_API_KEY"
                        .to_string(),
                ));
                protocol_ok = false;
            }
        }
    }

    let meta = runner.session_meta();
    let session_request_id = next_request_id;
    let prompt_request_id = session_request_id + 1;
    let session_id = if protocol_ok {
        match req.session.as_ref() {
            Some(session) => {
                let mut load_meta = meta;
                if let Some(object) = load_meta.as_object_mut() {
                    object.insert("noReplay".to_string(), Value::Bool(true));
                }
                protocol_ok &= send_request(
                    &mut input,
                    session_request_id,
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
                    protocol_ok &=
                        wait_for_response(&mut output, session_request_id, &events).is_some();
                }
                protocol_ok.then(|| session.as_str().to_string())
            }
            None => {
                protocol_ok &= send_request(
                    &mut input,
                    session_request_id,
                    "session/new",
                    json!({
                        "cwd": runner.cwd,
                        "mcpServers": [],
                        "_meta": meta
                    }),
                    &events,
                );
                let response = if protocol_ok {
                    wait_for_response(&mut output, session_request_id, &events)
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

    let mut output_thread = None;
    if let Some(session_id) = session_id {
        let _ = events.send(AgentEvent::SessionDiscovered(AgentSessionId(
            session_id.clone(),
        )));
        protocol_ok &= send_request(
            &mut input,
            prompt_request_id,
            "session/prompt",
            json!({
                "sessionId": session_id,
                "prompt": [{"type": "text", "text": req.prompt}]
            }),
            &events,
        );
        if protocol_ok {
            let (output_tx, output_rx) = mpsc::sync_channel(ACP_OUTPUT_QUEUE_CAPACITY);
            output_thread = Some(thread::spawn(move || {
                for line in output {
                    if output_tx.send(line).is_err() {
                        break;
                    }
                }
            }));
            let mut parser = GrokAcpParser::default();
            protocol_ok = process_prompt(
                &output_rx,
                &mut input,
                &mut parser,
                PromptContext {
                    permission_mode: runner.permission_mode,
                    cancel: &req.cancel,
                    session_id: &session_id,
                    request_id: prompt_request_id,
                    events: &events,
                },
            );
            for event in parser.finish(req.cancel.load(Ordering::Relaxed)) {
                let _ = events.send(event);
            }
        }
    }

    if !protocol_ok {
        let _ = process_tree.terminate();
    }
    // ACP stdio exits on EOF after the request has completed.
    drop(input);
    // Poll instead of holding the child mutex across `wait`: a cancelled ACP
    // server that acknowledges the turn but ignores stdin EOF must remain
    // killable by the grace-period watcher.
    let status = wait_for_child(&child);
    done.store(true, Ordering::Relaxed);
    if let Some(output_thread) = output_thread {
        let _ = output_thread.join();
    }
    if let Some(stderr_thread) = stderr_thread {
        let _ = stderr_thread.join();
    }
    let _ = watcher.join();

    match status {
        Some(status) => {
            let _ = events.send(AgentEvent::Exited {
                code: status.code(),
                ok: protocol_ok && transport_ok.load(Ordering::Relaxed) && status.success(),
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

fn wait_for_child(child: &Mutex<Child>) -> Option<ExitStatus> {
    loop {
        let status = child.lock().ok()?.try_wait().ok()?;
        if status.is_some() {
            return status;
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn send_request(
    input: &mut BufWriter<impl Write>,
    id: u64,
    method: &str,
    params: Value,
    events: &SyncSender<AgentEvent>,
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

fn send_notification(
    input: &mut BufWriter<impl Write>,
    method: &str,
    params: Value,
    events: &SyncSender<AgentEvent>,
) -> bool {
    let message = json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params
    });
    if let Err(err) = writeln!(input, "{message}").and_then(|_| input.flush()) {
        let _ = events.send(AgentEvent::Fatal(format!(
            "grok ACP {method} notification failed: {err}"
        )));
        false
    } else {
        true
    }
}

fn supports_load_session(response: &Value) -> bool {
    response["result"]["agentCapabilities"]["loadSession"].as_bool() == Some(true)
}

fn select_auth_method(response: &Value, api_key_present: bool) -> Option<&'static str> {
    let methods = response["result"]["authMethods"].as_array()?;
    let advertises = |wanted: &str| {
        methods
            .iter()
            .any(|method| str_field(method, "id") == Some(wanted))
    };
    if api_key_present && advertises("xai.api_key") {
        Some("xai.api_key")
    } else if advertises("cached_token") {
        Some("cached_token")
    } else {
        None
    }
}

fn wait_for_response(
    output: &mut impl Iterator<Item = std::io::Result<String>>,
    wanted_id: u64,
    events: &SyncSender<AgentEvent>,
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

struct PromptContext<'a> {
    permission_mode: Option<PermissionMode>,
    cancel: &'a AtomicBool,
    session_id: &'a str,
    request_id: u64,
    events: &'a SyncSender<AgentEvent>,
}

fn process_prompt(
    output: &Receiver<std::io::Result<String>>,
    input: &mut BufWriter<impl Write>,
    parser: &mut GrokAcpParser,
    context: PromptContext<'_>,
) -> bool {
    let mut cancel_sent = false;
    loop {
        if context.cancel.load(Ordering::Relaxed) && !cancel_sent {
            if !send_notification(
                input,
                "session/cancel",
                json!({"sessionId": context.session_id}),
                context.events,
            ) {
                return false;
            }
            cancel_sent = true;
        }
        let line = match output.recv_timeout(Duration::from_millis(25)) {
            Ok(line) => line,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) if cancel_sent => return false,
            Err(RecvTimeoutError::Disconnected) => {
                let _ = context.events.send(AgentEvent::Fatal(
                    "grok ACP closed before the prompt completed".to_string(),
                ));
                return false;
            }
        };
        let line = match line {
            Ok(line) => line,
            Err(err) => {
                let _ = context.events.send(AgentEvent::Fatal(format!(
                    "failed to read grok ACP output: {err}"
                )));
                return false;
            }
        };
        let _ = context.events.send(AgentEvent::Raw(line.clone()));
        let value: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(err) => {
                let _ = context.events.send(AgentEvent::ParseWarning(format!(
                    "grok ACP: invalid JSON line: {err}"
                )));
                continue;
            }
        };

        match str_field(&value, "method") {
            Some("session/update") | Some("x.ai/session/update") => {
                for event in parser.parse_update(&value["params"]["update"]) {
                    let _ = context.events.send(event);
                }
            }
            Some("session/request_permission") => {
                respond_to_permission(
                    input,
                    &value,
                    context.permission_mode,
                    cancel_sent || context.cancel.load(Ordering::Relaxed),
                    context.events,
                );
            }
            Some(method) if value.get("id").is_some() => {
                respond_method_not_found(input, &value, method, context.events);
            }
            _ => {}
        }

        if value.get("id").and_then(Value::as_u64) == Some(context.request_id) {
            if let Some(error) = value.get("error") {
                let _ = context.events.send(AgentEvent::Fatal(
                    str_field(error, "message")
                        .unwrap_or("grok prompt failed")
                        .to_string(),
                ));
                return false;
            }
            return match str_field(&value["result"], "stopReason") {
                Some("end_turn") => true,
                Some("cancelled") if cancel_sent || context.cancel.load(Ordering::Relaxed) => true,
                Some(reason) => {
                    let _ = context.events.send(AgentEvent::Fatal(format!(
                        "grok ACP prompt stopped before completion: {reason}"
                    )));
                    false
                }
                None => {
                    let _ = context.events.send(AgentEvent::Fatal(
                        "grok ACP prompt response did not include a stopReason".to_string(),
                    ));
                    false
                }
            };
        }
    }
}

fn respond_to_permission(
    input: &mut BufWriter<impl Write>,
    request: &Value,
    mode: Option<PermissionMode>,
    cancelled: bool,
    events: &SyncSender<AgentEvent>,
) {
    let Some(id) = request.get("id").cloned() else {
        return;
    };
    let tool_kind = str_field(&request["params"]["toolCall"], "kind").unwrap_or_default();
    let allow = match mode.unwrap_or(PermissionMode::Default) {
        PermissionMode::BypassPermissions => true,
        PermissionMode::AcceptEdits => matches!(tool_kind, "edit" | "delete" | "move"),
        PermissionMode::Default
        | PermissionMode::DontAsk
        | PermissionMode::Plan
        | PermissionMode::Auto => false,
    };
    let preferred = if allow {
        ["allow_once", "allow_always"]
    } else {
        ["reject_once", "reject_always"]
    };
    let option = (!cancelled)
        .then_some(&request["params"]["options"])
        .and_then(|options| options.as_array())
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
    events: &SyncSender<AgentEvent>,
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
                    if let Some(delta) =
                        append_bounded_text(&mut self.text_acc, text, MAX_MESSAGE_TEXT_BYTES)
                    {
                        out.push(AgentEvent::TextDelta(delta));
                    }
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
        let fallback = self
            .active_tools
            .remove(&id)
            .unwrap_or_else(|| "tool".to_string());
        if !self.completed_tools.insert(id.clone()) {
            return Vec::new();
        }
        let status = str_field(update, "status").unwrap_or("completed");
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

    pub fn finish(&mut self, cancelled: bool) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        self.close_message(&mut out);
        let summary = if cancelled {
            "cancelled"
        } else {
            "grok ACP stream ended before tool completion"
        };
        for (id, _) in std::mem::take(&mut self.active_tools) {
            if self.completed_tools.insert(id.clone()) {
                out.push(AgentEvent::ToolCompleted {
                    id,
                    ok: false,
                    summary: summary.to_string(),
                });
            }
        }
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
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::sync::mpsc;

    #[cfg(unix)]
    const TEST_STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
    #[cfg(unix)]
    const TEST_CANCEL_TIMEOUT: Duration = Duration::from_secs(5);

    #[cfg(unix)]
    fn fake_acp_server(
        dir: &Path,
        graceful: bool,
        linger_after_response: bool,
    ) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        let server = dir.join("fake-grok-acp");
        let cancel_log = dir.join("cancel.json");
        let permission_log = dir.join("permission.json");
        let eof_log = dir.join("stdin-closed");
        let mut lines = vec![
            "#!/bin/sh".to_string(),
            "IFS= read -r initialize".to_string(),
            "printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":1,\"agentCapabilities\":{\"loadSession\":true},\"authMethods\":[{\"id\":\"cached_token\"}]}}'".to_string(),
            "IFS= read -r authenticate".to_string(),
            "printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{}}'".to_string(),
            "IFS= read -r session".to_string(),
            "printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"sessionId\":\"fake-session\"}}'".to_string(),
            "IFS= read -r prompt".to_string(),
            "printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"sessionId\":\"fake-session\",\"update\":{\"sessionUpdate\":\"agent_thought_chunk\",\"content\":{\"type\":\"text\",\"text\":\"ready\"}}}}'".to_string(),
            "IFS= read -r cancel".to_string(),
            format!("printf '%s\\n' \"$cancel\" > '{}'", cancel_log.display()),
        ];
        if graceful {
            lines.extend([
                "printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"session/request_permission\",\"params\":{\"toolCall\":{\"kind\":\"execute\"},\"options\":[{\"optionId\":\"yes\",\"kind\":\"allow_once\"},{\"optionId\":\"no\",\"kind\":\"reject_once\"}]}}'".to_string(),
                "IFS= read -r permission".to_string(),
                format!(
                    "printf '%s\\n' \"$permission\" > '{}'",
                    permission_log.display()
                ),
                "printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":4,\"result\":{\"stopReason\":\"cancelled\"}}'".to_string(),
            ]);
            if linger_after_response {
                lines.push("sleep 30".to_string());
            } else {
                lines.extend([
                    "if IFS= read -r trailing; then exit 7; fi".to_string(),
                    format!("printf closed > '{}'", eof_log.display()),
                ]);
            }
        } else {
            lines.push("sleep 30".to_string());
        }
        std::fs::write(&server, lines.join("\n")).unwrap();
        let mut permissions = std::fs::metadata(&server).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&server, permissions).unwrap();
        (server, cancel_log, permission_log, eof_log)
    }

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
                "--no-auto-update",
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
    fn auto_mode_does_not_bypass_an_acp_permission_request() {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "session/request_permission",
            "params": {
                "toolCall": {"kind": "execute"},
                "options": [
                    {"optionId": "yes", "kind": "allow_once"},
                    {"optionId": "no", "kind": "reject_once"}
                ]
            }
        });
        let (events, _received) = mpsc::sync_channel(65_536);
        let mut output = BufWriter::new(Vec::new());

        respond_to_permission(
            &mut output,
            &request,
            Some(PermissionMode::Auto),
            false,
            &events,
        );
        let response: Value = serde_json::from_slice(output.get_ref()).unwrap();

        assert_eq!(response["result"]["outcome"]["outcome"], "selected");
        assert_eq!(response["result"]["outcome"]["optionId"], "no");
    }

    #[test]
    fn cancellation_sends_notification_and_cancels_permission() {
        let (output_tx, output) = mpsc::sync_channel(65_536);
        output_tx
            .send(Ok(json!({
                "jsonrpc": "2.0",
                "id": 9,
                "method": "session/request_permission",
                "params": {
                    "toolCall": {"kind": "execute"},
                    "options": [{"optionId": "yes", "kind": "allow_once"}]
                }
            })
            .to_string()))
            .unwrap();
        output_tx
            .send(Ok(
                json!({"jsonrpc":"2.0","id":4,"result":{"stopReason":"cancelled"}}).to_string(),
            ))
            .unwrap();
        let cancel = AtomicBool::new(true);
        let mut input = BufWriter::new(Vec::new());
        let mut parser = GrokAcpParser::default();
        let (events, _) = mpsc::sync_channel(65_536);

        assert!(process_prompt(
            &output,
            &mut input,
            &mut parser,
            PromptContext {
                permission_mode: Some(PermissionMode::BypassPermissions),
                cancel: &cancel,
                session_id: "session-1",
                request_id: 4,
                events: &events,
            },
        ));
        let messages = String::from_utf8(input.into_inner().unwrap()).unwrap();
        let messages = messages
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();

        assert_eq!(messages[0]["method"], "session/cancel");
        assert_eq!(messages[0]["params"]["sessionId"], "session-1");
        assert_eq!(messages[1]["id"], 9);
        assert_eq!(messages[1]["result"]["outcome"]["outcome"], "cancelled");
    }

    #[cfg(unix)]
    #[test]
    fn fake_acp_server_completes_graceful_cancellation() {
        let dir =
            std::env::temp_dir().join(format!("asterline-grok-acp-cancel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (server, cancel_log, permission_log, eof_log) = fake_acp_server(&dir, true, false);
        let member = TeamMember::new("grok", "Grok", BackendKind::Grok, "test");
        let runner =
            GrokAcpRunner::from_member(&member, &dir).with_binary(server.display().to_string());
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_for_run = Arc::clone(&cancel);
        let (events, received) = mpsc::sync_channel(65_536);
        let handle = thread::spawn(move || {
            runner.run(
                RunRequest {
                    prompt: "wait".to_string(),
                    session: None,
                    cancel: cancel_for_run,
                    effort: None,
                },
                events,
            );
        });

        let mut deadline = Instant::now() + TEST_STARTUP_TIMEOUT;
        let mut cancel_started = None;
        loop {
            match received
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap()
            {
                AgentEvent::Reasoning(text) if text == "ready" => {
                    let started = Instant::now();
                    cancel_started = Some(started);
                    deadline = started + TEST_CANCEL_TIMEOUT;
                    cancel.store(true, Ordering::Relaxed);
                }
                AgentEvent::Fatal(message) => panic!("graceful cancellation failed: {message}"),
                AgentEvent::Exited { ok, .. } => {
                    assert!(ok, "graceful cancellation must exit successfully");
                    break;
                }
                _ => {}
            }
        }
        handle.join().unwrap();
        assert!(cancel_started.unwrap().elapsed() < TEST_CANCEL_TIMEOUT);

        let notification: Value =
            serde_json::from_str(&std::fs::read_to_string(cancel_log).unwrap()).unwrap();
        let permission: Value =
            serde_json::from_str(&std::fs::read_to_string(permission_log).unwrap()).unwrap();
        assert_eq!(notification["method"], "session/cancel");
        assert_eq!(notification["params"]["sessionId"], "fake-session");
        assert_eq!(permission["result"]["outcome"]["outcome"], "cancelled");
        assert!(
            eof_log.exists(),
            "the ACP server did not observe stdin closing before exit"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn fake_acp_server_is_killed_after_cancel_timeout() {
        let dir =
            std::env::temp_dir().join(format!("asterline-grok-acp-timeout-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (server, cancel_log, _, _) = fake_acp_server(&dir, false, false);
        let member = TeamMember::new("grok", "Grok", BackendKind::Grok, "test");
        let runner =
            GrokAcpRunner::from_member(&member, &dir).with_binary(server.display().to_string());
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_for_run = Arc::clone(&cancel);
        let (events, received) = mpsc::sync_channel(65_536);
        let handle = thread::spawn(move || {
            runner.run(
                RunRequest {
                    prompt: "wait".to_string(),
                    session: None,
                    cancel: cancel_for_run,
                    effort: None,
                },
                events,
            );
        });

        let mut deadline = Instant::now() + TEST_STARTUP_TIMEOUT;
        let mut cancel_started = None;
        loop {
            match received
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap()
            {
                AgentEvent::Reasoning(text) if text == "ready" => {
                    let started = Instant::now();
                    cancel_started = Some(started);
                    deadline = started + TEST_CANCEL_TIMEOUT;
                    cancel.store(true, Ordering::Relaxed);
                }
                AgentEvent::Exited { .. } => break,
                _ => {}
            }
        }
        handle.join().unwrap();

        assert!(cancel_started.unwrap().elapsed() < TEST_CANCEL_TIMEOUT);
        let notification: Value =
            serde_json::from_str(&std::fs::read_to_string(cancel_log).unwrap()).unwrap();
        assert_eq!(notification["method"], "session/cancel");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn cancel_timeout_still_kills_after_cancelled_response_if_server_ignores_eof() {
        let dir = std::env::temp_dir().join(format!(
            "asterline-grok-acp-eof-timeout-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (server, _, _, _) = fake_acp_server(&dir, true, true);
        let member = TeamMember::new("grok", "Grok", BackendKind::Grok, "test");
        let runner =
            GrokAcpRunner::from_member(&member, &dir).with_binary(server.display().to_string());
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_for_run = Arc::clone(&cancel);
        let (events, received) = mpsc::sync_channel(65_536);
        let handle = thread::spawn(move || {
            runner.run(
                RunRequest {
                    prompt: "wait".to_string(),
                    session: None,
                    cancel: cancel_for_run,
                    effort: None,
                },
                events,
            );
        });

        let mut deadline = Instant::now() + TEST_STARTUP_TIMEOUT;
        let mut cancel_started = None;
        loop {
            match received
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap()
            {
                AgentEvent::Reasoning(text) if text == "ready" => {
                    let started = Instant::now();
                    cancel_started = Some(started);
                    deadline = started + TEST_CANCEL_TIMEOUT;
                    cancel.store(true, Ordering::Relaxed);
                }
                AgentEvent::Exited { .. } => break,
                _ => {}
            }
        }
        handle.join().unwrap();

        assert!(cancel_started.unwrap().elapsed() < TEST_CANCEL_TIMEOUT);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn oversized_acp_frame_is_fatal_and_kills_the_server() {
        let dir = std::env::temp_dir().join(format!(
            "asterline-grok-acp-oversized-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let server = dir.join("fake-grok-acp");
        std::fs::write(
            &server,
            format!(
                "#!/bin/sh\nhead -c {} /dev/zero | tr '\\000' x\nsleep 30\n",
                MAX_PROTOCOL_LINE_BYTES + 1
            ),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&server).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&server, permissions).unwrap();
        let member = TeamMember::new("grok", "Grok", BackendKind::Grok, "test");
        let runner =
            GrokAcpRunner::from_member(&member, &dir).with_binary(server.display().to_string());
        let (events, received) = mpsc::sync_channel(65_536);
        let handle = thread::spawn(move || {
            runner.run(
                RunRequest {
                    prompt: "wait".to_string(),
                    session: None,
                    cancel: Arc::new(AtomicBool::new(false)),
                    effort: None,
                },
                events,
            );
        });
        let frame_deadline = Instant::now() + Duration::from_secs(15);
        let mut exit_deadline = None;
        let mut observed = Vec::new();
        loop {
            let deadline = exit_deadline.unwrap_or(frame_deadline);
            let event = received
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("oversized ACP server did not terminate");
            if matches!(
                &event,
                AgentEvent::Fatal(message) if message.contains("line exceeded")
            ) {
                exit_deadline = Some(Instant::now() + Duration::from_secs(5));
            }
            let exited = matches!(event, AgentEvent::Exited { .. });
            observed.push(event);
            if exited {
                break;
            }
        }
        handle.join().unwrap();

        assert!(observed.iter().any(|event| matches!(
            event,
            AgentEvent::Fatal(message) if message.contains("line exceeded")
        )));
        assert!(
            observed
                .iter()
                .any(|event| matches!(event, AgentEvent::Exited { ok: false, .. }))
        );
        let _ = std::fs::remove_dir_all(dir);
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
        events.extend(parser.finish(false));

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
    fn finish_fails_all_active_tools_for_cancel_and_unexpected_end() {
        let mut cancelled = GrokAcpParser::default();
        cancelled.parse_update(&json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "cancelled-tool",
            "title": "Long command",
            "status": "in_progress"
        }));
        assert!(cancelled.finish(true).iter().any(|event| matches!(
            event,
            AgentEvent::ToolCompleted { id, ok: false, summary }
                if id == "cancelled-tool" && summary == "cancelled"
        )));

        let mut truncated = GrokAcpParser::default();
        truncated.parse_update(&json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "truncated-tool",
            "title": "Interrupted command",
            "status": "in_progress"
        }));
        assert!(truncated.finish(false).iter().any(|event| matches!(
            event,
            AgentEvent::ToolCompleted { id, ok: false, summary }
                if id == "truncated-tool" && summary.contains("ended before tool completion")
        )));
        assert!(truncated.finish(false).is_empty());
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

    #[test]
    fn initialize_capability_controls_session_loading() {
        assert!(supports_load_session(&json!({
            "result": {
                "protocolVersion": 1,
                "agentCapabilities": {"loadSession": true}
            }
        })));
        assert!(!supports_load_session(&json!({
            "result": {"protocolVersion": 1, "agentCapabilities": {}}
        })));
    }

    #[test]
    fn authentication_prefers_api_key_then_cached_token() {
        let response = json!({
            "result": {
                "authMethods": [
                    {"id": "cached_token"},
                    {"id": "xai.api_key"}
                ]
            }
        });

        assert_eq!(select_auth_method(&response, true), Some("xai.api_key"));
        assert_eq!(select_auth_method(&response, false), Some("cached_token"));
        assert_eq!(
            select_auth_method(
                &json!({"result": {"authMethods": [{"id": "interactive"}]}}),
                false
            ),
            None
        );
    }

    #[test]
    fn prompt_stop_reason_must_represent_completion() {
        let (successful_tx, successful_output) = mpsc::sync_channel(65_536);
        successful_tx
            .send(Ok(
                json!({"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}).to_string(),
            ))
            .unwrap();
        let mut input = BufWriter::new(Vec::new());
        let mut parser = GrokAcpParser::default();
        let (events, _) = mpsc::sync_channel(65_536);
        assert!(process_prompt(
            &successful_output,
            &mut input,
            &mut parser,
            PromptContext {
                permission_mode: None,
                cancel: &AtomicBool::new(false),
                session_id: "session-1",
                request_id: 3,
                events: &events,
            },
        ));

        let (incomplete_tx, incomplete_output) = mpsc::sync_channel(65_536);
        incomplete_tx
            .send(Ok(
                json!({"jsonrpc":"2.0","id":3,"result":{"stopReason":"max_tokens"}}).to_string(),
            ))
            .unwrap();
        let (events, received) = mpsc::sync_channel(65_536);
        assert!(!process_prompt(
            &incomplete_output,
            &mut input,
            &mut parser,
            PromptContext {
                permission_mode: None,
                cancel: &AtomicBool::new(false),
                session_id: "session-1",
                request_id: 3,
                events: &events,
            },
        ));
        assert!(matches!(received.recv().unwrap(), AgentEvent::Raw(_)));
        assert!(matches!(
            received.recv().unwrap(),
            AgentEvent::Fatal(message) if message.contains("max_tokens")
        ));
    }
}
