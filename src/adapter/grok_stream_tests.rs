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
fn default_grok_member_passes_workspace_sandbox_and_auto() {
    let member = crate::domain::config::default_member(BackendKind::Grok);
    let runner = GrokAcpRunner::from_member(&member, Path::new("/tmp/ws"));
    let args = runner.command_args(None);
    assert!(args.windows(2).any(|w| w == ["--sandbox", "workspace"]));
    assert!(args.windows(2).any(|w| w == ["--permission-mode", "auto"]));
    assert!(!args.contains(&"--always-approve".to_string()));
}

#[test]
fn always_approve_is_passed_as_the_grok_flag_not_bypass_permissions() {
    let mut member = TeamMember::new("grok", "Grok", BackendKind::Grok, "implementation");
    member.permission_mode = Some(PermissionMode::BypassPermissions);
    member.apply_visible_mode_sandbox();
    let runner = GrokAcpRunner::from_member(&member, Path::new("/tmp/ws"));
    let args = runner.command_args(None);
    assert!(args.windows(2).any(|w| w == ["--sandbox", "off"]));
    assert!(args.contains(&"--always-approve".to_string()));
    assert!(
        !args
            .windows(2)
            .any(|w| w == ["--permission-mode", "bypassPermissions"])
    );
    let always = args.iter().position(|a| a == "--always-approve").unwrap();
    let agent = args.iter().position(|a| a == "agent").unwrap();
    assert!(always < agent);
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
    std::fs::write(&server, "#!/bin/sh\nyes x | tr -d '\\n'\n").unwrap();
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
    let done = events
        .iter()
        .position(|event| event == &AgentEvent::MessageCompleted("Done".to_string()))
        .expect("pre-tool text is committed");
    let tool = events
        .iter()
        .position(|event| matches!(event, AgentEvent::ToolStarted { id, .. } if id == "tool-1"))
        .expect("tool start");
    assert!(done < tool);
    assert!(events.contains(&AgentEvent::MessageCompleted("Done".to_string())));
}

#[test]
fn post_tool_text_starts_a_new_message_after_the_tools() {
    let mut parser = GrokAcpParser::default();
    let mut events = Vec::new();
    events.extend(parser.parse_update(&json!({
        "sessionUpdate": "agent_message_chunk",
        "content": {"type": "text", "text": "I'll look it up."}
    })));
    events.extend(parser.parse_update(&json!({
        "sessionUpdate": "tool_call",
        "toolCallId": "search-1",
        "title": "search",
        "kind": "search",
        "status": "completed",
        "rawInput": {"query": "asterline"}
    })));
    events.extend(parser.parse_update(&json!({
        "sessionUpdate": "agent_message_chunk",
        "content": {"type": "text", "text": "Asterline is a team TUI."}
    })));
    events.extend(parser.finish(false));

    let preamble = events
        .iter()
        .position(|event| event == &AgentEvent::MessageCompleted("I'll look it up.".to_string()))
        .expect("preamble");
    let tool = events
        .iter()
        .position(|event| matches!(event, AgentEvent::ToolStarted { id, .. } if id == "search-1"))
        .expect("tool");
    let reply_start = events
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, event)| matches!(event, AgentEvent::MessageStarted).then_some(index))
        .expect("post-tool message start");
    let reply = events
        .iter()
        .position(|event| {
            event == &AgentEvent::MessageCompleted("Asterline is a team TUI.".to_string())
        })
        .expect("final reply");
    assert!(
        preamble < tool && tool < reply_start && reply_start < reply,
        "{events:?}"
    );
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
        files: vec![
            FileChangeItem::new("/tmp/ws/src/lib.rs", "add").with_texts(None::<String>, Some("x"),)
        ],
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
fn failed_tool_does_not_use_raw_input_as_the_result() {
    let mut parser = GrokAcpParser::default();
    parser.parse_update(&json!({
        "sessionUpdate": "tool_call",
        "toolCallId": "read-1",
        "title": "read_file",
        "kind": "read",
        "status": "in_progress",
        "rawInput": {"target_file": "missing.rs", "offset": 1}
    }));
    let events = parser.parse_update(&json!({
        "sessionUpdate": "tool_call_update",
        "toolCallId": "read-1",
        "status": "failed",
        "rawInput": {"target_file": "missing.rs", "offset": 1}
    }));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolCompleted { id, ok: false, summary }
            if id == "read-1" && !summary.contains("target_file") && !summary.contains('{')
    )));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::ToolProgress { .. }))
    );
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
