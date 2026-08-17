use super::super::*;
use super::*;

#[test]
fn backend_commands_are_never_rewritten_from_text_alone() {
    for backend in [
        BackendKind::Codex,
        BackendKind::Claude,
        BackendKind::Grok,
        BackendKind::Agy,
    ] {
        assert_eq!(
            normalize_backend_command(backend, "/model next".to_string()),
            "/model next"
        );
    }
}

#[test]
fn imported_transcript_merges_assistant_fragments_until_the_next_user_message() {
    let merged = coalesce_imported_assistant_messages(vec![
        ImportedMessage {
            from_user: true,
            text: "first question".to_string(),
        },
        ImportedMessage {
            from_user: false,
            text: "before the tool call".to_string(),
        },
        ImportedMessage {
            from_user: false,
            text: "after the tool call".to_string(),
        },
        ImportedMessage {
            from_user: true,
            text: "second question".to_string(),
        },
        ImportedMessage {
            from_user: false,
            text: "second answer".to_string(),
        },
    ]);

    assert_eq!(
        merged,
        vec![
            ImportedMessage {
                from_user: true,
                text: "first question".to_string(),
            },
            ImportedMessage {
                from_user: false,
                text: "before the tool call\n\nafter the tool call".to_string(),
            },
            ImportedMessage {
                from_user: true,
                text: "second question".to_string(),
            },
            ImportedMessage {
                from_user: false,
                text: "second answer".to_string(),
            },
        ]
    );
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
fn native_codex_approval_is_persisted_and_returns_the_user_decision_to_the_runner() {
    let mut rt = TeamRuntime::new(team(), SqliteStore::in_memory().unwrap()).with_approvals(true);
    rt.on_ui_command(user("make the change"));

    let requested = rt.on_agent_event(
        &MemberId::new("builder"),
        AgentEvent::NativeApprovalRequested {
            request_id: 41,
            action: "Codex command".to_string(),
            body: "git status".to_string(),
        },
    );
    let id = requested
        .events
        .iter()
        .find_map(|event| match event {
            RuntimeEvent::ApprovalRequested {
                id,
                member: Some(member),
                action,
                body,
            } if member == &MemberId::new("builder")
                && action == "Codex command"
                && body == "git status" =>
            {
                Some(*id)
            }
            _ => None,
        })
        .expect("native approval should be shown to the user");
    assert!(
        requested.actions.is_empty(),
        "the original turn stays paused"
    );

    let resolved = rt.on_ui_command(UiCommand::Approve {
        id,
        decision: ApprovalDecision::Approve,
    });
    assert!(
        resolved.actions.is_empty(),
        "do not enqueue a second prompt"
    );
    assert!(matches!(
        resolved.runner_controls.as_slice(),
        [RunnerControl::ResolveNativeApproval {
            member,
            request_id: 41,
            decision: ApprovalDecision::Approve,
        }] if member == &MemberId::new("builder")
    ));
}

#[test]
fn native_codex_approval_auto_passes_when_manual_approvals_are_off() {
    let mut rt = runtime();
    rt.on_ui_command(user("make the change"));

    let requested = rt.on_agent_event(
        &MemberId::new("builder"),
        AgentEvent::NativeApprovalRequested {
            request_id: 41,
            action: "Codex command".to_string(),
            body: "git status".to_string(),
        },
    );
    assert!(
        !requested
            .events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::ApprovalRequested { .. }))
    );
    assert!(matches!(
        requested.runner_controls.as_slice(),
        [RunnerControl::ResolveNativeApproval {
            member,
            request_id: 41,
            decision: ApprovalDecision::Approve,
        }] if member == &MemberId::new("builder")
    ));
}

