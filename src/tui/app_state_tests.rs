use super::*;
use crate::domain::event::{
    AgentSessionId, ApprovalDecision, MemberSummary, TurnId, WorkflowRunId, WorkflowRunStatus,
    WorkflowRunSummary, WorkflowStepSummary, WorkflowVerification,
};

fn ready() -> RuntimeEvent {
    RuntimeEvent::Ready {
        team: "mixed".to_string(),
        workspace: "/tmp/ws".to_string(),
        default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
        workflow_runs: Vec::new(),
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
fn workflow_run_updates_insert_then_replace() {
    let mut state = AppState::new(Vec::new());
    let run = WorkflowRunSummary {
        id: WorkflowRunId(1),
        goal: "ship parser".to_string(),
        status: WorkflowRunStatus::Running,
        coordinator: Some(MemberId::new("builder")),
        verification: None,
        created_at: "2026-06-28 10:00:00".to_string(),
        updated_at: "2026-06-28 10:00:00".to_string(),
        attempt: 1,
        events: Vec::new(),
        steps: Vec::new(),
    };

    state.apply(RuntimeEvent::WorkflowRunUpdated { run: run.clone() });
    assert_eq!(state.workflow_runs(), std::slice::from_ref(&run));
    assert_eq!(state.latest_workflow_run(), Some(&run));

    let updated = WorkflowRunSummary {
        status: WorkflowRunStatus::Done,
        verification: Some(WorkflowVerification {
            command: "cargo test".to_string(),
            ok: true,
            summary: "ok".to_string(),
        }),
        ..run
    };
    state.apply(RuntimeEvent::WorkflowRunUpdated {
        run: updated.clone(),
    });

    assert_eq!(state.workflow_runs(), std::slice::from_ref(&updated));
    assert_eq!(state.latest_workflow_run(), Some(&updated));
}

#[test]
fn runs_drawer_stages_selected_workflow_action_without_overwriting_draft() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    state.apply(RuntimeEvent::WorkflowRunUpdated {
        run: WorkflowRunSummary {
            id: WorkflowRunId(1),
            goal: "ship parser".to_string(),
            status: WorkflowRunStatus::Done,
            coordinator: Some(MemberId::new("builder")),
            verification: None,
            created_at: "2026-06-28 10:00:00".to_string(),
            updated_at: "2026-06-28 10:00:00".to_string(),
            attempt: 1,
            events: Vec::new(),
            steps: Vec::new(),
        },
    });

    state.toggle_drawer(Drawer::Runs);
    assert!(state.stage_selected_workflow_action());
    assert_eq!(state.drawer(), None);
    assert_eq!(state.composer().text(), "/verify run-1");

    state.clear_composer();
    state.insert_char('x');
    state.toggle_drawer(Drawer::Runs);
    assert!(!state.stage_selected_workflow_action());
    assert_eq!(state.drawer(), Some(Drawer::Runs));
    assert_eq!(state.composer().text(), "x");
}

#[test]
fn runs_drawer_can_select_an_older_workflow_run() {
    let mut state = AppState::new(Vec::new());
    state.apply(RuntimeEvent::Ready {
        team: "mixed".to_string(),
        workspace: "/tmp/ws".to_string(),
        default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
        workflow_runs: vec![
            WorkflowRunSummary {
                id: WorkflowRunId(1),
                goal: "ship parser".to_string(),
                status: WorkflowRunStatus::Done,
                coordinator: Some(MemberId::new("builder")),
                verification: None,
                created_at: "2026-06-28 10:00:00".to_string(),
                updated_at: "2026-06-28 10:05:00".to_string(),
                attempt: 1,
                events: Vec::new(),
                steps: Vec::new(),
            },
            WorkflowRunSummary {
                id: WorkflowRunId(2),
                goal: "refactor ui".to_string(),
                status: WorkflowRunStatus::Running,
                coordinator: Some(MemberId::new("builder")),
                verification: None,
                created_at: "2026-06-28 10:10:00".to_string(),
                updated_at: "2026-06-28 10:12:00".to_string(),
                attempt: 1,
                events: Vec::new(),
                steps: Vec::new(),
            },
        ],
        members: Vec::new(),
    });
    state.toggle_drawer(Drawer::Runs);

    assert_eq!(
        state.selected_workflow_run().map(|run| run.id),
        Some(WorkflowRunId(2))
    );
    state.select_older_workflow_run();
    assert_eq!(
        state.selected_workflow_run().map(|run| run.id),
        Some(WorkflowRunId(1))
    );
    assert_eq!(
        state.selected_workflow_action_command().as_deref(),
        Some("/verify run-1")
    );
    assert!(state.stage_selected_workflow_action());
    assert_eq!(state.composer().text(), "/verify run-1");
}

#[test]
fn runs_drawer_can_select_steps_and_stage_step_actions() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    state.apply(RuntimeEvent::WorkflowRunUpdated {
        run: WorkflowRunSummary {
            id: WorkflowRunId(1),
            goal: "ship checklist".to_string(),
            status: WorkflowRunStatus::Running,
            coordinator: Some(MemberId::new("builder")),
            verification: None,
            created_at: "2026-06-28 10:00:00".to_string(),
            updated_at: "2026-06-28 10:00:00".to_string(),
            attempt: 1,
            events: Vec::new(),
            steps: vec![
                WorkflowStepSummary {
                    number: 1,
                    status: WorkflowStepStatus::Todo,
                    owner: Some(MemberId::new("builder")),
                    title: "Write parser tests".to_string(),
                    note: None,
                    updated_at: "2026-06-28 10:01:00".to_string(),
                },
                WorkflowStepSummary {
                    number: 2,
                    status: WorkflowStepStatus::Doing,
                    owner: None,
                    title: "Wire checklist UI".to_string(),
                    note: None,
                    updated_at: "2026-06-28 10:02:00".to_string(),
                },
                WorkflowStepSummary {
                    number: 3,
                    status: WorkflowStepStatus::Blocked,
                    owner: None,
                    title: "Wait for credentials".to_string(),
                    note: None,
                    updated_at: "2026-06-28 10:03:00".to_string(),
                },
                WorkflowStepSummary {
                    number: 4,
                    status: WorkflowStepStatus::Done,
                    owner: None,
                    title: "Document result".to_string(),
                    note: None,
                    updated_at: "2026-06-28 10:04:00".to_string(),
                },
            ],
        },
    });
    state.toggle_drawer(Drawer::Runs);

    assert_eq!(state.selected_workflow_step(), None);
    assert_eq!(
        state.selected_workflow_stage_command().as_deref(),
        Some("/abort")
    );

    assert!(state.select_next_workflow_step());
    assert_eq!(state.selected_workflow_step(), Some(1));
    assert_eq!(
        state.selected_workflow_stage_command().as_deref(),
        Some("/step doing run-1 1")
    );
    assert_eq!(
        state.selected_workflow_dispatch_command().as_deref(),
        Some(
            "@builder Start run-1 step #1: Write parser tests. Update the checklist with @@workflow_step as you progress."
        )
    );

    state.select_newer_workflow_run();
    assert_eq!(state.selected_workflow_step(), None);
    assert_eq!(
        state.selected_workflow_stage_command().as_deref(),
        Some("/abort")
    );

    assert!(state.select_next_workflow_step());

    assert!(state.select_next_workflow_step());
    assert_eq!(
        state.selected_workflow_stage_command().as_deref(),
        Some("/step done run-1 2")
    );
    assert_eq!(
        state.selected_workflow_dispatch_command().as_deref(),
        Some("/step assign run-1 2 ")
    );

    assert!(state.select_next_workflow_step());
    assert_eq!(
        state.selected_workflow_stage_command().as_deref(),
        Some("/step doing run-1 3 blocker resolved")
    );

    assert!(state.select_next_workflow_step());
    assert_eq!(
        state.selected_workflow_stage_command().as_deref(),
        Some("/step todo run-1 4 reopen")
    );

    assert!(state.stage_selected_workflow_action());
    assert_eq!(state.composer().text(), "/step todo run-1 4 reopen");
}

