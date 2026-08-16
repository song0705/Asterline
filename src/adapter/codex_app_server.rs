//! Persistent Codex App Server runner.
//!
//! This runner owns a long-lived `codex app-server` child for each member. It
//! creates or resumes the member's native Codex thread through
//! JSON-RPC, streams structured notifications into [`AgentEvent`]s, and keeps
//! the child ready for the next turn. This is the product-default Codex path;
//! the exec adapter remains available as an explicit legacy transport.

use std::collections::{HashMap, HashSet};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::adapter::models::DiscoveredModel;
use crate::adapter::parser::{
    MAX_MESSAGE_TEXT_BYTES, append_bounded_text, bounded_text, summarize, tool_detail, tool_value,
};
use crate::adapter::process::{
    ChildProcessTree, MAX_STDERR_LINE_BYTES, bounded_lines, configure_process_tree,
};
use crate::adapter::{MemberRunner, RunRequest};
use crate::domain::config::resolve_binary_on_path;
use crate::domain::event::{AgentEvent, AgentSessionId, ApprovalDecision, FileChangeItem};
use crate::domain::team::{BackendKind, Effort, PermissionMode, SandboxPolicy, TeamMember};

const RPC_QUEUE_CAPACITY: usize = 256;
const RPC_RESPONSE_TIMEOUT: Duration = Duration::from_secs(15);
const RPC_POLL_INTERVAL: Duration = Duration::from_millis(50);
const CANCEL_GRACE: Duration = Duration::from_secs(5);
const TOOL_SUMMARY_MAX: usize = 160;
const TOOL_OUTPUT_MAX: usize = 32_000;
/// Codex App Server can put a complete command result into one JSON-RPC
/// notification. Keep a finite transport bound, but allow such a response to
/// reach the semantic output limits below instead of failing at 8 MiB.
const MAX_APP_SERVER_PROTOCOL_LINE_BYTES: usize = 64 * 1024 * 1024;
/// Raw protocol records are diagnostic-only. Large frames already have their
/// meaningful tool/message data recorded through bounded semantic events.
const MAX_PERSISTED_RAW_PROTOCOL_BYTES: usize = 512 * 1024;
const MODEL_LIST_PAGE_SIZE: u64 = 100;
const MAX_MODEL_LIST_PAGES: usize = 8;
const NATIVE_APPROVAL_DETAIL_MAX: usize = 4 * 1024;

/// A user decision routed from the Asterline runtime back to one live Codex
/// App Server client. The request id is local to that client, never a Codex
/// JSON-RPC id exposed to the UI.
#[derive(Clone, Copy, Debug)]
struct NativeApprovalDecision {
    request_id: u64,
    decision: ApprovalDecision,
}

/// Configuration captured when a member runner is built. Team configuration
/// changes rebuild the runner, while the persisted native thread id remains
/// the source of truth for context continuity.
#[derive(Clone, Debug)]
struct CodexAppServerConfig {
    binary: String,
    cwd: PathBuf,
    session_name: String,
    sandbox: SandboxPolicy,
    permission_mode: Option<PermissionMode>,
    model: Option<String>,
    system_prompt: Option<String>,
}

impl CodexAppServerConfig {
    fn from_member(member: &TeamMember, workspace: &Path) -> Self {
        Self {
            binary: "codex".to_string(),
            cwd: absolute_path(&member.resolved_cwd(workspace)),
            session_name: native_session_name(&member.display_name),
            sandbox: member.sandbox,
            permission_mode: member.permission_mode,
            model: member.model.clone(),
            system_prompt: member.system_prompt.clone(),
        }
    }

    fn native_session_name(&self) -> &str {
        &self.session_name
    }

    fn thread_params(&self) -> Value {
        let mut params = json!({
            "cwd": self.cwd,
            "model": self.model,
            "developerInstructions": self.system_prompt.as_deref().filter(|text| !text.trim().is_empty()),
            "sandbox": self.sandbox.codex_arg(),
        });
        params["approvalPolicy"] = Value::String(self.codex_approval_policy().to_string());
        params
    }

    fn codex_approval_policy(&self) -> &'static str {
        codex_approval_policy(self.permission_mode)
    }

    fn resume_params(&self, thread_id: &str) -> Value {
        let mut params = self.thread_params();
        params["threadId"] = Value::String(thread_id.to_string());
        params
    }

    fn turn_params(&self, thread_id: &str, prompt: &str, effort: Option<Effort>) -> Value {
        json!({
            "threadId": thread_id,
            "input": crate::adapter::prompt_images::codex_user_input(prompt),
            "cwd": self.cwd,
            "model": self.model,
            "effort": effort.map(Effort::codex_value),
            // Ask the server for protocol-supported reasoning summaries. The
            // UI still bounds and persists every received summary chunk.
            "summary": "auto",
        })
    }
}

/// Product Codex runner backed by a persistent App Server child.
pub struct CodexAppServerRunner {
    config: CodexAppServerConfig,
    client: Mutex<Option<AppServerClient>>,
    native_approval_tx: Mutex<Option<mpsc::Sender<NativeApprovalDecision>>>,
}

impl CodexAppServerRunner {
    pub fn from_member(member: &TeamMember, workspace: &Path) -> Self {
        Self {
            config: CodexAppServerConfig::from_member(member, workspace),
            client: Mutex::new(None),
            native_approval_tx: Mutex::new(None),
        }
    }

    #[cfg(test)]
    fn with_binary(mut self, binary: impl Into<String>) -> Self {
        self.config.binary = binary.into();
        self
    }
}

/// Discover the current account's Codex models through the same App Server
/// protocol as the product runner. The caller already performs this work in a
/// background catalog worker, so this short-lived connection cannot stall the
/// TUI event loop.
pub(crate) fn discover_models(cwd: &Path) -> Result<Vec<DiscoveredModel>, String> {
    let config = CodexAppServerConfig {
        binary: "codex".to_string(),
        cwd: absolute_path(cwd),
        session_name: "Asterline · model catalog".to_string(),
        sandbox: SandboxPolicy::ReadOnly,
        permission_mode: None,
        model: None,
        system_prompt: None,
    };
    // The catalog request normally has no notifications. Keep a bounded sink
    // nevertheless so an unexpectedly chatty server cannot allocate without
    // limit while discovery is in progress.
    let (events, _event_rx) = mpsc::sync_channel(RPC_QUEUE_CAPACITY);
    let (mut client, _native_approval_tx) = AppServerClient::start(&config, &events)?;
    let mut mapper = AppServerEventMapper::default();
    let mut cursor = None;
    let mut models = Vec::new();
    let mut seen = HashSet::new();

    for _ in 0..MAX_MODEL_LIST_PAGES {
        let response = client.request(
            "model/list",
            json!({ "cursor": cursor, "limit": MODEL_LIST_PAGE_SIZE }),
            &events,
            &mut mapper,
        )?;
        for model in parse_model_page(&response)? {
            if seen.insert(model.id.clone()) {
                models.push(model);
            }
        }
        cursor = response
            .get("nextCursor")
            .and_then(Value::as_str)
            .map(str::to_string);
        if cursor.is_none() {
            break;
        }
    }

    if models.is_empty() {
        Err("Codex App Server model/list returned no visible models".to_string())
    } else {
        Ok(models)
    }
}

fn parse_model_page(response: &Value) -> Result<Vec<DiscoveredModel>, String> {
    let data = response
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| "Codex App Server model/list response did not contain data".to_string())?;
    Ok(data
        .iter()
        .filter(|model| {
            !model
                .get("hidden")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .filter_map(|model| {
            let id = model
                .get("model")
                .or_else(|| model.get("id"))
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())?
                .to_string();
            let supported_efforts = model
                .get("supportedReasoningEfforts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|option| option.get("reasoningEffort").and_then(Value::as_str))
                .filter_map(Effort::parse)
                .collect();
            Some(DiscoveredModel {
                name: model
                    .get("displayName")
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty())
                    .unwrap_or(&id)
                    .to_string(),
                id,
                description: model
                    .get("description")
                    .and_then(Value::as_str)
                    .filter(|description| !description.is_empty())
                    .map(str::to_string),
                default_effort: model
                    .get("defaultReasoningEffort")
                    .and_then(Value::as_str)
                    .and_then(Effort::parse),
                supported_efforts,
                is_default: model
                    .get("isDefault")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            })
        })
        .collect())
}

impl MemberRunner for CodexAppServerRunner {
    fn backend(&self) -> BackendKind {
        BackendKind::Codex
    }