#[test]
fn cancelling_a_member_rejects_its_native_codex_approval() {
    let mut rt = TeamRuntime::new(team(), SqliteStore::in_memory().unwrap()).with_approvals(true);
    rt.on_ui_command(user("make the change"));
    let requested = rt.on_agent_event(
        &MemberId::new("builder"),
        AgentEvent::NativeApprovalRequested {
            request_id: 7,
            action: "Codex file change".to_string(),
            body: "write src/main.rs".to_string(),
        },
    );
    let id = requested
        .events
        .iter()
        .find_map(|event| match event {
            RuntimeEvent::ApprovalRequested { id, .. } => Some(*id),
            _ => None,
        })
        .expect("native approval should be pending");

    let cancelled = rt.on_ui_command(UiCommand::Cancel {
        member: Some(MemberId::new("builder")),
    });
    assert!(cancelled.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::ApprovalResolved {
            id: resolved,
            decision: ApprovalDecision::Reject,
        } if *resolved == id
    )));
    assert!(matches!(
        cancelled.runner_controls.as_slice(),
        [RunnerControl::ResolveNativeApproval {
            member,
            request_id: 7,
            decision: ApprovalDecision::Reject,
        }] if member == &MemberId::new("builder")
    ));
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
    assert!(changed.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Notice(text)
            if text.contains("mode → review")
                && text.contains("next plain text starts this run")
                && text.contains("builder")
                && text.contains("reviewer")
                && text.contains("@member still sends directly")
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
            if text.contains("cannot start a new chat") && text.contains("press Esc")
    )));
}

#[test]
fn new_chat_keeps_mode_overrides_and_restarts_run_numbers() {
    use crate::domain::mode::ModesConfig;

    let mut rt = runtime();
    let overrides = ModesConfig {
        review: Some(ReviewModeConfig {
            reviewer: Some(MemberId::new("reviewer")),
            max_iterations: Some(6),
            ..ReviewModeConfig::default()
        }),
        ..ModesConfig::default()
    };
    rt.on_ui_command(UiCommand::SetModeOverrides {
        overrides: overrides.clone(),
    });
    let first = rt.on_ui_command(run_mode("first review"));
    let first_run = first
        .events
        .iter()
        .find_map(|event| match event {
            RuntimeEvent::RunUpdated { run } => Some(run.clone()),
            _ => None,
        })
        .expect("first review run");
    assert_eq!(first_run.number, 1);
    assert_eq!(first_run.label(), "run-1");
    let builder = first.actions[0].member.clone();

    rt.on_ui_command(UiCommand::Cancel { member: None });
    rt.on_agent_event(
        &builder,
        AgentEvent::Exited {
            code: Some(0),
            ok: true,
        },
    );

    let reset = rt.on_ui_command(UiCommand::NewSession);
    assert!(
        reset
            .events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::SessionReset))
    );
    assert!(reset.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::ModesUpdated { overrides: next, .. } if next == &overrides
    )));
    assert_eq!(rt.active_mode(), TerminalMode::Normal);

    let second = rt.on_ui_command(run_mode("second review"));
    let second_run = second
        .events
        .iter()
        .find_map(|event| match event {
            RuntimeEvent::RunUpdated { run } => Some(run.clone()),
            _ => None,
        })
        .expect("second review run");
    assert_eq!(second_run.number, 1, "new chat must restart at run-1");
    assert_eq!(second_run.label(), "run-1");
    assert_ne!(second_run.id, first_run.id);
    assert!(
        second.events.iter().any(|event| matches!(
            event,
            RuntimeEvent::Notice(text)
                if text.contains("review run-1 started")
                    && !text.contains(&format!("review {} started", second_run.id))
        )),
        "chat notices must use the conversation-local handle, not the raw id: {:?}",
        second.events
    );
}

#[test]
fn new_chat_leaves_the_new_transcript_empty() {
    let mut rt = runtime();

    let reset = rt.on_ui_command(UiCommand::NewSession);

    assert!(
        reset
            .events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::SessionReset))
    );
    assert!(
        !reset
            .events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::Notice(_))),
        "a post-reset notice would become the first item in the fresh chat"
    );
}

