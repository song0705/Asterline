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
use crate::domain::event::{AgentEvent, AgentSessionId, FileChangeItem};
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
        match self.permission_mode {
            Some(PermissionMode::BypassPermissions) => {
                // Confirmed on grok 1.0.4: `--permission-mode always-approve`
                // is rejected. The accepted switch is the global flag.
                args.push("--always-approve".to_string());
            }
            Some(mode) => {
                if let Some(arg) = mode.grok_permission_mode_arg() {
                    args.push("--permission-mode".to_string());
                    args.push(arg.to_string());
                }
            }
            None => {}
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
                "prompt": crate::adapter::prompt_images::grok_prompt_blocks(&req.prompt)
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
                // A later agent_message_chunk is the post-tool reply. If we
                // leave this message open, that text is appended to the cell
                // created before the tools, and the final essay renders above
                // the search/read that actually ran first.
                self.close_message(&mut out);
                let id = str_field(update, "toolCallId")
                    .unwrap_or("grok-tool")
                    .to_string();
                let title = str_field(update, "title").unwrap_or("tool").to_string();
                let name = str_field(update, "kind").unwrap_or(&title).to_string();
                self.active_tools.insert(id.clone(), title.clone());
                let summary = update
                    .get("rawInput")
                    .map(crate::adapter::parser::tool_brief)
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| title.clone());
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
    // rawInput is the invocation, already shown as the tool title. Using it
    // as progress/result dumps structured arguments as if they were output.
    ["rawOutput", "content"].into_iter().find_map(|field| {
        update
            .get(field)
            .map(|value| tool_value(value, TOOL_OUTPUT_MAX))
            .filter(|value| !value.is_empty() && value != "null")
    })
}

fn tool_diff_files(update: &Value) -> Vec<FileChangeItem> {
    update
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            if str_field(item, "type") != Some("diff") {
                return None;
            }
            let path = str_field(item, "path")?;
            let kind = if item.get("oldText").is_none() {
                "add"
            } else if item.get("newText").is_none() {
                "delete"
            } else {
                "update"
            };
            Some(FileChangeItem::new(path, kind).with_texts(
                item.get("oldText").and_then(Value::as_str),
                item.get("newText").and_then(Value::as_str),
            ))
        })
        .collect()
}

#[cfg(test)]
#[path = "grok_stream_tests.rs"]
mod tests;