    fn run(&self, req: RunRequest, events: SyncSender<AgentEvent>) {
        let mut guard = match self.client.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if guard.as_mut().is_some_and(AppServerClient::has_exited) {
            *guard = None;
            *self
                .native_approval_tx
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        }
        if guard.is_none() {
            match AppServerClient::start(&self.config, &events) {
                Ok((client, native_approval_tx)) => {
                    *self
                        .native_approval_tx
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                        Some(native_approval_tx);
                    *guard = Some(client);
                }
                Err(error) => {
                    let _ = events.send(AgentEvent::Fatal(error));
                    let _ = events.send(AgentEvent::Exited {
                        code: None,
                        ok: false,
                    });
                    return;
                }
            }
        }

        let result = guard
            .as_mut()
            .expect("App Server client is initialized above")
            .run_turn(&self.config, req, &events);
        match result {
            Ok(ok) => {
                let _ = events.send(AgentEvent::Exited { code: None, ok });
            }
            Err(error) => {
                // Reset only on a transport/protocol failure. A normal failed
                // Codex turn returns Ok(false) and leaves the server ready for
                // a later turn in the same native thread.
                let _ = events.send(AgentEvent::Fatal(error));
                let _ = events.send(AgentEvent::Exited {
                    code: None,
                    ok: false,
                });
                *guard = None;
                *self
                    .native_approval_tx
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
            }
        }
    }

    fn resolve_native_approval(&self, request_id: u64, decision: ApprovalDecision) -> bool {
        self.native_approval_tx
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .is_some_and(|tx| {
                tx.send(NativeApprovalDecision {
                    request_id,
                    decision,
                })
                .is_ok()
            })
    }
}

/// One JSON-RPC stdio connection to a local Codex App Server.
struct AppServerClient {
    child: Child,
    process_tree: ChildProcessTree,
    stdin: ChildStdin,
    stdout_rx: Receiver<Result<String, String>>,
    stderr_rx: Receiver<Result<String, String>>,
    stdout_worker: Option<JoinHandle<()>>,
    stderr_worker: Option<JoinHandle<()>>,
    next_id: u64,
    // A live App Server already owns this thread. Re-resume only after this
    // client is rebuilt (for example, after the child exits).
    active_thread_id: Option<String>,
    native_approval_rx: Receiver<NativeApprovalDecision>,
    next_native_approval_id: u64,
}

impl AppServerClient {
    fn start(
        config: &CodexAppServerConfig,
        events: &SyncSender<AgentEvent>,
    ) -> Result<(Self, mpsc::Sender<NativeApprovalDecision>), String> {
        let program =
            resolve_binary_on_path(&config.binary).unwrap_or_else(|| PathBuf::from(&config.binary));
        let mut command = Command::new(program);
        command
            .args(["app-server", "--listen", "stdio://"])
            .current_dir(&config.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        configure_process_tree(&mut command);
        let mut child = command
            .spawn()
            .map_err(|error| format!("failed to start codex app-server: {error}"))?;
        let process_tree = ChildProcessTree::attach(&mut child).map_err(|error| {
            let _ = child.kill();
            let _ = child.wait();
            format!("failed to isolate codex app-server process tree: {error}")
        })?;
        let stdin = child.stdin.take().ok_or_else(|| {
            let _ = process_tree.terminate_with_fallback(&mut child);
            let _ = child.wait();
            "codex app-server did not expose stdin".to_string()
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            let _ = process_tree.terminate_with_fallback(&mut child);
            let _ = child.wait();
            "codex app-server did not expose stdout".to_string()
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            let _ = process_tree.terminate_with_fallback(&mut child);
            let _ = child.wait();
            "codex app-server did not expose stderr".to_string()
        })?;
        let (stdout_tx, stdout_rx) = mpsc::sync_channel(RPC_QUEUE_CAPACITY);
        let (stderr_tx, stderr_rx) = mpsc::sync_channel(RPC_QUEUE_CAPACITY);
        let (native_approval_tx, native_approval_rx) = mpsc::channel();
        let mut client = Self {
            child,
            process_tree,
            stdin,
            stdout_rx,
            stderr_rx,
            stdout_worker: Some(spawn_line_pump(
                stdout,
                MAX_APP_SERVER_PROTOCOL_LINE_BYTES,
                stdout_tx,
            )),
            stderr_worker: Some(spawn_line_pump(stderr, MAX_STDERR_LINE_BYTES, stderr_tx)),
            next_id: 1,
            active_thread_id: None,
            native_approval_rx,
            next_native_approval_id: 1,
        };

        let mut mapper = AppServerEventMapper::default();
        let _ = client.request(
            "initialize",
            json!({
                "clientInfo": {
                    "name": "asterline",
                    "title": "Asterline",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "capabilities": { "experimentalApi": false },
            }),
            events,
            &mut mapper,
        )?;
        client.notify("initialized", Some(json!({})))?;
        let _ = events.send(AgentEvent::Log(
            "connected to persistent Codex App Server".to_string(),
        ));
        Ok((client, native_approval_tx))
    }

    fn has_exited(&mut self) -> bool {
        self.child.try_wait().ok().flatten().is_some()
    }

    fn run_turn(
        &mut self,
        config: &CodexAppServerConfig,
        req: RunRequest,
        events: &SyncSender<AgentEvent>,
    ) -> Result<bool, String> {
        let mut mapper = AppServerEventMapper::default();
        let thread_id = self.resolve_thread(config, req.session.as_ref(), events, &mut mapper)?;
        mapper.thread_id = Some(thread_id.clone());
        let turn = self.request(
            "turn/start",
            config.turn_params(&thread_id, &req.prompt, req.effort),
            events,
            &mut mapper,
        )?;
        let turn_id = turn
            .pointer("/turn/id")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                "codex app-server turn/start response did not contain turn.id".to_string()
            })?
            .to_string();
        mapper.turn_id = Some(turn_id.clone());
        self.wait_for_turn(&thread_id, &turn_id, &req.cancel, events, &mut mapper)
    }