#[test]
fn new_chat_resets_members_to_fresh_policy_and_clears_sessions() {
    let mut rt = runtime();
    let builder = MemberId::new("builder");
    rt.on_ui_command(user("initial prompt"));
    rt.on_agent_event(
        &builder,
        AgentEvent::SessionDiscovered(AgentSessionId("bound-session-1".to_string())),
    );
    rt.on_agent_event(
        &builder,
        AgentEvent::Exited {
            code: Some(0),
            ok: true,
        },
    );

    let reset = rt.on_ui_command(UiCommand::NewSession);
    assert!(
        reset
            .events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::SessionReset))
    );

    let ready = rt.ready_event();
    if let RuntimeEvent::Ready { members, .. } = ready {
        for m in members {
            assert_eq!(m.session, None);
            assert_eq!(m.session_policy, SessionPolicy::Fresh);
            assert_eq!(m.status, MemberStatus::Idle);
        }
    } else {
        panic!("expected Ready event");
    }

    let next_turn = rt.on_ui_command(user("start of fresh chat"));
    assert_eq!(next_turn.actions[0].session, None);
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
                    ChatItem::User { body, .. } if body == "original question"
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
fn reasoning_section_break_clears_progress_and_starts_fresh() {
    let mut rt = runtime();
    let builder = MemberId::new("builder");
    rt.on_ui_command(user("explain the plan"));

    rt.on_agent_event(&builder, AgentEvent::Reasoning("first section".to_string()));
    let boundary = rt.on_agent_event(&builder, AgentEvent::ReasoningSectionBreak);
    assert!(boundary.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::ReasoningCompleted { member } if member == &builder
    )));

    let next = rt.on_agent_event(
        &builder,
        AgentEvent::Reasoning("second section".to_string()),
    );
    assert!(next.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Reasoning { member, text }
            if member == &builder && text == "second section"
    )));
    assert!(
        !rt.store
            .replay_chat()
            .unwrap()
            .iter()
            .any(|item| matches!(item, ChatItem::Thinking { .. }))
    );
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
    let files = vec![FileChangeItem::new("src/parser.rs", "update")];

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
            && member.system_prompt.as_deref().unwrap_or("").contains("Asterline team skill")
    )));
    let persisted = step.persist_team.expect("team persisted");
    let qa = persisted.member(&MemberId::new("qa")).unwrap();
    assert_eq!(qa.model.as_deref(), Some("gpt-5-codex"));
    assert_eq!(qa.effort, Some(Effort::High));
    assert_eq!(qa.system_prompt, None);
}

#[test]
fn agent_teammate_addition_skips_relay_approval() {
    let mut rt = TeamRuntime::new(team(), SqliteStore::in_memory().unwrap());
    rt.on_ui_command(user("plan it"));
    let builder = MemberId::new("builder");

    let added = rt.on_agent_event(
        &builder,
        AgentEvent::MessageCompleted(
            r#"@@team_member {"id":"qa","display_name":"QA","backend":"codex","role":"tests","sandbox":"danger-full-access"}"#
                .to_string(),
        ),
    );

    assert!(rt.config.member(&MemberId::new("qa")).is_some());
    assert!(added.runner_changes.iter().any(|change| matches!(
        change,
        RunnerChange::Upsert { member, .. } if member.id == MemberId::new("qa")
    )));
    assert!(
        !added.events.iter().any(|event| matches!(
            event,
            RuntimeEvent::ApprovalRequested { action, .. } if action == "team_member"
        )),
        "adding a teammate must not wait for /approve"
    );
}

#[test]
fn team_mode_default_refuses_agent_teammate_addition() {
    let mut rt = TeamRuntime::new(team(), SqliteStore::in_memory().unwrap());
    start_team(&mut rt, "ship the parser");
    let builder = MemberId::new("builder");

    let refused = rt.on_agent_event(
        &builder,
        AgentEvent::MessageCompleted(
            r#"@@team_member {"id":"qa","display_name":"QA","backend":"codex","role":"tests"}"#
                .to_string(),
        ),
    );

    assert!(rt.config.member(&MemberId::new("qa")).is_none());
    assert!(refused.runner_changes.is_empty());
    assert!(refused.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Notice(text)
            if text.contains("locked to the current roster") && text.contains("qa")
    )));
}

