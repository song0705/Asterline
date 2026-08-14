use super::*;
use crate::domain::event::ChatItem;
use crate::domain::mode::{
    BrainstormModeConfig, PlanModeConfig, ReviewModeConfig, TeamModeConfig, resolve_mode_roles,
};
use crate::domain::team::{
    ApprovalSurface, BackendKind, DefaultTarget, Effort, SessionPolicy, TeamMember,
};
use rusqlite::Connection;

fn team() -> TeamConfig {
    let mut config = TeamConfig::new("mixed", "/tmp/ws")
        .with_member(TeamMember::new(
            "builder",
            "Builder",
            BackendKind::Codex,
            "impl",
        ))
        .with_member(TeamMember::new(
            "reviewer",
            "Reviewer",
            BackendKind::Claude,
            "review",
        ));
    config.default_target = Some(DefaultTarget::Member(MemberId::new("builder")));
    config
}

fn runtime() -> TeamRuntime {
    TeamRuntime::new(team(), SqliteStore::in_memory().unwrap()).with_approvals(false)
}

fn runtime_in_workspace(workspace: impl Into<PathBuf>) -> TeamRuntime {
    let mut config = TeamConfig::new("mixed", workspace)
        .with_member(TeamMember::new(
            "builder",
            "Builder",
            BackendKind::Codex,
            "impl",
        ))
        .with_member(TeamMember::new(
            "reviewer",
            "Reviewer",
            BackendKind::Claude,
            "review",
        ));
    config.default_target = Some(DefaultTarget::Member(MemberId::new("builder")));
    TeamRuntime::new(config, SqliteStore::in_memory().unwrap()).with_approvals(false)
}

fn remove_sqlite_test_files(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    for suffix in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

#[test]
fn targeted_slash_skill_uses_backend_native_syntax() {
    assert_eq!(
        normalize_backend_command(BackendKind::Codex, "/review-patch staged".to_string()),
        "$review-patch staged"
    );
    for backend in [BackendKind::Claude, BackendKind::Grok, BackendKind::Agy] {
        assert_eq!(
            normalize_backend_command(backend, "/review-patch staged".to_string()),
            "/review-patch staged"
        );
    }
}

fn user(body: &str) -> UiCommand {
    UiCommand::UserMessage {
        target: MessageTarget::Default,
        body: body.to_string(),
    }
}

fn start_team(rt: &mut TeamRuntime, goal: &str) -> RuntimeStep {
    rt.on_ui_command(UiCommand::SetMode {
        mode: TerminalMode::Team,
    });
    rt.on_ui_command(user(goal))
}

#[test]
fn user_message_starts_a_run_for_default_member() {
    let mut rt = runtime();
    let step = rt.on_ui_command(user("build it"));

    assert_eq!(step.actions.len(), 1);
    assert_eq!(step.actions[0].member, MemberId::new("builder"));
    assert!(
        step.events
            .iter()
            .any(|e| matches!(e, RuntimeEvent::TurnStarted { .. }))
    );
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::MemberStatus {
            status: MemberStatus::Running,
            ..
        }
    )));
}

#[test]
fn user_message_is_not_dispatched_when_persistence_fails() {
    let path = std::env::temp_dir().join(format!(
        "asterline-store-failure-{}.sqlite3",
        std::process::id()
    ));
    remove_sqlite_test_files(&path);
    let store = SqliteStore::open(&path).unwrap();
    let mut rt = TeamRuntime::new(team(), store).with_approvals(false);
    let external = Connection::open(&path).unwrap();
    external.execute("DROP TABLE messages", []).unwrap();

    let step = rt.on_ui_command(user("must be durable"));

    assert!(step.actions.is_empty());
    assert!(
        !step
            .events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::UserMessage { .. }))
    );
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Notice(text) if text.contains("could not save the user message")
    )));
    drop(external);
    drop(rt);
    remove_sqlite_test_files(&path);
}

#[test]
fn agent_message_controls_are_not_executed_when_message_persistence_fails() {
    let path = std::env::temp_dir().join(format!(
        "asterline-agent-message-failure-{}.sqlite3",
        std::process::id()
    ));
    remove_sqlite_test_files(&path);
    let mut rt = TeamRuntime::new(team(), SqliteStore::open(&path).unwrap()).with_approvals(false);
    let external = Connection::open(&path).unwrap();
    rt.on_ui_command(user("must remain auditable"));
    external
        .execute_batch(
            "CREATE TRIGGER fail_agent_message
             BEFORE INSERT ON messages
             WHEN NEW.kind = 'agent'
             BEGIN SELECT RAISE(ABORT, 'agent message unavailable'); END;",
        )
        .unwrap();

    let step = rt.on_agent_event(
        &MemberId::new("builder"),
        AgentEvent::MessageCompleted(
            "visible answer\n@@team_message {\"to\":\"reviewer\",\"body\":\"act on this\"}"
                .to_string(),
        ),
    );

    assert!(step.actions.is_empty());
    assert!(
        !step
            .events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::Route { .. }))
    );
    assert!(!step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::MessageCompleted { text, .. } if !text.is_empty()
    )));
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Notice(text) if text.contains("could not save an agent message")
    )));
    let (agents, routes): (i64, i64) = external
        .query_row(
            "SELECT
                 SUM(CASE WHEN kind = 'agent' THEN 1 ELSE 0 END),
                 SUM(CASE WHEN kind = 'route' THEN 1 ELSE 0 END)
             FROM messages",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((agents, routes), (0, 0));
    drop(external);
    drop(rt);
    remove_sqlite_test_files(&path);
}

#[test]
fn control_only_message_is_not_executed_when_control_source_persistence_fails() {
    let path = std::env::temp_dir().join(format!(
        "asterline-control-source-failure-{}.sqlite3",
        std::process::id()
    ));
    remove_sqlite_test_files(&path);
    let mut rt = TeamRuntime::new(team(), SqliteStore::open(&path).unwrap()).with_approvals(false);
    let external = Connection::open(&path).unwrap();
    rt.on_ui_command(user("keep controls auditable"));
    external
        .execute_batch(
            "CREATE TRIGGER fail_control_source
             BEFORE INSERT ON messages
             WHEN NEW.kind = 'agent_control'
             BEGIN SELECT RAISE(ABORT, 'control source unavailable'); END;",
        )
        .unwrap();

    let step = rt.on_agent_event(
        &MemberId::new("builder"),
        AgentEvent::MessageCompleted(
            "@@team_message {\"to\":\"reviewer\",\"body\":\"act on this\"}".to_string(),
        ),
    );

    assert!(step.actions.is_empty());
    assert!(
        !step
            .events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::Route { .. }))
    );
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Notice(text) if text.contains("could not save an agent control source")
    )));
    let (sources, routes): (i64, i64) = external
        .query_row(
            "SELECT
                 SUM(CASE WHEN kind = 'agent_control' THEN 1 ELSE 0 END),
                 SUM(CASE WHEN kind = 'route' THEN 1 ELSE 0 END)
             FROM messages",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((sources, routes), (0, 0));
    drop(external);
    drop(rt);
    remove_sqlite_test_files(&path);
}

#[test]
fn controls_are_not_executed_after_raw_stream_persistence_fails() {
    let path = std::env::temp_dir().join(format!(
        "asterline-control-raw-failure-{}.sqlite3",
        std::process::id()
    ));
    remove_sqlite_test_files(&path);
    let mut rt = TeamRuntime::new(team(), SqliteStore::open(&path).unwrap()).with_approvals(false);
    let external = Connection::open(&path).unwrap();
    rt.on_ui_command(user("keep raw controls auditable"));
    external
        .execute_batch(
            "CREATE TRIGGER fail_raw_stream
             BEFORE INSERT ON stream_events
             BEGIN SELECT RAISE(ABORT, 'raw stream unavailable'); END;",
        )
        .unwrap();

    let raw = rt.on_agent_event(
        &MemberId::new("builder"),
        AgentEvent::Raw("backend control output".to_string()),
    );
    assert!(raw.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Notice(text) if text.contains("could not save a raw stream event")
    )));
    let second_raw = rt.on_agent_event(
        &MemberId::new("builder"),
        AgentEvent::Raw("later backend control output".to_string()),
    );
    assert!(
        second_raw.events.is_empty(),
        "raw persistence fails only once per run"
    );
    let completed = rt.on_agent_event(
        &MemberId::new("builder"),
        AgentEvent::MessageCompleted(
            "@@team_message {\"to\":\"reviewer\",\"body\":\"act on this\"}".to_string(),
        ),
    );

    assert!(completed.actions.is_empty());
    assert!(
        !completed
            .events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::Route { .. }))
    );
    assert!(completed.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Notice(text) if text.contains("ignored controls")
    )));
    let (sources, routes): (i64, i64) = external
        .query_row(
            "SELECT
                 SUM(CASE WHEN kind = 'agent_control' THEN 1 ELSE 0 END),
                 SUM(CASE WHEN kind = 'route' THEN 1 ELSE 0 END)
             FROM messages",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((sources, routes), (0, 0));
    drop(external);
    drop(rt);
    remove_sqlite_test_files(&path);
}

#[test]
fn route_is_not_emitted_or_dispatched_when_route_persistence_fails() {
    let path = std::env::temp_dir().join(format!(
        "asterline-route-failure-{}.sqlite3",
        std::process::id()
    ));
    remove_sqlite_test_files(&path);
    let mut rt = TeamRuntime::new(team(), SqliteStore::open(&path).unwrap()).with_approvals(false);
    let external = Connection::open(&path).unwrap();
    rt.on_ui_command(user("route durably"));
    external
        .execute_batch(
            "CREATE TRIGGER fail_route
             BEFORE INSERT ON messages
             WHEN NEW.kind = 'route'
             BEGIN SELECT RAISE(ABORT, 'route unavailable'); END;",
        )
        .unwrap();

    let step = rt.on_agent_event(
        &MemberId::new("builder"),
        AgentEvent::MessageCompleted(
            "done\n@@team_message {\"to\":\"reviewer\",\"body\":\"review this\"}".to_string(),
        ),
    );

    assert!(step.actions.is_empty());
    assert!(
        !step
            .events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::Route { .. }))
    );
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Notice(text) if text.contains("could not save an agent route")
    )));
    let route_count: i64 = external
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE kind = 'route'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(route_count, 0);
    drop(external);
    drop(rt);
    remove_sqlite_test_files(&path);
}

#[test]
fn selected_terminal_mode_dispatches_subsequent_plain_messages() {
    let mut rt = runtime();
    let changed = rt.on_ui_command(UiCommand::SetMode {
        mode: TerminalMode::Review,
    });
    assert!(changed.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::ModeChanged {
            mode: TerminalMode::Review
        }
    )));

    let step = rt.on_ui_command(user("fix parser"));
    assert_eq!(step.actions.len(), 1);
    assert_eq!(step.actions[0].member, MemberId::new("builder"));
    assert!(step.actions[0].prompt.contains("fix parser"));
    assert!(step.actions[0].prompt.contains("builder in review mode"));
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::RunUpdated { run }
            if run.mode.as_ref().is_some_and(|mode| mode.mode == CollabMode::Review)
    )));
}

#[test]
fn new_chat_resets_selected_terminal_mode_to_normal() {
    let mut rt = runtime();
    rt.on_ui_command(UiCommand::SetMode {
        mode: TerminalMode::Review,
    });
    let reset = rt.on_ui_command(UiCommand::NewSession);

    assert!(reset.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::ModeChanged {
            mode: TerminalMode::Normal
        }
    )));

    let normal = rt.on_ui_command(user("fix parser"));
    assert!(!normal.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::RunUpdated { run }
            if run.mode.as_ref().is_some_and(|mode| mode.mode == CollabMode::Review)
    )));
}

#[test]
fn selected_terminal_mode_is_restored_with_its_conversation() {
    let mut rt = runtime();
    let original = rt.store.active_conversation();
    rt.on_ui_command(UiCommand::SetMode {
        mode: TerminalMode::Review,
    });

    rt.on_ui_command(UiCommand::NewSession);
    assert_eq!(rt.active_mode(), TerminalMode::Normal);

    let resumed = rt.on_ui_command(UiCommand::ResumeConversation {
        conversation: original,
    });
    assert_eq!(rt.active_mode(), TerminalMode::Review);
    assert!(resumed.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::ModeChanged {
            mode: TerminalMode::Review
        }
    )));

    let dispatched = rt.on_ui_command(user("fix parser"));
    assert!(dispatched.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::RunUpdated { run }
            if run.mode.as_ref().is_some_and(|mode| mode.mode == CollabMode::Review)
    )));
}

#[test]
fn selected_terminal_mode_survives_runtime_restart() {
    let path = std::env::temp_dir().join(format!(
        "asterline-conversation-mode-{}.sqlite3",
        std::process::id()
    ));
    remove_sqlite_test_files(&path);
    {
        let store = SqliteStore::open(&path).unwrap();
        let mut rt = TeamRuntime::new(team(), store).with_approvals(false);
        rt.on_ui_command(UiCommand::SetMode {
            mode: TerminalMode::Plan,
        });
    }

    let restored = TeamRuntime::new(team(), SqliteStore::open(&path).unwrap());
    assert_eq!(restored.active_mode(), TerminalMode::Plan);
    drop(restored);
    remove_sqlite_test_files(&path);
}

#[test]
fn new_chat_is_rejected_while_a_member_is_active() {
    let mut rt = runtime();
    let original = rt.store.active_conversation();
    rt.on_ui_command(user("keep working"));

    let rejected = rt.on_ui_command(UiCommand::NewSession);

    assert_eq!(rt.store.active_conversation(), original);
    assert!(
        !rejected
            .events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::SessionReset))
    );
    assert!(rejected.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Notice(text)
            if text.contains("cannot start a new chat") && text.contains("/abort")
    )));
}

#[test]
fn new_chat_keeps_current_state_when_atomic_reset_fails() {
    let path = std::env::temp_dir().join(format!(
        "asterline-new-chat-reset-failure-{}.sqlite3",
        std::process::id()
    ));
    remove_sqlite_test_files(&path);
    let mut rt = TeamRuntime::new(team(), SqliteStore::open(&path).unwrap()).with_approvals(false);
    let builder = MemberId::new("builder");
    let original = rt.store.active_conversation();
    rt.on_ui_command(user("old chat"));
    rt.on_agent_event(
        &builder,
        AgentEvent::SessionDiscovered(AgentSessionId("old-session".to_string())),
    );
    rt.on_agent_event(
        &builder,
        AgentEvent::Exited {
            code: Some(0),
            ok: true,
        },
    );
    let external = Connection::open(&path).unwrap();
    external
        .execute_batch(
            "CREATE TRIGGER fail_new_chat_session_clear
             BEFORE DELETE ON agent_sessions
             BEGIN SELECT RAISE(ABORT, 'session clear unavailable'); END;",
        )
        .unwrap();

    let reset = rt.on_ui_command(UiCommand::NewSession);

    assert_eq!(rt.store.active_conversation(), original);
    assert!(
        !reset
            .events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::SessionReset))
    );
    assert!(reset.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Notice(text) if text.contains("could not start a new chat")
    )));
    let next = rt.on_ui_command(user("still old chat"));
    assert_eq!(
        next.actions[0].session,
        Some(AgentSessionId("old-session".to_string()))
    );
    drop(external);
    drop(rt);
    remove_sqlite_test_files(&path);
}

#[test]
fn new_chat_clears_retry_history() {
    let mut rt = runtime();
    let builder = MemberId::new("builder");
    rt.on_ui_command(user("old request"));
    rt.on_agent_event(
        &builder,
        AgentEvent::Exited {
            code: Some(0),
            ok: true,
        },
    );

    rt.on_ui_command(UiCommand::NewSession);
    let retry = rt.on_ui_command(UiCommand::Retry);

    assert!(retry.actions.is_empty());
    assert!(retry.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Notice(text) if text == "nothing to retry"
    )));
}

#[test]
fn new_and_resume_scope_ready_runs_to_the_selected_conversation() {
    let mut rt = runtime();
    let original = rt.store.active_conversation();
    let original_run = rt.store.create_run("original chat run", None).unwrap();
    assert!(matches!(
        rt.ready_event(),
        RuntimeEvent::Ready { runs, .. }
            if runs.iter().map(|run| run.id).collect::<Vec<_>>() == vec![original_run.id]
    ));

    rt.on_ui_command(UiCommand::NewSession);
    assert!(matches!(
        rt.ready_event(),
        RuntimeEvent::Ready { runs, .. } if runs.is_empty()
    ));

    let resumed = rt.on_ui_command(UiCommand::ResumeConversation {
        conversation: original,
    });
    assert!(resumed.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Ready { runs, .. }
            if runs.iter().map(|run| run.id).collect::<Vec<_>>() == vec![original_run.id]
    )));
}

#[test]
fn resume_picker_restores_chat_roster_and_native_member_sessions() {
    let mut rt = runtime();
    let original = rt.store.active_conversation();
    let builder = MemberId::new("builder");

    rt.on_ui_command(user("original question"));
    rt.on_agent_event(
        &builder,
        AgentEvent::SessionDiscovered(AgentSessionId("codex-original".to_string())),
    );
    rt.on_agent_event(
        &builder,
        AgentEvent::MessageCompleted("original answer".to_string()),
    );
    rt.on_agent_event(
        &builder,
        AgentEvent::Exited {
            code: Some(0),
            ok: true,
        },
    );

    rt.on_ui_command(UiCommand::NewSession);
    let replacement = TeamMember::new("researcher", "Researcher", BackendKind::Grok, "research");
    rt.on_ui_command(UiCommand::ReplaceTeam {
        members: vec![replacement],
        default_target: Some(DefaultTarget::Member(MemberId::new("researcher"))),
    });

    let choices = rt.on_ui_command(UiCommand::RequestResume);
    assert!(choices.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::ResumeChoices { conversations }
            if conversations.iter().any(|conversation| {
                conversation.id == original
                    && conversation.preview == "original question"
                    && conversation.member_count == 2
            })
    )));

    let resumed = rt.on_ui_command(UiCommand::ResumeConversation {
        conversation: original,
    });
    assert!(resumed.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::ConversationResumed { conversation, chat }
            if *conversation == original
                && chat.iter().any(|item| matches!(
                    item,
                    ChatItem::User { body } if body == "original question"
                ))
    )));
    assert!(resumed.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Ready { members, .. }
            if members.len() == 2
                && members.iter().any(|member| {
                    member.id == builder
                        && member.session.as_deref() == Some("codex-original")
                })
    )));
    assert_eq!(rt.store.active_conversation(), original);

    let continued = rt.on_ui_command(user("continue original"));
    assert_eq!(continued.actions[0].member, builder);
    assert_eq!(
        continued.actions[0].session,
        Some(AgentSessionId("codex-original".to_string()))
    );
}

#[test]
fn completed_message_is_emitted_and_persisted_then_turn_finishes() {
    let mut rt = runtime();
    rt.on_ui_command(user("build it"));
    let builder = MemberId::new("builder");

    let step = rt.on_agent_event(&builder, AgentEvent::MessageCompleted("done".to_string()));
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::MessageCompleted { text, .. } if text == "done"
    )));

    let step = rt.on_agent_event(
        &builder,
        AgentEvent::Exited {
            code: Some(0),
            ok: true,
        },
    );
    assert!(
        step.events
            .iter()
            .any(|e| matches!(e, RuntimeEvent::TurnFinished { .. }))
    );

    let items = rt.store.replay_chat().unwrap();
    assert!(items.iter().any(|i| matches!(
        i,
        crate::domain::event::ChatItem::Agent { text, .. } if text == "done"
    )));
}

