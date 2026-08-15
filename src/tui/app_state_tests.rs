use super::*;
use crate::domain::event::{
    AgentSessionId, ApprovalDecision, ConversationSummary, MemberSummary, RunId, RunStatus,
    RunStepSummary, RunSummary, RunVerification, TurnId,
};

fn ready() -> RuntimeEvent {
    RuntimeEvent::Ready {
        team: "mixed".to_string(),
        workspace: "/tmp/ws".to_string(),
        default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
        runs: Vec::new(),
        members: vec![MemberSummary {
            id: MemberId::new("builder"),
            display_name: "Builder".to_string(),
            backend: BackendKind::Codex,
            role: "impl".to_string(),
            status: MemberStatus::Idle,
            session: None,
            cwd: String::new(),
            model: None,
            effort: None,
            sandbox: SandboxPolicy::ReadOnly,
            permission_mode: Some(PermissionMode::Default),
            session_policy: SessionPolicy::Resume,
        }],
    }
}

#[test]
fn ready_populates_header() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    assert_eq!(state.team(), "mixed");
    assert_eq!(state.members().len(), 1);
}

#[test]
fn active_member_profile_uses_the_discovered_cli_default() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    state.model_catalog.seed(
        BackendKind::Codex,
        Path::new("/tmp/ws"),
        vec!["gpt-5.6-sol".to_string()],
    );

    assert_eq!(
        state.member_runtime_profile(&state.members[0]),
        "model: gpt-5.6-sol"
    );
}

#[test]
fn model_catalog_warms_every_detected_backend_once_per_ast_process() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    // Seed every startup key so this lifecycle test exercises the detected
    // backend fan-out without launching real CLI subprocesses.
    for backend in [
        BackendKind::Codex,
        BackendKind::Claude,
        BackendKind::Grok,
        BackendKind::Agy,
    ] {
        state.model_catalog.seed(
            backend,
            Path::new("/tmp/ws"),
            vec![backend.as_str().to_string()],
        );
    }
    let (tx, rx) = std::sync::mpsc::channel();
    state.model_catalog_detection = Some(rx);
    tx.send(crate::domain::config::DetectedBackends {
        codex: true,
        claude: true,
        grok: true,
        agy: true,
    })
    .unwrap();
    state.warm_model_catalog_once();
    assert!(state.model_catalog_warmed);
    for backend in [
        BackendKind::Codex,
        BackendKind::Claude,
        BackendKind::Grok,
        BackendKind::Agy,
    ] {
        assert!(state.model_catalog.contains(backend, Path::new("/tmp/ws")));
    }

    // A later roster refresh in the same process must not launch a second
    // startup sweep.
    state.warm_model_catalog_once();
    assert!(state.model_catalog_warmed);
}

#[test]
fn runtime_unavailable_is_visible_and_idempotent() {
    let mut state = AppState::new(Vec::new());

    state.mark_runtime_unavailable();

    assert!(!state.runtime_available());
    assert!(matches!(
        state.chat().last(),
        Some(ChatItem::Error { member: None, message })
            if message.contains("input is disabled")
    ));
    let chat_len = state.chat().len();
    state.mark_runtime_unavailable();
    assert_eq!(state.chat().len(), chat_len);
}

#[test]
fn runtime_unavailable_disables_stale_controls_and_attach() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    state.apply(RuntimeEvent::RunUpdated {
        run: RunSummary {
            id: RunId(9),
            goal: "verify release".to_string(),
            status: RunStatus::Verifying,
            coordinator: None,
            verification: None,
            created_at: "2026-08-12 00:00:00".to_string(),
            updated_at: "2026-08-12 00:00:00".to_string(),
            attempt: 1,
            events: Vec::new(),
            steps: Vec::new(),
            mode: None,
            legacy_mode: None,
        },
    });

    state.mark_runtime_unavailable();

    assert!(!state.has_cancelable_work());
    assert!(state.request_attach(0).is_none());
    assert!(state.take_attach_request().is_none());
    assert!(state.chat().iter().any(|item| matches!(
        item,
        ChatItem::Notice { text } if text.contains("runtime has stopped")
    )));
}

#[test]
fn new_chat_resets_visible_mode_to_normal() {
    let mut state = AppState::new(Vec::new());
    state.apply(RuntimeEvent::ModeChanged {
        mode: TerminalMode::Brainstorm,
    });

    state.apply(RuntimeEvent::SessionReset);

    assert_eq!(state.active_mode(), TerminalMode::Normal);
}

#[test]
fn new_chat_clears_conversation_scoped_controls() {
    let mut state = AppState::new(vec![ChatItem::Notice {
        text: "old chat".to_string(),
    }]);
    state.apply(RuntimeEvent::ApprovalRequested {
        id: ApprovalId(7),
        member: None,
        action: "shell".to_string(),
        body: "publish".to_string(),
    });
    state.apply(RuntimeEvent::RoutePaused {
        turn: TurnId(1),
        from: MemberId::new("builder"),
        to: vec!["reviewer".to_string()],
        reason: "relay paused".to_string(),
        queued: 2,
    });
    state.set_find("old");
    state.toggle_drawer(Drawer::Logs);

    state.apply(RuntimeEvent::SessionReset);

    assert!(state.pending_approvals().is_empty());
    assert_eq!(state.paused_routes(), 0);
    assert!(!state.find_active());
    assert_eq!(state.drawer(), None);
}

#[test]
fn route_queue_update_clears_resolved_or_aborted_routes() {
    let mut state = AppState::new(Vec::new());
    state.apply(RuntimeEvent::RoutePaused {
        turn: TurnId(1),
        from: MemberId::new("builder"),
        to: vec!["reviewer".to_string()],
        reason: "relay paused".to_string(),
        queued: 2,
    });
    assert_eq!(state.paused_routes(), 2);

    state.apply(RuntimeEvent::RouteQueueUpdated { queued: 1 });
    assert_eq!(state.paused_routes(), 1);
    state.apply(RuntimeEvent::RouteQueueUpdated { queued: 0 });
    assert_eq!(state.paused_routes(), 0);
}

#[test]
fn resume_choices_open_picker_and_selected_chat_replaces_transcript() {
    let mut state = AppState::new(vec![ChatItem::User {
        body: "current".to_string(),
        targets: Vec::new(),
        interrupted: Vec::new(),
    }]);
    state.apply(RuntimeEvent::ResumeChoices {
        conversations: vec![
            ConversationSummary {
                id: 7,
                created_at: "2026-07-28 12:00:00".to_string(),
                preview: "newer saved question".to_string(),
                message_count: 4,
                member_count: 2,
            },
            ConversationSummary {
                id: 3,
                created_at: "2026-07-27 09:00:00".to_string(),
                preview: "older saved question".to_string(),
                message_count: 9,
                member_count: 3,
            },
        ],
    });

    assert_eq!(state.drawer(), Some(Drawer::Resume));
    assert_eq!(
        state.selected_resume_command(),
        Some(UiCommand::ResumeConversation { conversation: 7 })
    );
    state.select_next_resume();
    assert_eq!(
        state.selected_resume_command(),
        Some(UiCommand::ResumeConversation { conversation: 3 })
    );

    state.apply(RuntimeEvent::ConversationResumed {
        conversation: 3,
        chat: vec![ChatItem::User {
            body: "older saved question".to_string(),
            targets: Vec::new(),
            interrupted: Vec::new(),
        }],
    });
    assert_eq!(state.drawer(), None);
    assert_eq!(
        state.chat(),
        &[ChatItem::User {
            body: "older saved question".to_string(),
            targets: Vec::new(),
            interrupted: Vec::new(),
        }]
    );
}

#[test]
fn team_field_paste_goes_to_the_focused_editor() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    state.toggle_drawer(Drawer::Team);
    state.handle_team_editor_key(KeyCode::Enter, KeyModifiers::NONE);
    state.handle_team_editor_key(KeyCode::Enter, KeyModifiers::NONE);

    assert!(state.insert_team_editor_text(" Lead\nEngineer"));
    assert_eq!(
        state
            .team_editor()
            .and_then(TeamEditor::editing)
            .map(|edit| edit.buffer.as_str()),
        Some("Builder Lead Engineer")
    );
    assert!(state.composer().is_empty());
}