#[test]
fn team_mode_free_add_joins_immediately_without_approval() {
    let mut rt = TeamRuntime::new(team(), SqliteStore::in_memory().unwrap());
    rt.config.modes.team = Some(TeamModeConfig {
        allow_add_members: Some(true),
        ..TeamModeConfig::default()
    });
    start_team(&mut rt, "ship the parser");
    let builder = MemberId::new("builder");

    let added = rt.on_agent_event(
        &builder,
        AgentEvent::MessageCompleted(
            r#"@@team_member {"id":"qa","display_name":"QA","backend":"codex","role":"tests"}"#
                .to_string(),
        ),
    );

    assert!(rt.config.member(&MemberId::new("qa")).is_some());
    assert!(added.runner_changes.iter().any(|change| matches!(
        change,
        RunnerChange::Upsert { member, .. } if member.id == MemberId::new("qa")
    )));
    assert!(
        !added
            .events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::ApprovalRequested { .. }))
    );
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
    assert_eq!(
        rt.members.get(&builder).map(|state| state.status),
        Some(MemberStatus::Running),
        "queue depth must not replace the active run status"
    );
    assert!(!step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::MemberStatus {
            status: MemberStatus::Queued,
            ..
        }
    )));
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::QueueUpdated { prompts, .. } if prompts == &["second".to_string()]
    )));
    assert!(
        !step.events.iter().any(|event| matches!(
            event,
            RuntimeEvent::UserMessage { body, .. } if body == "second"
        )),
        "an unsent queued message must stay out of chat history"
    );
    assert!(!rt.store.replay_chat().unwrap().iter().any(|item| matches!(
        item,
        ChatItem::User { body, .. } if body == "second"
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
            .any(|a| a.prompt.contains("second") && !a.prompt.contains("$asterline-team"))
    );
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::UserMessage { body, .. } if body == "second"
    )));
    assert!(rt.store.replay_chat().unwrap().iter().any(|item| matches!(
        item,
        ChatItem::User { body, .. } if body == "second"
    )));
}

#[test]
fn queued_message_for_multiple_members_enters_chat_once() {
    let mut rt = runtime();
    let builder = MemberId::new("builder");
    let reviewer = MemberId::new("reviewer");
    rt.on_ui_command(UiCommand::UserMessage {
        target: MessageTarget::Member(builder.clone()),
        body: "builder first".to_string(),
    });
    rt.on_ui_command(UiCommand::UserMessage {
        target: MessageTarget::Member(reviewer.clone()),
        body: "reviewer first".to_string(),
    });

    let queued = rt.on_ui_command(UiCommand::UserMessage {
        target: MessageTarget::All,
        body: "queued for both".to_string(),
    });
    assert!(!queued.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::UserMessage { body, .. } if body == "queued for both"
    )));

    let builder_exit = rt.on_agent_event(
        &builder,
        AgentEvent::Exited {
            code: Some(0),
            ok: true,
        },
    );
    assert_eq!(
        builder_exit
            .events
            .iter()
            .filter(|event| matches!(
                event,
                RuntimeEvent::UserMessage { body, .. } if body == "queued for both"
            ))
            .count(),
        1
    );

    let reviewer_exit = rt.on_agent_event(
        &reviewer,
        AgentEvent::Exited {
            code: Some(0),
            ok: true,
        },
    );
    assert!(!reviewer_exit.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::UserMessage { body, .. } if body == "queued for both"
    )));
}

#[test]
fn cancel_keeps_queued_prompt_and_sends_it_after_the_run_exits() {
    let mut rt = runtime();
    let builder = MemberId::new("builder");
    rt.on_ui_command(UiCommand::UserMessage {
        target: MessageTarget::Member(builder.clone()),
        body: "first".to_string(),
    });
    rt.on_ui_command(UiCommand::UserMessage {
        target: MessageTarget::Member(builder.clone()),
        body: "second".to_string(),
    });

    let cancelled = rt.on_ui_command(UiCommand::Cancel {
        member: Some(builder.clone()),
    });
    assert!(
        cancelled.actions.is_empty(),
        "Esc only interrupts the live run"
    );
    assert!(cancelled.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Notice(text) if text.contains("queued message")
    )));
    assert!(
        !cancelled
            .events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::QueueUpdated { prompts, .. } if prompts.is_empty())),
        "cancel must not drain the queue"
    );

    let exited = rt.on_agent_event(
        &builder,
        AgentEvent::Exited {
            code: Some(1),
            ok: false,
        },
    );
    assert!(
        exited
            .actions
            .iter()
            .any(|action| action.prompt.contains("second")),
        "the queued prompt starts after interrupt"
    );
}