#[test]
fn reasoning_events_are_member_scoped_bounded_snapshots() {
    let mut rt = runtime();
    let builder = MemberId::new("builder");
    rt.on_ui_command(user("explain the plan"));

    let first = rt.on_agent_event(&builder, AgentEvent::Reasoning("first ".to_string()));
    let second = rt.on_agent_event(&builder, AgentEvent::Reasoning("second".to_string()));

    assert!(first.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Reasoning { member, text }
            if member == &builder && text == "first "
    )));
    assert!(second.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Reasoning { member, text }
            if member == &builder && text == "first second"
    )));
}

#[test]
fn tool_input_progress_and_result_are_preserved_separately() {
    let mut rt = runtime();
    rt.on_ui_command(user("run tests"));
    let builder = MemberId::new("builder");

    rt.on_agent_event(
        &builder,
        AgentEvent::ToolStarted {
            id: "tool-1".to_string(),
            name: "shell".to_string(),
            summary: "cargo test".to_string(),
        },
    );
    let progress = rt.on_agent_event(
        &builder,
        AgentEvent::ToolProgress {
            id: "tool-1".to_string(),
            delta: "running parser tests\n".to_string(),
        },
    );
    let completed = rt.on_agent_event(
        &builder,
        AgentEvent::ToolCompleted {
            id: "tool-1".to_string(),
            ok: false,
            summary: "error: parser test failed".to_string(),
        },
    );

    assert!(progress.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::ToolProgress { delta, .. } if delta == "running parser tests\n"
    )));
    assert!(completed.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::ToolCompleted { ok: false, output, .. }
            if output.contains("running parser tests")
                && output.contains("error: parser test failed")
    )));
    assert!(rt.store.replay_chat().unwrap().iter().any(|item| matches!(
        item,
        ChatItem::Tool { summary, detail, ok: Some(false), .. }
            if summary == "cargo test"
                && detail.contains("running parser tests")
                && detail.contains("error: parser test failed")
    )));
}

#[test]
fn failed_file_change_keeps_its_failure_status_through_replay() {
    let mut rt = runtime();
    rt.on_ui_command(user("edit the parser"));
    let builder = MemberId::new("builder");
    let files = vec![("src/parser.rs".to_string(), "update".to_string())];

    let step = rt.on_agent_event(
        &builder,
        AgentEvent::FileChange {
            files: files.clone(),
            ok: false,
        },
    );

    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::FileChange { files: changed, ok: false, .. } if changed == &files
    )));
    assert!(rt.store.replay_chat().unwrap().iter().any(|item| matches!(
        item,
        ChatItem::Diff { files: changed, ok: false, .. } if changed == &files
    )));
}

#[test]
fn codex_can_explain_frontend_design_to_agy_via_team_message() {
    let mut config = TeamConfig::new("frontend", "/tmp/ws")
        .with_member(TeamMember::new(
            "codex",
            "Codex",
            BackendKind::Codex,
            "frontend implementation",
        ))
        .with_member(TeamMember::new(
            "agy",
            "Agy",
            BackendKind::Agy,
            "frontend research",
        ));
    config.default_target = Some(DefaultTarget::Member(MemberId::new("codex")));
    let mut rt = TeamRuntime::new(config, SqliteStore::in_memory().unwrap()).with_approvals(false);
    rt.on_ui_command(user("Explain the frontend design to Agy"));
    let codex = MemberId::new("codex");
    let agy = MemberId::new("agy");
    let explanation = "Use a chat-first layout, persistent member identity, and visible handoffs.";

    let route = rt.on_agent_event(
        &codex,
        AgentEvent::MessageCompleted(format!(
            r#"@@team_message {{"to":"agy","body":"{explanation}"}}"#
        )),
    );

    assert!(route.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Route { from, to, body, .. }
            if from == &codex && to == &vec!["agy".to_string()] && body == explanation
    )));
    let dispatch = route
        .actions
        .iter()
        .find(|action| action.member == agy)
        .expect("Codex handoff must dispatch a turn to Agy");
    assert!(dispatch.prompt.contains(explanation));

    rt.on_agent_event(
        &agy,
        AgentEvent::MessageCompleted(
            "Understood; I will evaluate that frontend structure.".to_string(),
        ),
    );
    let replay = rt.store.replay_chat().unwrap();
    assert!(replay.iter().any(|item| matches!(
        item,
        ChatItem::Route { from, to, body }
            if from == &codex && to == &vec!["agy".to_string()] && body == explanation
    )));
    assert!(replay.iter().any(|item| matches!(
        item,
        ChatItem::Agent { member, backend: BackendKind::Agy, text, .. }
            if member == &agy && text.contains("evaluate that frontend structure")
    )));
}

#[test]
fn unknown_route_target_reports_error() {
    let mut rt = runtime();
    rt.on_ui_command(user("plan it"));
    let builder = MemberId::new("builder");

    let step = rt.on_agent_event(
        &builder,
        AgentEvent::MessageCompleted(r#"@@team_message {"to":"ghost","body":"hi"}"#.to_string()),
    );
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::RouteError { target, body, .. }
            if target == "ghost" && body == "hi"
    )));
    assert!(step.actions.is_empty());
}

#[test]
fn agent_can_add_teammate_with_team_member_envelope() {
    let mut rt = runtime();
    rt.on_ui_command(user("plan it"));
    let builder = MemberId::new("builder");

    let step = rt.on_agent_event(
            &builder,
            AgentEvent::MessageCompleted(
                r#"Need a QA specialist.
@@team_member {"id":"qa","display_name":"QA","backend":"codex","role":"tests","model":"gpt-5-codex","effort":"high"}"#
                    .to_string(),
            ),
        );

    assert!(rt.config.member(&MemberId::new("qa")).is_some());
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::MessageCompleted { text, .. } if text == "Need a QA specialist."
    )));
    assert!(step.events.iter().any(|event| matches!(
            event,
            RuntimeEvent::Ready { members, .. } if members.iter().any(|member| member.id == MemberId::new("qa"))
        )));
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Notice(text) if text.contains("builder added teammate qa")
    )));
    assert!(step.runner_changes.iter().any(|change| matches!(
        change,
        RunnerChange::Upsert { member, .. } if member.id == MemberId::new("qa")
            && member.system_prompt.as_deref().unwrap_or("").contains("$asterline-team")
    )));
    let persisted = step.persist_team.expect("team persisted");
    let qa = persisted.member(&MemberId::new("qa")).unwrap();
    assert_eq!(qa.model.as_deref(), Some("gpt-5-codex"));
    assert_eq!(qa.effort, Some(Effort::High));
    assert_eq!(qa.system_prompt, None);
}

#[test]
fn agent_teammate_addition_requires_relay_approval() {
    let mut rt = TeamRuntime::new(team(), SqliteStore::in_memory().unwrap());
    rt.on_ui_command(user("plan it"));
    let builder = MemberId::new("builder");

    let requested = rt.on_agent_event(
        &builder,
        AgentEvent::MessageCompleted(
            r#"@@team_member {"id":"qa","display_name":"QA","backend":"codex","role":"tests","sandbox":"danger-full-access"}"#
                .to_string(),
        ),
    );

    assert!(rt.config.member(&MemberId::new("qa")).is_none());
    assert!(requested.runner_changes.is_empty());
    let approval_id = requested
        .events
        .iter()
        .find_map(|event| match event {
            RuntimeEvent::ApprovalRequested {
                id,
                member: Some(from),
                action,
                ..
            } if from == &builder && action == "team_member" => Some(*id),
            _ => None,
        })
        .expect("teammate approval requested");

    let approved = rt.on_ui_command(UiCommand::Approve {
        id: approval_id,
        decision: ApprovalDecision::Approve,
    });
    assert!(rt.config.member(&MemberId::new("qa")).is_some());
    assert!(approved.runner_changes.iter().any(|change| matches!(
        change,
        RunnerChange::Upsert { member, .. } if member.id == MemberId::new("qa")
    )));
}

#[test]
fn agent_cannot_add_duplicate_teammate() {
    let mut rt = runtime();
    rt.on_ui_command(user("plan it"));
    let builder = MemberId::new("builder");

    let step = rt.on_agent_event(
        &builder,
        AgentEvent::MessageCompleted(
            r#"@@team_member {"id":"reviewer","backend":"codex","role":"tests"}"#.to_string(),
        ),
    );

    assert_eq!(rt.config.members.len(), 2);
    assert!(step.persist_team.is_none());
    assert!(step.runner_changes.is_empty());
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Notice(text) if text.contains("member already exists")
    )));
}

#[test]
fn second_message_to_busy_member_is_queued_then_runs() {
    let mut rt = runtime();
    let builder = MemberId::new("builder");
    rt.on_ui_command(UiCommand::UserMessage {
        target: MessageTarget::Member(builder.clone()),
        body: "first".to_string(),
    });
    let step = rt.on_ui_command(UiCommand::UserMessage {
        target: MessageTarget::Member(builder.clone()),
        body: "second".to_string(),
    });
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::MemberStatus {
            status: MemberStatus::Queued,
            ..
        }
    )));
    assert!(
        step.actions.is_empty(),
        "busy member does not start a second run"
    );

    // Finishing the first run starts the queued prompt.
    rt.on_agent_event(&builder, AgentEvent::MessageCompleted("a".to_string()));
    let step = rt.on_agent_event(
        &builder,
        AgentEvent::Exited {
            code: Some(0),
            ok: true,
        },
    );
    assert!(
        step.actions
            .iter()
            .any(|a| { a.prompt.contains("second") && a.prompt.contains("$asterline-team") })
    );
}

#[test]
fn relay_can_be_paused_by_user() {
    let mut rt = runtime();
    rt.on_ui_command(UiCommand::SetRelayPaused(true));
    rt.on_ui_command(user("plan"));
    let builder = MemberId::new("builder");

    let step = rt.on_agent_event(
        &builder,
        AgentEvent::MessageCompleted(
            r#"@@team_message {"to":"reviewer","body":"check"}"#.to_string(),
        ),
    );
    assert!(
        step.events
            .iter()
            .any(|e| matches!(e, RuntimeEvent::RoutePaused { .. }))
    );
    assert!(
        !step
            .actions
            .iter()
            .any(|a| a.member == MemberId::new("reviewer"))
    );

    // Resolving with resume delivers it.
    let step = rt.on_ui_command(UiCommand::ResolvePausedRoute { resume: true });
    assert!(
        step.events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::RouteQueueUpdated { queued: 0 }))
    );
    assert!(
        step.actions
            .iter()
            .any(|a| a.member == MemberId::new("reviewer"))
    );
}

#[test]
fn abort_finishes_a_turn_held_only_by_a_paused_route() {
    let mut rt = runtime();
    rt.on_ui_command(UiCommand::SetRelayPaused(true));
    rt.on_ui_command(user("plan"));
    let builder = MemberId::new("builder");

    rt.on_agent_event(
        &builder,
        AgentEvent::MessageCompleted(
            r#"@@team_message {"to":"reviewer","body":"check"}"#.to_string(),
        ),
    );
    let exited = rt.on_agent_event(
        &builder,
        AgentEvent::Exited {
            code: Some(0),
            ok: true,
        },
    );
    assert!(
        !exited
            .events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::TurnFinished { .. })),
        "the paused route must keep the turn open before abort"
    );

    let aborted = rt.on_ui_command(UiCommand::Cancel { member: None });
    assert!(
        aborted
            .events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::RouteQueueUpdated { queued: 0 }))
    );
    assert!(
        aborted
            .events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::TurnFinished { .. })),
        "dropping the paused route must finish its turn"
    );
    let new_chat = rt.on_ui_command(UiCommand::NewSession);
    assert!(
        new_chat
            .events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::SessionReset)),
        "a cancelled paused route must not permanently block /new"
    );
}

#[test]
fn relay_guard_pauses_after_limit() {
    let mut config = team();
    config.max_auto_relays = 1;
    let mut rt = TeamRuntime::new(config, SqliteStore::in_memory().unwrap()).with_approvals(false);
    rt.on_ui_command(user("go"));
    let builder = MemberId::new("builder");

    // First relay: delivered.
    let step = rt.on_agent_event(
        &builder,
        AgentEvent::MessageCompleted(r#"@@team_message {"to":"reviewer","body":"1"}"#.to_string()),
    );
    assert!(
        step.actions
            .iter()
            .any(|a| a.member == MemberId::new("reviewer"))
    );

    // Second relay from the same member in the same turn: paused.
    let step = rt.on_agent_event(
        &builder,
        AgentEvent::MessageCompleted(r#"@@team_message {"to":"reviewer","body":"2"}"#.to_string()),
    );
    assert!(
        step.events
            .iter()
            .any(|e| matches!(e, RuntimeEvent::RoutePaused { .. }))
    );
}

#[test]
fn session_discovered_is_persisted_and_emitted() {
    let mut rt = runtime();
    rt.on_ui_command(user("hi"));
    let builder = MemberId::new("builder");

    let step = rt.on_agent_event(
        &builder,
        AgentEvent::SessionDiscovered(AgentSessionId("thread-1".to_string())),
    );
    assert!(
        step.events
            .iter()
            .any(|e| matches!(e, RuntimeEvent::SessionUpdated { .. }))
    );
    assert_eq!(
        rt.store.session_for(&builder).unwrap(),
        Some(AgentSessionId("thread-1".to_string()))
    );
}

#[test]
fn configured_session_id_is_used_for_the_first_turn() {
    let mut config = team();
    config
        .members
        .iter_mut()
        .find(|member| member.id == MemberId::new("builder"))
        .unwrap()
        .session_id = Some("chosen-thread".to_string());
    let mut rt = TeamRuntime::new(config, SqliteStore::in_memory().unwrap()).with_approvals(false);

    let step = rt.on_ui_command(user("continue selected history"));
    assert_eq!(
        step.actions[0].session,
        Some(AgentSessionId("chosen-thread".to_string()))
    );
}

#[test]
fn fresh_session_is_created_once_then_pinned_for_later_turns() {
    let mut config = team();
    config
        .members
        .iter_mut()
        .find(|member| member.id == MemberId::new("builder"))
        .unwrap()
        .session_policy = SessionPolicy::Fresh;
    let mut rt = TeamRuntime::new(config, SqliteStore::in_memory().unwrap()).with_approvals(false);
    let builder = MemberId::new("builder");

    let first = rt.on_ui_command(user("first"));
    assert_eq!(first.actions[0].session, None);
    rt.on_agent_event(
        &builder,
        AgentEvent::SessionDiscovered(AgentSessionId("fresh-thread".to_string())),
    );
    rt.on_agent_event(
        &builder,
        AgentEvent::Exited {
            code: Some(0),
            ok: true,
        },
    );

    let second = rt.on_ui_command(user("second"));
    assert_eq!(
        second.actions[0].session,
        Some(AgentSessionId("fresh-thread".to_string()))
    );
}

#[test]
fn fresh_session_keeps_its_pinned_id_after_runtime_restart() {
    let mut config = team();
    config
        .members
        .iter_mut()
        .find(|member| member.id == MemberId::new("builder"))
        .unwrap()
        .session_policy = SessionPolicy::Fresh;
    let store = SqliteStore::in_memory().unwrap();
    store
        .upsert_session(
            &MemberId::new("builder"),
            BackendKind::Codex,
            &AgentSessionId("pinned-thread".to_string()),
        )
        .unwrap();

    let mut restarted = TeamRuntime::new(config, store).with_approvals(false);
    let step = restarted.on_ui_command(user("continue"));
    assert_eq!(
        step.actions[0].session,
        Some(AgentSessionId("pinned-thread".to_string()))
    );
}

#[test]
fn switching_to_fresh_discards_old_session_only_once() {
    let mut rt = runtime();
    let builder = MemberId::new("builder");
    rt.on_ui_command(user("resume this"));
    rt.on_agent_event(
        &builder,
        AgentEvent::SessionDiscovered(AgentSessionId("old-thread".to_string())),
    );
    rt.on_agent_event(
        &builder,
        AgentEvent::Exited {
            code: Some(0),
            ok: true,
        },
    );

    let mut updated = team();
    updated
        .members
        .iter_mut()
        .find(|member| member.id == builder)
        .unwrap()
        .session_policy = SessionPolicy::Fresh;
    rt.on_ui_command(UiCommand::ReplaceTeam {
        members: updated.members,
        default_target: updated.default_target,
    });

    assert_eq!(rt.store.session_for(&builder).unwrap(), None);
    let first_fresh = rt.on_ui_command(user("start fresh"));
    assert_eq!(first_fresh.actions[0].session, None);
}

#[test]
fn risky_request_is_gated_until_approved() {
    let mut rt = TeamRuntime::new(team(), SqliteStore::in_memory().unwrap()); // approvals on
    let step = rt.on_ui_command(user("run git push origin main"));

    let approval_id = step.events.iter().find_map(|e| match e {
        RuntimeEvent::ApprovalRequested { id, .. } => Some(*id),
        _ => None,
    });
    let id = approval_id.expect("approval requested");
    assert!(step.actions.is_empty(), "gated request does not run yet");

    let step = rt.on_ui_command(UiCommand::Approve {
        id,
        decision: ApprovalDecision::Approve,
    });
    assert!(
        step.actions
            .iter()
            .any(|a| a.member == MemberId::new("builder"))
    );
}

#[test]
fn restart_rejects_approvals_that_cannot_be_reconstructed() {
    let store = SqliteStore::in_memory().unwrap();
    let conversation = store.current_conversation().unwrap();
    store.set_conversation(conversation).unwrap();
    store
        .insert_approval(None, None, "git", "git push origin main")
        .unwrap();

    let mut rt = TeamRuntime::new(team(), store);
    assert!(rt.store.pending_approvals().unwrap().is_empty());
    assert!(rt.take_startup_events().iter().any(|event| matches!(
        event,
        RuntimeEvent::Notice(text)
            if text.contains("rejected 1 approval request")
                && text.contains("interrupted by restart")
    )));
}

#[test]
fn streaming_text_deltas_build_a_message() {
    let mut rt = runtime();
    rt.on_ui_command(user("hi"));
    let reviewer_unused = MemberId::new("builder");

    rt.on_agent_event(&reviewer_unused, AgentEvent::MessageStarted);
    let step = rt.on_agent_event(&reviewer_unused, AgentEvent::TextDelta("Hel".to_string()));
    assert!(
        step.events
            .iter()
            .any(|e| matches!(e, RuntimeEvent::MessageDelta { .. }))
    );
    rt.on_agent_event(&reviewer_unused, AgentEvent::TextDelta("lo".to_string()));
    let step = rt.on_agent_event(
        &reviewer_unused,
        AgentEvent::MessageCompleted("Hello".to_string()),
    );
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::MessageCompleted { text, .. } if text == "Hello"
    )));
}

#[test]
fn cancelled_run_is_not_reported_as_error() {
    let mut rt = runtime();
    rt.on_ui_command(user("build it"));
    let builder = MemberId::new("builder");

    rt.on_ui_command(UiCommand::Cancel {
        member: Some(builder.clone()),
    });
    let fatal = rt.on_agent_event(
        &builder,
        AgentEvent::Fatal("process ended while being killed".to_string()),
    );
    assert!(
        !fatal
            .events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::MemberError { .. })),
        "a cancellation-side transport error must remain diagnostic"
    );
    // The killed process exits unsuccessfully with no exit code.
    let step = rt.on_agent_event(
        &builder,
        AgentEvent::Exited {
            code: None,
            ok: false,
        },
    );

    assert!(
        !step
            .events
            .iter()
            .any(|e| matches!(e, RuntimeEvent::MemberError { .. })),
        "a cancelled run must not surface as an error"
    );
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::Notice(text) if text.contains("cancelled")
    )));
}