    fn resolve_thread(
        &mut self,
        config: &CodexAppServerConfig,
        requested_session: Option<&AgentSessionId>,
        events: &SyncSender<AgentEvent>,
        mapper: &mut AppServerEventMapper,
    ) -> Result<String, String> {
        if let Some(session) = requested_session
            && self.active_thread_id.as_deref() == Some(session.as_str())
        {
            return Ok(session.as_str().to_string());
        }

        let response = if let Some(session) = requested_session {
            match self.request(
                "thread/resume",
                config.resume_params(session.as_str()),
                events,
                mapper,
            ) {
                Ok(response) => response,
                Err(error) if resume_target_is_missing(&error) => {
                    let _ = events.send(AgentEvent::Log(format!(
                        "Codex thread {session} is unavailable; starting a new native thread"
                    )));
                    self.request("thread/start", config.thread_params(), events, mapper)?
                }
                Err(error) => return Err(error),
            }
        } else {
            self.request("thread/start", config.thread_params(), events, mapper)?
        };
        let thread_id = response
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                "codex app-server thread response did not contain thread.id".to_string()
            })?
            .to_string();
        let _ = events.send(AgentEvent::SessionDiscovered(AgentSessionId(
            thread_id.clone(),
        )));
        self.set_native_thread_name(&thread_id, config, events, mapper);
        self.active_thread_id = Some(thread_id.clone());
        Ok(thread_id)
    }

    /// Keep App Server-created threads recognizable in Codex's own resume
    /// list. Naming is cosmetic; an older CLI that does not expose this
    /// optional method must not prevent the actual chat from starting.
    fn set_native_thread_name(
        &mut self,
        thread_id: &str,
        config: &CodexAppServerConfig,
        events: &SyncSender<AgentEvent>,
        mapper: &mut AppServerEventMapper,
    ) {
        if let Err(error) = self.request(
            "thread/name/set",
            json!({ "threadId": thread_id, "name": config.native_session_name() }),
            events,
            mapper,
        ) {
            let _ = events.send(AgentEvent::ParseWarning(format!(
                "could not label native Codex thread {thread_id}: {error}"
            )));
        }
    }

    fn wait_for_turn(
        &mut self,
        thread_id: &str,
        turn_id: &str,
        cancel: &Arc<AtomicBool>,
        events: &SyncSender<AgentEvent>,
        mapper: &mut AppServerEventMapper,
    ) -> Result<bool, String> {
        let mut interrupt_id = None;
        let mut cancel_deadline = None;
        loop {
            if cancel.load(Ordering::Relaxed) && interrupt_id.is_none() {
                let request_id = self.send_request(
                    "turn/interrupt",
                    json!({ "threadId": thread_id, "turnId": turn_id }),
                )?;
                interrupt_id = Some(request_id);
                cancel_deadline = Some(Instant::now() + CANCEL_GRACE);
            }
            if cancel_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err(
                    "timed out waiting for Codex App Server to interrupt the turn".to_string(),
                );
            }

            let Some(message) = self.next_message(RPC_POLL_INTERVAL, events)? else {
                continue;
            };
            if message.get("method").is_none()
                && message.get("id").is_some()
                && interrupt_id
                    .as_ref()
                    .is_some_and(|id| message.get("id") == Some(id))
            {
                if let Some(error) = rpc_error_message(&message) {
                    let _ = events.send(AgentEvent::ParseWarning(format!(
                        "codex App Server turn/interrupt failed: {error}"
                    )));
                }
                continue;
            }
            self.handle_message(&message, events, mapper)?;
            if let Some(ok) = mapper.terminal.take() {
                return Ok(ok && !cancel.load(Ordering::Relaxed));
            }
        }
    }

    fn request(
        &mut self,
        method: &str,
        params: Value,
        events: &SyncSender<AgentEvent>,
        mapper: &mut AppServerEventMapper,
    ) -> Result<Value, String> {
        let request_id = self.send_request(method, params)?;
        let deadline = Instant::now() + RPC_RESPONSE_TIMEOUT;
        loop {
            if Instant::now() >= deadline {
                return Err(format!("timed out waiting for codex app-server {method}"));
            }
            let Some(message) = self.next_message(RPC_POLL_INTERVAL, events)? else {
                continue;
            };
            if message.get("method").is_none()
                && message.get("id").is_some()
                && message.get("id") == Some(&request_id)
            {
                return message.get("result").cloned().ok_or_else(|| {
                    format!(
                        "codex app-server {method} failed: {}",
                        rpc_error_message(&message).unwrap_or_else(|| {
                            "response contained neither result nor error".to_string()
                        })
                    )
                });
            }
            self.handle_message(&message, events, mapper)?;
        }
    }

    fn send_request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = Value::from(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        self.write_message(json!({
            "id": id,
            "method": method,
            "params": params,
        }))?;
        Ok(id)
    }

    fn notify(&mut self, method: &str, params: Option<Value>) -> Result<(), String> {
        let mut message = json!({ "method": method });
        if let Some(params) = params {
            message["params"] = params;
        }
        self.write_message(message)
    }

    fn write_message(&mut self, message: Value) -> Result<(), String> {
        serde_json::to_writer(&mut self.stdin, &message)
            .map_err(|error| format!("could not encode Codex App Server request: {error}"))?;
        self.stdin
            .write_all(b"\n")
            .and_then(|()| self.stdin.flush())
            .map_err(|error| format!("could not write to Codex App Server: {error}"))
    }

    fn next_message(
        &mut self,
        timeout: Duration,
        events: &SyncSender<AgentEvent>,
    ) -> Result<Option<Value>, String> {
        self.drain_stderr(events);
        match self.stdout_rx.recv_timeout(timeout) {
            Ok(Ok(line)) => {
                match serde_json::from_str::<Value>(&line) {
                    Ok(message) => {
                        // Deltas can arrive much faster than the runtime's
                        // SQLite raw-event path. Their semantic equivalents
                        // are persisted through normal chat/tool state, so
                        // retain protocol raw records for boundaries and
                        // unknown messages rather than every streamed chunk.
                        if should_persist_raw_message(&line, &message) {
                            let _ = events.send(AgentEvent::Raw(line));
                        }
                        Ok(Some(message))
                    }
                    Err(error) => {
                        let _ = events.send(AgentEvent::Raw(bounded_text(
                            &line,
                            MAX_PERSISTED_RAW_PROTOCOL_BYTES,
                        )));
                        Err(format!("invalid JSON from Codex App Server: {error}"))
                    }
                }
            }
            Ok(Err(error)) => Err(format!("could not read Codex App Server stdout: {error}")),
            Err(RecvTimeoutError::Timeout) => {
                if let Some(status) = self
                    .child
                    .try_wait()
                    .map_err(|error| format!("could not poll Codex App Server: {error}"))?
                {
                    return Err(format!("Codex App Server exited unexpectedly: {status}"));
                }
                Ok(None)
            }
            Err(RecvTimeoutError::Disconnected) => {
                Err("Codex App Server stdout closed".to_string())
            }
        }
    }

    fn drain_stderr(&mut self, events: &SyncSender<AgentEvent>) {
        loop {
            match self.stderr_rx.try_recv() {
                Ok(Ok(line)) => {
                    let _ = events.send(AgentEvent::Stderr(line));
                }
                Ok(Err(error)) => {
                    let _ = events.send(AgentEvent::ParseWarning(format!(
                        "could not read Codex App Server stderr: {error}"
                    )));
                    break;
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
    }

    fn handle_message(
        &mut self,
        message: &Value,
        events: &SyncSender<AgentEvent>,
        mapper: &mut AppServerEventMapper,
    ) -> Result<(), String> {
        let method = message.get("method").and_then(Value::as_str);
        let has_id = message.get("id").is_some();
        if has_id && method.is_some() {
            let request_id = message.get("id").cloned().unwrap_or(Value::Null);
            let request_method = method.unwrap_or("unknown");
            let params = message.get("params").unwrap_or(&Value::Null);
            let response =
                if let Some((action, body)) = native_approval_details(request_method, params) {
                    let native_request_id = self.next_native_approval_id;
                    self.next_native_approval_id = self.next_native_approval_id.saturating_add(1);
                    events
                        .send(AgentEvent::NativeApprovalRequested {
                            request_id: native_request_id,
                            action: action.to_string(),
                            body,
                        })
                        .map_err(|_| {
                            "Asterline runtime stopped while waiting for Codex approval".to_string()
                        })?;
                    let decision = self.wait_for_native_approval(native_request_id, events);
                    Some(native_approval_result(request_method, params, decision))
                } else {
                    unsupported_native_request_response(request_method)
                };
            let message = match response {
                Some(result) => json!({ "id": request_id, "result": result }),
                None => json!({
                    "id": request_id,
                    "error": {
                        "code": -32000,
                        "message": "Asterline does not implement this Codex App Server client request"
                    }
                }),
            };
            self.write_message(message)?;
            return Ok(());
        }
        if let Some(method) = method {
            mapper.handle_notification(
                method,
                message.get("params").unwrap_or(&Value::Null),
                events,
            );
        }
        Ok(())
    }

    /// Pause a Codex turn until its matching Asterline approval is resolved.
    /// The decision channel is owned by the runner rather than this mutex-held
    /// client, so the runtime can respond while `run()` is awaiting App Server
    /// output.
    fn wait_for_native_approval(
        &mut self,
        request_id: u64,
        events: &SyncSender<AgentEvent>,
    ) -> ApprovalDecision {
        loop {
            match self.native_approval_rx.recv_timeout(RPC_POLL_INTERVAL) {
                Ok(response) if response.request_id == request_id => return response.decision,
                Ok(_) => continue,
                Err(RecvTimeoutError::Timeout) => {
                    self.drain_stderr(events);
                    if self.has_exited() {
                        return ApprovalDecision::Reject;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => return ApprovalDecision::Reject,
            }
        }
    }
}

fn should_persist_raw_message(line: &str, message: &Value) -> bool {
    line.len() <= MAX_PERSISTED_RAW_PROTOCOL_BYTES
        && !matches!(
            message.get("method").and_then(Value::as_str),
            Some(
                "item/agentMessage/delta"
                    | "item/reasoning/summaryTextDelta"
                    | "item/reasoning/summaryPartAdded"
                    | "item/reasoning/textDelta"
                    | "item/commandExecution/outputDelta"
                    | "item/mcpToolCall/progress"
                    | "item/plan/delta"
                    | "item/fileChange/patchUpdated"
            )
        )
}

/// Details that a user must see before granting a Codex App Server request.
/// Input/elicitation requests use richer multi-choice payloads and stay
/// explicitly unsupported until Asterline has a matching interaction surface.
fn native_approval_details(method: &str, params: &Value) -> Option<(&'static str, String)> {
    match method {
        "item/commandExecution/requestApproval" => {
            let command = params
                .get("command")
                .and_then(Value::as_str)
                .filter(|command| !command.trim().is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| tool_value(params, NATIVE_APPROVAL_DETAIL_MAX));
            let reason = params
                .get("reason")
                .and_then(Value::as_str)
                .filter(|reason| !reason.trim().is_empty())
                .map(|reason| format!("\n\n{reason}"))
                .unwrap_or_default();
            Some((
                "Codex command",
                crate::adapter::parser::tool_detail(
                    &format!("{command}{reason}"),
                    NATIVE_APPROVAL_DETAIL_MAX,
                ),
            ))
        }
        "item/fileChange/requestApproval" => Some((
            "Codex file change",
            tool_value(params, NATIVE_APPROVAL_DETAIL_MAX),
        )),
        "item/permissions/requestApproval" => Some((
            "Codex permission escalation",
            tool_value(params, NATIVE_APPROVAL_DETAIL_MAX),
        )),
        _ => None,
    }
}

/// Convert a user decision into the exact response shape for the App Server
/// request. Asterline intentionally offers a one-time decision only; it never
/// writes Codex's session/persistent policy amendments on the user's behalf.
fn native_approval_result(method: &str, params: &Value, decision: ApprovalDecision) -> Value {
    match method {
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => json!({
            "decision": if decision == ApprovalDecision::Approve { "accept" } else { "decline" }
        }),
        "item/permissions/requestApproval" => json!({
            "permissions": if decision == ApprovalDecision::Approve {
                params.get("permissions").cloned().unwrap_or_else(|| json!({}))
            } else {
                json!({})
            }
        }),
        _ => json!({}),
    }
}

/// Request types that Asterline cannot faithfully collect input for yet remain
/// fail-closed. They are not approval-policy decisions.
fn unsupported_native_request_response(method: &str) -> Option<Value> {
    match method {
        "item/tool/requestUserInput" => Some(json!({ "answers": {} })),
        "mcpServer/elicitation/request" => Some(json!({ "action": "decline" })),
        _ => None,
    }
}

fn codex_approval_policy(mode: Option<PermissionMode>) -> &'static str {
    match mode {
        None | Some(PermissionMode::Default) => "never",
        Some(PermissionMode::AcceptEdits | PermissionMode::Plan) => "untrusted",
        Some(PermissionMode::Auto) => "on-request",
        Some(PermissionMode::DontAsk | PermissionMode::BypassPermissions) => "never",
    }
}

fn native_session_name(display_name: &str) -> String {
    format!("Asterline · {}", display_name.trim())
}

impl Drop for AppServerClient {
    fn drop(&mut self) {
        // A line pump can be blocked in `SyncSender::send` when its bounded
        // queue is full. Drop the real receivers before joining the pumps so
        // those sends return `Disconnected` instead of hanging teardown.
        let (_, replacement_stdout) = mpsc::sync_channel(0);
        let (_, replacement_stderr) = mpsc::sync_channel(0);
        drop(std::mem::replace(&mut self.stdout_rx, replacement_stdout));
        drop(std::mem::replace(&mut self.stderr_rx, replacement_stderr));
        let _ = self.process_tree.terminate_with_fallback(&mut self.child);
        let _ = self.child.wait();
        if let Some(worker) = self.stdout_worker.take() {
            let _ = worker.join();
        }
        if let Some(worker) = self.stderr_worker.take() {
            let _ = worker.join();
        }
    }
}

fn spawn_line_pump<R: Read + Send + 'static>(
    pipe: R,
    max_bytes: usize,
    tx: mpsc::SyncSender<Result<String, String>>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let reader = BufReader::new(pipe);
        for line in bounded_lines(reader, max_bytes) {
            let result = line.map_err(|error| error.to_string());
            let stop = result.is_err();
            if tx.send(result).is_err() || stop {
                break;
            }
        }
    })
}