#[test]
fn edit_queued_prompt_returns_the_last_unsent_body() {
    let mut rt = runtime();
    let builder = MemberId::new("builder");
    rt.on_ui_command(UiCommand::UserMessage {
        target: MessageTarget::Member(builder.clone()),
        body: "first".to_string(),
    });
    rt.on_ui_command(UiCommand::UserMessage {
        target: MessageTarget::Member(builder.clone()),
        body: "keep".to_string(),
    });
    rt.on_ui_command(UiCommand::UserMessage {
        target: MessageTarget::Member(builder.clone()),
        body: "edit me".to_string(),
    });

    let edited = rt.on_ui_command(UiCommand::EditQueuedPrompt {
        member: Some(builder.clone()),
    });
    assert!(edited.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::QueuedPromptReturned { body, .. } if body == "edit me"
    )));
    assert!(edited.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::QueueUpdated { prompts, .. }
            if prompts == &["keep".to_string()]
    )));

    let empty = rt.on_ui_command(UiCommand::EditQueuedPrompt { member: None });
    assert!(empty.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::QueuedPromptReturned { body, .. } if body == "keep"
    )));
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
fn prompt_keywords_do_not_hold_a_user_message() {
    let mut rt = TeamRuntime::new(team(), SqliteStore::in_memory().unwrap());
    let step = rt.on_ui_command(user("run git push origin main"));

    assert!(
        !step
            .events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::ApprovalRequested { .. })),
        "prompt-keyword gating was removed"
    );
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
fn tools_commit_streamed_text_so_the_final_reply_starts_after_them() {
    let mut rt = runtime();
    rt.on_ui_command(user("what is asterline"));
    let builder = MemberId::new("builder");

    rt.on_agent_event(&builder, AgentEvent::MessageStarted);
    rt.on_agent_event(
        &builder,
        AgentEvent::TextDelta("I'll look it up.".to_string()),
    );
    let tool = rt.on_agent_event(
        &builder,
        AgentEvent::ToolStarted {
            id: "search-1".to_string(),
            name: "search".to_string(),
            summary: "asterline".to_string(),
        },
    );
    assert!(tool.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::MessageCompleted { text, .. } if text == "I'll look it up."
    )));
    rt.on_agent_event(
        &builder,
        AgentEvent::ToolCompleted {
            id: "search-1".to_string(),
            ok: true,
            summary: "hits".to_string(),
        },
    );
    rt.on_agent_event(
        &builder,
        AgentEvent::TextDelta("Asterline is a team TUI.".to_string()),
    );
    rt.on_agent_event(
        &builder,
        AgentEvent::MessageCompleted("Asterline is a team TUI.".to_string()),
    );

    let replay = rt.store.replay_chat().unwrap();
    let kinds: Vec<&str> = replay
        .iter()
        .filter_map(|item| match item {
            ChatItem::Agent { text, .. } if text == "I'll look it up." => Some("preamble"),
            ChatItem::Tool { name, .. } if name == "search" => Some("tool"),
            ChatItem::Agent { text, .. } if text == "Asterline is a team TUI." => Some("reply"),
            _ => None,
        })
        .collect();
    assert_eq!(kinds, ["preamble", "tool", "reply"], "{replay:?}");
}

#[test]
fn empty_terminal_message_keeps_the_streamed_answer() {
    let mut rt = runtime();
    rt.on_ui_command(user("hi"));
    let builder = MemberId::new("builder");

    rt.on_agent_event(&builder, AgentEvent::MessageStarted);
    rt.on_agent_event(
        &builder,
        AgentEvent::TextDelta("streamed answer".to_string()),
    );
    let step = rt.on_agent_event(&builder, AgentEvent::MessageCompleted(String::new()));

    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::MessageCompleted { text, .. } if text == "streamed answer"
    )));
}

#[test]
fn control_only_completion_discards_thinking_progress() {
    let mut rt = runtime();
    rt.on_ui_command(user("hi"));
    let builder = MemberId::new("builder");

    rt.on_agent_event(
        &builder,
        AgentEvent::Reasoning("Planer 的方案已经到了".to_string()),
    );
    rt.on_agent_event(
        &builder,
        AgentEvent::MessageCompleted(
            r#"@@team_message {"to":"planer","kind":"reply","body":"ack"}"#.to_string(),
        ),
    );

    assert!(
        !rt.store
            .replay_chat()
            .unwrap()
            .iter()
            .any(|item| matches!(item, ChatItem::Thinking { .. }))
    );
}