#[test]
fn fatal_backend_event_cannot_be_overridden_by_a_zero_exit() {
    let mut rt = runtime();
    let builder = MemberId::new("builder");
    let started = start_team(&mut rt, "build it");
    let run_id = find_run_id(&started);

    let fatal = rt.on_agent_event(
        &builder,
        AgentEvent::Fatal("backend rejected the turn".to_string()),
    );
    assert!(fatal.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::MemberError { message, .. }
            if message == "backend rejected the turn"
    )));

    let exited = rt.on_agent_event(
        &builder,
        AgentEvent::Exited {
            code: Some(0),
            ok: true,
        },
    );

    assert_eq!(rt.store.run(run_id).unwrap().status, RunStatus::Failed);
    assert!(!exited.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::MemberError { message, .. }
            if message.contains("exited without success")
    )));
}

#[test]
fn user_message_with_at_prefix_strips_prefix_for_agent_run() {
    let mut rt = runtime();
    let builder = MemberId::new("builder");
    let step = rt.on_ui_command(UiCommand::UserMessage {
        target: MessageTarget::Member(builder.clone()),
        body: "@builder nihao".to_string(),
    });

    assert_eq!(step.actions.len(), 1);
    assert_eq!(step.actions[0].member, builder);
    assert!(step.actions[0].prompt.contains("nihao"));
    assert!(!step.actions[0].prompt.contains("@builder"));
    assert!(step.actions[0].prompt.contains("$asterline-team"));
}

#[test]
fn codex_prompt_includes_current_team_cards() {
    let mut builder = TeamMember::new("builder", "Builder", BackendKind::Codex, "impl");
    builder.model = Some("gpt-5-codex".to_string());
    builder.effort = Some(Effort::High);
    let mut reviewer = TeamMember::new("reviewer", "Reviewer", BackendKind::Claude, "review");
    reviewer.model = Some("sonnet".to_string());
    reviewer.effort = Some(Effort::Medium);
    reviewer.cwd = Some(PathBuf::from("/tmp/review"));
    let mut config = TeamConfig::new("mixed", "/tmp/ws")
        .with_member(builder)
        .with_member(reviewer);
    config.default_target = Some(DefaultTarget::Member(MemberId::new("builder")));
    let mut rt = TeamRuntime::new(config, SqliteStore::in_memory().unwrap()).with_approvals(false);

    let step = rt.on_ui_command(user("who is on the team?"));
    let prompt = &step.actions[0].prompt;

    assert!(prompt.contains("Current Asterline team roster"));
    assert!(prompt.contains("available members only"));
    assert!(prompt.contains("do not message them unless collaboration is necessary"));
    assert!(prompt.contains("If routing is needed, use member ids"));
    assert!(prompt.contains("You are: id=builder"));
    assert!(prompt.contains("Default target: builder"));
    assert!(prompt.contains("id=builder display_name=\"Builder\" backend=codex role=\"impl\" status=running model=gpt-5-codex effort=high cwd=\"/tmp/ws\""));
    assert!(prompt.contains("id=reviewer display_name=\"Reviewer\" backend=claude role=\"review\" status=idle model=sonnet effort=medium cwd=\"/tmp/review\""));
}

#[test]
fn set_effort_updates_member_and_carries_into_runs() {
    let mut rt = runtime();
    let builder = MemberId::new("builder");

    let step = rt.on_ui_command(UiCommand::SetEffort {
        member: builder.clone(),
        effort: Effort::High,
    });
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::MemberEffort { effort, .. } if *effort == Effort::High
    )));

    let step = rt.on_ui_command(user("go"));
    assert_eq!(step.actions[0].effort, Some(Effort::High));
}

#[test]
fn set_effort_does_not_commit_memory_or_success_event_when_snapshot_fails() {
    let path = std::env::temp_dir().join(format!(
        "asterline-effort-snapshot-failure-{}.sqlite3",
        std::process::id()
    ));
    remove_sqlite_test_files(&path);
    let mut config = team();
    config.members[0].effort = Some(Effort::Low);
    let mut rt = TeamRuntime::new(config, SqliteStore::open(&path).unwrap()).with_approvals(false);
    let external = Connection::open(&path).unwrap();
    external
        .execute_batch(
            "CREATE TRIGGER fail_effort_snapshot
             BEFORE UPDATE OF team_json ON conversation_snapshots
             BEGIN SELECT RAISE(ABORT, 'snapshot unavailable'); END;",
        )
        .unwrap();

    let changed = rt.on_ui_command(UiCommand::SetEffort {
        member: MemberId::new("builder"),
        effort: Effort::High,
    });

    assert!(
        !changed
            .events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::MemberEffort { .. }))
    );
    assert!(changed.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Notice(text) if text.contains("could not save member effort")
    )));
    let run = rt.on_ui_command(user("use retained effort"));
    assert_eq!(run.actions[0].effort, Some(Effort::Low));
    drop(external);
    drop(rt);
    remove_sqlite_test_files(&path);
}

#[test]
fn set_effort_restores_from_active_snapshot_on_restart() {
    let path = std::env::temp_dir().join(format!(
        "asterline-effort-restart-{}.sqlite3",
        std::process::id()
    ));
    remove_sqlite_test_files(&path);
    let mut launch = team();
    launch.members[0].effort = Some(Effort::Low);
    {
        let mut rt = TeamRuntime::new(launch.clone(), SqliteStore::open(&path).unwrap())
            .with_approvals(false);
        rt.on_ui_command(UiCommand::SetEffort {
            member: MemberId::new("builder"),
            effort: Effort::High,
        });
    }

    let store = SqliteStore::open(&path).unwrap();
    let restored = store.restore_active_team_config(&launch).unwrap();
    let mut rt = TeamRuntime::new(restored, store).with_approvals(false);
    let run = rt.on_ui_command(user("use restored effort"));

    assert_eq!(run.actions[0].effort, Some(Effort::High));
    drop(rt);
    remove_sqlite_test_files(&path);
}

#[test]
fn replace_team_model_and_effort_carry_into_runner_and_run() {
    let mut rt = runtime();
    let mut members = team().members;
    let builder = members
        .iter_mut()
        .find(|member| member.id == MemberId::new("builder"))
        .unwrap();
    builder.model = Some("gpt-5.6-sol".to_string());
    builder.effort = Some(Effort::Xhigh);

    let replaced = rt.on_ui_command(UiCommand::ReplaceTeam {
        members,
        default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
    });

    assert!(replaced.runner_changes.iter().any(|change| matches!(
        change,
        RunnerChange::Upsert { member, .. }
            if member.id == MemberId::new("builder")
                && member.model.as_deref() == Some("gpt-5.6-sol")
                && member.effort == Some(Effort::Xhigh)
    )));
    let run = rt.on_ui_command(user("go"));
    assert_eq!(run.actions[0].effort, Some(Effort::Xhigh));
}

#[test]
fn set_effort_rejects_levels_the_backend_cannot_use() {
    let mut rt = runtime();
    let step = rt.on_ui_command(UiCommand::SetEffort {
        member: MemberId::new("reviewer"),
        effort: Effort::Ultra,
    });
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Notice(message) if message.contains("does not support ultra")
    )));
    assert!(
        !step
            .events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::MemberEffort { .. }))
    );
}

#[test]
fn replace_team_adds_member_and_requests_runner() {
    let mut rt = runtime();
    let mut members = team().members;
    let mut researcher = TeamMember::new("researcher", "Researcher", BackendKind::Agy, "research");
    researcher.model = Some("agy-pro".to_string());
    members.push(researcher);

    let step = rt.on_ui_command(UiCommand::ReplaceTeam {
        members,
        default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
    });

    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Ready { members, .. } if members.len() == 3
    )));
    assert!(step.runner_changes.iter().any(|change| matches!(
        change,
        RunnerChange::Upsert { member, .. } if member.id == MemberId::new("researcher")
            && member.system_prompt.as_deref().unwrap_or("").contains("$asterline-team")
    )));
    let persisted = step.persist_team.expect("team persisted");
    let researcher = persisted.member(&MemberId::new("researcher")).unwrap();
    assert_eq!(researcher.model.as_deref(), Some("agy-pro"));
    assert_eq!(researcher.system_prompt, None);
}

#[test]
fn replace_team_removes_idle_member_and_runner() {
    let mut rt = runtime();
    let members = vec![TeamMember::new(
        "builder",
        "Builder",
        BackendKind::Codex,
        "impl",
    )];

    let step = rt.on_ui_command(UiCommand::ReplaceTeam {
        members,
        default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
    });

    assert!(step.runner_changes.iter().any(|change| matches!(
        change,
        RunnerChange::Remove(member) if member == &MemberId::new("reviewer")
    )));
    assert!(rt.config.member(&MemberId::new("reviewer")).is_none());
}

#[test]
fn replace_team_keeps_runtime_unchanged_when_atomic_store_write_fails() {
    let path = std::env::temp_dir().join(format!(
        "asterline-team-atomic-failure-{}.sqlite3",
        std::process::id()
    ));
    remove_sqlite_test_files(&path);
    let store = SqliteStore::open(&path).unwrap();
    let mut rt = TeamRuntime::new(team(), store).with_approvals(false);
    let external = Connection::open(&path).unwrap();
    external
        .execute_batch(
            "CREATE TRIGGER fail_team_snapshot
             BEFORE UPDATE ON conversation_snapshots
             BEGIN SELECT RAISE(ABORT, 'snapshot unavailable'); END;",
        )
        .unwrap();
    let mut members = team().members;
    members.push(TeamMember::new(
        "researcher",
        "Researcher",
        BackendKind::Agy,
        "research",
    ));

    let failed = rt.on_ui_command(UiCommand::ReplaceTeam {
        members,
        default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
    });

    assert!(failed.runner_changes.is_empty());
    assert!(failed.persist_team.is_none());
    assert!(rt.config.member(&MemberId::new("researcher")).is_none());
    assert!(failed.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Notice(text) if text.contains("save the updated team atomically")
    )));
    drop(external);
    drop(rt);
    remove_sqlite_test_files(&path);
}

#[test]
fn replace_team_resolves_approval_held_for_removed_member() {
    let mut rt = TeamRuntime::new(team(), SqliteStore::in_memory().unwrap());
    let gated = rt.on_ui_command(UiCommand::UserMessage {
        target: MessageTarget::Member(MemberId::new("reviewer")),
        body: "run git status".to_string(),
    });
    let approval_id = gated
        .events
        .iter()
        .find_map(|event| match event {
            RuntimeEvent::ApprovalRequested { id, .. } => Some(*id),
            _ => None,
        })
        .expect("approval held for reviewer");

    let replaced = rt.on_ui_command(UiCommand::ReplaceTeam {
        members: vec![TeamMember::new(
            "builder",
            "Builder",
            BackendKind::Codex,
            "impl",
        )],
        default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
    });

    assert!(replaced.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::ApprovalResolved {
            id,
            decision: ApprovalDecision::Reject,
        } if *id == approval_id
    )));
    assert!(
        replaced
            .events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::TurnFinished { .. }))
    );
    assert!(rt.store.pending_approvals().unwrap().is_empty());
}

#[test]
fn replace_team_rejects_removing_active_member() {
    let mut rt = runtime();
    rt.on_ui_command(user("go"));
    let members = vec![TeamMember::new(
        "reviewer",
        "Reviewer",
        BackendKind::Claude,
        "review",
    )];

    let step = rt.on_ui_command(UiCommand::ReplaceTeam {
        members,
        default_target: Some(DefaultTarget::Member(MemberId::new("reviewer"))),
    });

    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Notice(text) if text.contains("cannot remove builder")
    )));
    assert!(step.runner_changes.is_empty());
    assert!(rt.config.member(&MemberId::new("builder")).is_some());
}

#[test]
fn replace_team_rejects_changing_an_active_member_backend() {
    let mut rt = runtime();
    rt.on_ui_command(user("go"));
    let mut members = team().members;
    let builder = members
        .iter_mut()
        .find(|member| member.id == MemberId::new("builder"))
        .unwrap();
    builder.backend = BackendKind::Claude;

    let step = rt.on_ui_command(UiCommand::ReplaceTeam {
        members,
        default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
    });

    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Notice(text) if text.contains("cannot update builder")
    )));
    assert!(step.runner_changes.is_empty());
    assert_eq!(
        rt.config.member(&MemberId::new("builder")).unwrap().backend,
        BackendKind::Codex
    );
}

#[test]
fn team_mode_kicks_off_via_a_coordinator() {
    let mut rt = runtime();
    let step = start_team(&mut rt, "ship the parser");

    let run = step
        .events
        .iter()
        .find_map(|e| match e {
            RuntimeEvent::RunUpdated { run } => Some(run),
            _ => None,
        })
        .expect("team run event");
    assert_eq!(run.status, RunStatus::Running);
    assert_eq!(
        run.mode.as_ref().map(|mode| mode.mode),
        Some(CollabMode::Team)
    );
    assert_eq!(run.mode.as_ref().map(|m| m.state.iteration), Some(1));
    assert_eq!(run.mode.as_ref().map(|m| m.state.max_iterations), Some(3));
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::UserMessage { body, .. }
            if body.starts_with("[team run-") && body.ends_with(": ship the parser")
    )));
    assert_eq!(step.actions.len(), 1);
    assert!(step.actions[0].prompt.contains("ship the parser"));
    assert!(step.actions[0].prompt.contains("$asterline-team"));
    assert!(
        step.actions[0].prompt.contains("@@run_step"),
        "start prompt must require checklist-first discipline"
    );
    assert!(!step.actions[0].prompt.contains("@@team_message"));
}

#[test]
fn team_run_complete_dispatches_auto_verify() {
    let dir = std::env::temp_dir().join(format!("asterline-team-verify-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();

    let mut rt = runtime_in_workspace(dir.clone());
    let builder = MemberId::new("builder");
    let step = start_team(&mut rt, "ship it");
    let run_id = find_run_id(&step);

    let step = complete_ok(&mut rt, &builder, "coordinated; all done");
    assert!(
        !step.verify_actions.is_empty(),
        "team finish should schedule verification"
    );
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::RunUpdated { run }
            if run.id == run_id && run.status == RunStatus::Verifying
    )));
    assert_eq!(
        rt.store
            .run(run_id)
            .unwrap()
            .mode
            .as_ref()
            .map(|m| m.state.phase.as_str()),
        Some("verifying")
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn team_verify_command_config_honored() {
    let mut rt = runtime();
    rt.config.modes.team = Some(crate::domain::mode::TeamModeConfig {
        verify_command: Some("just check".to_string()),
        ..crate::domain::mode::TeamModeConfig::default()
    });
    let builder = MemberId::new("builder");
    start_team(&mut rt, "use just");
    let step = complete_ok(&mut rt, &builder, "done");
    assert_eq!(
        step.verify_actions
            .iter()
            .map(|a| a.command.as_str())
            .collect::<Vec<_>>(),
        vec!["just check"]
    );
}

#[test]
fn team_verify_pass_marks_done() {
    let dir =
        std::env::temp_dir().join(format!("asterline-team-verify-pass-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();

    let mut rt = runtime_in_workspace(dir.clone());
    let builder = MemberId::new("builder");
    let step = start_team(&mut rt, "pass gate");
    let run_id = find_run_id(&step);
    let step = complete_ok(&mut rt, &builder, "done");
    let command = step.verify_actions[0].command.clone();
    let step = rt.on_verify_output(VerifyOutput {
        run_id,
        command,
        ok: true,
        stdout: b"ok".to_vec(),
        stderr: Vec::new(),
        start_error: None,
        cancelled: false,
    });
    assert!(step.actions.is_empty(), "pass must not re-dispatch");
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::RunUpdated { run }
            if run.id == run_id && run.status == RunStatus::Done
    )));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn team_verify_fail_auto_continues_coordinator() {
    let dir =
        std::env::temp_dir().join(format!("asterline-team-verify-fail-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();

    let mut rt = runtime_in_workspace(dir.clone());
    let builder = MemberId::new("builder");
    let step = start_team(&mut rt, "repair after fail");
    let run_id = find_run_id(&step);
    let step = complete_ok(&mut rt, &builder, "first pass");
    let command = step.verify_actions[0].command.clone();
    let step = rt.on_verify_output(VerifyOutput {
        run_id,
        command: command.clone(),
        ok: false,
        stdout: b"team gate failed: missing tests".to_vec(),
        stderr: Vec::new(),
        start_error: None,
        cancelled: false,
    });
    assert!(
        step.actions.iter().any(|a| {
            a.member == builder
                && a.prompt.contains("missing tests")
                && a.prompt.contains(&command)
                && a.prompt.contains("@@run_step")
                && a.prompt.contains("failed")
        }),
        "coordinator should auto-continue with failure + checklist: {:?}",
        step.actions.iter().map(|a| &a.prompt).collect::<Vec<_>>()
    );
    let run = rt.store.run(run_id).unwrap();
    assert_eq!(run.status, RunStatus::Running);
    assert_eq!(run.attempt, 2);
    assert_eq!(run.mode.as_ref().map(|m| m.state.iteration), Some(2));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn team_verify_fail_exhausted_stays_failed() {
    let dir = std::env::temp_dir().join(format!(
        "asterline-team-verify-exhausted-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();

    let mut rt = runtime_in_workspace(dir.clone());
    rt.config.modes.team = Some(crate::domain::mode::TeamModeConfig {
        max_iterations: Some(1),
        ..crate::domain::mode::TeamModeConfig::default()
    });
    let builder = MemberId::new("builder");
    let step = start_team(&mut rt, "no budget");
    let run_id = find_run_id(&step);
    let step = complete_ok(&mut rt, &builder, "done");
    let step = rt.on_verify_output(VerifyOutput {
        run_id,
        command: step.verify_actions[0].command.clone(),
        ok: false,
        stdout: b"boom".to_vec(),
        stderr: Vec::new(),
        start_error: None,
        cancelled: false,
    });
    assert!(step.actions.is_empty(), "exhausted must not re-dispatch");
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::Notice(text)
            if text.contains("after 1 attempts") && text.contains("team run failed")
    )));
    assert_eq!(rt.store.run(run_id).unwrap().status, RunStatus::Failed);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn team_auto_verify_false_marks_done_immediately() {
    let mut rt = runtime();
    rt.config.modes.team = Some(crate::domain::mode::TeamModeConfig {
        auto_verify: Some(false),
        ..crate::domain::mode::TeamModeConfig::default()
    });
    let builder = MemberId::new("builder");
    let step = start_team(&mut rt, "no verify");
    let run_id = find_run_id(&step);
    let step = complete_ok(&mut rt, &builder, "all done");
    assert!(
        step.verify_actions.is_empty(),
        "auto_verify false must not verify"
    );
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::RunUpdated { run }
            if run.id == run_id && run.status == RunStatus::Done
    )));
}

#[test]
fn team_verify_cancelled_no_auto_continue() {
    let dir = std::env::temp_dir().join(format!(
        "asterline-team-verify-cancel-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();

    let mut rt = runtime_in_workspace(dir.clone());
    let builder = MemberId::new("builder");
    let step = start_team(&mut rt, "cancel gate");
    let run_id = find_run_id(&step);
    let step = complete_ok(&mut rt, &builder, "done");
    let step = rt.on_verify_output(VerifyOutput {
        run_id,
        command: step.verify_actions[0].command.clone(),
        ok: false,
        stdout: Vec::new(),
        stderr: Vec::new(),
        start_error: None,
        cancelled: true,
    });
    assert!(
        step.actions.is_empty(),
        "cancelled verification must not auto-continue"
    );
    assert_eq!(rt.store.run(run_id).unwrap().status, RunStatus::Blocked);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn team_mode_uses_mode_approval_gate() {
    let mut rt = TeamRuntime::new(team(), SqliteStore::in_memory().unwrap());
    let step = start_team(&mut rt, "run `cargo test`");
    assert!(step.actions.is_empty());
    let approval = step.events.iter().find_map(|event| match event {
        RuntimeEvent::ApprovalRequested { id, .. } => Some(*id),
        _ => None,
    });
    let id = approval.expect("team dispatch should request approval");
    let step = rt.on_ui_command(UiCommand::Approve {
        id,
        decision: ApprovalDecision::Reject,
    });
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::RunUpdated { run }
            if run.status == RunStatus::Blocked
    )));
}

#[test]
fn abort_blocks_team_run_and_continue_resumes_it() {
    let mut rt = runtime();
    let step = start_team(&mut rt, "ship parser");
    let run_id = find_run_id(&step);

    rt.on_ui_command(UiCommand::Cancel { member: None });
    rt.on_agent_event(
        &MemberId::new("builder"),
        AgentEvent::Exited {
            code: None,
            ok: false,
        },
    );
    assert_eq!(rt.store.run(run_id).unwrap().status, RunStatus::Blocked);

    let step = rt.on_ui_command(UiCommand::ContinueRun {
        run_id: Some(run_id),
        note: Some("resume".to_string()),
    });
    assert!(
        step.actions
            .iter()
            .any(|action| action.member == MemberId::new("builder"))
    );
    let run = rt.store.run(run_id).unwrap();
    assert_eq!(run.status, RunStatus::Running);
    assert_eq!(run.attempt, 2);
}

#[test]
fn explicit_run_id_cannot_modify_another_conversation() {
    let mut rt = runtime();
    let old_run = rt.store.create_run("old chat", None).unwrap();
    let reset = rt.on_ui_command(UiCommand::NewSession);
    assert!(
        reset
            .events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::SessionReset))
    );

    let step = rt.on_ui_command(UiCommand::BlockRun {
        run_id: Some(old_run.id),
        reason: "must stay scoped".to_string(),
    });

    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Notice(message) if message.contains("was not found")
    )));
    assert_eq!(rt.store.run(old_run.id).unwrap().status, RunStatus::Running);
}