#[test]
fn run_updates_insert_then_replace() {
    let mut state = AppState::new(Vec::new());
    let run = RunSummary {
        id: RunId(1),
        goal: "ship parser".to_string(),
        status: RunStatus::Running,
        coordinator: Some(MemberId::new("builder")),
        verification: None,
        created_at: "2026-06-28 10:00:00".to_string(),
        updated_at: "2026-06-28 10:00:00".to_string(),
        attempt: 1,
        events: Vec::new(),
        steps: Vec::new(),
        mode: None,
        legacy_mode: None,
    };

    state.apply(RuntimeEvent::RunUpdated { run: run.clone() });
    assert_eq!(state.runs(), std::slice::from_ref(&run));
    assert_eq!(state.latest_run(), Some(&run));

    let updated = RunSummary {
        status: RunStatus::Done,
        verification: Some(RunVerification {
            command: "cargo test".to_string(),
            ok: true,
            summary: "ok".to_string(),
        }),
        ..run
    };
    state.apply(RuntimeEvent::RunUpdated {
        run: updated.clone(),
    });

    assert_eq!(state.runs(), std::slice::from_ref(&updated));
    assert_eq!(state.latest_run(), Some(&updated));
}

#[test]
fn runs_drawer_stages_selected_run_action_without_overwriting_draft() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    state.apply(RuntimeEvent::RunUpdated {
        run: RunSummary {
            id: RunId(1),
            goal: "ship parser".to_string(),
            status: RunStatus::Done,
            coordinator: Some(MemberId::new("builder")),
            verification: None,
            created_at: "2026-06-28 10:00:00".to_string(),
            updated_at: "2026-06-28 10:00:00".to_string(),
            attempt: 1,
            events: Vec::new(),
            steps: Vec::new(),
            mode: None,
            legacy_mode: None,
        },
    });

    state.toggle_drawer(Drawer::Runs);
    assert!(state.stage_selected_run_action());
    assert_eq!(state.drawer(), None);
    assert_eq!(state.composer().text(), "/verify run-1");

    state.clear_composer();
    state.insert_char('x');
    state.toggle_drawer(Drawer::Runs);
    assert!(!state.stage_selected_run_action());
    assert_eq!(state.drawer(), Some(Drawer::Runs));
    assert_eq!(state.composer().text(), "x");
}

#[test]
fn runs_drawer_can_select_an_older_run() {
    let mut state = AppState::new(Vec::new());
    state.apply(RuntimeEvent::Ready {
        team: "mixed".to_string(),
        workspace: "/tmp/ws".to_string(),
        default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
        runs: vec![
            RunSummary {
                id: RunId(1),
                goal: "ship parser".to_string(),
                status: RunStatus::Done,
                coordinator: Some(MemberId::new("builder")),
                verification: None,
                created_at: "2026-06-28 10:00:00".to_string(),
                updated_at: "2026-06-28 10:05:00".to_string(),
                attempt: 1,
                events: Vec::new(),
                steps: Vec::new(),
                mode: None,
                legacy_mode: None,
            },
            RunSummary {
                id: RunId(2),
                goal: "refactor ui".to_string(),
                status: RunStatus::Running,
                coordinator: Some(MemberId::new("builder")),
                verification: None,
                created_at: "2026-06-28 10:10:00".to_string(),
                updated_at: "2026-06-28 10:12:00".to_string(),
                attempt: 1,
                events: Vec::new(),
                steps: Vec::new(),
                mode: None,
                legacy_mode: None,
            },
        ],
        members: Vec::new(),
    });
    state.toggle_drawer(Drawer::Runs);

    assert_eq!(state.selected_run().map(|run| run.id), Some(RunId(2)));
    state.select_older_run();
    assert_eq!(state.selected_run().map(|run| run.id), Some(RunId(1)));
    assert_eq!(
        state.selected_run_action_command().as_deref(),
        Some("/verify run-1")
    );
    assert!(state.stage_selected_run_action());
    assert_eq!(state.composer().text(), "/verify run-1");
}

#[test]
fn runs_drawer_can_select_steps_and_stage_step_actions() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    state.apply(RuntimeEvent::RunUpdated {
        run: RunSummary {
            id: RunId(1),
            goal: "ship checklist".to_string(),
            status: RunStatus::Running,
            coordinator: Some(MemberId::new("builder")),
            verification: None,
            created_at: "2026-06-28 10:00:00".to_string(),
            updated_at: "2026-06-28 10:00:00".to_string(),
            attempt: 1,
            events: Vec::new(),
            steps: vec![
                RunStepSummary {
                    number: 1,
                    status: RunStepStatus::Todo,
                    owner: Some(MemberId::new("builder")),
                    title: "Write parser tests".to_string(),
                    note: None,
                    updated_at: "2026-06-28 10:01:00".to_string(),
                },
                RunStepSummary {
                    number: 2,
                    status: RunStepStatus::Doing,
                    owner: None,
                    title: "Wire checklist UI".to_string(),
                    note: None,
                    updated_at: "2026-06-28 10:02:00".to_string(),
                },
                RunStepSummary {
                    number: 3,
                    status: RunStepStatus::Blocked,
                    owner: None,
                    title: "Wait for credentials".to_string(),
                    note: None,
                    updated_at: "2026-06-28 10:03:00".to_string(),
                },
                RunStepSummary {
                    number: 4,
                    status: RunStepStatus::Done,
                    owner: None,
                    title: "Document result".to_string(),
                    note: None,
                    updated_at: "2026-06-28 10:04:00".to_string(),
                },
            ],
            mode: None,
            legacy_mode: None,
        },
    });
    state.toggle_drawer(Drawer::Runs);

    assert_eq!(state.selected_run_step(), None);
    assert_eq!(state.selected_run_stage_command(), None);
    assert!(!state.stage_selected_run_action());
    assert_eq!(state.drawer(), Some(Drawer::Runs));

    assert!(state.select_next_run_step());
    assert_eq!(state.selected_run_step(), Some(1));
    assert_eq!(
        state.selected_run_stage_command().as_deref(),
        Some("/step doing run-1 1")
    );
    assert_eq!(
        state.selected_run_dispatch_command().as_deref(),
        Some(
            "@builder Start run-1 step #1: Write parser tests. Update the checklist with @@run_step as you progress."
        )
    );

    state.select_newer_run();
    assert_eq!(state.selected_run_step(), None);
    assert_eq!(state.selected_run_stage_command(), None);

    assert!(state.select_next_run_step());

    assert!(state.select_next_run_step());
    assert_eq!(
        state.selected_run_stage_command().as_deref(),
        Some("/step done run-1 2")
    );
    assert_eq!(
        state.selected_run_dispatch_command().as_deref(),
        Some("/step assign run-1 2 ")
    );

    assert!(state.select_next_run_step());
    assert_eq!(
        state.selected_run_stage_command().as_deref(),
        Some("/step doing run-1 3 blocker resolved")
    );

    assert!(state.select_next_run_step());
    assert_eq!(
        state.selected_run_stage_command().as_deref(),
        Some("/step todo run-1 4 reopen")
    );

    assert!(state.stage_selected_run_action());
    assert_eq!(state.composer().text(), "/step todo run-1 4 reopen");
}

