use super::super::*;
use super::*;

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
            files: vec![FileChangeItem::new("src/parser.rs", "+1")],
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
        modes: Default::default(),
        mode_overrides: Default::default(),
        suggested_verify: None,
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
fn clear_line_keeps_other_composer_lines() {
    let mut state = AppState::new(Vec::new());
    state.insert_text("keep\nremove\nkeep");
    state.composer_up();
    state.clear_line();
    assert_eq!(state.composer().text(), "keep\nkeep");
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
fn accepting_complete_mode_command_keeps_it_submit_ready() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    state.insert_text("/mode");

    assert!(!state.accept_completion());
    assert_eq!(state.composer().text(), "/mode");
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

#[test]
fn page_keys_scroll_a_viewport_and_clamp() {
    let mut state = AppState::new(Vec::new());
    state.set_chat_page_rows(12);
    state.scroll_up();
    assert_eq!(state.scroll(), 12);
    state.clamp_scroll(8);
    assert_eq!(state.scroll(), 8);
}

#[test]
fn paste_image_path_attaches_instead_of_inserting_text() {
    let dir = std::env::temp_dir().join(format!("asterline-ui-img-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("shot.png");
    std::fs::write(&path, [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']).unwrap();

    let mut state = AppState::new(Vec::new());
    state.paste_text_or_image(path.to_str().unwrap());
    assert!(state.composer().is_empty());
    assert_eq!(state.pending_images().len(), 1);
    assert!(state.has_composer_draft());
    let attached = state.pending_images()[0].path.clone();
    assert_ne!(attached, path);
    assert!(
        attached.to_string_lossy().contains("asterline-pasted"),
        "{attached:?}"
    );
    assert_eq!(
        std::fs::read(&attached).unwrap(),
        [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']
    );

    state.backspace();
    assert!(state.pending_images().is_empty());
    assert!(!state.has_composer_draft());
    assert!(!attached.exists());

    state.paste_text_or_image(path.to_str().unwrap());
    let sent = state.pending_images()[0].path.clone();
    state.take_composer();
    assert!(state.pending_images().is_empty());
    assert!(sent.exists(), "sent copies stay until stale prune");
    let _ = std::fs::remove_file(&sent);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn paste_plain_text_still_inserts_into_the_composer() {
    let mut state = AppState::new(Vec::new());
    state.paste_text_or_image("hello there");
    assert_eq!(state.composer().text(), "hello there");
    assert!(state.pending_images().is_empty());
}