#[test]
fn checklist_rejects_an_owner_outside_the_roster() {
    let mut rt = runtime();
    let run = rt.store.create_run("scoped checklist", None).unwrap();

    let step = rt.on_ui_command(UiCommand::AddRunStep {
        run_id: Some(run.id),
        owner: Some(MemberId::new("ghost")),
        title: "cannot execute".to_string(),
    });

    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Notice(message) if message.contains("unknown step owner: ghost")
    )));
    assert!(rt.store.run(run.id).unwrap().steps.is_empty());
}

#[test]
fn second_team_run_while_active_is_refused() {
    let mut rt = runtime();
    start_team(&mut rt, "first");
    let step = start_team(&mut rt, "second");
    assert!(step.actions.is_empty());
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Notice(text) if text.contains("already active")
    )));
}

#[test]
fn run_marks_done_when_its_turn_finishes() {
    let mut rt = runtime();
    let step = start_team(&mut rt, "ship the parser");
    let run_id = step
        .events
        .iter()
        .find_map(|e| match e {
            RuntimeEvent::RunUpdated { run } => Some(run.id),
            _ => None,
        })
        .expect("run id");

    let step = rt.on_agent_event(
        &MemberId::new("builder"),
        AgentEvent::Exited {
            code: Some(0),
            ok: true,
        },
    );

    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::RunUpdated { run }
            if run.id == run_id && run.status == RunStatus::Done
    )));
    assert!(
        step.events
            .iter()
            .any(|e| matches!(e, RuntimeEvent::TurnFinished { .. }))
    );
}

#[test]
fn verify_run_records_successful_check() {
    let dir = std::env::temp_dir().join(format!("asterline-verify-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut rt = runtime_in_workspace(dir.clone());

    start_team(&mut rt, "ship the parser");
    complete_ok(&mut rt, &MemberId::new("builder"), "done");
    let step = rt.on_ui_command(UiCommand::VerifyRun {
        run_id: None,
        command: Some("printf verified".to_string()),
    });

    assert_eq!(step.verify_actions.len(), 1);
    let action = &step.verify_actions[0];
    assert_eq!(action.command, "printf verified");
    assert_eq!(action.workspace, dir);
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::RunUpdated { run }
            if run.status == RunStatus::Verifying
    )));

    let step = rt.on_verify_output(VerifyOutput {
        run_id: action.run_id,
        command: action.command.clone(),
        ok: true,
        stdout: b"verified".to_vec(),
        stderr: Vec::new(),
        start_error: None,
        cancelled: false,
    });
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::RunUpdated { run }
            if run.status == RunStatus::Done
                && run.verification.as_ref().is_some_and(|v| {
                    v.ok && v.command == "printf verified" && v.summary == "verified"
                })
    )));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn verify_run_can_target_an_older_run() {
    let dir = std::env::temp_dir().join(format!("asterline-verify-target-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut rt = runtime_in_workspace(dir.clone());

    let first = start_team(&mut rt, "ship parser");
    let first_id = first
        .events
        .iter()
        .find_map(|e| match e {
            RuntimeEvent::RunUpdated { run } => Some(run.id),
            _ => None,
        })
        .expect("first run id");
    complete_ok(&mut rt, &MemberId::new("builder"), "first done");
    let second = start_team(&mut rt, "refactor ui");
    let second_id = second
        .events
        .iter()
        .find_map(|e| match e {
            RuntimeEvent::RunUpdated { run } => Some(run.id),
            _ => None,
        })
        .expect("second run id");

    let verify = rt.on_ui_command(UiCommand::VerifyRun {
        run_id: Some(first_id),
        command: Some("printf first".to_string()),
    });

    assert_eq!(verify.verify_actions.len(), 1);
    assert_eq!(verify.verify_actions[0].run_id, first_id);
    assert_ne!(verify.verify_actions[0].run_id, second_id);
    assert!(verify.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::RunUpdated { run }
            if run.id == first_id && run.status == RunStatus::Verifying
    )));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn continue_run_resumes_failed_run() {
    let mut rt = runtime();
    let step = start_team(&mut rt, "ship the parser");
    let run_id = step
        .events
        .iter()
        .find_map(|event| match event {
            RuntimeEvent::RunUpdated { run } => Some(run.id),
            _ => None,
        })
        .expect("run id");
    rt.on_agent_event(
        &MemberId::new("builder"),
        AgentEvent::Exited {
            code: Some(0),
            ok: true,
        },
    );
    let verify = rt.on_ui_command(UiCommand::VerifyRun {
        run_id: Some(run_id),
        command: Some("cargo test".to_string()),
    });
    let action = &verify.verify_actions[0];
    rt.on_verify_output(VerifyOutput {
        run_id,
        command: action.command.clone(),
        ok: false,
        stdout: b"test failed".to_vec(),
        stderr: Vec::new(),
        start_error: None,
        cancelled: false,
    });

    let step = rt.on_ui_command(UiCommand::ContinueRun {
        run_id: Some(run_id),
        note: Some("fix verification".to_string()),
    });

    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::RunUpdated { run }
            if run.id == run_id && run.status == RunStatus::Running && run.attempt == 2
    )));
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::UserMessage { body, .. }
            if body == "/continue run-1 fix verification"
    )));
    assert_eq!(step.actions.len(), 1);
    assert!(
        step.actions[0]
            .prompt
            .contains("Previous verification: cargo test (failed)")
    );
    assert!(
        step.actions[0]
            .prompt
            .contains("User note: fix verification")
    );
    assert!(step.actions[0].prompt.contains("$asterline-team"));

    let step = rt.on_agent_event(
        &MemberId::new("builder"),
        AgentEvent::Exited {
            code: Some(0),
            ok: true,
        },
    );
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::RunUpdated { run }
            if run.id == run_id && run.status == RunStatus::Done
                && run.attempt == 2
            && run.verification.is_none()
    )));
}

#[test]
fn run_note_and_block_update_timeline() {
    let mut rt = runtime();
    let step = start_team(&mut rt, "ship the parser");
    let run_id = step
        .events
        .iter()
        .find_map(|event| match event {
            RuntimeEvent::RunUpdated { run } => Some(run.id),
            _ => None,
        })
        .expect("run id");

    let step = rt.on_ui_command(UiCommand::NoteRun {
        run_id: Some(run_id),
        note: "waiting for API docs".to_string(),
    });
    assert!(step.actions.is_empty());
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::RunUpdated { run }
            if run.id == run_id
                && run.status == RunStatus::Running
                && run.events.last().is_some_and(|event| {
                    event.kind == "note"
                        && event.detail.as_deref() == Some("waiting for API docs")
                })
    )));

    let step = rt.on_ui_command(UiCommand::BlockRun {
        run_id: Some(run_id),
        reason: "missing API token".to_string(),
    });
    assert!(step.actions.is_empty());
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::RunUpdated { run }
            if run.id == run_id
                && run.status == RunStatus::Blocked
                && run.events.last().is_some_and(|event| {
                    event.kind == "blocked"
                        && event.detail.as_deref() == Some("missing API token")
                })
    )));
}

#[test]
fn run_steps_update_checklist_without_running_agents() {
    let mut rt = runtime();
    let step = start_team(&mut rt, "ship the parser");
    let run_id = step
        .events
        .iter()
        .find_map(|event| match event {
            RuntimeEvent::RunUpdated { run } => Some(run.id),
            _ => None,
        })
        .expect("run id");

    let step = rt.on_ui_command(UiCommand::AddRunStep {
        run_id: Some(run_id),
        owner: None,
        title: "write parser tests".to_string(),
    });
    assert!(step.actions.is_empty());
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::RunUpdated { run }
            if run.id == run_id
                && run.steps.len() == 1
                && run.steps[0].status == RunStepStatus::Todo
                && run.steps[0].owner.is_none()
                && run.steps[0].title == "write parser tests"
    )));

    let step = rt.on_ui_command(UiCommand::AssignRunStep {
        run_id: Some(run_id),
        step: 1,
        owner: Some(MemberId::new("reviewer")),
    });
    assert!(step.actions.is_empty());
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::RunUpdated { run }
            if run.id == run_id
                && run.steps[0].owner == Some(MemberId::new("reviewer"))
                && run.events.last().is_some_and(|event| event.kind == "step_assigned")
    )));

    let step = rt.on_ui_command(UiCommand::UpdateRunStep {
        run_id: Some(run_id),
        step: 1,
        status: RunStepStatus::Done,
        note: Some("covered lexer edge cases".to_string()),
    });
    assert!(step.actions.is_empty());
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::RunUpdated { run }
            if run.id == run_id
                && run.steps[0].status == RunStepStatus::Done
                && run.steps[0].note.as_deref() == Some("covered lexer edge cases")
                && run.events.last().is_some_and(|event| event.kind == "step_updated")
    )));

    rt.on_ui_command(UiCommand::AddRunStep {
        run_id: Some(run_id),
        owner: None,
        title: "obsolete duplicate".to_string(),
    });
    let step = rt.on_ui_command(UiCommand::RenameRunStep {
        run_id: Some(run_id),
        step: 2,
        title: "document parser setup".to_string(),
    });
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::RunUpdated { run }
            if run.id == run_id
                && run.steps[1].title == "document parser setup"
                && run.events.last().is_some_and(|event| event.kind == "step_renamed")
    )));

    let step = rt.on_ui_command(UiCommand::RemoveRunStep {
        run_id: Some(run_id),
        step: 1,
    });
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::RunUpdated { run }
            if run.id == run_id
                && run.steps.len() == 1
                && run.steps[0].number == 1
                && run.steps[0].title == "document parser setup"
                && run.events.last().is_some_and(|event| event.kind == "step_removed")
    )));

    let step = rt.on_ui_command(UiCommand::AssignRunStep {
        run_id: Some(run_id),
        step: 1,
        owner: None,
    });
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::RunUpdated { run }
            if run.id == run_id && run.steps[0].owner.is_none()
    )));
}

#[test]
fn agent_run_step_envelope_updates_active_run_checklist() {
    let mut rt = runtime();
    let step = start_team(&mut rt, "ship the parser");
    let run_id = step
        .events
        .iter()
        .find_map(|event| match event {
            RuntimeEvent::RunUpdated { run } => Some(run.id),
            _ => None,
        })
        .expect("run id");

    let step = rt.on_agent_event(
        &MemberId::new("builder"),
        AgentEvent::MessageCompleted(
            r#"@@run_step {"action":"add","owner":"builder","title":"Write parser tests"}"#
                .to_string(),
        ),
    );
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::RunUpdated { run }
            if run.id == run_id
                && run.steps.len() == 1
                && run.steps[0].status == RunStepStatus::Todo
                && run.steps[0].owner == Some(MemberId::new("builder"))
                && run.steps[0].title == "Write parser tests"
    )));

    let step = rt.on_agent_event(
        &MemberId::new("builder"),
        AgentEvent::MessageCompleted(
            r#"@@run_step {"action":"assign","step":1,"owner":"reviewer"}"#.to_string(),
        ),
    );
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::RunUpdated { run }
            if run.id == run_id && run.steps[0].owner == Some(MemberId::new("reviewer"))
    )));

    let step = rt.on_agent_event(
        &MemberId::new("builder"),
        AgentEvent::MessageCompleted(
            r#"@@run_step {"action":"done","step":1,"note":"Covered edge cases"}"#.to_string(),
        ),
    );
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::RunUpdated { run }
            if run.id == run_id
                && run.steps[0].status == RunStepStatus::Done
                && run.steps[0].note.as_deref() == Some("Covered edge cases")
    )));

    let step = rt.on_agent_event(
        &MemberId::new("builder"),
        AgentEvent::MessageCompleted(
            r#"@@run_step {"action":"rename","step":1,"title":"Write parser coverage tests"}"#
                .to_string(),
        ),
    );
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::RunUpdated { run }
            if run.id == run_id && run.steps[0].title == "Write parser coverage tests"
    )));

    rt.on_agent_event(
        &MemberId::new("builder"),
        AgentEvent::MessageCompleted(
            r#"@@run_step {"action":"add","title":"Temporary duplicate"}"#.to_string(),
        ),
    );
    let step = rt.on_agent_event(
        &MemberId::new("builder"),
        AgentEvent::MessageCompleted(r#"@@run_step {"action":"remove","step":2}"#.to_string()),
    );
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::RunUpdated { run }
            if run.id == run_id
                && run.steps.len() == 1
                && run.steps[0].title == "Write parser coverage tests"
    )));
}

#[test]
fn agent_run_step_envelope_outside_a_run_is_ignored() {
    let mut rt = runtime();
    rt.on_ui_command(user("@builder hello"));

    let step = rt.on_agent_event(
        &MemberId::new("builder"),
        AgentEvent::MessageCompleted(
            r#"@@run_step {"action":"add","title":"Write parser tests"}"#.to_string(),
        ),
    );

    assert!(
        !step
            .events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::RunUpdated { .. }))
    );
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Notice(text)
            if text.contains("ignored run step update: no active run")
    )));
}

#[test]
fn failed_verification_remains_failed() {
    let dir = std::env::temp_dir().join(format!("asterline-verify-fail-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut rt = runtime_in_workspace(dir.clone());

    let step = start_team(&mut rt, "ship the parser");
    let run_id = step
        .events
        .iter()
        .find_map(|e| match e {
            RuntimeEvent::RunUpdated { run } => Some(run.id),
            _ => None,
        })
        .expect("run id");
    complete_ok(&mut rt, &MemberId::new("builder"), "done");
    let verify = rt.on_ui_command(UiCommand::VerifyRun {
        run_id: None,
        command: Some("printf nope; exit 2".to_string()),
    });
    let action = &verify.verify_actions[0];
    rt.on_verify_output(VerifyOutput {
        run_id: action.run_id,
        command: action.command.clone(),
        ok: false,
        stdout: b"nope".to_vec(),
        stderr: Vec::new(),
        start_error: None,
        cancelled: false,
    });

    let step = rt.on_agent_event(
        &MemberId::new("builder"),
        AgentEvent::Exited {
            code: Some(0),
            ok: true,
        },
    );

    assert!(!step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::RunUpdated { run }
            if run.id == run_id && run.status == RunStatus::Done
    )));
    assert_eq!(
        rt.store.latest_run().unwrap().unwrap().status,
        RunStatus::Failed
    );

    std::fs::remove_dir_all(&dir).ok();
}

fn two_member_team() -> TeamConfig {
    let mut config = TeamConfig::new("ab", "/tmp/ws")
        .with_member(TeamMember::new("a", "A", BackendKind::Codex, "impl"))
        .with_member(TeamMember::new("b", "B", BackendKind::Claude, "review"));
    config.default_target = Some(DefaultTarget::Member(MemberId::new("a")));
    config
}

/// Drive a non-risky user message to member A, then have A emit a team_message.
fn relay_after_user(rt: &mut TeamRuntime, body: &str) -> RuntimeStep {
    let a = MemberId::new("a");
    rt.on_ui_command(UiCommand::UserMessage {
        target: MessageTarget::Member(a.clone()),
        body: "please coordinate with b".to_string(),
    });
    rt.on_agent_event(
        &a,
        AgentEvent::MessageCompleted(format!(r#"@@team_message {{"to":"b","body":"{body}"}}"#)),
    )
}

#[test]
fn relay_with_risky_body_requires_approval() {
    let mut rt = TeamRuntime::new(two_member_team(), SqliteStore::in_memory().unwrap());
    let step = relay_after_user(&mut rt, "please run git status");

    assert!(
        step.events.iter().any(|e| matches!(
            e,
            RuntimeEvent::ApprovalRequested {
                member: Some(m),
                action,
                body,
                ..
            } if m.as_str() == "a" && action == "git" && body == "please run git status"
        )),
        "risky relay must request approval from the sender: {step_events:?}",
        step_events = step.events
    );
    assert!(
        !step.actions.iter().any(|a| a.member == MemberId::new("b")),
        "no RunAction for b while approval is held"
    );
}

#[test]
fn approved_relay_dispatches_wrapped_prompt() {
    let mut rt = TeamRuntime::new(two_member_team(), SqliteStore::in_memory().unwrap());
    let step = relay_after_user(&mut rt, "please run git status");
    let id = step
        .events
        .iter()
        .find_map(|e| match e {
            RuntimeEvent::ApprovalRequested { id, .. } => Some(*id),
            _ => None,
        })
        .expect("approval requested");

    let step = rt.on_ui_command(UiCommand::Approve {
        id,
        decision: ApprovalDecision::Approve,
    });
    let action = step
        .actions
        .iter()
        .find(|a| a.member == MemberId::new("b"))
        .expect("approved relay must dispatch to b");
    assert!(
        action.prompt.starts_with("[relay from"),
        "prompt should be relay-wrapped: {}",
        action.prompt
    );
    assert!(
        action.prompt.contains("please run git status"),
        "prompt should contain original body: {}",
        action.prompt
    );
    assert!(
        action
            .prompt
            .contains(r#"@@team_message {"to":"a","kind":"reply""#),
        "ordinary relays must require a reply to their sender: {}",
        action.prompt
    );
}

#[test]
fn reply_relay_does_not_require_an_acknowledgement_loop() {
    let mut rt = TeamRuntime::new(two_member_team(), SqliteStore::in_memory().unwrap())
        .with_approvals(false);
    let a = MemberId::new("a");
    let b = MemberId::new("b");
    rt.on_ui_command(UiCommand::UserMessage {
        target: MessageTarget::Member(a.clone()),
        body: "coordinate with b".to_string(),
    });

    let step = rt.on_agent_event(
        &a,
        AgentEvent::MessageCompleted(
            r#"@@team_message {"to":"b","kind":"reply","body":"done"}"#.to_string(),
        ),
    );
    let action = step
        .actions
        .iter()
        .find(|action| action.member == b)
        .expect("reply should still be delivered");

    assert!(action.prompt.contains("marked as a reply"));
    assert!(!action.prompt.contains("MUST answer the sender"));
}

#[test]
fn rejected_relay_finishes_turn() {
    let mut rt = TeamRuntime::new(two_member_team(), SqliteStore::in_memory().unwrap());
    let a = MemberId::new("a");
    let step = relay_after_user(&mut rt, "please run git status");
    let id = step
        .events
        .iter()
        .find_map(|e| match e {
            RuntimeEvent::ApprovalRequested { id, .. } => Some(*id),
            _ => None,
        })
        .expect("approval requested");

    // A finishes so the held approval is the only thing keeping the turn alive.
    rt.on_agent_event(
        &a,
        AgentEvent::Exited {
            code: Some(0),
            ok: true,
        },
    );

    let step = rt.on_ui_command(UiCommand::Approve {
        id,
        decision: ApprovalDecision::Reject,
    });
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::Notice(text) if text.contains("request rejected")
    )));
    assert!(
        step.events
            .iter()
            .any(|e| matches!(e, RuntimeEvent::TurnFinished { .. })),
        "rejecting the only pending work should finish the turn: {:?}",
        step.events
    );
    assert!(
        !step.actions.iter().any(|a| a.member == MemberId::new("b")),
        "reject must not dispatch to b"
    );
}