#[test]
fn run_action_previews_detected_verify_command() {
    let dir = std::env::temp_dir().join(format!("asterline-action-preview-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
    let mut state = AppState::new(Vec::new());
    state.apply(RuntimeEvent::Ready {
        team: "mixed".to_string(),
        workspace: dir.display().to_string(),
        default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
        runs: Vec::new(),
        members: Vec::new(),
    });
    state.apply(RuntimeEvent::RunUpdated {
        run: RunSummary {
            id: RunId(1),
            goal: "ship parser".to_string(),
            status: RunStatus::Done,
            coordinator: Some(MemberId::new("builder")),
            verification: None,
            created_at: "2026-06-28 10:00:00".to_string(),
            updated_at: "2026-06-28 10:00:00".to_string(),
            attempt: 1,
            events: Vec::new(),
            steps: Vec::new(),
            mode: None,
            legacy_mode: None,
        },
    });

    assert_eq!(
        state.latest_run_action_command().as_deref(),
        Some("/verify cargo test")
    );
    state.toggle_drawer(Drawer::Runs);
    assert!(state.stage_selected_run_action());
    assert_eq!(state.composer().text(), "/verify run-1 cargo test");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn run_action_continues_failed_and_blocked_runs() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    state.apply(RuntimeEvent::RunUpdated {
        run: RunSummary {
            id: RunId(1),
            goal: "ship parser".to_string(),
            status: RunStatus::Failed,
            coordinator: Some(MemberId::new("builder")),
            verification: Some(RunVerification {
                command: "cargo test".to_string(),
                ok: false,
                summary: "failed".to_string(),
            }),
            created_at: "2026-06-28 10:00:00".to_string(),
            updated_at: "2026-06-28 10:00:00".to_string(),
            attempt: 2,
            events: Vec::new(),
            steps: Vec::new(),
            mode: None,
            legacy_mode: None,
        },
    });

    assert_eq!(
        state.latest_run_action_command().as_deref(),
        Some("/continue fix failing verification")
    );
    state.toggle_drawer(Drawer::Runs);
    assert!(state.stage_selected_run_action());
    assert_eq!(
        state.composer().text(),
        "/continue run-1 fix failing verification"
    );

    state.apply(RuntimeEvent::RunUpdated {
        run: RunSummary {
            id: RunId(2),
            goal: "unblock release".to_string(),
            status: RunStatus::Blocked,
            coordinator: Some(MemberId::new("builder")),
            verification: None,
            created_at: "2026-06-28 10:00:00".to_string(),
            updated_at: "2026-06-28 10:00:00".to_string(),
            attempt: 2,
            events: Vec::new(),
            steps: Vec::new(),
            mode: None,
            legacy_mode: None,
        },
    });

    assert_eq!(
        state.latest_run_action_command().as_deref(),
        Some("/continue blocker resolved")
    );
}

#[test]
fn streaming_message_builds_agent_cell() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    let builder = MemberId::new("builder");
    state.apply(RuntimeEvent::MessageStarted {
        msg: MessageId(1),
        turn: TurnId(1),
        member: builder.clone(),
    });
    state.apply(RuntimeEvent::MessageDelta {
        msg: MessageId(1),
        text: "Hel".to_string(),
    });
    state.apply(RuntimeEvent::MessageDelta {
        msg: MessageId(1),
        text: "lo".to_string(),
    });
    state.apply(RuntimeEvent::MessageCompleted {
        msg: MessageId(1),
        text: "Hello".to_string(),
    });

    assert!(matches!(
        state.chat().last(),
        Some(ChatItem::Agent { text, .. }) if text == "Hello"
    ));
}

#[test]
fn message_completion_does_not_finish_the_member_process() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    let builder = MemberId::new("builder");
    state.apply(RuntimeEvent::MemberStatus {
        member: builder.clone(),
        status: MemberStatus::Running,
    });
    state.apply(RuntimeEvent::Reasoning {
        member: builder.clone(),
        text: "thinking".to_string(),
    });
    state.apply(RuntimeEvent::MessageStarted {
        msg: MessageId(10),
        turn: TurnId(1),
        member: builder.clone(),
    });

    state.apply(RuntimeEvent::MessageCompleted {
        msg: MessageId(10),
        text: "answer".to_string(),
    });

    assert_eq!(state.members()[0].status, MemberStatus::Running);
    assert_eq!(state.running_count(), 1);
    assert!(!state.active_reasoning().contains_key(&builder));
}

#[test]
fn reasoning_deltas_accumulate_without_clearing_other_live_members() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    let builder = MemberId::new("builder");
    state.apply(RuntimeEvent::Reasoning {
        member: builder.clone(),
        text: "checking ".to_string(),
    });
    state.apply(RuntimeEvent::Reasoning {
        member: builder.clone(),
        text: "invariants".to_string(),
    });
    state.apply(RuntimeEvent::TurnStarted { turn: TurnId(2) });
    state.apply(RuntimeEvent::TurnFinished { turn: TurnId(2) });

    assert_eq!(
        state.active_reasoning().get(&builder).map(String::as_str),
        Some("checking invariants")
    );
}

#[test]
fn streamed_message_and_tool_detail_have_total_memory_bounds() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    let builder = MemberId::new("builder");
    state.apply(RuntimeEvent::MessageStarted {
        msg: MessageId(9),
        turn: TurnId(1),
        member: builder.clone(),
    });
    state.apply(RuntimeEvent::MessageDelta {
        msg: MessageId(9),
        text: "你".repeat(MAX_CHAT_ITEM_BYTES),
    });
    state.apply(RuntimeEvent::ToolStarted {
        member: builder.clone(),
        tool_id: "large-tool".to_string(),
        name: "shell".to_string(),
        summary: "large output".to_string(),
    });
    state.apply(RuntimeEvent::ToolProgress {
        member: builder,
        tool_id: "large-tool".to_string(),
        delta: "x".repeat(MAX_CHAT_ITEM_BYTES + 1),
    });

    let message = state
        .chat()
        .iter()
        .find_map(|item| match item {
            ChatItem::Agent { text, .. } => Some(text),
            _ => None,
        })
        .unwrap();
    assert!(message.len() <= MAX_CHAT_ITEM_BYTES);
    assert!(message.contains("output truncated"));
    assert!(message.is_char_boundary(message.len()));

    let detail = state
        .chat()
        .iter()
        .find_map(|item| match item {
            ChatItem::Tool { detail, .. } => Some(detail),
            _ => None,
        })
        .unwrap();
    assert!(detail.len() <= MAX_CHAT_ITEM_BYTES);
    assert!(detail.contains("output truncated"));
}

#[test]
fn replayed_and_live_chat_are_bounded_by_item_and_byte_budgets() {
    let replayed = (0..MAX_CHAT_ITEMS + 25)
        .map(|index| ChatItem::Notice {
            text: format!("history-{index}"),
        })
        .collect::<Vec<_>>();
    let mut state = AppState::new(replayed);
    assert_eq!(state.chat().len(), MAX_CHAT_ITEMS);
    assert!(matches!(
        state.chat().first(),
        Some(ChatItem::Notice { text }) if text.contains("Earlier history omitted")
    ));
    assert!(matches!(
        state.chat().get(1),
        Some(ChatItem::Notice { text }) if text == "history-26"
    ));

    for index in 0..40 {
        state.apply(RuntimeEvent::Notice(format!(
            "large-{index}-{}",
            "x".repeat(512 * 1024)
        )));
    }
    let retained_bytes = state.chat().iter().map(chat_item_bytes).sum::<usize>();
    assert!(retained_bytes <= MAX_CHAT_BYTES);
    assert!(state.chat().len() < 40);
}

#[test]
fn oversized_replayed_and_live_items_are_visibly_truncated() {
    let mut state = AppState::new(vec![ChatItem::Notice {
        text: "r".repeat(2 * 1024 * 1024),
    }]);
    let replayed = state.chat().first().unwrap();
    assert!(chat_item_bytes(replayed) <= MAX_CHAT_ITEM_BYTES);
    assert!(matches!(replayed, ChatItem::Notice { text } if text.contains("output truncated")));

    state.apply(RuntimeEvent::UserMessage {
        turn: TurnId(1),
        targets: Vec::new(),
        body: "你".repeat(1024 * 1024),
    });
    let live = state.chat().last().unwrap();
    assert!(chat_item_bytes(live) <= MAX_CHAT_ITEM_BYTES);
    assert!(matches!(live, ChatItem::User { body, .. } if body.contains("output truncated")));
    assert!(state.chat().iter().map(chat_item_bytes).sum::<usize>() <= MAX_CHAT_BYTES);
}

#[test]
fn completed_and_streaming_updates_cannot_bypass_chat_budget() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    let builder = MemberId::new("builder");
    state.apply(RuntimeEvent::MessageStarted {
        msg: MessageId(71),
        turn: TurnId(1),
        member: builder.clone(),
    });
    state.apply(RuntimeEvent::MessageDelta {
        msg: MessageId(71),
        text: "m".repeat(2 * 1024 * 1024),
    });
    assert_chat_budget(&state);
    state.apply(RuntimeEvent::MessageCompleted {
        msg: MessageId(71),
        text: "c".repeat(2 * 1024 * 1024),
    });
    assert_chat_budget(&state);

    state.apply(RuntimeEvent::ToolStarted {
        member: builder.clone(),
        tool_id: "bounded-tool".to_string(),
        name: "shell".to_string(),
        summary: "large".to_string(),
    });
    state.apply(RuntimeEvent::ToolProgress {
        member: builder.clone(),
        tool_id: "bounded-tool".to_string(),
        delta: "p".repeat(2 * 1024 * 1024),
    });
    assert_chat_budget(&state);
    state.apply(RuntimeEvent::ToolCompleted {
        member: builder.clone(),
        tool_id: "bounded-tool".to_string(),
        ok: true,
        output: "o".repeat(2 * 1024 * 1024),
    });
    assert_chat_budget(&state);
    state.apply(RuntimeEvent::ToolCompleted {
        member: builder,
        tool_id: "missing-tool".to_string(),
        ok: false,
        output: "f".repeat(2 * 1024 * 1024),
    });
    assert_chat_budget(&state);
    assert!(
        state
            .chat()
            .iter()
            .all(|item| chat_item_bytes(item) <= MAX_CHAT_ITEM_BYTES)
    );
}