#[derive(Default)]
struct AppServerEventMapper {
    thread_id: Option<String>,
    turn_id: Option<String>,
    agent_text: HashMap<String, String>,
    started_messages: HashSet<String>,
    streamed_reasoning: HashSet<String>,
    reasoning_summary_indices: HashMap<String, u64>,
    command_output: HashMap<String, String>,
    terminal: Option<bool>,
}

impl AppServerEventMapper {
    fn handle_notification(
        &mut self,
        method: &str,
        params: &Value,
        events: &SyncSender<AgentEvent>,
    ) {
        if !self.belongs_to_active_turn(params) {
            return;
        }
        match method {
            "thread/started" => {
                if let Some(id) = params.pointer("/thread/id").and_then(Value::as_str) {
                    self.thread_id = Some(id.to_string());
                    let _ = events.send(AgentEvent::SessionDiscovered(AgentSessionId(
                        id.to_string(),
                    )));
                }
            }
            "turn/started" => {
                self.turn_id = params
                    .pointer("/turn/id")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            "item/started" => {
                self.handle_item(params.get("item").unwrap_or(&Value::Null), true, events)
            }
            "item/completed" => {
                self.handle_item(params.get("item").unwrap_or(&Value::Null), false, events)
            }
            "item/agentMessage/delta" => self.handle_agent_delta(params, events),
            "item/reasoning/summaryTextDelta" => {
                self.begin_reasoning_section(params, events);
                if let Some(delta) = params.get("delta").and_then(Value::as_str) {
                    let delta = bounded_text(delta, MAX_MESSAGE_TEXT_BYTES);
                    if let Some(id) = params.get("itemId").and_then(Value::as_str)
                        && !id.is_empty()
                    {
                        self.streamed_reasoning.insert(id.to_string());
                    }
                    let _ = events.send(AgentEvent::Reasoning(delta));
                }
            }
            "item/reasoning/summaryPartAdded" => self.begin_reasoning_section(params, events),
            "item/commandExecution/outputDelta" => {
                let id = params
                    .get("itemId")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let delta = params
                    .get("delta")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !id.is_empty() && !delta.is_empty() {
                    let output = self.command_output.entry(id.to_string()).or_default();
                    if let Some(delta) = append_bounded_text(output, delta, TOOL_OUTPUT_MAX) {
                        let _ = events.send(AgentEvent::ToolProgress {
                            id: id.to_string(),
                            delta,
                        });
                    }
                }
            }
            "item/mcpToolCall/progress" => {
                let id = params
                    .get("itemId")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let message = params
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !id.is_empty() && !message.is_empty() {
                    let _ = events.send(AgentEvent::ToolProgress {
                        id: id.to_string(),
                        delta: bounded_text(message, TOOL_OUTPUT_MAX),
                    });
                }
            }
            "turn/completed" => {
                let status = params
                    .pointer("/turn/status")
                    .and_then(Value::as_str)
                    .unwrap_or("failed");
                let ok = status == "completed";
                if !ok {
                    let message = params
                        .pointer("/turn/error/message")
                        .and_then(Value::as_str)
                        .unwrap_or("Codex turn failed");
                    let _ = events.send(AgentEvent::Fatal(message.to_string()));
                }
                // App Server normally emits item/completed before its turn
                // terminal notification. Treat the accumulated deltas as the
                // final answer if a server version ends the turn without that
                // item event, rather than silently dropping the reply.
                self.finish_open_agent_messages(events);
                self.streamed_reasoning.clear();
                self.reasoning_summary_indices.clear();
                self.terminal = Some(ok);
            }
            "error" => {
                let message = params
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("Codex App Server error");
                if is_recoverable_stream_error(message) {
                    let _ = events.send(AgentEvent::ParseWarning(format!(
                        "codex transient stream error: {message}"
                    )));
                } else {
                    let _ = events.send(AgentEvent::Fatal(message.to_string()));
                    self.terminal = Some(false);
                }
            }
            "warning" | "configWarning" | "deprecationNotice" => {
                let message = params
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or(method);
                let _ = events.send(AgentEvent::ParseWarning(format!(
                    "codex {method}: {message}"
                )));
            }
            "thread/closed" => {
                let _ = events.send(AgentEvent::Fatal(
                    "Codex App Server closed the active thread".to_string(),
                ));
                self.terminal = Some(false);
            }
            _ => {}
        }
    }

    fn belongs_to_active_turn(&self, params: &Value) -> bool {
        let thread_matches = self.thread_id.as_ref().is_none_or(|expected| {
            params
                .get("threadId")
                .and_then(Value::as_str)
                .is_none_or(|id| id == expected)
        });
        let turn_matches = self.turn_id.as_ref().is_none_or(|expected| {
            params
                .get("turnId")
                .and_then(Value::as_str)
                .is_none_or(|id| id == expected)
        });
        thread_matches && turn_matches
    }

    fn handle_agent_delta(&mut self, params: &Value, events: &SyncSender<AgentEvent>) {
        let id = params
            .get("itemId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let delta = params
            .get("delta")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if id.is_empty() || delta.is_empty() {
            return;
        }
        self.start_message(id, events);
        let text = self.agent_text.entry(id.to_string()).or_default();
        if let Some(delta) = append_bounded_text(text, delta, MAX_MESSAGE_TEXT_BYTES) {
            let _ = events.send(AgentEvent::TextDelta(delta));
        }
    }

    fn begin_reasoning_section(&mut self, params: &Value, events: &SyncSender<AgentEvent>) {
        let Some(id) = params
            .get("itemId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        else {
            return;
        };
        let Some(index) = params.get("summaryIndex").and_then(Value::as_u64) else {
            return;
        };
        if self
            .reasoning_summary_indices
            .insert(id.to_string(), index)
            .is_some_and(|previous| previous != index)
        {
            let _ = events.send(AgentEvent::ReasoningSectionBreak);
        }
    }

    fn finish_open_agent_messages(&mut self, events: &SyncSender<AgentEvent>) {
        let ids = self.started_messages.drain().collect::<Vec<_>>();
        for id in ids {
            let text = self.agent_text.remove(&id).unwrap_or_default();
            let _ = events.send(AgentEvent::MessageCompleted(text));
        }
    }

    fn handle_item(&mut self, item: &Value, started: bool, events: &SyncSender<AgentEvent>) {
        let item_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
        let id = item.get("id").and_then(Value::as_str).unwrap_or_default();
        match item_type {
            "agentMessage" => {
                if started {
                    self.start_message(id, events);
                    return;
                }
                let text = item.get("text").and_then(Value::as_str).unwrap_or_default();
                self.start_message(id, events);
                let canonical = if text.is_empty() {
                    self.agent_text.remove(id).unwrap_or_default()
                } else {
                    bounded_text(text, MAX_MESSAGE_TEXT_BYTES)
                };
                let _ = events.send(AgentEvent::MessageCompleted(canonical));
                self.agent_text.remove(id);
                self.started_messages.remove(id);
            }
            "reasoning" if !started => {
                if !self.streamed_reasoning.remove(id) {
                    let text = item
                        .get("summary")
                        .map(|value| tool_value(value, MAX_MESSAGE_TEXT_BYTES))
                        .unwrap_or_default();
                    if !text.is_empty() {
                        let _ = events.send(AgentEvent::Reasoning(text));
                    }
                }
                self.reasoning_summary_indices.remove(id);
                let _ = events.send(AgentEvent::ReasoningCompleted);
            }
            "commandExecution" => {
                let summary = summarize(
                    item.get("command")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                    TOOL_SUMMARY_MAX,
                );
                if started {
                    self.command_output.insert(id.to_string(), String::new());
                    let _ = events.send(AgentEvent::ToolStarted {
                        id: id.to_string(),
                        name: "shell".to_string(),
                        summary,
                    });
                } else {
                    let output = item
                        .get("aggregatedOutput")
                        .and_then(Value::as_str)
                        .map(|text| tool_detail(text, TOOL_OUTPUT_MAX))
                        .filter(|text| !text.is_empty())
                        .or_else(|| self.command_output.remove(id))
                        .unwrap_or_default();
                    let status = item
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let exit_ok = item
                        .get("exitCode")
                        .and_then(Value::as_i64)
                        .is_none_or(|code| code == 0);
                    let _ = events.send(AgentEvent::ToolCompleted {
                        id: id.to_string(),
                        ok: status == "completed" && exit_ok,
                        summary: output,
                    });
                }
            }
            "fileChange" if !started => {
                let status = item
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let files = item
                    .get("changes")
                    .and_then(Value::as_array)
                    .map(|changes| {
                        changes
                            .iter()
                            .map(|change| {
                                let path = change
                                    .get("path")
                                    .or_else(|| change.get("movePath"))
                                    .and_then(Value::as_str)
                                    .unwrap_or_default();
                                let kind = change
                                    .get("type")
                                    .and_then(Value::as_str)
                                    .or_else(|| {
                                        change.pointer("/kind/type").and_then(Value::as_str)
                                    })
                                    .or_else(|| change.get("kind").and_then(Value::as_str))
                                    .unwrap_or("update");
                                FileChangeItem::new(path, kind)
                                    .with_texts(
                                        change
                                            .get("oldText")
                                            .or_else(|| change.get("before"))
                                            .and_then(Value::as_str),
                                        change
                                            .get("newText")
                                            .or_else(|| change.get("after"))
                                            .and_then(Value::as_str),
                                    )
                                    .with_patch(
                                        change
                                            .get("diff")
                                            .or_else(|| change.get("patch"))
                                            .and_then(Value::as_str),
                                    )
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let _ = events.send(AgentEvent::FileChange {
                    files,
                    ok: status == "completed",
                });
            }
            "mcpToolCall" => self.handle_mcp_item(item, started, events),
            "dynamicToolCall" => self.handle_dynamic_tool_item(item, started, events),
            "webSearch" => {
                let query = summarize(
                    item.get("query")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                    TOOL_SUMMARY_MAX,
                );
                if started {
                    let _ = events.send(AgentEvent::ToolStarted {
                        id: id.to_string(),
                        name: "web_search".to_string(),
                        summary: query,
                    });
                } else {
                    let detail = item
                        .get("results")
                        .map(|value| tool_value(value, TOOL_OUTPUT_MAX))
                        .unwrap_or(query);
                    let _ = events.send(AgentEvent::ToolCompleted {
                        id: id.to_string(),
                        ok: true,
                        summary: detail,
                    });
                }
            }
            "collabAgentToolCall" => {
                let name = item
                    .get("tool")
                    .and_then(Value::as_str)
                    .unwrap_or("collab")
                    .to_string();
                if started {
                    let summary = item
                        .get("prompt")
                        .and_then(Value::as_str)
                        .map(|text| summarize(text, TOOL_SUMMARY_MAX))
                        .unwrap_or_else(|| name.clone());
                    let _ = events.send(AgentEvent::ToolStarted {
                        id: id.to_string(),
                        name,
                        summary,
                    });
                } else {
                    let status = item
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let detail = item
                        .get("agentsStates")
                        .map(|value| tool_value(value, TOOL_OUTPUT_MAX))
                        .unwrap_or_default();
                    let _ = events.send(AgentEvent::ToolCompleted {
                        id: id.to_string(),
                        ok: status == "completed",
                        summary: detail,
                    });
                }
            }
            "subAgentActivity" => {
                let kind = item
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("activity");
                let summary = item
                    .get("agentPath")
                    .and_then(Value::as_str)
                    .filter(|path| !path.is_empty())
                    .unwrap_or("subagent")
                    .to_string();
                if started {
                    let _ = events.send(AgentEvent::ToolStarted {
                        id: id.to_string(),
                        name: format!("subagent:{kind}"),
                        summary,
                    });
                } else {
                    let _ = events.send(AgentEvent::ToolCompleted {
                        id: id.to_string(),
                        ok: true,
                        summary,
                    });
                }
            }
            "imageGeneration" => {
                let summary = item
                    .get("revisedPrompt")
                    .or_else(|| item.get("result"))
                    .and_then(Value::as_str)
                    .map(|text| summarize(text, TOOL_SUMMARY_MAX))
                    .filter(|text| !text.is_empty())
                    .unwrap_or_else(|| "image generation".to_string());
                if started {
                    let _ = events.send(AgentEvent::ToolStarted {
                        id: id.to_string(),
                        name: "image_generation".to_string(),
                        summary,
                    });
                } else {
                    let status = item
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let _ = events.send(AgentEvent::ToolCompleted {
                        id: id.to_string(),
                        ok: status == "completed",
                        summary,
                    });
                }
            }
            _ => {}
        }
    }

    fn handle_dynamic_tool_item(
        &mut self,
        item: &Value,
        started: bool,
        events: &SyncSender<AgentEvent>,
    ) {
        let id = item
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let tool = item.get("tool").and_then(Value::as_str).unwrap_or("tool");
        let name = match item.get("namespace").and_then(Value::as_str) {
            Some(namespace) if !namespace.is_empty() => format!("{namespace}/{tool}"),
            _ => tool.to_string(),
        };
        if started {
            let summary = item
                .get("arguments")
                .map(|arguments| {
                    summarize(
                        &crate::adapter::parser::tool_brief(arguments),
                        TOOL_SUMMARY_MAX,
                    )
                })
                .filter(|summary| !summary.is_empty())
                .unwrap_or_else(|| name.clone());
            let _ = events.send(AgentEvent::ToolStarted { id, name, summary });
        } else {
            let summary = item
                .get("contentItems")
                .map(|content| tool_value(content, TOOL_OUTPUT_MAX))
                .filter(|summary| !summary.is_empty())
                .unwrap_or(name);
            let ok = item
                .get("success")
                .and_then(Value::as_bool)
                .unwrap_or_else(|| item.get("status").and_then(Value::as_str) == Some("completed"));
            let _ = events.send(AgentEvent::ToolCompleted { id, ok, summary });
        }
    }

    fn handle_mcp_item(&mut self, item: &Value, started: bool, events: &SyncSender<AgentEvent>) {
        let id = item
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let name = format!(
            "{}/{}",
            item.get("server").and_then(Value::as_str).unwrap_or("mcp"),
            item.get("tool").and_then(Value::as_str).unwrap_or("tool")
        );
        if started {
            let summary = item
                .get("arguments")
                .map(|value| {
                    summarize(&crate::adapter::parser::tool_brief(value), TOOL_SUMMARY_MAX)
                })
                .filter(|text| !text.is_empty())
                .unwrap_or_else(|| name.clone());
            let _ = events.send(AgentEvent::ToolStarted { id, name, summary });
        } else {
            let status = item
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let summary = item
                .get("result")
                .or_else(|| item.get("error"))
                .map(|value| tool_value(value, TOOL_OUTPUT_MAX))
                .filter(|text| !text.is_empty())
                .unwrap_or(name);
            let _ = events.send(AgentEvent::ToolCompleted {
                id,
                ok: status == "completed",
                summary,
            });
        }
    }

    fn start_message(&mut self, id: &str, events: &SyncSender<AgentEvent>) {
        if self.started_messages.insert(id.to_string()) {
            let _ = events.send(AgentEvent::MessageStarted);
        }
    }
}

fn absolute_path(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    absolute.canonicalize().unwrap_or(absolute)
}

fn rpc_error_message(message: &Value) -> Option<String> {
    message
        .pointer("/error/message")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| message.get("error").map(|value| value.to_string()))
}

fn resume_target_is_missing(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    [
        "not found",
        "does not exist",
        "unknown thread",
        "no rollout",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn is_recoverable_stream_error(message: &str) -> bool {
    message
        .trim_start()
        .to_ascii_lowercase()
        .starts_with("reconnecting")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    use crate::domain::team::TeamMember;

    fn mapper_events(method: &str, params: Value) -> Vec<AgentEvent> {
        let mut mapper = AppServerEventMapper::default();
        let (tx, rx) = mpsc::sync_channel(32);
        mapper.handle_notification(method, &params, &tx);
        drop(tx);
        rx.try_iter().collect()
    }

    #[test]
    fn thread_started_discovers_the_native_session() {
        let events = mapper_events("thread/started", json!({ "thread": { "id": "thread-1" } }));
        assert_eq!(
            events,
            vec![AgentEvent::SessionDiscovered(AgentSessionId(
                "thread-1".to_string()
            ))]
        );
    }

    #[test]
    fn app_server_streams_agent_text_and_finalizes_the_item() {
        let mut mapper = AppServerEventMapper {
            thread_id: Some("thread-1".to_string()),
            turn_id: Some("turn-1".to_string()),
            ..Default::default()
        };
        let (tx, rx) = mpsc::sync_channel(32);
        mapper.handle_notification(
            "item/started",
            &json!({
                "threadId": "thread-1", "turnId": "turn-1",
                "item": { "id": "message-1", "type": "agentMessage", "phase": "final", "text": "" }
            }),
            &tx,
        );
        mapper.handle_notification(
            "item/agentMessage/delta",
            &json!({ "threadId": "thread-1", "turnId": "turn-1", "itemId": "message-1", "delta": "hello" }),
            &tx,
        );
        mapper.handle_notification(
            "item/completed",
            &json!({
                "threadId": "thread-1", "turnId": "turn-1",
                "item": { "id": "message-1", "type": "agentMessage", "phase": "final", "text": "hello world" }
            }),
            &tx,
        );
        drop(tx);
        assert_eq!(
            rx.try_iter().collect::<Vec<_>>(),
            vec![
                AgentEvent::MessageStarted,
                AgentEvent::TextDelta("hello".to_string()),
                AgentEvent::MessageCompleted("hello world".to_string()),
            ]
        );
    }

    #[test]
    fn app_server_streams_commentary_as_an_agent_message() {
        let mut mapper = AppServerEventMapper {
            thread_id: Some("thread-1".to_string()),
            turn_id: Some("turn-1".to_string()),
            ..Default::default()
        };
        let (tx, rx) = mpsc::sync_channel(32);
        mapper.handle_notification(
            "item/started",
            &json!({
                "threadId": "thread-1", "turnId": "turn-1",
                "item": { "id": "commentary-1", "type": "agentMessage", "phase": "commentary" }
            }),
            &tx,
        );
        mapper.handle_notification(
            "item/agentMessage/delta",
            &json!({
                "threadId": "thread-1", "turnId": "turn-1",
                "itemId": "commentary-1", "delta": "**Planning the next tool call**"
            }),
            &tx,
        );
        mapper.handle_notification(
            "item/completed",
            &json!({
                "threadId": "thread-1", "turnId": "turn-1",
                "item": {
                    "id": "commentary-1", "type": "agentMessage", "phase": "commentary",
                    "text": "**Planning the next tool call**"
                }
            }),
            &tx,
        );
        drop(tx);

        assert_eq!(
            rx.try_iter().collect::<Vec<_>>(),
            vec![
                AgentEvent::MessageStarted,
                AgentEvent::TextDelta("**Planning the next tool call**".to_string()),
                AgentEvent::MessageCompleted("**Planning the next tool call**".to_string()),
            ]
        );
    }

    #[test]
    fn app_server_keeps_commentary_and_final_as_separate_messages() {
        let mut mapper = AppServerEventMapper {
            thread_id: Some("thread-1".to_string()),
            turn_id: Some("turn-1".to_string()),
            ..Default::default()
        };
        let (tx, rx) = mpsc::sync_channel(32);
        for (id, phase, text) in [
            ("commentary-1", "commentary", "I will inspect the renderer."),
            ("final-1", "final_answer", "The renderer is fixed."),
        ] {
            mapper.handle_notification(
                "item/started",
                &json!({
                    "threadId": "thread-1", "turnId": "turn-1",
                    "item": { "id": id, "type": "agentMessage", "phase": phase }
                }),
                &tx,
            );
            mapper.handle_notification(
                "item/agentMessage/delta",
                &json!({
                    "threadId": "thread-1", "turnId": "turn-1", "itemId": id, "delta": text
                }),
                &tx,
            );
            mapper.handle_notification(
                "item/completed",
                &json!({
                    "threadId": "thread-1", "turnId": "turn-1",
                    "item": { "id": id, "type": "agentMessage", "phase": phase, "text": text }
                }),
                &tx,
            );
        }
        drop(tx);

        assert_eq!(
            rx.try_iter().collect::<Vec<_>>(),
            vec![
                AgentEvent::MessageStarted,
                AgentEvent::TextDelta("I will inspect the renderer.".to_string()),
                AgentEvent::MessageCompleted("I will inspect the renderer.".to_string()),
                AgentEvent::MessageStarted,
                AgentEvent::TextDelta("The renderer is fixed.".to_string()),
                AgentEvent::MessageCompleted("The renderer is fixed.".to_string()),
            ]
        );
    }

    #[test]
    fn app_server_keeps_streamed_reasoning_summary_without_completed_duplicate() {
        let mut mapper = AppServerEventMapper {
            thread_id: Some("thread-1".to_string()),
            turn_id: Some("turn-1".to_string()),
            ..Default::default()
        };
        let (tx, rx) = mpsc::sync_channel(32);
        mapper.handle_notification(
            "item/reasoning/summaryTextDelta",
            &json!({
                "threadId": "thread-1", "turnId": "turn-1",
                "itemId": "reasoning-1", "delta": "Inspecting "
            }),
            &tx,
        );
        mapper.handle_notification(
            "item/reasoning/summaryTextDelta",
            &json!({
                "threadId": "thread-1", "turnId": "turn-1",
                "itemId": "reasoning-1", "delta": "the adapter"
            }),
            &tx,
        );
        mapper.handle_notification(
            "item/completed",
            &json!({
                "threadId": "thread-1", "turnId": "turn-1",
                "item": {
                    "id": "reasoning-1", "type": "reasoning",
                    "summary": "Inspecting the adapter"
                }
            }),
            &tx,
        );
        drop(tx);

        assert_eq!(
            rx.try_iter().collect::<Vec<_>>(),
            vec![
                AgentEvent::Reasoning("Inspecting ".to_string()),
                AgentEvent::Reasoning("the adapter".to_string()),
                AgentEvent::ReasoningCompleted,
            ]
        );
    }

    #[test]
    fn app_server_forwards_bold_reasoning_summaries_without_guessing_their_meaning() {
        let mut mapper = AppServerEventMapper {
            thread_id: Some("thread-1".to_string()),
            turn_id: Some("turn-1".to_string()),
            ..Default::default()
        };
        let (tx, rx) = mpsc::sync_channel(32);
        mapper.handle_notification(
            "item/reasoning/summaryTextDelta",
            &json!({
                "threadId": "thread-1", "turnId": "turn-1",
                "itemId": "reasoning-1", "delta": "**Planning comprehensive Codex documentation search**"
            }),
            &tx,
        );
        mapper.handle_notification(
            "item/completed",
            &json!({
                "threadId": "thread-1", "turnId": "turn-1",
                "item": {
                    "id": "reasoning-1", "type": "reasoning",
                    "summary": "**Planning comprehensive Codex documentation search**"
                }
            }),
            &tx,
        );
        drop(tx);

        assert_eq!(
            rx.try_iter().collect::<Vec<_>>(),
            vec![
                AgentEvent::Reasoning(
                    "**Planning comprehensive Codex documentation search**".to_string()
                ),
                AgentEvent::ReasoningCompleted,
            ]
        );
    }

    #[test]
    fn app_server_uses_summary_section_boundaries_without_text_heuristics() {
        let mut mapper = AppServerEventMapper {
            thread_id: Some("thread-1".to_string()),
            turn_id: Some("turn-1".to_string()),
            ..Default::default()
        };
        let (tx, rx) = mpsc::sync_channel(32);
        mapper.handle_notification(
            "item/reasoning/summaryTextDelta",
            &json!({
                "threadId": "thread-1", "turnId": "turn-1",
                "itemId": "reasoning-1", "summaryIndex": 0, "delta": "**First**\nbody one"
            }),
            &tx,
        );
        mapper.handle_notification(
            "item/reasoning/summaryPartAdded",
            &json!({
                "threadId": "thread-1", "turnId": "turn-1",
                "itemId": "reasoning-1", "summaryIndex": 1
            }),
            &tx,
        );
        mapper.handle_notification(
            "item/reasoning/summaryTextDelta",
            &json!({
                "threadId": "thread-1", "turnId": "turn-1",
                "itemId": "reasoning-1", "summaryIndex": 1, "delta": "**Second**\nbody two"
            }),
            &tx,
        );
        mapper.handle_notification(
            "item/completed",
            &json!({
                "threadId": "thread-1", "turnId": "turn-1",
                "item": { "id": "reasoning-1", "type": "reasoning", "summary": [] }
            }),
            &tx,
        );
        drop(tx);

        assert_eq!(
            rx.try_iter().collect::<Vec<_>>(),
            vec![
                AgentEvent::Reasoning("**First**\nbody one".to_string()),
                AgentEvent::ReasoningSectionBreak,
                AgentEvent::Reasoning("**Second**\nbody two".to_string()),
                AgentEvent::ReasoningCompleted,
            ]
        );
    }

    #[test]
    fn app_server_ignores_raw_reasoning_and_plan_deltas() {
        let mut mapper = AppServerEventMapper {
            thread_id: Some("thread-1".to_string()),
            turn_id: Some("turn-1".to_string()),
            ..Default::default()
        };
        let (tx, rx) = mpsc::sync_channel(32);
        mapper.handle_notification(
            "item/reasoning/textDelta",
            &json!({
                "threadId": "thread-1", "turnId": "turn-1",
                "itemId": "reasoning-1", "delta": "raw reasoning"
            }),
            &tx,
        );
        mapper.handle_notification(
            "item/plan/delta",
            &json!({
                "threadId": "thread-1", "turnId": "turn-1", "delta": "plan progress"
            }),
            &tx,
        );
        drop(tx);

        assert!(rx.try_iter().next().is_none());
    }

    #[test]
    fn app_server_finalizes_streamed_text_when_turn_ends_without_item_completed() {
        let mut mapper = AppServerEventMapper {
            thread_id: Some("thread-1".to_string()),
            turn_id: Some("turn-1".to_string()),
            ..Default::default()
        };
        let (tx, rx) = mpsc::sync_channel(32);
        mapper.handle_notification(
            "item/started",
            &json!({
                "threadId": "thread-1", "turnId": "turn-1",
                "item": { "id": "message-1", "type": "agentMessage", "phase": "final_answer" }
            }),
            &tx,
        );
        mapper.handle_notification(
            "item/agentMessage/delta",
            &json!({
                "threadId": "thread-1", "turnId": "turn-1", "itemId": "message-1", "delta": "final reply"
            }),
            &tx,
        );
        mapper.handle_notification(
            "turn/completed",
            &json!({
                "threadId": "thread-1", "turn": { "id": "turn-1", "status": "completed" }
            }),
            &tx,
        );
        drop(tx);

        assert_eq!(
            rx.try_iter().collect::<Vec<_>>(),
            vec![
                AgentEvent::MessageStarted,
                AgentEvent::TextDelta("final reply".to_string()),
                AgentEvent::MessageCompleted("final reply".to_string()),
            ]
        );
    }

    #[test]
    fn app_server_maps_completed_command_without_a_false_start() {
        let mut mapper = AppServerEventMapper::default();
        let (tx, rx) = mpsc::sync_channel(32);
        mapper.handle_notification(
            "item/started",
            &json!({
                "item": { "id": "cmd-1", "type": "commandExecution", "command": "cargo test", "status": "inProgress" }
            }),
            &tx,
        );
        mapper.handle_notification(
            "item/completed",
            &json!({
                "item": { "id": "cmd-1", "type": "commandExecution", "command": "cargo test", "status": "completed", "exitCode": 0, "aggregatedOutput": "ok" }
            }),
            &tx,
        );
        drop(tx);
        assert_eq!(
            rx.try_iter().collect::<Vec<_>>(),
            vec![
                AgentEvent::ToolStarted {
                    id: "cmd-1".to_string(),
                    name: "shell".to_string(),
                    summary: "cargo test".to_string()
                },
                AgentEvent::ToolCompleted {
                    id: "cmd-1".to_string(),
                    ok: true,
                    summary: "ok".to_string()
                },
            ]
        );
    }

    #[test]
    fn app_server_preserves_native_file_change_patch() {
        let events = mapper_events(
            "item/completed",
            json!({
                "item": {
                    "id": "file-1", "type": "fileChange", "status": "completed",
                    "changes": [{
                        "path": "/workspace/src/lib.rs",
                        "kind": { "type": "update" },
                        "diff": "@@ -4 +4 @@\n-old\n+new\n"
                    }]
                }
            }),
        );
        assert_eq!(
            events,
            vec![AgentEvent::FileChange {
                files: vec![
                    FileChangeItem::new("/workspace/src/lib.rs", "update")
                        .with_patch(Some("@@ -4 +4 @@\n-old\n+new\n"),)
                ],
                ok: true,
            }]
        );
    }

    #[test]
    fn app_server_forwards_member_sandbox_and_default_policy_to_start_and_resume() {
        let mut member =
            TeamMember::new("builder", "Builder", BackendKind::Codex, "implementation");
        member.sandbox = SandboxPolicy::WorkspaceWrite;
        let config = CodexAppServerConfig::from_member(&member, Path::new("/tmp"));
        assert_eq!(config.native_session_name(), "Asterline · Builder");
        assert_eq!(config.thread_params()["sandbox"], "workspace-write");
        assert_eq!(config.thread_params()["approvalPolicy"], "never");
        let resume = config.resume_params("thread-7");
        assert_eq!(resume["threadId"], "thread-7");
        assert_eq!(resume["sandbox"], "workspace-write");
        assert_eq!(resume["approvalPolicy"], "never");
        member.permission_mode = Some(PermissionMode::Default);
        let config = CodexAppServerConfig::from_member(&member, Path::new("/tmp"));
        assert_eq!(config.thread_params()["approvalPolicy"], "never");
        assert!(
            config.turn_params("thread", "hello", Some(Effort::High))["sandboxPolicy"].is_null()
        );
        let img_dir =
            std::env::temp_dir().join(format!("asterline-codex-img-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&img_dir);
        let img = img_dir.join("shot.png");
        std::fs::write(&img, [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']).unwrap();
        let input = config.turn_params(
            "thread",
            &format!("look\n[asterline-image]: {}", img.display()),
            None,
        )["input"]
            .as_array()
            .cloned()
            .unwrap();
        assert_eq!(input[0]["type"], "text");
        assert_eq!(input[0]["text"], "look");
        assert_eq!(input[1]["type"], "localImage");
        assert_eq!(input[1]["path"], img.to_string_lossy().as_ref());
        let _ = std::fs::remove_dir_all(&img_dir);
    }

    #[test]
    fn app_server_maps_member_permission_modes_to_native_approval_policies() {
        let mut member =
            TeamMember::new("builder", "Builder", BackendKind::Codex, "implementation");
        let expected = [
            (PermissionMode::AcceptEdits, "untrusted"),
            (PermissionMode::Plan, "untrusted"),
            (PermissionMode::Auto, "on-request"),
            (PermissionMode::DontAsk, "never"),
            (PermissionMode::BypassPermissions, "never"),
        ];

        for (mode, policy) in expected {
            member.permission_mode = Some(mode);
            let config = CodexAppServerConfig::from_member(&member, Path::new("/tmp"));
            assert_eq!(config.thread_params()["approvalPolicy"], policy, "{mode:?}");
        }
    }

    #[test]
    fn app_server_model_page_uses_turn_model_id_and_advertised_efforts() {
        let models = parse_model_page(&json!({
            "data": [
                {
                    "id": "internal-id",
                    "model": "gpt-5.6-terra",
                    "displayName": "GPT-5.6 Terra",
                    "description": "General coding model",
                    "defaultReasoningEffort": "medium",
                    "supportedReasoningEfforts": [
                        { "reasoningEffort": "low" },
                        { "reasoningEffort": "high" }
                    ],
                    "isDefault": true,
                    "hidden": false
                },
                {
                    "id": "hidden",
                    "model": "hidden",
                    "displayName": "Hidden",
                    "description": "",
                    "defaultReasoningEffort": "low",
                    "supportedReasoningEfforts": [],
                    "isDefault": false,
                    "hidden": true
                }
            ]
        }))
        .unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "gpt-5.6-terra");
        assert_eq!(models[0].name, "GPT-5.6 Terra");
        assert_eq!(models[0].default_effort, Some(Effort::Medium));
        assert_eq!(models[0].supported_efforts, vec![Effort::Low, Effort::High]);
        assert!(models[0].is_default);
    }

    #[test]
    fn native_approval_results_follow_the_user_decision() {
        let command = json!({ "command": "cargo test", "reason": "run tests" });
        assert_eq!(
            native_approval_details("item/commandExecution/requestApproval", &command,),
            Some(("Codex command", "cargo test\n\nrun tests".to_string()))
        );
        assert_eq!(
            native_approval_result(
                "item/fileChange/requestApproval",
                &Value::Null,
                ApprovalDecision::Approve,
            ),
            json!({ "decision": "accept" })
        );
        assert_eq!(
            native_approval_result(
                "item/commandExecution/requestApproval",
                &Value::Null,
                ApprovalDecision::Reject,
            ),
            json!({ "decision": "decline" })
        );
        let requested = json!({
            "permissions": {
                "fileSystem": {"write": ["/tmp/example"]},
                "network": {"enabled": true}
            }
        });
        assert_eq!(
            native_approval_result(
                "item/permissions/requestApproval",
                &requested,
                ApprovalDecision::Reject,
            ),
            json!({ "permissions": {} })
        );
        assert_eq!(
            native_approval_result(
                "item/permissions/requestApproval",
                &requested,
                ApprovalDecision::Approve,
            ),
            json!({ "permissions": requested["permissions"] })
        );
        assert_eq!(
            native_approval_details("item/tool/call", &Value::Null),
            None
        );
        assert_eq!(
            unsupported_native_request_response("item/tool/requestUserInput"),
            Some(json!({ "answers": {} }))
        );
    }

    #[test]
    fn missing_rollouts_can_fall_back_but_auth_errors_cannot() {
        assert!(resume_target_is_missing("thread not found"));
        assert!(resume_target_is_missing("no rollout exists"));
        assert!(!resume_target_is_missing("authentication required"));
    }

    #[test]
    fn runner_reports_codex_backend() {
        let member = TeamMember::new("builder", "Builder", BackendKind::Codex, "implementation");
        let runner =
            CodexAppServerRunner::from_member(&member, Path::new("/tmp")).with_binary("fixture");
        assert_eq!(runner.backend(), BackendKind::Codex);
    }

    #[test]
    fn line_pump_unblocks_when_its_receiver_is_dropped() {
        let (tx, rx) = mpsc::sync_channel(0);
        let worker = spawn_line_pump(Cursor::new(b"one\n"), 64, tx);
        drop(rx);
        assert!(worker.join().is_ok());
    }

    #[test]
    fn app_server_accepts_large_tool_frames_without_persisting_raw_payloads() {
        let output = "x".repeat(8 * 1024 * 1024);
        let message = json!({
            "method": "item/completed",
            "params": {
                "item": {
                    "id": "cmd-1",
                    "type": "commandExecution",
                    "command": "cargo test",
                    "status": "completed",
                    "exitCode": 0,
                    "aggregatedOutput": output,
                }
            }
        });
        let mut encoded = serde_json::to_string(&message).unwrap();
        assert!(encoded.len() > 8 * 1024 * 1024);
        assert!(encoded.len() < MAX_APP_SERVER_PROTOCOL_LINE_BYTES);
        encoded.push('\n');

        let line = bounded_lines(
            BufReader::new(Cursor::new(encoded)),
            MAX_APP_SERVER_PROTOCOL_LINE_BYTES,
        )
        .next()
        .unwrap()
        .unwrap();
        let parsed: Value = serde_json::from_str(&line).unwrap();
        assert!(!should_persist_raw_message(&line, &parsed));

        let events = mapper_events("item/completed", parsed["params"].clone());
        let [AgentEvent::ToolCompleted { ok, summary, .. }] = events.as_slice() else {
            panic!("expected one bounded tool completion, got {events:?}");
        };
        assert!(*ok);
        assert!(
            summary.chars().count() <= TOOL_OUTPUT_MAX,
            "{}",
            summary.chars().count()
        );
    }

    #[cfg(unix)]
    #[test]
    fn persistent_runner_reuses_its_live_native_thread() {
        let dir =
            std::env::temp_dir().join(format!("asterline-codex-app-server-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let server = dir.join("fake-codex-app-server");
        let input_log = dir.join("input.jsonl");
        let script = format!(
            r#"#!/bin/sh
log='{log}'
read_line() {{
  IFS= read -r line || exit 0
  printf '%s\n' "$line" >> "$log"
}}
read_line
printf '%s\n' '{{"id":1,"result":{{"userAgent":"fake-codex"}}}}'
read_line
read_line
printf '%s\n' '{{"id":2,"result":{{"thread":{{"id":"thread-1"}}}}}}'
read_line
printf '%s\n' '{{"id":3,"result":{{}}}}'
read_line
printf '%s\n' '{{"id":4,"result":{{"turn":{{"id":"turn-1"}}}}}}'
printf '%s\n' '{{"method":"item/started","params":{{"threadId":"thread-1","turnId":"turn-1","item":{{"id":"message-1","type":"agentMessage","phase":"final","text":""}}}}}}'
printf '%s\n' '{{"method":"item/agentMessage/delta","params":{{"threadId":"thread-1","turnId":"turn-1","itemId":"message-1","delta":"first"}}}}'
printf '%s\n' '{{"method":"item/completed","params":{{"threadId":"thread-1","turnId":"turn-1","item":{{"id":"message-1","type":"agentMessage","phase":"final","text":"first"}}}}}}'
printf '%s\n' '{{"method":"turn/completed","params":{{"threadId":"thread-1","turnId":"turn-1","turn":{{"id":"turn-1","status":"completed"}}}}}}'
read_line
  printf '%s\n' '{{"id":5,"result":{{"turn":{{"id":"turn-2"}}}}}}'
printf '%s\n' '{{"method":"turn/completed","params":{{"threadId":"thread-1","turnId":"turn-2","turn":{{"id":"turn-2","status":"completed"}}}}}}'
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$log"
done
"#,
            log = input_log.display(),
        );
        std::fs::write(&server, script).unwrap();
        let mut permissions = std::fs::metadata(&server).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&server, permissions).unwrap();

        let member = TeamMember::new("builder", "Builder", BackendKind::Codex, "implementation");
        let runner = CodexAppServerRunner::from_member(&member, &dir)
            .with_binary(server.display().to_string());
        let (events, received) = mpsc::sync_channel(64);
        runner.run(
            RunRequest {
                prompt: "first prompt".to_string(),
                session: None,
                cancel: Arc::new(AtomicBool::new(false)),
                effort: None,
            },
            events.clone(),
        );
        let first_events = received.try_iter().collect::<Vec<_>>();
        assert!(
            first_events.contains(&AgentEvent::SessionDiscovered(AgentSessionId(
                "thread-1".to_string()
            )))
        );
        assert!(first_events.contains(&AgentEvent::MessageCompleted("first".to_string())));
        assert!(first_events.contains(&AgentEvent::Exited {
            code: None,
            ok: true
        }));

        runner.run(
            RunRequest {
                prompt: "second prompt".to_string(),
                session: Some(AgentSessionId("thread-1".to_string())),
                cancel: Arc::new(AtomicBool::new(false)),
                effort: Some(Effort::High),
            },
            events,
        );
        let second_events = received.try_iter().collect::<Vec<_>>();
        assert!(second_events.contains(&AgentEvent::Exited {
            code: None,
            ok: true
        }));
        drop(runner);

        let input = std::fs::read_to_string(&input_log)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            input
                .iter()
                .filter(|message| message["method"] == "initialize")
                .count(),
            1
        );
        assert!(input.iter().all(|message| message.get("jsonrpc").is_none()));
        assert!(
            input
                .iter()
                .any(|message| message["method"] == "thread/start")
        );
        assert!(input.iter().any(|message| {
            message["method"] == "thread/name/set"
                && message["params"]["name"] == "Asterline · Builder"
        }));
        assert!(
            !input
                .iter()
                .any(|message| message["method"] == "thread/resume")
        );
        assert_eq!(
            input
                .iter()
                .filter(|message| message["method"] == "turn/start")
                .count(),
            2
        );
        assert!(input.iter().any(|message| {
            message["method"] == "turn/start" && message["params"]["effort"] == "high"
        }));
        let _ = std::fs::remove_dir_all(dir);
    }
}