#[test]
fn relay_gate_respects_apply_to() {
    let mut config = two_member_team();
    config.approvals.apply_to = Some(vec![ApprovalSurface::User]);
    let mut rt = TeamRuntime::new(config, SqliteStore::in_memory().unwrap());

    let step = relay_after_user(&mut rt, "please run git status");
    assert!(
        !step
            .events
            .iter()
            .any(|e| matches!(e, RuntimeEvent::ApprovalRequested { .. })),
        "relay surface not in apply_to must skip the gate"
    );
    assert!(
        step.actions.iter().any(|a| a.member == MemberId::new("b")),
        "risky relay must dispatch immediately when only User is gated"
    );
}

#[test]
fn custom_keyword_category_gates_user_message() {
    let mut config = two_member_team();
    config
        .approvals
        .keywords
        .insert("deploy".to_string(), vec!["kubectl".to_string()]);
    let mut rt = TeamRuntime::new(config, SqliteStore::in_memory().unwrap());

    let step = rt.on_ui_command(UiCommand::UserMessage {
        target: MessageTarget::Member(MemberId::new("a")),
        body: "kubectl apply now".to_string(),
    });
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::ApprovalRequested { action, body, .. }
            if action == "deploy" && body == "kubectl apply now"
    )));
    assert!(step.actions.is_empty(), "custom keyword must gate the run");
}

#[test]
fn debug_mode_disables_all_gates() {
    let mut rt = TeamRuntime::new(two_member_team(), SqliteStore::in_memory().unwrap())
        .with_approvals(false);

    let step = rt.on_ui_command(UiCommand::UserMessage {
        target: MessageTarget::Member(MemberId::new("a")),
        body: "run git push origin main".to_string(),
    });
    assert!(
        !step
            .events
            .iter()
            .any(|e| matches!(e, RuntimeEvent::ApprovalRequested { .. }))
    );
    assert!(
        step.actions.iter().any(|a| a.member == MemberId::new("a")),
        "risky user message must dispatch when approvals are disabled"
    );

    let step = rt.on_agent_event(
        &MemberId::new("a"),
        AgentEvent::MessageCompleted(
            r#"@@team_message {"to":"b","body":"please run git status"}"#.to_string(),
        ),
    );
    assert!(
        !step
            .events
            .iter()
            .any(|e| matches!(e, RuntimeEvent::ApprovalRequested { .. }))
    );
    assert!(
        step.actions.iter().any(|a| a.member == MemberId::new("b")),
        "risky relay must dispatch when approvals are disabled"
    );
}

// --- Review, plan, and brainstorm modes ------------------------------------

use crate::domain::mode::CollabMode;
use crate::runtime::mode_prompts::{
    BRAINSTORM_BUILD_HINT, BRAINSTORM_PROPOSE_HINT, BRAINSTORM_STRETCH_HINT,
    BRAINSTORM_SYNTHESIS_HINT, BRAINSTORM_VOTE_HINT, PLAN_MODE_HINT, REVIEW_PROTOCOL_HINT,
};

fn run_mode(task: &str) -> UiCommand {
    UiCommand::RunMode {
        mode: CollabMode::Review,
        task: task.to_string(),
    }
}

fn complete_ok(rt: &mut TeamRuntime, member: &MemberId, text: &str) -> RuntimeStep {
    let mut step = rt.on_agent_event(member, AgentEvent::MessageCompleted(text.to_string()));
    let exit = rt.on_agent_event(
        member,
        AgentEvent::Exited {
            code: Some(0),
            ok: true,
        },
    );
    // Merge so callers can assert on envelopes recorded at MessageCompleted
    // and transitions that fire on Exited (TurnFinished / mode dispatch).
    step.events.extend(exit.events);
    step.actions.extend(exit.actions);
    step.verify_actions.extend(exit.verify_actions);
    step.runner_changes.extend(exit.runner_changes);
    if exit.persist_team.is_some() {
        step.persist_team = exit.persist_team;
    }
    step
}

fn latest_run(rt: &TeamRuntime) -> RunSummary {
    rt.store.latest_run().unwrap().expect("run exists")
}

fn find_run_id(step: &RuntimeStep) -> RunId {
    step.events
        .iter()
        .find_map(|e| match e {
            RuntimeEvent::RunUpdated { run } => Some(run.id),
            _ => None,
        })
        .expect("run id")
}

#[test]
fn review_approve_flow_completes_run() {
    let mut rt = runtime();
    let builder = MemberId::new("builder");
    let reviewer = MemberId::new("reviewer");

    let step = rt.on_ui_command(run_mode("add unit tests"));
    assert!(
        step.actions
            .iter()
            .any(|a| a.member == builder && a.prompt.contains("add unit tests")),
        "builder should receive the task: {:?}",
        step.actions.iter().map(|a| &a.prompt).collect::<Vec<_>>()
    );
    let run_id = find_run_id(&step);

    let step = complete_ok(&mut rt, &builder, "implemented the tests");
    assert!(
        step.actions.iter().any(|a| {
            a.member == reviewer
                && a.prompt.contains(REVIEW_PROTOCOL_HINT)
                && a.prompt.contains("implemented the tests")
        }),
        "reviewer prompt should include protocol and builder output"
    );

    let step = complete_ok(
        &mut rt,
        &reviewer,
        "Looks good.\n@@review {\"verdict\":\"approve\",\"summary\":\"solid work\"}",
    );
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::Verdict {
            approve: true,
            summary,
            ..
        } if summary == "solid work"
    )));
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::RunUpdated { run }
            if run.id == run_id && run.status == RunStatus::Done
    )));
    assert!(
        step.verify_actions.is_empty(),
        "no verify file in workspace → no VerifyAction"
    );

    // Session freed: a second RunMode succeeds.
    let step = rt.on_ui_command(run_mode("another task"));
    assert!(
        step.actions.iter().any(|a| a.member == builder),
        "second review should start: {:?}",
        step.events
    );
    assert!(
        !step
            .events
            .iter()
            .any(|e| matches!(e, RuntimeEvent::Notice(t) if t.contains("already active")))
    );
}

#[test]
fn review_verdict_is_durable_before_memory_or_fsm_advances() {
    let path = std::env::temp_dir().join(format!(
        "asterline-verdict-atomic-failure-{}.sqlite3",
        std::process::id()
    ));
    remove_sqlite_test_files(&path);
    let mut rt = TeamRuntime::new(team(), SqliteStore::open(&path).unwrap()).with_approvals(false);
    let external = Connection::open(&path).unwrap();
    let builder = MemberId::new("builder");
    let reviewer = MemberId::new("reviewer");
    let started = rt.on_ui_command(run_mode("persist verdict atomically"));
    let run_id = find_run_id(&started);
    complete_ok(&mut rt, &builder, "implementation done");
    external
        .execute_batch(
            "CREATE TRIGGER fail_verdict_state
             BEFORE UPDATE OF mode_state ON runs
             BEGIN SELECT RAISE(ABORT, 'mode state unavailable'); END;",
        )
        .unwrap();

    let completed = rt.on_agent_event(
        &reviewer,
        AgentEvent::MessageCompleted(
            "@@review {\"verdict\":\"approve\",\"summary\":\"looks good\"}".to_string(),
        ),
    );

    assert!(
        !completed
            .events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::Verdict { .. }))
    );
    assert!(completed.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Notice(text) if text.contains("could not save a review verdict")
    )));
    assert!(rt.mode_sessions[&run_id].pending_verdict.is_none());
    let stored: serde_json::Value =
        serde_json::from_str(&rt.store.run_mode_state(run_id).unwrap().unwrap()).unwrap();
    assert!(stored.get("pending_verdict").is_none());
    let (messages, events): (i64, i64) = external
        .query_row(
            "SELECT
                 (SELECT COUNT(*) FROM messages WHERE kind = 'verdict'),
                 (SELECT COUNT(*) FROM run_events WHERE run_id = ?1 AND kind = 'verdict')",
            [run_id.0 as i64],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((messages, events), (0, 0));

    let exited = rt.on_agent_event(
        &reviewer,
        AgentEvent::Exited {
            code: Some(0),
            ok: true,
        },
    );
    assert!(exited.verify_actions.is_empty());
    assert!(exited.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::RunUpdated { run }
            if run.id == run_id && run.status == RunStatus::Blocked
    )));
    drop(external);
    drop(rt);
    remove_sqlite_test_files(&path);
}

#[test]
fn persisted_pending_verdict_survives_restart_and_continue() {
    let path = std::env::temp_dir().join(format!(
        "asterline-verdict-restart-{}.sqlite3",
        std::process::id()
    ));
    remove_sqlite_test_files(&path);
    let run_id = {
        let mut rt =
            TeamRuntime::new(team(), SqliteStore::open(&path).unwrap()).with_approvals(false);
        let builder = MemberId::new("builder");
        let reviewer = MemberId::new("reviewer");
        let started = rt.on_ui_command(run_mode("resume durable verdict"));
        let run_id = find_run_id(&started);
        complete_ok(&mut rt, &builder, "implementation done");

        let completed = rt.on_agent_event(
            &reviewer,
            AgentEvent::MessageCompleted(
                "@@review {\"verdict\":\"approve\",\"summary\":\"durable\"}".to_string(),
            ),
        );
        assert!(completed.events.iter().any(|event| matches!(
            event,
            RuntimeEvent::Verdict { run, approve: true, .. } if *run == run_id
        )));
        let state: serde_json::Value =
            serde_json::from_str(&rt.store.run_mode_state(run_id).unwrap().unwrap()).unwrap();
        assert_eq!(state["pending_verdict"]["verdict"], "approve");
        assert_eq!(state["pending_verdict"]["summary"], "durable");
        run_id
    };

    let mut resumed =
        TeamRuntime::new(team(), SqliteStore::open(&path).unwrap()).with_approvals(false);
    assert_eq!(
        resumed.store.run(run_id).unwrap().status,
        RunStatus::Blocked,
        "startup reconciliation should make interrupted work explicit"
    );
    let continued = resumed.on_ui_command(UiCommand::ContinueRun {
        run_id: Some(run_id),
        note: Some("resume accepted verdict".to_string()),
    });
    assert!(continued.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::RunUpdated { run }
            if run.id == run_id && run.status == RunStatus::Done
    )));
    assert_eq!(resumed.store.run(run_id).unwrap().status, RunStatus::Done);
    drop(resumed);
    remove_sqlite_test_files(&path);
}

#[test]
fn mode_transition_does_not_dispatch_when_state_persistence_fails() {
    let path = std::env::temp_dir().join(format!(
        "asterline-mode-state-failure-{}.sqlite3",
        std::process::id()
    ));
    remove_sqlite_test_files(&path);
    let mut rt = TeamRuntime::new(team(), SqliteStore::open(&path).unwrap()).with_approvals(false);
    let external = Connection::open(&path).unwrap();
    let builder = MemberId::new("builder");
    let reviewer = MemberId::new("reviewer");
    let started = rt.on_ui_command(run_mode("persist every transition"));
    let run_id = find_run_id(&started);
    external
        .execute_batch(
            "CREATE TRIGGER fail_mode_state
             BEFORE UPDATE OF mode_state ON runs
             BEGIN SELECT RAISE(ABORT, 'mode state unavailable'); END;",
        )
        .unwrap();

    let completed = complete_ok(&mut rt, &builder, "implementation done");

    assert!(
        !completed
            .actions
            .iter()
            .any(|action| action.member == reviewer),
        "review dispatch must wait for durable mode state"
    );
    assert!(completed.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Notice(text) if text.contains("could not save mode state")
    )));
    assert_eq!(
        rt.store.run(run_id).unwrap().mode.unwrap().state.phase,
        "building"
    );
    drop(external);
    drop(rt);
    remove_sqlite_test_files(&path);
}

#[test]
fn manual_verification_is_not_dispatched_when_status_write_fails() {
    let path = std::env::temp_dir().join(format!(
        "asterline-manual-verify-failure-{}.sqlite3",
        std::process::id()
    ));
    remove_sqlite_test_files(&path);
    let mut rt = TeamRuntime::new(team(), SqliteStore::open(&path).unwrap()).with_approvals(false);
    let external = Connection::open(&path).unwrap();
    let run = rt.store.create_run("verify durably", None).unwrap();
    external
        .execute_batch(
            "CREATE TRIGGER fail_verifying_event
             BEFORE INSERT ON run_events
             WHEN NEW.kind = 'verifying'
             BEGIN SELECT RAISE(ABORT, 'verification event unavailable'); END;",
        )
        .unwrap();

    let verify = rt.on_ui_command(UiCommand::VerifyRun {
        run_id: Some(run.id),
        command: Some("true".to_string()),
    });

    assert!(verify.verify_actions.is_empty());
    assert!(verify.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Notice(text) if text.contains("could not start verification")
    )));
    assert_eq!(rt.store.run(run.id).unwrap().status, RunStatus::Running);
    drop(external);
    drop(rt);
    remove_sqlite_test_files(&path);
}

#[test]
fn review_auto_verify_runs_on_approve() {
    let dir = std::env::temp_dir().join(format!("asterline-review-verify-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();

    let mut rt = runtime_in_workspace(dir.clone());
    let builder = MemberId::new("builder");
    let reviewer = MemberId::new("reviewer");

    rt.on_ui_command(run_mode("ship it"));
    complete_ok(&mut rt, &builder, "done");
    let step = complete_ok(
        &mut rt,
        &reviewer,
        "@@review {\"verdict\":\"approve\",\"summary\":\"ok\"}",
    );
    assert!(
        !step.verify_actions.is_empty(),
        "approve with Cargo.toml should schedule verification"
    );
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::RunUpdated { run }
            if run.status == RunStatus::Verifying
    )));
    let run_id = step.verify_actions[0].run_id;

    let step = rt.on_verify_output(VerifyOutput {
        run_id,
        command: step.verify_actions[0].command.clone(),
        ok: true,
        stdout: b"ok".to_vec(),
        stderr: Vec::new(),
        start_error: None,
        cancelled: false,
    });
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::RunUpdated { run }
            if run.id == run_id && run.status == RunStatus::Done
    )));

    // Session freed.
    let step = rt.on_ui_command(run_mode("next"));
    assert!(step.actions.iter().any(|a| a.member == builder));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn verification_result_failure_keeps_the_mode_session_owned() {
    let dir = std::env::temp_dir().join(format!(
        "asterline-review-verify-store-failure-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    let path = dir.join("state.sqlite3");
    let mut config = team();
    config.workspace = dir.clone();
    let mut rt = TeamRuntime::new(config, SqliteStore::open(&path).unwrap()).with_approvals(false);
    let external = Connection::open(&path).unwrap();
    let builder = MemberId::new("builder");
    let reviewer = MemberId::new("reviewer");

    rt.on_ui_command(run_mode("finish durably"));
    complete_ok(&mut rt, &builder, "done");
    let approve = complete_ok(
        &mut rt,
        &reviewer,
        "@@review {\"verdict\":\"approve\",\"summary\":\"ok\"}",
    );
    let verify = &approve.verify_actions[0];
    let run_id = verify.run_id;
    let command = verify.command.clone();
    external
        .execute_batch(
            "CREATE TRIGGER fail_verification_done
             BEFORE UPDATE OF status ON runs
             WHEN NEW.status = 'done'
             BEGIN SELECT RAISE(ABORT, 'done unavailable'); END;",
        )
        .unwrap();

    let step = rt.on_verify_output(VerifyOutput {
        run_id,
        command,
        ok: true,
        stdout: b"ok".to_vec(),
        stderr: Vec::new(),
        start_error: None,
        cancelled: false,
    });

    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Notice(text) if text.contains("could not save verification result")
    )));
    assert!(rt.mode_sessions.contains_key(&run_id));
    assert_eq!(rt.store.run(run_id).unwrap().status, RunStatus::Verifying);
    let second = rt.on_ui_command(run_mode("must wait"));
    assert!(second.actions.is_empty());
    assert!(second.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Notice(text) if text.contains("already active")
    )));

    drop(external);
    drop(rt);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn block_failure_keeps_the_mode_session_owned() {
    let path = std::env::temp_dir().join(format!(
        "asterline-mode-block-failure-{}.sqlite3",
        std::process::id()
    ));
    remove_sqlite_test_files(&path);
    let mut rt = TeamRuntime::new(team(), SqliteStore::open(&path).unwrap()).with_approvals(false);
    let external = Connection::open(&path).unwrap();
    let started = rt.on_ui_command(run_mode("do not lose ownership"));
    let run_id = find_run_id(&started);
    external
        .execute_batch(
            "CREATE TRIGGER fail_run_block
             BEFORE UPDATE OF status ON runs
             WHEN NEW.status = 'blocked'
             BEGIN SELECT RAISE(ABORT, 'block unavailable'); END;",
        )
        .unwrap();

    let step = rt.on_ui_command(UiCommand::Cancel { member: None });

    assert!(rt.mode_sessions.contains_key(&run_id));
    assert!(!rt.failed_runs.contains(&run_id));
    assert_eq!(rt.store.run(run_id).unwrap().status, RunStatus::Running);
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Notice(text) if text.contains("could not block mode run")
    )));
    drop(external);
    drop(rt);
    remove_sqlite_test_files(&path);
}

#[test]
fn plain_run_finish_failure_retains_turn_ownership() {
    let path = std::env::temp_dir().join(format!(
        "asterline-run-finish-failure-{}.sqlite3",
        std::process::id()
    ));
    remove_sqlite_test_files(&path);
    let mut config = team();
    config.modes.team = Some(TeamModeConfig {
        auto_verify: Some(false),
        ..TeamModeConfig::default()
    });
    let mut rt = TeamRuntime::new(config, SqliteStore::open(&path).unwrap()).with_approvals(false);
    let external = Connection::open(&path).unwrap();
    let started = start_team(&mut rt, "finish atomically");
    let run_id = find_run_id(&started);
    let builder = MemberId::new("builder");
    rt.on_agent_event(&builder, AgentEvent::MessageCompleted("done".to_string()));
    let queued_turn = rt.store.create_turn().unwrap();
    rt.enqueue_prompt(
        &builder,
        queued_turn,
        "queued after completion".to_string(),
        &mut RuntimeStep::default(),
    );
    assert_eq!(rt.members[&builder].queue.len(), 1);
    external
        .execute_batch(
            "CREATE TRIGGER fail_run_done
             BEFORE UPDATE OF status ON runs
             WHEN NEW.status = 'done'
             BEGIN SELECT RAISE(ABORT, 'done unavailable'); END;",
        )
        .unwrap();

    let step = rt.on_agent_event(
        &builder,
        AgentEvent::Exited {
            code: Some(0),
            ok: true,
        },
    );

    assert!(step.actions.is_empty());
    assert!(
        !step
            .events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::TurnFinished { .. }))
    );
    assert!(rt.run_turns.values().any(|id| *id == run_id));
    assert_eq!(rt.members[&builder].queue.len(), 1);
    assert_eq!(rt.store.run(run_id).unwrap().status, RunStatus::Running);
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Notice(text) if text.contains("could not finish the run")
    )));
    drop(external);
    drop(rt);
    remove_sqlite_test_files(&path);
}