#[test]
fn workflow_action_previews_detected_verify_command() {
    let dir = std::env::temp_dir().join(format!("asterline-action-preview-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
    let mut state = AppState::new(Vec::new());
    state.apply(RuntimeEvent::Ready {
        team: "mixed".to_string(),
        workspace: dir.display().to_string(),
        default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
        workflow_runs: Vec::new(),
        members: Vec::new(),
    });
    state.apply(RuntimeEvent::WorkflowRunUpdated {
        run: WorkflowRunSummary {
            id: WorkflowRunId(1),
            goal: "ship parser".to_string(),
            status: WorkflowRunStatus::Done,
            coordinator: Some(MemberId::new("builder")),
            verification: None,
            created_at: "2026-06-28 10:00:00".to_string(),
            updated_at: "2026-06-28 10:00:00".to_string(),
            attempt: 1,
            events: Vec::new(),
            steps: Vec::new(),
        },
    });

    assert_eq!(
        state.latest_workflow_action_command().as_deref(),
        Some("/verify cargo test")
    );
    state.toggle_drawer(Drawer::Runs);
    assert!(state.stage_selected_workflow_action());
    assert_eq!(state.composer().text(), "/verify run-1 cargo test");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn workflow_action_continues_failed_and_blocked_runs() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    state.apply(RuntimeEvent::WorkflowRunUpdated {
        run: WorkflowRunSummary {
            id: WorkflowRunId(1),
            goal: "ship parser".to_string(),
            status: WorkflowRunStatus::Failed,
            coordinator: Some(MemberId::new("builder")),
            verification: Some(WorkflowVerification {
                command: "cargo test".to_string(),
                ok: false,
                summary: "failed".to_string(),
            }),
            created_at: "2026-06-28 10:00:00".to_string(),
            updated_at: "2026-06-28 10:00:00".to_string(),
            attempt: 2,
            events: Vec::new(),
            steps: Vec::new(),
        },
    });

    assert_eq!(
        state.latest_workflow_action_command().as_deref(),
        Some("/continue fix failing verification")
    );
    state.toggle_drawer(Drawer::Runs);
    assert!(state.stage_selected_workflow_action());
    assert_eq!(
        state.composer().text(),
        "/continue run-1 fix failing verification"
    );

    state.apply(RuntimeEvent::WorkflowRunUpdated {
        run: WorkflowRunSummary {
            id: WorkflowRunId(2),
            goal: "unblock release".to_string(),
            status: WorkflowRunStatus::Blocked,
            coordinator: Some(MemberId::new("builder")),
            verification: None,
            created_at: "2026-06-28 10:00:00".to_string(),
            updated_at: "2026-06-28 10:00:00".to_string(),
            attempt: 2,
            events: Vec::new(),
            steps: Vec::new(),
        },
    });

    assert_eq!(
        state.latest_workflow_action_command().as_deref(),
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
fn skill_picker_stages_one_shot_targeted_prompt() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    state.set_skills(vec![crate::tui::skills::SkillInfo {
        name: "review".to_string(),
        description: "Review changes".to_string(),
        path: PathBuf::from("/tmp/review/SKILL.md"),
    }]);
    state.toggle_drawer(Drawer::Skills);

    assert!(state.stage_selected_skill());
    assert_eq!(state.composer().text(), "@builder $review ");
    assert_eq!(state.drawer(), None);
}

#[test]
fn skill_invocation_matches_backend_native_syntax() {
    let skill = crate::tui::skills::SkillInfo {
        name: "review".to_string(),
        description: String::new(),
        path: PathBuf::from("/tmp/review/SKILL.md"),
    };

    assert_eq!(skill_invocation(BackendKind::Codex, &skill), "$review");
    assert_eq!(skill_invocation(BackendKind::Claude, &skill), "/review");
    for backend in [BackendKind::Grok, BackendKind::Agy] {
        let invocation = skill_invocation(backend, &skill);
        assert!(invocation.contains("review"));
        assert!(invocation.contains("/tmp/review/SKILL.md"));
    }
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
fn default_target_all_marks_every_member_running_optimistically() {
    let mut state = AppState::new(Vec::new());
    state.apply(RuntimeEvent::Ready {
        team: "mixed".to_string(),
        workspace: "/tmp/ws".to_string(),
        default_target: Some(DefaultTarget::All),
        workflow_runs: Vec::new(),
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

    state.handle_user_message_submitted(&MessageTarget::Default, "go".to_string());

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
        },
        ChatItem::Agent {
            member: MemberId::new("builder"),
            display_name: "Builder".to_string(),
            backend: BackendKind::Codex,
            text: "ok".to_string(),
        },
        ChatItem::User {
            body: "second".to_string(),
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