#[test]
fn active_streams_are_preferred_but_do_not_bypass_hard_budget() {
    let mut state = AppState::new(Vec::new());
    let member = MemberId::new("builder");
    for id in 0..5 {
        state.apply(RuntimeEvent::MessageStarted {
            msg: MessageId(id),
            turn: TurnId(1),
            member: member.clone(),
        });
        state.apply(RuntimeEvent::MessageDelta {
            msg: MessageId(id),
            text: id.to_string().repeat(MAX_CHAT_ITEM_BYTES),
        });
        assert_chat_budget(&state);
    }
    assert!(state.chat().iter().any(|item| matches!(
        item,
        ChatItem::Agent { text, .. } if text.contains("live response preview omitted")
    )));
    state.apply(RuntimeEvent::MessageCompleted {
        msg: MessageId(0),
        text: format!(
            "final answer survives eviction {}",
            "z".repeat(MAX_CHAT_ITEM_BYTES)
        ),
    });
    assert_chat_budget(&state);
    assert!(state.chat().iter().any(|item| matches!(
        item,
        ChatItem::Agent { member: cell_member, text, .. }
            if cell_member == &member
                && text.contains("live response preview omitted")
                && text.contains("final answer survives eviction")
    )));
}

#[test]
fn evicted_active_tool_completion_keeps_identity_and_omission() {
    let mut state = AppState::new(Vec::new());
    let member = MemberId::new("builder");
    for id in 0..5 {
        state.apply(RuntimeEvent::ToolStarted {
            member: member.clone(),
            tool_id: format!("tool-{id}"),
            name: format!("shell-{id}"),
            summary: format!("command-{id}"),
        });
        state.apply(RuntimeEvent::ToolProgress {
            member: member.clone(),
            tool_id: format!("tool-{id}"),
            delta: id.to_string().repeat(MAX_CHAT_ITEM_BYTES),
        });
    }
    assert_chat_budget(&state);
    assert!(state.tool_index.values().any(|cell| cell.omitted));

    state.apply(RuntimeEvent::ToolCompleted {
        member,
        tool_id: "tool-0".to_string(),
        ok: true,
        output: "completed output".to_string(),
    });

    assert_chat_budget(&state);
    assert!(state.chat().iter().any(|item| matches!(
        item,
        ChatItem::Tool {
            name,
            summary,
            detail,
            ok: Some(true),
            ..
        } if name == "shell-0"
            && summary == "command-0"
            && detail.contains("live tool output omitted")
            && detail.contains("completed output")
    )));
}

#[test]
fn inactive_member_retires_unfinished_message_and_tool_cells() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    let member = MemberId::new("builder");
    state.apply(RuntimeEvent::MessageStarted {
        msg: MessageId(81),
        turn: TurnId(1),
        member: member.clone(),
    });
    state.apply(RuntimeEvent::MessageDelta {
        msg: MessageId(81),
        text: "partial answer".to_string(),
    });
    state.apply(RuntimeEvent::ToolStarted {
        member: member.clone(),
        tool_id: "interrupted-tool".to_string(),
        name: "shell".to_string(),
        summary: "cargo test".to_string(),
    });
    state.apply(RuntimeEvent::ToolProgress {
        member: member.clone(),
        tool_id: "interrupted-tool".to_string(),
        delta: "partial output".to_string(),
    });

    state.apply(RuntimeEvent::MemberStatus {
        member: member.clone(),
        status: MemberStatus::Idle,
    });

    assert!(!state.has_active_message(&member));
    assert_eq!(state.omitted_active_output_count(), 0);
    assert!(state.chat().iter().any(|item| matches!(
        item,
        ChatItem::Agent { text, .. }
            if text.contains("ended before a completion event")
                && text.contains("partial answer")
    )));
    assert!(state.chat().iter().any(|item| matches!(
        item,
        ChatItem::Tool {
            name,
            summary,
            detail,
            ok: Some(false),
            ..
        } if name == "shell"
            && summary == "cargo test"
            && detail.contains("ended before a completion event")
            && detail.contains("partial output")
    )));
}

#[test]
fn inactive_member_retires_evicted_live_tombstones() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    let member = MemberId::new("builder");
    for id in 0..5 {
        state.apply(RuntimeEvent::MessageStarted {
            msg: MessageId(90 + id),
            turn: TurnId(1),
            member: member.clone(),
        });
        state.apply(RuntimeEvent::MessageDelta {
            msg: MessageId(90 + id),
            text: id.to_string().repeat(MAX_CHAT_ITEM_BYTES),
        });
    }
    assert!(state.message_index.values().any(|cell| cell.omitted));

    state.apply(RuntimeEvent::MemberStatus {
        member: member.clone(),
        status: MemberStatus::Idle,
    });

    assert!(!state.has_active_message(&member));
    assert_eq!(state.omitted_active_output_count(), 0);
    assert_chat_budget(&state);
    assert!(state.chat().iter().any(|item| matches!(
        item,
        ChatItem::Agent { text, .. }
            if text.contains("ended before a completion event")
    )));
}

#[test]
fn runtime_disconnect_retires_live_work_and_unavailable_controls() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    let member = MemberId::new("builder");
    state.apply(RuntimeEvent::MemberStatus {
        member: member.clone(),
        status: MemberStatus::Running,
    });
    state.apply(RuntimeEvent::MessageStarted {
        msg: MessageId(89),
        turn: TurnId(1),
        member: member.clone(),
    });
    state.apply(RuntimeEvent::RoutePaused {
        turn: TurnId(1),
        from: member,
        to: vec!["reviewer".to_string()],
        reason: "relay paused".to_string(),
        queued: 1,
    });
    state.apply(RuntimeEvent::ApprovalRequested {
        id: ApprovalId(88),
        member: None,
        action: "shell".to_string(),
        body: "publish".to_string(),
    });

    state.mark_runtime_unavailable();

    assert!(!state.runtime_available());
    assert_eq!(state.running_count(), 0);
    assert_eq!(state.members()[0].status, MemberStatus::Failed);
    assert!(state.running_since.is_empty());
    assert_eq!(state.paused_routes(), 0);
    assert!(state.pending_approvals().is_empty());
    assert!(state.message_index.is_empty());
}

#[test]
fn duplicate_tool_start_retires_the_previous_cell_and_bounds_metadata() {
    let mut state = AppState::new(Vec::new());
    let member = MemberId::new("builder");
    let id = "same-tool".repeat(MAX_ACTIVE_TOOL_ID_BYTES);
    state.apply(RuntimeEvent::ToolStarted {
        member: member.clone(),
        tool_id: id.clone(),
        name: "first".to_string(),
        summary: "old command".to_string(),
    });
    state.apply(RuntimeEvent::ToolStarted {
        member: member.clone(),
        tool_id: id.clone(),
        name: "n".repeat(MAX_CHAT_ITEM_BYTES),
        summary: "s".repeat(MAX_CHAT_ITEM_BYTES),
    });

    assert_eq!(state.tool_index.len(), 1);
    assert!(state.chat().iter().any(|item| matches!(
        item,
        ChatItem::Tool {
            name,
            detail,
            ok: Some(false),
            ..
        } if name == "first" && detail.contains("ended before a completion event")
    )));
    let active = state.tool_index.values().next().unwrap();
    assert!(active.name.len() <= MAX_ACTIVE_TOOL_NAME_BYTES);
    assert!(active.summary.len() <= MAX_ACTIVE_TOOL_SUMMARY_BYTES);
    assert!(state.tool_index.keys().next().unwrap().1.len() <= MAX_ACTIVE_TOOL_ID_BYTES);

    state.apply(RuntimeEvent::ToolCompleted {
        member,
        tool_id: id,
        ok: true,
        output: "done".to_string(),
    });
    assert!(state.tool_index.is_empty());
    assert_chat_budget(&state);
}

