use super::super::*;
use super::*;

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
    state.apply(RuntimeEvent::QueueUpdated {
        member: MemberId::new("builder"),
        prompts: vec!["queued".to_string()],
    });

    state.apply(RuntimeEvent::SessionReset);

    assert!(state.pending_approvals().is_empty());
    assert_eq!(state.paused_routes(), 0);
    assert!(!state.find_active());
    assert_eq!(state.drawer(), None);
    assert_eq!(state.queued_prompt_count(), 0);
}

#[test]
fn queued_prompt_can_be_returned_to_the_composer() {
    let mut state = AppState::new(Vec::new());
    let builder = MemberId::new("builder");
    state.apply(RuntimeEvent::QueueUpdated {
        member: builder.clone(),
        prompts: vec!["first".to_string(), "second".to_string()],
    });
    assert_eq!(state.queued_prompt_count(), 2);

    state.apply(RuntimeEvent::QueueUpdated {
        member: builder.clone(),
        prompts: vec!["first".to_string()],
    });
    state.apply(RuntimeEvent::QueuedPromptReturned {
        member: builder,
        body: "second".to_string(),
    });

    assert_eq!(state.queued_prompt_count(), 1);
    assert_eq!(state.composer().text(), "second");
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
        modes: Default::default(),
        mode_overrides: Default::default(),
        suggested_verify: None,
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
        modes: Default::default(),
        mode_overrides: Default::default(),
        suggested_verify: None,
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