#[test]
fn thought_only_exit_discards_thinking_progress() {
    let mut rt = runtime();
    rt.on_ui_command(user("hi"));
    let builder = MemberId::new("builder");

    rt.on_agent_event(
        &builder,
        AgentEvent::Reasoning("this is the actual reply".to_string()),
    );
    rt.on_agent_event(
        &builder,
        AgentEvent::Exited {
            code: Some(0),
            ok: true,
        },
    );

    assert!(
        !rt.store
            .replay_chat()
            .unwrap()
            .iter()
            .any(|item| matches!(item, ChatItem::Thinking { .. }))
    );
}

#[test]
fn tools_discard_thinking_progress() {
    let mut rt = runtime();
    rt.on_ui_command(user("hi"));
    let builder = MemberId::new("builder");

    rt.on_agent_event(&builder, AgentEvent::Reasoning("read roster".to_string()));
    rt.on_agent_event(
        &builder,
        AgentEvent::ToolStarted {
            id: "t1".to_string(),
            name: "read".to_string(),
            summary: "roster.md".to_string(),
        },
    );
    rt.on_agent_event(
        &builder,
        AgentEvent::ToolCompleted {
            id: "t1".to_string(),
            ok: true,
            summary: "ok".to_string(),
        },
    );
    rt.on_agent_event(&builder, AgentEvent::Reasoning("read mode.rs".to_string()));
    rt.on_agent_event(
        &builder,
        AgentEvent::Exited {
            code: Some(0),
            ok: true,
        },
    );

    let thoughts: Vec<_> = rt
        .store
        .replay_chat()
        .unwrap()
        .into_iter()
        .filter_map(|item| match item {
            ChatItem::Thinking { text, .. } => Some(text),
            _ => None,
        })
        .collect();
    assert!(thoughts.is_empty());
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
    assert!(!step.actions[0].prompt.contains("$asterline-team"));
}

#[test]
fn live_roster_is_written_to_workspace_not_every_codex_prompt() {
    let dir = std::env::temp_dir().join(format!(
        "asterline-roster-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let mut builder = TeamMember::new("builder", "Builder", BackendKind::Codex, "impl");
    builder.model = Some("gpt-5-codex".to_string());
    builder.effort = Some(Effort::High);
    let mut reviewer = TeamMember::new("reviewer", "Reviewer", BackendKind::Claude, "review");
    reviewer.model = Some("sonnet".to_string());
    reviewer.effort = Some(Effort::Medium);
    reviewer.cwd = Some(PathBuf::from("/tmp/review"));
    let mut config = TeamConfig::new("mixed", &dir)
        .with_member(builder)
        .with_member(reviewer);
    config.default_target = Some(DefaultTarget::Member(MemberId::new("builder")));
    let mut rt = TeamRuntime::new(config, SqliteStore::in_memory().unwrap()).with_approvals(false);

    let idle = std::fs::read_to_string(dir.join(".asterline/roster.md")).unwrap();
    assert!(idle.contains("Default target: builder"));
    assert!(idle.contains(
        "id=builder display_name=\"Builder\" backend=codex role=\"impl\" status=idle model=gpt-5-codex effort=high"
    ));
    assert!(idle.contains(
        "id=reviewer display_name=\"Reviewer\" backend=claude role=\"review\" status=idle model=sonnet effort=medium cwd=\"/tmp/review\""
    ));

    let step = rt.on_ui_command(user("who is on the team?"));
    let prompt = &step.actions[0].prompt;
    assert!(!prompt.contains("$asterline-team"));
    assert!(!prompt.contains(".asterline/roster.md"));
    assert!(!prompt.contains("id=builder display_name="));
    assert!(!prompt.contains("status=running"));

    let running = std::fs::read_to_string(dir.join(".asterline/roster.md")).unwrap();
    assert!(running.contains(
        "id=builder display_name=\"Builder\" backend=codex role=\"impl\" status=running model=gpt-5-codex effort=high"
    ));
    assert!(running.contains("status=idle model=sonnet"));

    std::fs::remove_dir_all(&dir).ok();
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