#[test]
fn failed_run_status_failure_does_not_finish_or_start_queued_work() {
    let path = std::env::temp_dir().join(format!(
        "asterline-run-failed-status-failure-{}.sqlite3",
        std::process::id()
    ));
    remove_sqlite_test_files(&path);
    let mut config = team();
    config.modes.team = Some(TeamModeConfig {
        auto_verify: Some(false),
        ..TeamModeConfig::default()
    });
    let mut rt = TeamRuntime::new(config, SqliteStore::open(&path).unwrap()).with_approvals(false);
    let external = Connection::open(&path).unwrap();
    let started = start_team(&mut rt, "persist failure status");
    let run_id = find_run_id(&started);
    let builder = MemberId::new("builder");
    let queued_turn = rt.store.create_turn().unwrap();
    rt.enqueue_prompt(
        &builder,
        queued_turn,
        "queued work".to_string(),
        &mut RuntimeStep::default(),
    );
    assert_eq!(rt.members[&builder].queue.len(), 1);
    rt.on_agent_event(&builder, AgentEvent::Fatal("backend failed".to_string()));
    external
        .execute_batch(
            "CREATE TRIGGER fail_run_failed
             BEFORE UPDATE OF status ON runs
             WHEN NEW.status = 'failed'
             BEGIN SELECT RAISE(ABORT, 'failed unavailable'); END;",
        )
        .unwrap();

    let step = rt.on_agent_event(
        &builder,
        AgentEvent::Exited {
            code: Some(1),
            ok: false,
        },
    );

    assert!(step.actions.is_empty());
    assert!(
        !step
            .events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::TurnFinished { .. }))
    );
    assert!(rt.run_turns.values().any(|id| *id == run_id));
    assert_eq!(rt.members[&builder].queue.len(), 1);
    assert_eq!(rt.store.run(run_id).unwrap().status, RunStatus::Running);
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Notice(text) if text.contains("could not save a run status")
    )));
    drop(external);
    drop(rt);
    remove_sqlite_test_files(&path);
}

#[test]
fn review_verify_fail_loops_builder_then_passes() {
    let dir = std::env::temp_dir().join(format!(
        "asterline-review-verify-loop-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();

    let mut rt = runtime_in_workspace(dir.clone());
    let builder = MemberId::new("builder");
    let reviewer = MemberId::new("reviewer");

    rt.on_ui_command(run_mode("ship feature"));
    complete_ok(&mut rt, &builder, "first attempt");
    let step = complete_ok(
        &mut rt,
        &reviewer,
        "@@review {\"verdict\":\"approve\",\"summary\":\"looks fine\"}",
    );
    assert!(!step.verify_actions.is_empty());
    let run_id = step.verify_actions[0].run_id;
    let command = step.verify_actions[0].command.clone();

    let step = rt.on_verify_output(VerifyOutput {
        run_id,
        command: command.clone(),
        ok: false,
        stdout: b"test failed: edge case".to_vec(),
        stderr: Vec::new(),
        start_error: None,
        cancelled: false,
    });
    assert!(
        step.actions.iter().any(|a| {
            a.member == builder && a.prompt.contains(&command) && a.prompt.contains("edge case")
        }),
        "builder should get verify_failure_prompt with command+summary: {:?}",
        step.actions.iter().map(|a| &a.prompt).collect::<Vec<_>>()
    );
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::RunUpdated { run }
            if run.id == run_id && run.status == RunStatus::Running
    )));
    assert_eq!(latest_run(&rt).mode.as_ref().unwrap().state.iteration, 2);

    complete_ok(&mut rt, &builder, "fixed the edge case");
    let step = complete_ok(
        &mut rt,
        &reviewer,
        "@@review {\"verdict\":\"approve\",\"summary\":\"good now\"}",
    );
    assert!(!step.verify_actions.is_empty());
    let step = rt.on_verify_output(VerifyOutput {
        run_id,
        command: step.verify_actions[0].command.clone(),
        ok: true,
        stdout: b"ok".to_vec(),
        stderr: Vec::new(),
        start_error: None,
        cancelled: false,
    });
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::RunUpdated { run }
            if run.id == run_id && run.status == RunStatus::Done
    )));
    // Session freed: another review can start.
    let step = rt.on_ui_command(run_mode("next"));
    assert!(step.actions.iter().any(|a| a.member == builder));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn review_verify_fail_exhausted_stays_failed() {
    let dir = std::env::temp_dir().join(format!(
        "asterline-review-verify-exhausted-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();

    let mut rt = runtime_in_workspace(dir.clone());
    rt.config.modes.review = Some(ReviewModeConfig {
        max_iterations: Some(1),
        ..ReviewModeConfig::default()
    });
    let builder = MemberId::new("builder");
    let reviewer = MemberId::new("reviewer");

    rt.on_ui_command(run_mode("tight"));
    complete_ok(&mut rt, &builder, "attempt");
    let step = complete_ok(
        &mut rt,
        &reviewer,
        "@@review {\"verdict\":\"approve\",\"summary\":\"ok\"}",
    );
    let run_id = step.verify_actions[0].run_id;
    let step = rt.on_verify_output(VerifyOutput {
        run_id,
        command: step.verify_actions[0].command.clone(),
        ok: false,
        stdout: b"boom".to_vec(),
        stderr: Vec::new(),
        start_error: None,
        cancelled: false,
    });
    assert!(
        step.actions.is_empty(),
        "exhausted iterations must not re-dispatch"
    );
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::Notice(text) if text.contains("after 1 iterations") && text.contains("failed")
    )));
    let run = rt.store.run(run_id).unwrap();
    assert_eq!(run.status, RunStatus::Failed);
    // Session gone: new review can start.
    let step = rt.on_ui_command(run_mode("again"));
    assert!(step.actions.iter().any(|a| a.member == builder));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn review_verify_cancelled_no_loopback() {
    let dir = std::env::temp_dir().join(format!(
        "asterline-review-verify-cancel-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();

    let mut rt = runtime_in_workspace(dir.clone());
    let builder = MemberId::new("builder");
    let reviewer = MemberId::new("reviewer");

    rt.on_ui_command(run_mode("cancel verify"));
    complete_ok(&mut rt, &builder, "work");
    let step = complete_ok(
        &mut rt,
        &reviewer,
        "@@review {\"verdict\":\"approve\",\"summary\":\"ok\"}",
    );
    let run_id = step.verify_actions[0].run_id;
    let step = rt.on_verify_output(VerifyOutput {
        run_id,
        command: step.verify_actions[0].command.clone(),
        ok: false,
        stdout: Vec::new(),
        stderr: Vec::new(),
        start_error: None,
        cancelled: true,
    });
    assert!(
        step.actions.is_empty(),
        "cancelled verification must not loop back"
    );
    assert_eq!(rt.store.run(run_id).unwrap().status, RunStatus::Blocked);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn review_verify_command_config_reaches_verify_action() {
    let mut rt = runtime();
    rt.config.modes.review = Some(ReviewModeConfig {
        verify_command: Some("just check".to_string()),
        ..ReviewModeConfig::default()
    });
    let builder = MemberId::new("builder");
    let reviewer = MemberId::new("reviewer");
    rt.on_ui_command(run_mode("use just"));
    complete_ok(&mut rt, &builder, "done");
    let step = complete_ok(
        &mut rt,
        &reviewer,
        "@@review {\"verdict\":\"approve\",\"summary\":\"ok\"}",
    );
    assert_eq!(
        step.verify_actions
            .iter()
            .map(|a| a.command.as_str())
            .collect::<Vec<_>>(),
        vec!["just check"]
    );
}

#[test]
fn plan_verify_fail_loops_leader_with_plan_hint() {
    let dir =
        std::env::temp_dir().join(format!("asterline-plan-verify-loop-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();

    let mut config = plan_team();
    config.workspace = dir.clone();
    let mut rt = TeamRuntime::new(config, SqliteStore::in_memory().unwrap()).with_approvals(false);
    let planner = MemberId::new("planner");
    let builder = MemberId::new("builder");
    let reviewer = MemberId::new("reviewer");

    let step = rt.on_ui_command(run_plan("ship plan"));
    let run_id = find_run_id(&step);
    complete_ok(
        &mut rt,
        &planner,
        "@@run_step {\"action\":\"add\",\"owner\":\"builder\",\"title\":\"Do it\"}",
    );
    complete_ok(
        &mut rt,
        &builder,
        "@@run_step {\"action\":\"done\",\"step\":1}\ndone",
    );
    let step = complete_ok(
        &mut rt,
        &reviewer,
        "@@review {\"verdict\":\"approve\",\"summary\":\"ok\"}",
    );
    assert!(!step.verify_actions.is_empty());
    let command = step.verify_actions[0].command.clone();
    let step = rt.on_verify_output(VerifyOutput {
        run_id,
        command: command.clone(),
        ok: false,
        stdout: b"plan verify failed: missing tests".to_vec(),
        stderr: Vec::new(),
        start_error: None,
        cancelled: false,
    });
    assert!(
        step.actions.iter().any(|a| {
            a.member == planner
                && a.prompt.contains(PLAN_MODE_HINT)
                && a.prompt.contains(&command)
                && a.prompt.contains("missing tests")
        }),
        "leader should get plan_verify_failure_prompt: {:?}",
        step.actions.iter().map(|a| &a.prompt).collect::<Vec<_>>()
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn plan_verify_command_config_reaches_verify_action() {
    let mut rt = plan_runtime();
    rt.config.modes.plan = Some(PlanModeConfig {
        verify_command: Some("just check".to_string()),
        ..PlanModeConfig::default()
    });
    let planner = MemberId::new("planner");
    let builder = MemberId::new("builder");
    let reviewer = MemberId::new("reviewer");
    rt.on_ui_command(run_plan("use just for plan"));
    complete_ok(
        &mut rt,
        &planner,
        "@@run_step {\"action\":\"add\",\"owner\":\"builder\",\"title\":\"Do it\"}",
    );
    complete_ok(
        &mut rt,
        &builder,
        "@@run_step {\"action\":\"done\",\"step\":1}\ndone",
    );
    let step = complete_ok(
        &mut rt,
        &reviewer,
        "@@review {\"verdict\":\"approve\",\"summary\":\"ok\"}",
    );
    assert_eq!(
        step.verify_actions
            .iter()
            .map(|a| a.command.as_str())
            .collect::<Vec<_>>(),
        vec!["just check"]
    );
}

#[test]
fn plan_progress_prompt_includes_blocked_step_note() {
    let mut rt = plan_runtime();
    let planner = MemberId::new("planner");
    let builder = MemberId::new("builder");
    rt.on_ui_command(run_plan("note fidelity"));
    complete_ok(
        &mut rt,
        &planner,
        "@@run_step {\"action\":\"add\",\"owner\":\"builder\",\"title\":\"Do the thing\"}",
    );
    let step = complete_ok(
        &mut rt,
        &builder,
        "@@run_step {\"action\":\"block\",\"step\":1,\"note\":\"waiting for secret\"}\nblocked",
    );
    assert!(
        step.actions.iter().any(|a| {
            a.member == planner
                && a.prompt.contains("waiting for secret")
                && a.prompt.contains('—')
                && a.prompt.contains("assign an owner")
        }),
        "progress prompt must include blocked note: {:?}",
        step.actions.iter().map(|a| &a.prompt).collect::<Vec<_>>()
    );
}

#[test]
fn plan_executing_member_failure_replans_leader() {
    let mut rt = plan_runtime();
    let planner = MemberId::new("planner");
    let builder = MemberId::new("builder");

    rt.on_ui_command(run_plan("owner fail replan"));
    complete_ok(
        &mut rt,
        &planner,
        "@@run_step {\"action\":\"add\",\"owner\":\"builder\",\"title\":\"Fragile work\"}",
    );
    // Builder process fails during Executing.
    let step = rt.on_agent_event(
        &builder,
        AgentEvent::Exited {
            code: Some(1),
            ok: false,
        },
    );
    assert!(
        step.actions.iter().any(|a| {
            a.member == planner
                && a.prompt.contains("member run failed")
                && a.prompt.contains(PLAN_MODE_HINT)
        }),
        "leader should re-plan after owner failure: {:?}",
        step.actions.iter().map(|a| &a.prompt).collect::<Vec<_>>()
    );
    assert_eq!(latest_run(&rt).status, RunStatus::Running);
}

#[test]
fn plan_executing_member_failure_exhausted_blocks() {
    let mut rt = plan_runtime();
    rt.config.modes.plan = Some(PlanModeConfig {
        max_iterations: Some(1),
        ..PlanModeConfig::default()
    });
    let planner = MemberId::new("planner");
    let builder = MemberId::new("builder");

    let step = rt.on_ui_command(run_plan("owner fail exhaust"));
    let run_id = find_run_id(&step);
    complete_ok(
        &mut rt,
        &planner,
        "@@run_step {\"action\":\"add\",\"owner\":\"builder\",\"title\":\"Fragile work\"}",
    );
    let step = rt.on_agent_event(
        &builder,
        AgentEvent::Exited {
            code: Some(1),
            ok: false,
        },
    );
    assert!(step.actions.is_empty(), "exhausted must not re-plan");
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::RunUpdated { run }
            if run.id == run_id && run.status == RunStatus::Blocked
    )));
}

#[test]
fn plan_executing_user_abort_blocks_immediately() {
    let mut rt = plan_runtime();
    let planner = MemberId::new("planner");

    let step = rt.on_ui_command(run_plan("abort mid execute"));
    let run_id = find_run_id(&step);
    complete_ok(
        &mut rt,
        &planner,
        "@@run_step {\"action\":\"add\",\"owner\":\"builder\",\"title\":\"Work\"}",
    );
    // Global cancel blocks all mode sessions immediately (no re-plan).
    let step = rt.on_ui_command(UiCommand::Cancel { member: None });
    assert!(
        step.actions.is_empty(),
        "user abort must not re-plan: {} actions",
        step.actions.len()
    );
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::RunUpdated { run }
            if run.id == run_id && run.status == RunStatus::Blocked
    )));
    assert_eq!(rt.store.run(run_id).unwrap().status, RunStatus::Blocked);
}

#[test]
fn review_request_changes_iterates_builder() {
    let mut rt = runtime();
    let builder = MemberId::new("builder");
    let reviewer = MemberId::new("reviewer");

    rt.on_ui_command(run_mode("build feature"));
    complete_ok(&mut rt, &builder, "first pass");
    let step = complete_ok(
        &mut rt,
        &reviewer,
        "@@review {\"verdict\":\"request_changes\",\"summary\":\"add edge-case tests\"}",
    );
    assert!(
        step.actions
            .iter()
            .any(|a| { a.member == builder && a.prompt.contains("add edge-case tests") }),
        "builder should receive feedback: {:?}",
        step.actions.iter().map(|a| &a.prompt).collect::<Vec<_>>()
    );
    let run = latest_run(&rt);
    assert_eq!(run.mode.as_ref().unwrap().state.iteration, 2);
    assert_eq!(run.status, RunStatus::Running);
}

#[test]
fn review_max_iterations_blocks() {
    let mut rt = runtime();
    let builder = MemberId::new("builder");
    let reviewer = MemberId::new("reviewer");

    rt.config.modes.review = Some(ReviewModeConfig {
        max_iterations: Some(1),
        ..ReviewModeConfig::default()
    });
    let step = rt.on_ui_command(UiCommand::RunMode {
        mode: CollabMode::Review,
        task: "tight loop".to_string(),
    });
    let run_id = find_run_id(&step);

    complete_ok(&mut rt, &builder, "attempt 1");
    let step = complete_ok(
        &mut rt,
        &reviewer,
        "@@review {\"verdict\":\"request_changes\",\"summary\":\"still broken\"}",
    );
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::RunUpdated { run }
            if run.id == run_id && run.status == RunStatus::Blocked
    )));
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::Notice(text) if text.contains("max iterations")
    )));
    assert!(
        !step.actions.iter().any(|a| a.member == builder),
        "must not start another builder iteration"
    );

    // Session freed.
    let step = rt.on_ui_command(run_mode("fresh"));
    assert!(step.actions.iter().any(|a| a.member == builder));
}

#[test]
fn review_missing_verdict_nudges_then_treats_text_as_changes() {
    let mut rt = runtime();
    let builder = MemberId::new("builder");
    let reviewer = MemberId::new("reviewer");

    rt.on_ui_command(run_mode("review this"));
    complete_ok(&mut rt, &builder, "builder output");

    let step = complete_ok(&mut rt, &reviewer, "I have concerns about the API");
    assert!(
        step.actions
            .iter()
            .any(|a| { a.member == reviewer && a.prompt.contains(REVIEW_PROTOCOL_HINT) }),
        "missing verdict should nudge the reviewer"
    );

    let step = complete_ok(&mut rt, &reviewer, "please fix the API shape");
    assert!(
        step.actions
            .iter()
            .any(|a| { a.member == builder && a.prompt.contains("please fix the API shape") }),
        "second miss should treat reviewer text as request_changes: {:?}",
        step.actions.iter().map(|a| &a.prompt).collect::<Vec<_>>()
    );
    assert_eq!(latest_run(&rt).mode.as_ref().unwrap().state.iteration, 2);
}

#[test]
fn abort_blocks_mode_run() {
    let mut rt = runtime();
    let builder = MemberId::new("builder");

    let step = rt.on_ui_command(run_mode("in progress"));
    let run_id = find_run_id(&step);

    let step = rt.on_ui_command(UiCommand::Cancel { member: None });
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::RunUpdated { run }
            if run.id == run_id && run.status == RunStatus::Blocked
    )));
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::Notice(text) if text.contains("aborted by user")
    )));

    // After the builder exits, status must stay Blocked (not Done).
    let step = rt.on_agent_event(
        &builder,
        AgentEvent::Exited {
            code: None,
            ok: false,
        },
    );
    let run = rt.store.run(run_id).unwrap();
    assert_eq!(run.status, RunStatus::Blocked);
    assert!(
        !step.events.iter().any(|e| matches!(
            e,
            RuntimeEvent::RunUpdated { run }
                if run.id == run_id && run.status == RunStatus::Done
        )),
        "abort must not be overwritten to Done"
    );

    // Session freed.
    let step = rt.on_ui_command(run_mode("again"));
    assert!(step.actions.iter().any(|a| a.member == builder));
}

#[test]
fn mode_dispatch_hits_approval_gate_and_reject_blocks() {
    let mut config = team();
    // Default approvals gate git keywords on all surfaces including Mode.
    let mut rt = TeamRuntime::new(config.clone(), SqliteStore::in_memory().unwrap());

    let step = rt.on_ui_command(run_mode("run git status"));
    let run_id = find_run_id(&step);
    assert!(
        step.actions.is_empty(),
        "mode dispatch with git keyword must not auto-run"
    );
    let id = step
        .events
        .iter()
        .find_map(|e| match e {
            RuntimeEvent::ApprovalRequested { id, .. } => Some(*id),
            _ => None,
        })
        .expect("ApprovalRequested");

    let step = rt.on_ui_command(UiCommand::Approve {
        id,
        decision: ApprovalDecision::Reject,
    });
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::RunUpdated { run }
            if run.id == run_id && run.status == RunStatus::Blocked
    )));
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::Notice(text) if text.contains("dispatch rejected by user")
    )));

    // Separate case: Approve dispatches the builder.
    config = team();
    let mut rt = TeamRuntime::new(config, SqliteStore::in_memory().unwrap());
    let step = rt.on_ui_command(run_mode("run git status"));
    let id = step
        .events
        .iter()
        .find_map(|e| match e {
            RuntimeEvent::ApprovalRequested { id, .. } => Some(*id),
            _ => None,
        })
        .expect("ApprovalRequested");
    let step = rt.on_ui_command(UiCommand::Approve {
        id,
        decision: ApprovalDecision::Approve,
    });
    assert!(
        step.actions
            .iter()
            .any(|a| a.member == MemberId::new("builder")),
        "approve must dispatch builder"
    );
}