#[test]
fn long_tool_ids_with_the_same_prefix_remain_distinct() {
    let mut state = AppState::new(Vec::new());
    let member = MemberId::new("builder");
    let prefix = "p".repeat(MAX_ACTIVE_TOOL_ID_BYTES + 1);
    let first = format!("{prefix}-first");
    let second = format!("{prefix}-second");
    for (id, name) in [(&first, "first"), (&second, "second")] {
        state.apply(RuntimeEvent::ToolStarted {
            member: member.clone(),
            tool_id: id.to_string(),
            name: name.to_string(),
            summary: format!("{name} command"),
        });
    }
    assert_eq!(state.tool_index.len(), 2);

    state.apply(RuntimeEvent::ToolCompleted {
        member: member.clone(),
        tool_id: first,
        ok: true,
        output: "first done".to_string(),
    });
    assert_eq!(state.tool_index.len(), 1);
    state.apply(RuntimeEvent::ToolCompleted {
        member,
        tool_id: second,
        ok: true,
        output: "second done".to_string(),
    });
    assert!(state.tool_index.is_empty());
    assert!(state.chat().iter().any(|item| matches!(
        item,
        ChatItem::Tool { name, detail, .. } if name == "first" && detail == "first done"
    )));
    assert!(state.chat().iter().any(|item| matches!(
        item,
        ChatItem::Tool { name, detail, .. } if name == "second" && detail == "second done"
    )));
}

#[test]
fn prompt_history_seed_and_submissions_obey_hard_budget() {
    let replayed = (0..MAX_PROMPT_HISTORY_ITEMS + 20)
        .map(|index| ChatItem::User {
            body: format!("seed-{index}"),
            targets: Vec::new(),
            interrupted: Vec::new(),
        })
        .collect();
    let mut state = AppState::new(replayed);
    assert!(state.prompt_history.len() <= MAX_PROMPT_HISTORY_ITEMS);
    assert!(
        state.prompt_history.iter().map(String::len).sum::<usize>() <= MAX_PROMPT_HISTORY_BYTES
    );

    for index in 0..8 {
        state.record_submission(&format!(
            "large-{index}-{}",
            "x".repeat(MAX_CHAT_ITEM_BYTES)
        ));
    }
    assert!(state.prompt_history.len() <= MAX_PROMPT_HISTORY_ITEMS);
    assert!(
        state
            .prompt_history
            .iter()
            .all(|entry| entry.len() <= MAX_CHAT_ITEM_BYTES)
    );
    assert!(
        state.prompt_history.iter().map(String::len).sum::<usize>() <= MAX_PROMPT_HISTORY_BYTES
    );
    assert!(
        state
            .prompt_history
            .last()
            .is_some_and(|entry| entry.contains("output truncated"))
    );
}

#[test]
fn oversized_paste_is_utf8_bounded_and_warns_once() {
    let mut state = AppState::new(Vec::new());
    let paste = "你\r\n".repeat(MAX_COMPOSER_BYTES);

    state.insert_text(&paste);
    state.insert_text(&paste);

    let text = state.composer().text();
    assert!(text.len() <= MAX_COMPOSER_BYTES);
    assert!(text.is_char_boundary(text.len()));
    assert_eq!(
        state
            .chat()
            .iter()
            .filter(
                |item| matches!(item, ChatItem::Notice { text } if text == COMPOSER_INPUT_TRUNCATED)
            )
            .count(),
        1
    );
}

fn assert_chat_budget(state: &AppState) {
    assert!(state.chat().len() <= MAX_CHAT_ITEMS);
    assert!(state.chat().iter().map(chat_item_bytes).sum::<usize>() <= MAX_CHAT_BYTES);
}

#[test]
fn chat_eviction_rebases_active_message_and_tool_indices() {
    let mut state = AppState::new(
        (0..MAX_CHAT_ITEMS - 2)
            .map(|index| ChatItem::Notice {
                text: format!("old-{index}"),
            })
            .collect(),
    );
    let builder = MemberId::new("builder");
    state.apply(RuntimeEvent::MessageStarted {
        msg: MessageId(99),
        turn: TurnId(1),
        member: builder.clone(),
    });
    state.apply(RuntimeEvent::ToolStarted {
        member: builder.clone(),
        tool_id: "active-tool".to_string(),
        name: "shell".to_string(),
        summary: "work".to_string(),
    });
    state.apply(RuntimeEvent::Notice("forces-eviction".to_string()));
    state.apply(RuntimeEvent::MessageDelta {
        msg: MessageId(99),
        text: "still here".to_string(),
    });
    state.apply(RuntimeEvent::ToolProgress {
        member: builder,
        tool_id: "active-tool".to_string(),
        delta: "tool output".to_string(),
    });

    assert!(state.chat().iter().any(|item| matches!(
        item,
        ChatItem::Agent { text, .. } if text == "still here"
    )));
    assert!(state.chat().iter().any(|item| matches!(
        item,
        ChatItem::Tool { detail, .. } if detail == "tool output"
    )));
    assert!(state.chat().len() <= MAX_CHAT_ITEMS);
}

#[test]
fn tool_completion_updates_existing_cell() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    let builder = MemberId::new("builder");
    state.apply(RuntimeEvent::ToolStarted {
        member: builder.clone(),
        tool_id: "t1".to_string(),
        name: "shell".to_string(),
        summary: "cargo test".to_string(),
    });
    state.apply(RuntimeEvent::ToolCompleted {
        member: builder,
        tool_id: "t1".to_string(),
        ok: true,
        output: "all tests passed".to_string(),
    });
    // One tool cell, now marked ok.
    let tools: Vec<_> = state
        .chat()
        .iter()
        .filter(|i| matches!(i, ChatItem::Tool { .. }))
        .collect();
    assert_eq!(tools.len(), 1);
    assert!(matches!(
        tools[0],
        ChatItem::Tool {
            ok: Some(true),
            summary,
            detail,
            ..
        } if summary == "cargo test" && detail == "all tests passed"
    ));
}