fn brainstorm_approval_runtime() -> TeamRuntime {
    let mut config = plan_team();
    config.approvals.gate = Some(Vec::new());
    config.approvals.keywords.insert(
        "brainstorm_protocol".to_string(),
        vec!["deployed $asterline-brainstorm".to_string()],
    );
    config.approvals.apply_to = Some(vec![ApprovalSurface::Mode]);
    TeamRuntime::new(config, SqliteStore::in_memory().unwrap())
}

#[test]
fn rejecting_one_mode_approval_rejects_all_run_siblings() {
    let mut rt = brainstorm_approval_runtime();
    let started = rt.on_ui_command(run_brainstorm("generate release ideas"));
    let run_id = find_run_id(&started);
    let ids: Vec<ApprovalId> = started
        .events
        .iter()
        .filter_map(|event| match event {
            RuntimeEvent::ApprovalRequested { id, .. } => Some(*id),
            _ => None,
        })
        .collect();
    assert_eq!(ids.len(), 3);

    let rejected = rt.on_ui_command(UiCommand::Approve {
        id: ids[0],
        decision: ApprovalDecision::Reject,
    });

    assert_eq!(
        rejected
            .events
            .iter()
            .filter(|event| matches!(
                event,
                RuntimeEvent::ApprovalResolved {
                    decision: ApprovalDecision::Reject,
                    ..
                }
            ))
            .count(),
        3
    );
    assert!(rt.store.pending_approvals().unwrap().is_empty());
    assert_eq!(rt.store.run(run_id).unwrap().status, RunStatus::Blocked);
}

#[test]
fn approving_mode_dispatch_rejects_it_when_run_is_no_longer_active() {
    let mut rt = brainstorm_approval_runtime();
    let started = rt.on_ui_command(run_brainstorm("generate release ideas"));
    let run_id = find_run_id(&started);
    let id = started
        .events
        .iter()
        .find_map(|event| match event {
            RuntimeEvent::ApprovalRequested { id, .. } => Some(*id),
            _ => None,
        })
        .expect("mode approval");
    rt.store.block_run(run_id, "external stop").unwrap();

    let approval = rt.on_ui_command(UiCommand::Approve {
        id,
        decision: ApprovalDecision::Approve,
    });

    assert!(approval.actions.is_empty());
    assert!(approval.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Notice(text)
            if text.contains("no longer active") && text.contains(&run_id.to_string())
    )));
    assert!(rt.store.pending_approvals().unwrap().is_empty());
    assert_eq!(rt.store.run(run_id).unwrap().status, RunStatus::Blocked);
}

#[test]
fn restart_blocks_running_mode_run() {
    let dir = std::env::temp_dir().join(format!("asterline-mode-restart-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("state.db");

    let store = SqliteStore::open(&path).unwrap();
    let mut rt = TeamRuntime::new(team(), store).with_approvals(false);
    let step = rt.on_ui_command(run_mode("interrupted work"));
    let run_id = find_run_id(&step);
    assert_eq!(rt.store.run(run_id).unwrap().status, RunStatus::Running);
    drop(rt);

    let store = SqliteStore::open(&path).unwrap();
    let _rt = TeamRuntime::new(team(), store).with_approvals(false);
    drop(_rt);

    let store = SqliteStore::open(&path).unwrap();
    let run = store.run(run_id).unwrap();
    assert_eq!(run.status, RunStatus::Blocked);
    assert!(
        run.events
            .iter()
            .any(|e| e.kind == "blocked" && e.detail.as_deref() == Some("interrupted by restart")),
        "expected restart block event: {:?}",
        run.events
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn restart_disables_dispatch_when_interrupted_run_cannot_be_blocked() {
    let dir = std::env::temp_dir().join(format!(
        "asterline-mode-restart-block-failure-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("state.db");
    let store = SqliteStore::open(&path).unwrap();
    let mut rt = TeamRuntime::new(team(), store).with_approvals(false);
    let started = rt.on_ui_command(run_mode("interrupted work"));
    let run_id = find_run_id(&started);
    drop(rt);
    let external = Connection::open(&path).unwrap();
    external
        .execute_batch(
            "CREATE TRIGGER fail_restart_block
             BEFORE UPDATE OF status ON runs
             WHEN NEW.status = 'blocked'
             BEGIN SELECT RAISE(ABORT, 'block unavailable'); END;",
        )
        .unwrap();

    let store = SqliteStore::open(&path).unwrap();
    let mut restarted = TeamRuntime::new(team(), store).with_approvals(false);
    let conversation = restarted.store.active_conversation();
    let dispatch = restarted.on_ui_command(user("must not overlap interrupted run"));
    let new_chat = restarted.on_ui_command(UiCommand::NewSession);

    assert!(dispatch.actions.is_empty());
    assert!(dispatch.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Notice(text) if text.contains("interrupted runs were not reconciled")
    )));
    assert_eq!(restarted.store.active_conversation(), conversation);
    assert!(
        !new_chat
            .events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::SessionReset))
    );
    assert_eq!(
        restarted.store.run(run_id).unwrap().status,
        RunStatus::Running
    );
    drop(restarted);
    drop(external);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn restart_blocks_running_team_run() {
    let dir = std::env::temp_dir().join(format!("asterline-team-restart-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("state.db");

    let store = SqliteStore::open(&path).unwrap();
    let mut rt = TeamRuntime::new(team(), store).with_approvals(false);
    let step = start_team(&mut rt, "interrupted team work");
    let run_id = find_run_id(&step);
    drop(rt);

    let store = SqliteStore::open(&path).unwrap();
    drop(TeamRuntime::new(team(), store).with_approvals(false));

    let store = SqliteStore::open(&path).unwrap();
    let run = store.run(run_id).unwrap();
    assert_eq!(run.status, RunStatus::Blocked);
    assert_eq!(
        run.mode.as_ref().map(|mode| mode.mode),
        Some(CollabMode::Team)
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn continue_resumes_blocked_review() {
    let mut rt = runtime();
    let builder = MemberId::new("builder");

    let step = rt.on_ui_command(run_mode("resume me"));
    let run_id = find_run_id(&step);
    rt.on_ui_command(UiCommand::Cancel { member: None });
    // Drain the cancelled builder exit so the turn is fully idle.
    let _ = rt.on_agent_event(
        &builder,
        AgentEvent::Exited {
            code: None,
            ok: false,
        },
    );
    assert_eq!(rt.store.run(run_id).unwrap().status, RunStatus::Blocked);

    let step = rt.on_ui_command(UiCommand::ContinueRun {
        run_id: Some(run_id),
        note: None,
    });
    assert!(
        step.actions.iter().any(|a| a.member == builder),
        "continue should re-dispatch the building phase: {:?}",
        step.actions.iter().map(|a| &a.member).collect::<Vec<_>>()
    );
    let run = rt.store.run(run_id).unwrap();
    assert_eq!(run.status, RunStatus::Running);
    assert_eq!(run.attempt, 2);
}

#[test]
fn second_run_mode_while_active_is_refused() {
    let mut rt = runtime();
    rt.on_ui_command(run_mode("first"));
    let step = rt.on_ui_command(run_mode("second"));
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::Notice(text) if text.contains("already active")
    )));
    assert!(step.actions.is_empty());
}

#[test]
fn verdict_outside_review_is_ignored() {
    let mut rt = runtime();
    let builder = MemberId::new("builder");
    rt.on_ui_command(user("plain chat"));
    let step = rt.on_agent_event(
        &builder,
        AgentEvent::MessageCompleted(
            "done\n@@review {\"verdict\":\"approve\",\"summary\":\"oops\"}".to_string(),
        ),
    );
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::Notice(text) if text.contains("outside an active review")
    )));
    assert!(
        !step
            .events
            .iter()
            .any(|e| matches!(e, RuntimeEvent::Verdict { .. })),
        "no Verdict event for free-form turns"
    );
}

// --- Plan + brainstorm helpers --------------------------------------------

fn plan_team() -> TeamConfig {
    let mut config = TeamConfig::new("plan-team", "/tmp/ws")
        .with_member(TeamMember::new(
            "planner",
            "Planner",
            BackendKind::Codex,
            "planning lead",
        ))
        .with_member(TeamMember::new(
            "builder",
            "Builder",
            BackendKind::Claude,
            "impl",
        ))
        .with_member(TeamMember::new(
            "reviewer",
            "Reviewer",
            BackendKind::Grok,
            "review",
        ));
    config.default_target = Some(DefaultTarget::Member(MemberId::new("builder")));
    config
}

fn plan_runtime() -> TeamRuntime {
    TeamRuntime::new(plan_team(), SqliteStore::in_memory().unwrap()).with_approvals(false)
}

fn run_plan(task: &str) -> UiCommand {
    UiCommand::RunMode {
        mode: CollabMode::Plan,
        task: task.to_string(),
    }
}

fn run_brainstorm(task: &str) -> UiCommand {
    UiCommand::RunMode {
        mode: CollabMode::Brainstorm,
        task: task.to_string(),
    }
}

fn complete_all(rt: &mut TeamRuntime, members: &[(MemberId, &str)]) -> RuntimeStep {
    let mut merged = RuntimeStep::default();
    for (member, text) in members {
        let step = complete_ok(rt, member, text);
        merged.events.extend(step.events);
        merged.actions.extend(step.actions);
        merged.verify_actions.extend(step.verify_actions);
        merged.runner_changes.extend(step.runner_changes);
        if step.persist_team.is_some() {
            merged.persist_team = step.persist_team;
        }
    }
    merged
}

#[test]
fn plan_dispatches_owned_todo_steps() {
    let mut rt = plan_runtime();
    let planner = MemberId::new("planner");
    let builder = MemberId::new("builder");
    let reviewer = MemberId::new("reviewer");

    let step = rt.on_ui_command(run_plan("ship the feature"));
    assert!(
        step.actions.iter().any(|a| {
            a.member == planner
                && a.prompt.contains(PLAN_MODE_HINT)
                && a.prompt.contains("Teammates: ")
        }),
        "leader should get plan prompt: {:?}",
        step.actions.iter().map(|a| &a.prompt).collect::<Vec<_>>()
    );
    let run_id = find_run_id(&step);

    let step = complete_ok(
        &mut rt,
        &planner,
        "plan\n\
         @@run_step {\"action\":\"add\",\"owner\":\"builder\",\"title\":\"Implement core\"}\n\
         @@run_step {\"action\":\"add\",\"owner\":\"reviewer\",\"title\":\"Write tests\"}",
    );
    let builder_action = step
        .actions
        .iter()
        .find(|a| a.member == builder)
        .expect("builder RunAction");
    let reviewer_action = step
        .actions
        .iter()
        .find(|a| a.member == reviewer)
        .expect("reviewer RunAction");
    assert!(
        builder_action.prompt.contains("step #1"),
        "builder owns step 1: {}",
        builder_action.prompt
    );
    assert!(
        !builder_action.prompt.contains("step #2"),
        "builder should not see reviewer step"
    );
    assert!(
        builder_action
            .prompt
            .contains(r#"@@team_message {"to":"planner","kind":"reply""#),
        "builder must report completion to the planning lead: {}",
        builder_action.prompt
    );
    assert!(
        reviewer_action.prompt.contains("step #2"),
        "reviewer owns step 2: {}",
        reviewer_action.prompt
    );
    assert!(
        !reviewer_action.prompt.contains("step #1"),
        "reviewer should not see builder step"
    );
    assert!(
        reviewer_action
            .prompt
            .contains(r#"@@team_message {"to":"planner","kind":"reply""#),
        "every owner must report completion to the planning lead"
    );

    let run = rt.store.run(run_id).unwrap();
    assert!(
        run.steps.iter().all(|s| s.status == RunStepStatus::Doing),
        "owned todos should be Doing: {:?}",
        run.steps
    );
}

#[test]
fn plan_does_not_dispatch_when_its_checklist_cannot_be_loaded() {
    let path = std::env::temp_dir().join(format!(
        "asterline-plan-store-error-{}.sqlite3",
        std::process::id()
    ));
    remove_sqlite_test_files(&path);
    let store = SqliteStore::open(&path).unwrap();
    let mut rt = TeamRuntime::new(plan_team(), store).with_approvals(false);
    let planner = MemberId::new("planner");
    let started = rt.on_ui_command(run_plan("ship safely"));
    let run_id = find_run_id(&started);

    rt.on_agent_event(
        &planner,
        AgentEvent::MessageCompleted(
            "plan\n@@run_step {\"action\":\"add\",\"owner\":\"builder\",\"title\":\"Implement\"}"
                .to_string(),
        ),
    );
    let external = Connection::open(&path).unwrap();
    external.execute("DROP TABLE run_steps", []).unwrap();

    let step = rt.on_agent_event(
        &planner,
        AgentEvent::Exited {
            code: Some(0),
            ok: true,
        },
    );

    assert!(step.actions.is_empty());
    let status: String = external
        .query_row(
            "SELECT status FROM runs WHERE id = ?1",
            [run_id.0 as i64],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        status,
        RunStatus::Blocked.as_str(),
        "events: {:?}",
        step.events
    );
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Notice(message) if message.contains("load the plan checklist")
    )));

    drop(external);
    drop(rt);
    remove_sqlite_test_files(&path);
}

#[test]
fn plan_empty_checklist_nudges_then_blocks() {
    let mut rt = plan_runtime();
    let planner = MemberId::new("planner");

    let step = rt.on_ui_command(run_plan("empty plan"));
    let run_id = find_run_id(&step);

    let step = complete_ok(&mut rt, &planner, "I thought about it but wrote nothing");
    assert!(
        step.actions
            .iter()
            .any(|a| a.member == planner && a.prompt.contains(PLAN_MODE_HINT)),
        "empty checklist should nudge the leader"
    );

    let step = complete_ok(&mut rt, &planner, "still nothing useful");
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::RunUpdated { run }
            if run.id == run_id && run.status == RunStatus::Blocked
    )));
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::Notice(text) if text.contains("no actionable plan")
    )));
}

#[test]
fn plan_unfinished_steps_return_to_leader() {
    let mut rt = plan_runtime();
    let planner = MemberId::new("planner");
    let builder = MemberId::new("builder");

    rt.on_ui_command(run_plan("partial work"));
    complete_ok(
        &mut rt,
        &planner,
        "@@run_step {\"action\":\"add\",\"owner\":\"builder\",\"title\":\"Do the thing\"}",
    );

    let step = complete_ok(&mut rt, &builder, "I worked but forgot to mark done");
    assert!(
        step.actions.iter().any(|a| {
            a.member == planner && (a.prompt.contains("Do the thing") || a.prompt.contains("#1"))
        }),
        "leader should see unfinished step: {:?}",
        step.actions.iter().map(|a| &a.prompt).collect::<Vec<_>>()
    );
    let run = latest_run(&rt);
    assert_eq!(run.mode.as_ref().unwrap().state.iteration, 2);
    assert_eq!(run.mode.as_ref().unwrap().state.phase, "planning");
}

#[test]
fn plan_all_done_enters_review_and_approve_finishes() {
    let mut rt = plan_runtime();
    let planner = MemberId::new("planner");
    let builder = MemberId::new("builder");
    let reviewer = MemberId::new("reviewer");

    let step = rt.on_ui_command(run_plan("finish path"));
    let run_id = find_run_id(&step);

    complete_ok(
        &mut rt,
        &planner,
        "@@run_step {\"action\":\"add\",\"owner\":\"builder\",\"title\":\"Implement core\"}\n\
         @@run_step {\"action\":\"add\",\"owner\":\"reviewer\",\"title\":\"Write docs\"}",
    );

    let step = complete_all(
        &mut rt,
        &[
            (
                builder,
                "@@run_step {\"action\":\"done\",\"step\":1}\ncore done",
            ),
            (
                reviewer.clone(),
                "@@run_step {\"action\":\"done\",\"step\":2}\ndocs done",
            ),
        ],
    );
    assert!(
        step.actions.iter().any(|a| {
            a.member == reviewer
                && a.prompt.contains(REVIEW_PROTOCOL_HINT)
                && a.prompt.contains("Implement core")
                && a.prompt.contains("Write docs")
        }),
        "reviewer should get plan review with step titles: {:?}",
        step.actions.iter().map(|a| &a.prompt).collect::<Vec<_>>()
    );

    let step = complete_ok(
        &mut rt,
        &reviewer,
        "@@review {\"verdict\":\"approve\",\"summary\":\"looks good\"}",
    );
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::RunUpdated { run }
            if run.id == run_id && run.status == RunStatus::Done
    )));
}

#[test]
fn plan_request_changes_returns_to_leader() {
    let mut rt = plan_runtime();
    let planner = MemberId::new("planner");
    let builder = MemberId::new("builder");
    let reviewer = MemberId::new("reviewer");

    rt.on_ui_command(run_plan("needs changes"));
    complete_ok(
        &mut rt,
        &planner,
        "@@run_step {\"action\":\"add\",\"owner\":\"builder\",\"title\":\"Build it\"}",
    );
    complete_ok(
        &mut rt,
        &builder,
        "@@run_step {\"action\":\"done\",\"step\":1}\ndone",
    );
    let step = complete_ok(
        &mut rt,
        &reviewer,
        "@@review {\"verdict\":\"request_changes\",\"summary\":\"add edge-case coverage\"}",
    );
    assert!(
        step.actions
            .iter()
            .any(|a| { a.member == planner && a.prompt.contains("add edge-case coverage") }),
        "feedback should go to the leader: {:?}",
        step.actions.iter().map(|a| &a.prompt).collect::<Vec<_>>()
    );
    let run = latest_run(&rt);
    assert_eq!(run.mode.as_ref().unwrap().state.phase, "planning");
}

#[test]
fn brainstorm_records_original_topic_as_visible_user_message() {
    let mut rt = plan_runtime();
    let topic = "ways to redesign graph retrieval";
    let step = rt.on_ui_command(run_brainstorm(topic));

    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::UserMessage { body, .. } if body == topic
    )));
    assert!(rt.store.replay_chat().unwrap().iter().any(|item| matches!(
        item,
        ChatItem::User { body } if body == topic
    )));
    assert_eq!(step.actions.len(), 3);
    assert!(
        step.actions
            .iter()
            .all(|action| action.prompt.contains(BRAINSTORM_PROPOSE_HINT)
                && action.prompt.contains("Suspend judgment")
                && action.prompt.contains("$asterline-brainstorm")
                && action.prompt.contains("@@brainstorm_card")
                && !action.prompt.contains("trade-offs and a first step"))
    );
}

#[test]
fn brainstorm_structured_cards_are_rendered_and_persisted() {
    let mut rt = plan_runtime();
    let planner = MemberId::new("planner");
    let start = rt.on_ui_command(run_brainstorm("structured cards"));
    let run_id = find_run_id(&start);
    let completed = rt.on_agent_event(
        &planner,
        AgentEvent::MessageCompleted(
            "@@brainstorm_card {\"title\":\"Graph memory\",\"proposal\":\"Retrieve prior subgraphs\",\"mechanism\":\"Index WL fingerprints\",\"operation\":\"seed\",\"sources\":[]}\n@@brainstorm_card {\"title\":\"Path memory\",\"proposal\":\"Retrieve useful walks\",\"mechanism\":\"Rank constrained paths\",\"operation\":\"seed\",\"sources\":[]}".to_string(),
        ),
    );

    let rendered = completed
        .events
        .iter()
        .find_map(|event| match event {
            RuntimeEvent::MessageCompleted { text, .. } => Some(text),
            _ => None,
        })
        .expect("rendered message");
    assert!(rendered.contains("### Card 1 · Graph memory"));
    assert!(rendered.contains("### Card 2 · Path memory"));
    assert!(!rendered.contains("@@brainstorm_card"));

    let state: serde_json::Value =
        serde_json::from_str(&rt.store.run_mode_state(run_id).unwrap().unwrap()).unwrap();
    assert_eq!(state["idea_count"], 2);
    assert_eq!(state["idea_batches"][0]["cards"][0]["operation"], "SEED");
    assert_eq!(state["idea_batches"][0]["cards"][1]["title"], "Path memory");
}

#[test]
fn brainstorm_retries_append_changed_ideas_without_duplicating_exact_replays() {
    let mut rt = plan_runtime();
    let planner = MemberId::new("planner");
    let start = rt.on_ui_command(run_brainstorm("preserve attempts"));
    let run_id = find_run_id(&start);

    rt.on_agent_event(
        &planner,
        AgentEvent::MessageCompleted("first seed batch".to_string()),
    );
    rt.on_agent_event(
        &planner,
        AgentEvent::MessageCompleted("revised seed batch".to_string()),
    );
    rt.on_agent_event(
        &planner,
        AgentEvent::MessageCompleted("revised seed batch".to_string()),
    );

    let state: serde_json::Value =
        serde_json::from_str(&rt.store.run_mode_state(run_id).unwrap().unwrap()).unwrap();
    let batches = state["idea_batches"].as_array().expect("idea batches");
    assert_eq!(batches.len(), 2);
    assert_eq!(batches[0]["text"], "first seed batch");
    assert_eq!(batches[1]["text"], "revised seed batch");
}

#[test]
fn brainstorm_runs_generation_private_vote_and_ranked_synthesis() {
    let mut rt = plan_runtime();
    let planner = MemberId::new("planner");
    let builder = MemberId::new("builder");
    let reviewer = MemberId::new("reviewer");
    let step = rt.on_ui_command(run_brainstorm("expand architecture options"));
    let run_id = find_run_id(&step);

    let build = complete_all(
        &mut rt,
        &[
            (
                planner.clone(),
                "@@brainstorm_card {\"title\":\"planner seed\",\"proposal\":\"planner proposal\",\"mechanism\":\"planner mechanism\",\"operation\":\"SEED\",\"sources\":[]}",
            ),
            (
                builder.clone(),
                "@@brainstorm_card {\"title\":\"builder seed\",\"proposal\":\"builder proposal\",\"mechanism\":\"builder mechanism\",\"operation\":\"SEED\",\"sources\":[]}",
            ),
            (
                reviewer.clone(),
                "@@brainstorm_card {\"title\":\"reviewer seed\",\"proposal\":\"reviewer proposal\",\"mechanism\":\"reviewer mechanism\",\"operation\":\"SEED\",\"sources\":[]}",
            ),
        ],
    );
    assert_eq!(build.actions.len(), 3);
    assert!(
        build
            .actions
            .iter()
            .all(|action| action.prompt.contains(BRAINSTORM_BUILD_HINT))
    );
    let planner_build = build
        .actions
        .iter()
        .find(|action| action.member == planner)
        .expect("planner build prompt");
    assert!(planner_build.prompt.contains("planner seed"));
    assert!(planner_build.prompt.contains("builder seed"));
    assert!(
        !planner_build.prompt.contains("reviewer seed"),
        "each member should receive a rotating peer subset, not all prior ideas"
    );

    let stretch = complete_all(
        &mut rt,
        &[
            (
                planner.clone(),
                "@@brainstorm_card {\"title\":\"planner build\",\"proposal\":\"planner build proposal\",\"mechanism\":\"planner build mechanism\",\"operation\":\"BUILD\",\"sources\":[\"R1-A#1\"]}",
            ),
            (
                builder.clone(),
                "@@brainstorm_card {\"title\":\"builder build\",\"proposal\":\"builder build proposal\",\"mechanism\":\"builder build mechanism\",\"operation\":\"BUILD\",\"sources\":[\"R1-B#1\"]}",
            ),
            (
                reviewer.clone(),
                "@@brainstorm_card {\"title\":\"reviewer build\",\"proposal\":\"reviewer build proposal\",\"mechanism\":\"reviewer build mechanism\",\"operation\":\"BUILD\",\"sources\":[\"R1-C#1\"]}",
            ),
        ],
    );
    assert_eq!(stretch.actions.len(), 3);
    assert!(
        stretch
            .actions
            .iter()
            .all(|action| action.prompt.contains(BRAINSTORM_STRETCH_HINT)
                && action.prompt.contains("do not select a preferred option"))
    );
    let planner_stretch = stretch
        .actions
        .iter()
        .find(|action| action.member == planner)
        .expect("planner stretch prompt");
    assert!(planner_stretch.prompt.contains("planner build"));
    assert!(planner_stretch.prompt.contains("reviewer build"));
    assert!(!planner_stretch.prompt.contains("builder build"));

    let vote = complete_all(
        &mut rt,
        &[
            (
                planner.clone(),
                "@@brainstorm_card {\"title\":\"planner stretch\",\"proposal\":\"planner stretch proposal\",\"mechanism\":\"planner stretch mechanism\",\"operation\":\"INVERT\",\"sources\":[\"R2-A#1\"]}",
            ),
            (
                builder.clone(),
                "@@brainstorm_card {\"title\":\"builder stretch\",\"proposal\":\"builder stretch proposal\",\"mechanism\":\"builder stretch mechanism\",\"operation\":\"ANALOGY\",\"sources\":[\"R2-B#1\"]}",
            ),
            (
                reviewer.clone(),
                "@@brainstorm_card {\"title\":\"reviewer stretch\",\"proposal\":\"reviewer stretch proposal\",\"mechanism\":\"reviewer stretch mechanism\",\"operation\":\"BRIDGE\",\"sources\":[\"R2-C#1\"]}",
            ),
        ],
    );
    assert_eq!(vote.actions.len(), 3);
    assert!(
        vote.actions
            .iter()
            .all(|action| action.prompt.contains(BRAINSTORM_VOTE_HINT)
                && action.prompt.contains("@@brainstorm_vote")
                && action.prompt.contains("[R1-A#1] planner seed")
                && action.prompt.contains("[R3-C#1] reviewer stretch"))
    );

    let synthesize = complete_all(
        &mut rt,
        &[
            (
                planner.clone(),
                "planner ballot\n@@brainstorm_vote {\"ranked\":[\"R1-A#1\",\"R2-B#1\",\"R3-C#1\"],\"summary\":\"balanced\"}",
            ),
            (
                builder,
                "builder ballot\n@@brainstorm_vote {\"ranked\":[\"R2-B#1\",\"R1-A#1\",\"R3-C#1\"],\"summary\":\"feasible\"}",
            ),
            (
                reviewer,
                "reviewer ballot\n@@brainstorm_vote {\"ranked\":[\"R1-A#1\",\"R3-C#1\",\"R2-B#1\"],\"summary\":\"high leverage\"}",
            ),
        ],
    );
    assert_eq!(synthesize.actions.len(), 1);
    assert_eq!(synthesize.actions[0].member, planner);
    assert!(
        synthesize.actions[0]
            .prompt
            .contains(BRAINSTORM_SYNTHESIS_HINT)
    );
    assert!(synthesize.actions[0].prompt.contains("R1-A#1 — 14 points"));

    let done = complete_ok(
        &mut rt,
        &MemberId::new("planner"),
        "## Ranked result\n\n1. R1-A#1\n2. R2-B#1\n\nPrimary: test R1-A#1.",
    );
    assert!(done.actions.is_empty());
    assert!(done.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Notice(text)
            if text.contains("ranked result ready")
                && text.contains("9 idea cards from 9 contributions")
                && text.contains("3 generation waves")
                && text.contains("3/3 private ballots")
    )));
    let run = rt.store.run(run_id).unwrap();
    assert_eq!(run.status, RunStatus::Done);
    assert_eq!(
        run.mode.as_ref().map(|mode| mode.state.phase.as_str()),
        Some("done")
    );
    assert!(
        run.events
            .iter()
            .filter(|event| event.kind == "vote")
            .count()
            == 3,
        "every private ballot must be recorded in the run timeline"
    );
    let state: serde_json::Value =
        serde_json::from_str(&rt.store.run_mode_state(run_id).unwrap().unwrap()).unwrap();
    assert_eq!(
        state["idea_batches"].as_array().map(Vec::len),
        Some(9),
        "all generation waves must remain append-only in the IdeaSet"
    );
    assert_eq!(state["vote_count"], 3);
    assert_eq!(state["votes"].as_array().map(Vec::len), Some(3));
    assert!(
        state["brainstorm_summary"]
            .as_str()
            .is_some_and(|summary| summary.contains("Primary"))
    );
}

fn enter_fallback_voting(rt: &mut TeamRuntime) -> RunId {
    rt.config.modes.brainstorm = Some(BrainstormModeConfig {
        generation_rounds: Some(2),
        ..BrainstormModeConfig::default()
    });
    let planner = MemberId::new("planner");
    let builder = MemberId::new("builder");
    let reviewer = MemberId::new("reviewer");
    let started = rt.on_ui_command(run_brainstorm("fallback candidates"));
    let run_id = find_run_id(&started);
    let next_round = complete_all(
        rt,
        &[
            (planner, "planner free-text idea"),
            (builder, "builder free-text idea"),
            (reviewer, "reviewer free-text idea"),
        ],
    );
    assert_eq!(next_round.actions.len(), 3);
    let voting = complete_all(
        rt,
        &[
            (MemberId::new("planner"), "planner second idea"),
            (MemberId::new("builder"), "builder second idea"),
            (MemberId::new("reviewer"), "reviewer second idea"),
        ],
    );
    assert!(voting.actions.iter().all(|action| {
        action.prompt.contains("[R1-A#1]")
            && action.prompt.contains("[R1-B#1]")
            && action.prompt.contains("[R1-C#1]")
            && action.prompt.contains("[R2-A#1]")
    }));
    run_id
}

#[test]
fn brainstorm_rejects_well_formed_but_unknown_candidate_ids() {
    let mut rt = plan_runtime();
    let planner = MemberId::new("planner");
    let run_id = enter_fallback_voting(&mut rt);

    let rejected = rt.on_agent_event(
        &planner,
        AgentEvent::MessageCompleted(
            "@@brainstorm_vote {\"ranked\":[\"R99-Z#1\"],\"summary\":\"ghost\"}".to_string(),
        ),
    );

    assert!(rejected.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Notice(text)
            if text.contains("unknown brainstorm candidate") && text.contains("R99-Z#1")
    )));
    assert!(rt.mode_sessions[&run_id].votes.is_empty());
    let state: serde_json::Value =
        serde_json::from_str(&rt.store.run_mode_state(run_id).unwrap().unwrap()).unwrap();
    assert_eq!(state["vote_count"], 0);
    assert_eq!(state["votes"].as_array().map(Vec::len), Some(0));
    assert!(
        rt.store
            .run(run_id)
            .unwrap()
            .events
            .iter()
            .all(|event| event.kind != "vote")
    );
}

#[test]
fn brainstorm_vote_updates_memory_only_after_atomic_persistence() {
    let path = std::env::temp_dir().join(format!(
        "asterline-vote-atomic-failure-{}.sqlite3",
        std::process::id()
    ));
    remove_sqlite_test_files(&path);
    let mut rt =
        TeamRuntime::new(plan_team(), SqliteStore::open(&path).unwrap()).with_approvals(false);
    let planner = MemberId::new("planner");
    let run_id = enter_fallback_voting(&mut rt);
    let external = Connection::open(&path).unwrap();
    external
        .execute_batch(
            "CREATE TRIGGER fail_vote_state
             BEFORE UPDATE OF mode_state ON runs
             BEGIN SELECT RAISE(ABORT, 'mode state unavailable'); END;",
        )
        .unwrap();

    let rejected = rt.on_agent_event(
        &planner,
        AgentEvent::MessageCompleted(
            "@@brainstorm_vote {\"ranked\":[\"R1-A#1\",\"R1-B#1\"]}".to_string(),
        ),
    );

    assert!(rejected.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Notice(text) if text.contains("could not save a brainstorm vote")
    )));
    assert!(rt.mode_sessions[&run_id].votes.is_empty());
    let state: serde_json::Value =
        serde_json::from_str(&rt.store.run_mode_state(run_id).unwrap().unwrap()).unwrap();
    assert_eq!(state["vote_count"], 0);
    assert_eq!(state["votes"].as_array().map(Vec::len), Some(0));
    assert!(
        rt.store
            .run(run_id)
            .unwrap()
            .events
            .iter()
            .all(|event| event.kind != "vote")
    );
    drop(external);
    drop(rt);
    remove_sqlite_test_files(&path);
}

#[test]
fn brainstorm_respects_configured_generation_budget() {
    let mut rt = plan_runtime();
    rt.config.modes.brainstorm = Some(BrainstormModeConfig {
        generation_rounds: Some(2),
        ideas_per_round: Some(6),
        ..BrainstormModeConfig::default()
    });
    let planner = MemberId::new("planner");
    let builder = MemberId::new("builder");
    let reviewer = MemberId::new("reviewer");

    let start = rt.on_ui_command(run_brainstorm("two waves"));
    assert!(
        start
            .actions
            .iter()
            .all(|action| action.prompt.contains("at least 6"))
    );
    let stretch = complete_all(
        &mut rt,
        &[
            (planner.clone(), "p seed"),
            (builder.clone(), "b seed"),
            (reviewer.clone(), "r seed"),
        ],
    );
    assert!(
        stretch
            .actions
            .iter()
            .all(|action| action.prompt.contains(BRAINSTORM_STRETCH_HINT))
    );
    let vote = complete_all(
        &mut rt,
        &[
            (planner, "p stretch"),
            (builder, "b stretch"),
            (reviewer, "r stretch"),
        ],
    );
    assert_eq!(vote.actions.len(), 3);
    assert!(
        vote.actions
            .iter()
            .all(|action| action.prompt.contains(BRAINSTORM_VOTE_HINT))
    );
    assert_eq!(latest_run(&rt).status, RunStatus::Running);
}

#[test]
fn brainstorm_roles_are_only_the_participant_set() {
    let config = TeamConfig::new("pair", "/tmp/ws")
        .with_member(TeamMember::new(
            "alice",
            "Alice",
            BackendKind::Codex,
            "impl",
        ))
        .with_member(TeamMember::new(
            "bob",
            "Bob",
            BackendKind::Claude,
            "research",
        ));
    let (roles, _) = resolve_mode_roles(&config, CollabMode::Brainstorm).unwrap();
    assert_eq!(
        roles.participants,
        vec![MemberId::new("alice"), MemberId::new("bob")]
    );
}

#[test]
fn brainstorm_single_participant_refused() {
    let mut rt = plan_runtime();
    rt.config.modes.brainstorm = Some(BrainstormModeConfig {
        participants: Some(vec![MemberId::new("builder")]),
        ..BrainstormModeConfig::default()
    });
    let step = rt.on_ui_command(run_brainstorm("solo"));
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::Notice(text) if text.contains("at least two participants")
    )));
    assert!(step.actions.is_empty());
}

#[test]
fn brainstorm_resume_mid_generation_preserves_prior_ideas() {
    let mut rt = plan_runtime();
    let planner = MemberId::new("planner");
    let builder = MemberId::new("builder");
    let reviewer = MemberId::new("reviewer");
    let start = rt.on_ui_command(run_brainstorm("resume generation"));
    let run_id = find_run_id(&start);
    complete_all(
        &mut rt,
        &[
            (planner.clone(), "p seed"),
            (builder.clone(), "b seed"),
            (reviewer.clone(), "r seed"),
        ],
    );

    rt.on_ui_command(UiCommand::Cancel { member: None });
    for member in [&planner, &builder, &reviewer] {
        let _ = rt.on_agent_event(
            member,
            AgentEvent::Exited {
                code: None,
                ok: false,
            },
        );
    }
    assert_eq!(rt.store.run(run_id).unwrap().status, RunStatus::Blocked);

    let resumed = rt.on_ui_command(UiCommand::ContinueRun {
        run_id: Some(run_id),
        note: None,
    });
    assert_eq!(resumed.actions.len(), 3);
    assert!(resumed.actions.iter().all(|action| {
        action.prompt.contains(BRAINSTORM_BUILD_HINT) && action.prompt.contains("seed")
    }));
}

#[test]
fn continue_refuses_legacy_roundtable_mode() {
    let mut rt = plan_runtime();
    let builder = MemberId::new("builder");
    let run = rt
        .store
        .insert_run_with_raw_mode(
            "old roundtable topic",
            Some(&builder),
            "roundtable",
            Some(r#"{"phase":"rounds","round":1,"rounds":2}"#),
            RunStatus::Done,
        )
        .unwrap();
    assert_eq!(run.legacy_mode.as_deref(), Some("roundtable"));
    assert!(run.mode.is_none());

    let step = rt.on_ui_command(UiCommand::ContinueRun {
        run_id: Some(run.id),
        note: None,
    });
    assert!(
        step.actions.is_empty(),
        "legacy mode must not dispatch (got {} actions)",
        step.actions.len()
    );
    assert!(
        step.events.iter().any(|e| matches!(
            e,
            RuntimeEvent::Notice(text)
                if text.contains(&run.id.to_string())
                    && text.contains("older Asterline")
                    && text.contains("roundtable")
        )),
        "expected legacy-mode notice: {:?}",
        step.events
    );
    // Status must stay unchanged (no silent team continue).
    assert_eq!(rt.store.run(run.id).unwrap().status, RunStatus::Done);
}

#[test]
fn plan_resume_after_abort_redispatches_leader() {
    let mut rt = plan_runtime();
    let planner = MemberId::new("planner");

    let step = rt.on_ui_command(run_plan("resume plan"));
    let run_id = find_run_id(&step);
    rt.on_ui_command(UiCommand::Cancel { member: None });
    let _ = rt.on_agent_event(
        &planner,
        AgentEvent::Exited {
            code: None,
            ok: false,
        },
    );
    assert_eq!(rt.store.run(run_id).unwrap().status, RunStatus::Blocked);

    let step = rt.on_ui_command(UiCommand::ContinueRun {
        run_id: Some(run_id),
        note: None,
    });
    assert!(
        step.actions.iter().any(|a| a.member == planner),
        "continue should re-dispatch the leader: {:?}",
        step.actions.iter().map(|a| &a.member).collect::<Vec<_>>()
    );
    let run = rt.store.run(run_id).unwrap();
    assert_eq!(run.status, RunStatus::Running);
}

#[test]
fn continue_refuses_when_mode_member_left_roster() {
    let mut rt = runtime();
    let step = rt.on_ui_command(run_mode("review this"));
    let run_id = find_run_id(&step);
    rt.on_ui_command(UiCommand::Cancel { member: None });
    // Drop the reviewer from the roster, then try to resume the blocked run.
    rt.on_ui_command(UiCommand::ReplaceTeam {
        members: vec![TeamMember::new(
            "builder",
            "Builder",
            BackendKind::Codex,
            "impl",
        )],
        default_target: None,
    });

    let step = rt.on_ui_command(UiCommand::ContinueRun {
        run_id: Some(run_id),
        note: None,
    });

    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::Notice(text) if text.contains("left the roster") && text.contains("reviewer")
    )));
    assert!(step.actions.is_empty(), "no dispatch to a missing member");
    assert_eq!(
        rt.store.run(run_id).unwrap().status,
        RunStatus::Blocked,
        "the run stays blocked instead of half-resuming"
    );
}

#[test]
fn manual_verify_on_active_mode_run_is_refused() {
    let mut rt = runtime();
    let step = rt.on_ui_command(run_mode("review this"));
    let run_id = find_run_id(&step);

    let step = rt.on_ui_command(UiCommand::VerifyRun {
        run_id: Some(run_id),
        command: Some("true".to_string()),
    });

    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::Notice(text) if text.contains("active mode run")
    )));
    assert!(step.verify_actions.is_empty());
}