#[test]
fn same_backend_tool_id_is_isolated_by_member() {
    let mut state = AppState::new(Vec::new());
    let builder = MemberId::new("builder");
    let reviewer = MemberId::new("reviewer");
    for (member, summary) in [(&builder, "build"), (&reviewer, "review")] {
        state.apply(RuntimeEvent::ToolStarted {
            member: member.clone(),
            tool_id: "agy-step-0".to_string(),
            name: "shell".to_string(),
            summary: summary.to_string(),
        });
    }
    state.apply(RuntimeEvent::ToolProgress {
        member: builder.clone(),
        tool_id: "agy-step-0".to_string(),
        delta: "builder progress".to_string(),
    });
    state.apply(RuntimeEvent::ToolCompleted {
        member: builder.clone(),
        tool_id: "agy-step-0".to_string(),
        ok: true,
        output: "builder output".to_string(),
    });
    state.apply(RuntimeEvent::ToolCompleted {
        member: reviewer.clone(),
        tool_id: "agy-step-0".to_string(),
        ok: false,
        output: "reviewer output".to_string(),
    });

    let tools = state
        .chat()
        .iter()
        .filter_map(|item| match item {
            ChatItem::Tool {
                member,
                summary,
                detail,
                ok,
                ..
            } => Some((member, summary, detail, ok)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(tools.len(), 2);
    assert!(matches!(
        tools[0],
        (member, summary, detail, Some(true))
            if member == &builder && summary == "build" && detail == "builder output"
    ));
    assert!(matches!(
        tools[1],
        (member, summary, detail, Some(false))
            if member == &reviewer && summary == "review" && detail == "reviewer output"
    ));
}

#[test]
fn skill_picker_preserves_the_discovered_invocation() {
    let skill = crate::tui::skills::SkillInfo {
        name: "frontend-design".to_string(),
        description: String::new(),
        path: PathBuf::from("/tmp/frontend-design/SKILL.md"),
        backend: BackendKind::Claude,
        invocation: "/frontend-design:frontend-design".to_string(),
    };

    assert_eq!(skill.invocation, "/frontend-design:frontend-design");
}

#[test]
fn manual_known_codex_slash_skill_is_normalized_without_touching_unknown_commands() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    state.set_skills(vec![crate::tui::skills::SkillInfo {
        name: "review".to_string(),
        description: String::new(),
        path: PathBuf::from("/tmp/review/SKILL.md"),
        backend: BackendKind::Codex,
        invocation: "$review".to_string(),
    }]);
    let known = UiCommand::UserMessage {
        target: MessageTarget::Member(MemberId::new("builder")),
        body: "@builder /review focus on tests".to_string(),
    };
    let unknown = UiCommand::UserMessage {
        target: MessageTarget::Member(MemberId::new("builder")),
        body: "@builder /compact".to_string(),
    };

    assert_eq!(
        state.normalize_known_skill_invocation(known),
        UiCommand::UserMessage {
            target: MessageTarget::Member(MemberId::new("builder")),
            body: "@builder $review focus on tests".to_string(),
        }
    );
    assert_eq!(
        state.normalize_known_skill_invocation(unknown.clone()),
        unknown
    );
}

#[test]
fn find_matches_case_insensitively_and_navigates() {
    let mut state = AppState::new(vec![
        ChatItem::User {
            body: "Fix the Parser".to_string(),
            targets: Vec::new(),
            interrupted: Vec::new(),
        },
        ChatItem::Agent {
            member: MemberId::new("builder"),
            display_name: "Builder".to_string(),
            backend: BackendKind::Codex,
            text: "looking at parser.rs".to_string(),
        },
        ChatItem::Tool {
            member: MemberId::new("builder"),
            name: "shell".to_string(),
            summary: "cargo test".to_string(),
            detail: "ok".to_string(),
            ok: Some(true),
        },
        ChatItem::Notice {
            text: "other note".to_string(),
        },
        ChatItem::Error {
            member: None,
            message: "parser panic".to_string(),
        },
        ChatItem::Verdict {
            member: MemberId::new("reviewer"),
            approve: false,
            summary: "needs PARSER coverage".to_string(),
        },
        ChatItem::Diff {
            member: MemberId::new("builder"),
            files: vec![("src/parser.rs".to_string(), "+1".to_string())],
            ok: true,
        },
        ChatItem::Route {
            from: MemberId::new("builder"),
            to: vec!["reviewer".to_string()],
            body: "please review parser".to_string(),
        },
    ]);

    state.set_find("PARSER");
    let (query, current, total) = state.find().expect("find active");
    assert_eq!(query, "PARSER");
    // User, agent, error, verdict, diff, route — not the cargo test tool.
    assert_eq!(total, 6);
    assert_eq!(current, 6, "starts at newest match");
    assert_eq!(state.find_current_chat_index(), Some(7));

    // Jump sets scroll to sum of estimate_item_lines for items below the match.
    assert_eq!(state.scroll(), 0, "newest match is last item → scroll 0");

    state.find_prev();
    assert_eq!(state.find().map(|(_, c, _)| c), Some(5));
    assert_eq!(state.find_current_chat_index(), Some(6));
    // Item 7 (route) below → estimate_item_lines(Route) = 1 + body lines
    assert!(state.scroll() > 0);

    state.find_next();
    assert_eq!(state.find().map(|(_, c, _)| c), Some(6));
    state.find_next(); // wrap
    assert_eq!(state.find().map(|(_, c, _)| c), Some(1));
    assert_eq!(state.find_current_chat_index(), Some(0));

    state.set_find("no-such-needle-xyz");
    assert_eq!(state.find(), Some(("no-such-needle-xyz", 0, 0)));
    assert!(state.find_active());

    state.set_find("   ");
    assert!(!state.find_active());
    assert_eq!(state.find(), None);

    // New items after set_find do not panic navigation (stale indices skipped).
    state.set_find("parser");
    state.apply(RuntimeEvent::Notice("brand new".to_string()));
    let _ = state.find();
    state.find_next();
    state.find_prev();
}

#[test]
fn find_jump_scroll_sums_items_below() {
    let mut state = AppState::new(vec![
        ChatItem::User {
            body: "alpha\nline2".to_string(),
            targets: Vec::new(),
            interrupted: Vec::new(),
        },
        ChatItem::Notice {
            text: "beta".to_string(),
        },
        ChatItem::Notice {
            text: "gamma".to_string(),
        },
    ]);
    state.set_find("alpha");
    // Items below idx 0: beta (1) + gamma (1)
    assert_eq!(state.scroll(), 2);
    state.clear_find();
    assert!(!state.find_active());
}

#[test]
fn logs_do_not_enter_chat() {
    let mut state = AppState::new(Vec::new());
    state.apply(RuntimeEvent::Log(LogEntry::warn("builder", "stderr noise")));
    assert!(state.chat().is_empty());
    assert_eq!(state.logs().len(), 1);
}

#[test]
fn seeded_logs_replay_into_the_drawer() {
    let mut state = AppState::new(Vec::new());
    state.seed_logs(vec![
        LogEntry::info("builder", "started"),
        LogEntry::warn("reviewer", "slow"),
    ]);
    assert_eq!(state.logs().len(), 2);
    // Live logs still append after seeding.
    state.apply(RuntimeEvent::Log(LogEntry::error("runtime", "boom")));
    assert_eq!(state.logs().len(), 3);
}

#[test]
fn logs_are_bounded_by_entry_and_total_bytes() {
    let mut state = AppState::new(Vec::new());
    for index in 0..200 {
        state.apply(RuntimeEvent::Log(LogEntry::warn(
            format!("member-{index}"),
            "x".repeat(crate::domain::event::MAX_LOG_MESSAGE_BYTES * 2),
        )));
    }

    assert!(state.logs().len() < 200);
    assert!(
        state
            .logs()
            .iter()
            .all(|entry| entry.message.len() <= crate::domain::event::MAX_LOG_MESSAGE_BYTES)
    );
    assert!(state.logs().iter().map(LogEntry::byte_len).sum::<usize>() <= MAX_LOG_BYTES);
}

#[test]
fn approvals_track_pending_and_resolve() {
    let mut state = AppState::new(Vec::new());
    state.apply(RuntimeEvent::ApprovalRequested {
        id: ApprovalId(1),
        member: None,
        action: "git".to_string(),
        body: "git push".to_string(),
    });
    assert_eq!(state.first_pending_approval(), Some(ApprovalId(1)));
    state.apply(RuntimeEvent::ApprovalResolved {
        id: ApprovalId(1),
        decision: ApprovalDecision::Approve,
    });
    assert!(state.first_pending_approval().is_none());
}

#[test]
fn member_status_drives_running_count() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    let builder = MemberId::new("builder");
    state.apply(RuntimeEvent::MemberStatus {
        member: builder.clone(),
        status: MemberStatus::Running,
    });
    assert_eq!(state.running_count(), 1);
    state.apply(RuntimeEvent::MemberStatus {
        member: builder,
        status: MemberStatus::Idle,
    });
    assert_eq!(state.running_count(), 0);
}

#[test]
fn queued_member_remains_busy_and_cannot_attach() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    let builder = MemberId::new("builder");
    state.apply(RuntimeEvent::MemberStatus {
        member: builder.clone(),
        status: MemberStatus::Running,
    });
    state.apply(RuntimeEvent::MemberStatus {
        member: builder.clone(),
        status: MemberStatus::Queued,
    });

    assert_eq!(state.running_count(), 1);
    assert!(state.member_elapsed_secs(&builder).is_some());
    assert!(state.request_attach(0).is_none());
    assert!(state.take_attach_request().is_none());

    state.apply(RuntimeEvent::MemberStatus {
        member: builder,
        status: MemberStatus::Idle,
    });
    assert_eq!(state.request_attach(0), Some(MemberId::new("builder")));
    state.apply(RuntimeEvent::AttachGranted {
        member: MemberId::new("builder"),
    });
    assert!(state.take_attach_request().is_some());
}

#[test]
fn fresh_claude_attach_is_given_a_deterministic_session_id() {
    let mut ready_event = ready();
    if let RuntimeEvent::Ready { members, .. } = &mut ready_event {
        members[0].backend = BackendKind::Claude;
        members[0].session = None;
    }
    let mut state = AppState::new(Vec::new());
    state.apply(ready_event);

    assert_eq!(state.request_attach(0), Some(MemberId::new("builder")));
    state.apply(RuntimeEvent::AttachGranted {
        member: MemberId::new("builder"),
    });
    let request = state
        .take_attach_request()
        .expect("fresh Claude attach request");
    assert!(request.session.is_none());
    let fresh = request
        .fresh_session
        .as_ref()
        .expect("Asterline-owned Claude UUID");
    assert!(uuid::Uuid::parse_str(fresh.as_str()).is_ok());
    assert_eq!(request.transcript_session(), Some(fresh.as_str()));
}

#[test]
fn attach_is_blocked_by_work_elsewhere_and_pending_approval() {
    let mut ready_event = ready();
    if let RuntimeEvent::Ready { members, .. } = &mut ready_event {
        let mut reviewer = members[0].clone();
        reviewer.id = MemberId::new("reviewer");
        reviewer.display_name = "Reviewer".to_string();
        members.push(reviewer);
    }
    let mut state = AppState::new(Vec::new());
    state.apply(ready_event);
    state.apply(RuntimeEvent::MemberStatus {
        member: MemberId::new("builder"),
        status: MemberStatus::Running,
    });

    assert!(state.request_attach(1).is_none());
    assert!(state.take_attach_request().is_none());
    assert!(state.chat().iter().any(|item| matches!(
        item,
        ChatItem::Notice { text } if text.contains("Cannot attach while")
    )));

    state.apply(RuntimeEvent::MemberStatus {
        member: MemberId::new("builder"),
        status: MemberStatus::Idle,
    });
    state.apply(RuntimeEvent::ApprovalRequested {
        id: ApprovalId(77),
        member: None,
        action: "shell".to_string(),
        body: "publish".to_string(),
    });
    assert!(state.request_attach(1).is_none());
    assert!(state.take_attach_request().is_none());

    state.apply(RuntimeEvent::ApprovalResolved {
        id: ApprovalId(77),
        decision: ApprovalDecision::Reject,
    });
    assert_eq!(state.request_attach(1), Some(MemberId::new("reviewer")));
    state.apply(RuntimeEvent::AttachGranted {
        member: MemberId::new("reviewer"),
    });
    assert_eq!(
        state.take_attach_request().map(|request| request.member),
        Some(MemberId::new("reviewer"))
    );
}

#[test]
fn authoritative_route_queue_update_unblocks_attach() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    state.apply(RuntimeEvent::RoutePaused {
        turn: TurnId(1),
        from: MemberId::new("builder"),
        to: vec!["reviewer".to_string()],
        reason: "relay paused".to_string(),
        queued: 1,
    });
    assert!(state.request_attach(0).is_none());
    assert!(state.take_attach_request().is_none());

    state.apply(RuntimeEvent::RouteQueueUpdated { queued: 0 });
    assert_eq!(state.request_attach(0), Some(MemberId::new("builder")));
    state.apply(RuntimeEvent::AttachGranted {
        member: MemberId::new("builder"),
    });
    assert!(state.take_attach_request().is_some());
}

#[test]
fn duplicate_attach_request_is_suppressed_until_runtime_resolves_it() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());

    assert_eq!(state.request_attach(0), Some(MemberId::new("builder")));
    assert!(state.request_attach(0).is_none());

    state.apply(RuntimeEvent::AttachGranted {
        member: MemberId::new("builder"),
    });
    assert!(state.take_attach_request().is_some());
}

#[test]
fn named_attach_resolves_a_display_name_to_the_member_id() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    let builder = MemberId::new("builder");

    assert_eq!(
        state.request_attach_member_by_name(&MemberId::new("BUILDER")),
        Some(builder.clone())
    );
    state.apply(RuntimeEvent::AttachGranted {
        member: builder.clone(),
    });

    let request = state.take_attach_request().expect("attached request");
    assert_eq!(request.member, builder);
    assert_eq!(request.display_name, "Builder");
}

#[test]
fn pending_attach_is_cancelable_and_late_grants_are_released() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    let member = state.request_attach(0).unwrap();

    assert!(state.has_cancelable_work());
    assert_eq!(state.cancel_pending_attach(), Some(member.clone()));
    assert!(!state.has_cancelable_work());

    // The runtime may have granted the reservation just as cancellation was
    // sent. Do not launch the CLI; release that stale reservation instead.
    state.apply(RuntimeEvent::AttachGranted {
        member: member.clone(),
    });
    assert!(state.take_attach_request().is_none());
    assert_eq!(state.take_attach_release_pending(), Some(member));
}

#[test]
fn granted_attach_is_cancelable_before_terminal_handoff() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    let member = state.request_attach(0).unwrap();
    state.apply(RuntimeEvent::AttachGranted {
        member: member.clone(),
    });

    assert!(state.has_cancelable_work());
    assert_eq!(state.cancel_pending_attach(), Some(member));
    assert!(state.take_attach_request().is_none());
    assert!(!state.has_cancelable_work());
}

#[test]
fn attach_denial_clears_pending_request() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    let member = MemberId::new("builder");

    assert_eq!(state.request_attach(0), Some(member.clone()));
    state.apply(RuntimeEvent::AttachDenied {
        member: member.clone(),
        reason: "work became active".to_string(),
    });

    assert_eq!(state.request_attach(0), Some(member));
}

#[test]
fn accepted_runtime_events_drive_user_message_and_member_status() {
    let mut state = AppState::new(Vec::new());
    state.apply(RuntimeEvent::Ready {
        team: "mixed".to_string(),
        workspace: "/tmp/ws".to_string(),
        default_target: Some(DefaultTarget::All),
        runs: Vec::new(),
        members: vec![
            MemberSummary {
                id: MemberId::new("builder"),
                display_name: "Builder".to_string(),
                backend: BackendKind::Codex,
                role: "impl".to_string(),
                status: MemberStatus::Idle,
                session: None,
                cwd: String::new(),
                model: None,
                effort: None,
                sandbox: SandboxPolicy::ReadOnly,
                permission_mode: Some(PermissionMode::Default),
                session_policy: SessionPolicy::Resume,
            },
            MemberSummary {
                id: MemberId::new("reviewer"),
                display_name: "Reviewer".to_string(),
                backend: BackendKind::Claude,
                role: "review".to_string(),
                status: MemberStatus::Idle,
                session: None,
                cwd: String::new(),
                model: None,
                effort: None,
                sandbox: SandboxPolicy::ReadOnly,
                permission_mode: Some(PermissionMode::Default),
                session_policy: SessionPolicy::Resume,
            },
        ],
    });

    state.remember_user_message_target(&MessageTarget::Default);
    assert_eq!(state.running_count(), 0);
    assert!(state.chat().is_empty());

    state.apply(RuntimeEvent::UserMessage {
        turn: TurnId(1),
        targets: vec![MemberId::new("builder"), MemberId::new("reviewer")],
        body: "go".to_string(),
    });
    assert!(matches!(
        state.chat().last(),
        Some(ChatItem::User { body, .. }) if body == "go"
    ));
    assert_eq!(state.running_count(), 0);

    for member in [MemberId::new("builder"), MemberId::new("reviewer")] {
        state.apply(RuntimeEvent::MemberStatus {
            member,
            status: MemberStatus::Running,
        });
    }

    assert_eq!(state.running_count(), 2);
}

#[test]
fn drawer_toggles() {
    let mut state = AppState::new(Vec::new());
    state.toggle_drawer(Drawer::Logs);
    assert_eq!(state.drawer(), Some(Drawer::Logs));
    state.toggle_drawer(Drawer::Logs);
    assert_eq!(state.drawer(), None);
    let _ = AgentSessionId("x".to_string());
}

#[test]
fn drawer_scroll_down_increases_render_offset() {
    let mut state = AppState::new(Vec::new());
    state.toggle_drawer(Drawer::Logs);

    state.drawer_scroll_up();
    assert_eq!(state.drawer_scroll(), 0);

    state.drawer_scroll_down();
    assert_eq!(state.drawer_scroll(), 1);

    state.drawer_scroll_up();
    assert_eq!(state.drawer_scroll(), 0);
}

#[test]
fn quit_requires_two_consecutive_requests() {
    let mut state = AppState::new(Vec::new());

    state.request_quit();
    assert!(!state.should_quit());
    assert!(state.chat().iter().any(|item| matches!(
        item,
        ChatItem::Notice { text } if text.contains("Ctrl+C again")
    )));

    state.request_quit();
    assert!(state.should_quit());
}

#[test]
fn quit_confirmation_is_disarmed_by_input() {
    let mut state = AppState::new(Vec::new());

    state.request_quit();
    state.insert_char('x');
    state.clear_composer();
    state.request_quit();

    assert!(!state.should_quit());
}

#[test]
fn team_drawer_editor_can_add_and_apply_member() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    state.toggle_drawer(Drawer::Team);

    let add = state.handle_team_editor_key(KeyCode::Char('a'), KeyModifiers::NONE);
    assert_eq!(add, TeamEditorOutcome::Consumed(None));

    let apply = state.handle_team_editor_key(KeyCode::Char('s'), KeyModifiers::NONE);
    let TeamEditorOutcome::Consumed(Some(crate::domain::event::UiCommand::ReplaceTeam {
        members,
        ..
    })) = apply
    else {
        panic!("expected replace team command");
    };
    assert_eq!(members.len(), 2);
}

#[test]
fn targeted_completion_accepts_a_member_display_name_case_insensitively() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    state.insert_text("@BUILDER /");

    let completion = state
        .completion()
        .expect("Codex completion for display name");
    assert_eq!(completion.title, "member actions & skills");
    assert!(
        completion
            .items
            .iter()
            .any(|item| item.insert == "/attach ")
    );
    assert!(!completion.items.iter().any(|item| item.insert == "/model"));
    assert!(!completion.items.iter().any(|item| item.insert == "/fast"));
}

#[test]
fn team_editor_reuses_model_catalog_after_the_drawer_closes() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    state.model_catalog.seed(
        BackendKind::Codex,
        Path::new("/tmp/ws"),
        vec!["gpt-5.6-sol".to_string()],
    );

    state.toggle_drawer(Drawer::Team);
    assert!(state.team_editor().is_some());
    state.close_drawer();
    assert!(
        state
            .model_catalog
            .contains(BackendKind::Codex, Path::new("/tmp/ws"))
    );

    state.toggle_drawer(Drawer::Team);
    assert!(state.team_editor().is_some_and(|editor| {
        editor
            .model_catalog()
            .contains(BackendKind::Codex, Path::new("/tmp/ws"))
    }));
}

#[test]
fn slash_opens_command_popup_and_accept_inserts() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    for ch in "/as".chars() {
        state.insert_char(ch);
    }
    let completion = state.completion().expect("command popup");
    assert_eq!(completion.items[0].insert, "/ask ");
    assert!(state.accept_completion());
    assert_eq!(state.composer().text(), "/ask ");
}

#[test]
fn accepting_a_no_argument_command_still_leaves_a_trailing_space() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    state.insert_text("/te");

    assert!(state.accept_completion());
    assert_eq!(state.composer().text(), "/team ");
}

#[test]
fn targeted_skill_completion_excludes_other_backends() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    state.members.push(MemberView {
        id: MemberId::new("claude"),
        display_name: "Claude".to_string(),
        backend: BackendKind::Claude,
        role: "review".to_string(),
        status: MemberStatus::Idle,
        session: None,
        cwd: String::new(),
        model: None,
        effort: None,
        sandbox: SandboxPolicy::ReadOnly,
        permission_mode: Some(PermissionMode::Default),
        session_policy: SessionPolicy::Resume,
    });
    state.default_target = Some(DefaultTarget::Member(MemberId::new("claude")));
    state.set_skills(vec![
        crate::tui::skills::SkillInfo {
            name: "review".to_string(),
            description: "Codex only".to_string(),
            path: PathBuf::from("/tmp/codex/SKILL.md"),
            backend: BackendKind::Codex,
            invocation: "$review".to_string(),
        },
        crate::tui::skills::SkillInfo {
            name: "wake".to_string(),
            description: "Claude only".to_string(),
            path: PathBuf::from("/tmp/claude/SKILL.md"),
            backend: BackendKind::Claude,
            invocation: "/plugin:wake".to_string(),
        },
    ]);

    state.insert_text("@claude /");
    let completion = state.completion().expect("Claude skill completion");
    assert_eq!(
        completion
            .items
            .iter()
            .map(|item| item.insert.as_str())
            .collect::<Vec<_>>(),
        vec!["/attach ", "/plugin:wake "]
    );
}

#[test]
fn accepting_mode_command_immediately_opens_mode_choices() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    state.insert_text("/mode");

    assert!(state.accept_completion());
    assert_eq!(state.composer().text(), "/mode ");
    let modes = state.completion().expect("second-level mode popup");
    assert_eq!(modes.title, "modes");
    assert_eq!(modes.items[0].insert, "normal ");
}

#[test]
fn at_opens_member_popup_and_accepts() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    for ch in "@bu".chars() {
        state.insert_char(ch);
    }
    let completion = state.completion().expect("member popup");
    assert_eq!(completion.items[0].insert, "@builder ");
    state.accept_completion();
    assert_eq!(state.composer().text(), "@builder ");
}

#[test]
fn dismiss_hides_popup_until_text_changes() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    state.insert_char('/');
    assert!(state.completion().is_some());
    state.dismiss_popup();
    assert!(state.completion().is_none());
    state.insert_char('a');
    assert!(state.completion().is_some());
}

#[test]
fn header_roster_selection() {
    let mut state = AppState::new(Vec::new());
    state.members = vec![
        MemberView {
            id: MemberId::new("builder"),
            display_name: "Builder".to_string(),
            backend: BackendKind::Codex,
            role: "impl".to_string(),
            status: MemberStatus::Idle,
            session: None,
            cwd: String::new(),
            model: None,
            effort: None,
            sandbox: SandboxPolicy::ReadOnly,
            permission_mode: Some(PermissionMode::Default),
            session_policy: SessionPolicy::Resume,
        },
        MemberView {
            id: MemberId::new("reviewer"),
            display_name: "Reviewer".to_string(),
            backend: BackendKind::Claude,
            role: "review".to_string(),
            status: MemberStatus::Idle,
            session: None,
            cwd: String::new(),
            model: None,
            effort: None,
            sandbox: SandboxPolicy::ReadOnly,
            permission_mode: Some(PermissionMode::Default),
            session_policy: SessionPolicy::Resume,
        },
    ];
    assert_eq!(state.header_selected(), None);

    state.select_next_member();
    assert_eq!(state.header_selected(), Some(0)); // builder

    state.select_next_member();
    assert_eq!(state.header_selected(), Some(1)); // reviewer

    state.select_prev_member();
    assert_eq!(state.header_selected(), Some(0)); // builder

    state.insert_char('x');
    assert_eq!(state.header_selected(), None); // cleared on typing
}

#[test]
fn prompt_history_seeds_from_replayed_user_messages() {
    let chat = vec![
        ChatItem::User {
            body: "first".to_string(),
            targets: Vec::new(),
            interrupted: Vec::new(),
        },
        ChatItem::Agent {
            member: MemberId::new("builder"),
            display_name: "Builder".to_string(),
            backend: BackendKind::Codex,
            text: "ok".to_string(),
        },
        ChatItem::User {
            body: "second".to_string(),
            targets: Vec::new(),
            interrupted: Vec::new(),
        },
    ];
    let mut state = AppState::new(chat);

    // ↑ recalls newest-first across sessions.
    state.history_prev();
    assert_eq!(state.composer().text(), "second");
    state.history_prev();
    assert_eq!(state.composer().text(), "first");
    // Already at the oldest entry — further ↑ stays put.
    state.history_prev();
    assert_eq!(state.composer().text(), "first");
    // ↓ walks back toward newest.
    state.history_next();
    assert_eq!(state.composer().text(), "second");
}

#[test]
fn history_preserves_and_restores_the_live_draft() {
    let mut state = AppState::new(vec![ChatItem::User {
        body: "prior".to_string(),
        targets: Vec::new(),
        interrupted: Vec::new(),
    }]);
    for ch in "draft".chars() {
        state.insert_char(ch);
    }
    state.history_prev();
    assert_eq!(state.composer().text(), "prior");
    assert!(state.browsing_history());
    // Stepping past the newest restores what was being typed.
    state.history_next();
    assert_eq!(state.composer().text(), "draft");
    assert!(!state.browsing_history());
}

#[test]
fn submitting_records_history_and_skips_consecutive_dupes() {
    let mut state = AppState::new(Vec::new());
    state.record_submission("build it");
    state.record_submission("build it"); // dup ignored
    state.record_submission("   "); // blank ignored
    state.record_submission("review it");

    state.history_prev();
    assert_eq!(state.composer().text(), "review it");
    state.history_prev();
    assert_eq!(state.composer().text(), "build it");
}

#[test]
fn reverse_history_search_finds_steps_and_accepts() {
    let mut state = AppState::new(Vec::new());
    state.record_submission("build the parser");
    state.record_submission("review the parser");
    state.record_submission("run tests");

    state.start_history_search();
    assert!(state.in_history_search());

    for ch in "parser".chars() {
        state.history_search_input(ch);
    }
    // Newest match first.
    assert_eq!(state.history_search().unwrap().1, Some("review the parser"));
    // Ctrl+R again → next older match.
    state.history_search_again();
    assert_eq!(state.history_search().unwrap().1, Some("build the parser"));

    state.accept_history_search();
    assert!(!state.in_history_search());
    assert_eq!(state.composer().text(), "build the parser");
}

#[test]
fn reverse_history_search_cancel_keeps_composer() {
    let mut state = AppState::new(Vec::new());
    state.record_submission("hello world");
    for ch in "draft".chars() {
        state.insert_char(ch);
    }
    state.start_history_search();
    state.history_search_input('h');
    state.cancel_history_search();
    assert!(!state.in_history_search());
    assert_eq!(state.composer().text(), "draft");
}

#[test]
fn wheel_scroll_moves_several_lines_at_once() {
    let mut state = AppState::new(Vec::new());
    state.scroll_by(10);
    assert_eq!(state.scroll(), 10);
    state.scroll_by(-3);
    assert_eq!(state.scroll(), 7);
    state.scroll_by(-20);
    assert_eq!(state.scroll(), 0);
}
