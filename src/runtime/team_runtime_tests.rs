use super::*;
use crate::domain::event::ChatItem;
use crate::domain::team::{
    ApprovalSurface, BackendKind, DefaultTarget, Effort, SessionPolicy, TeamMember,
};

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
    assert!(
        step.actions[0]
            .prompt
            .contains("builder in a review workflow")
    );
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::WorkflowRunUpdated { run }
            if run.mode.as_ref().is_some_and(|mode| mode.mode == CollabMode::Review)
    )));
}

#[test]
fn new_chat_keeps_selected_terminal_mode_until_explicitly_changed() {
    let mut rt = runtime();
    rt.on_ui_command(UiCommand::SetMode {
        mode: TerminalMode::Review,
    });
    rt.on_ui_command(UiCommand::NewSession);

    let review = rt.on_ui_command(user("fix parser"));
    assert!(review.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::WorkflowRunUpdated { run }
            if run.mode.as_ref().is_some_and(|mode| mode.mode == CollabMode::Review)
    )));

    let changed = rt.on_ui_command(UiCommand::SetMode {
        mode: TerminalMode::Normal,
    });
    assert!(changed.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::ModeChanged {
            mode: TerminalMode::Normal
        }
    )));
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
        step.actions
            .iter()
            .any(|a| a.member == MemberId::new("reviewer"))
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
fn replace_team_adds_member_and_requests_runner() {
    let mut rt = runtime();
    let mut members = team().members;
    let mut researcher = TeamMember::new("researcher", "Researcher", BackendKind::Agy, "research");
    researcher.model = Some("agy-pro".to_string());
    researcher.effort = Some(Effort::High);
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
fn workflow_kicks_off_via_a_coordinator() {
    let mut rt = runtime();
    let step = rt.on_ui_command(UiCommand::RunWorkflow {
        goal: "ship the parser".to_string(),
    });

    let run = step
        .events
        .iter()
        .find_map(|e| match e {
            RuntimeEvent::WorkflowRunUpdated { run } => Some(run),
            _ => None,
        })
        .expect("workflow run event");
    assert_eq!(run.status, WorkflowRunStatus::Running);
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::UserMessage { body, .. } if body == "/plan ship the parser"
    )));
    assert_eq!(step.actions.len(), 1);
    assert!(step.actions[0].prompt.contains("ship the parser"));
    assert!(step.actions[0].prompt.contains("$asterline-team"));
    assert!(!step.actions[0].prompt.contains("@@team_message"));
}

#[test]
fn workflow_marks_done_when_its_turn_finishes() {
    let mut rt = runtime();
    let step = rt.on_ui_command(UiCommand::RunWorkflow {
        goal: "ship the parser".to_string(),
    });
    let run_id = step
        .events
        .iter()
        .find_map(|e| match e {
            RuntimeEvent::WorkflowRunUpdated { run } => Some(run.id),
            _ => None,
        })
        .expect("workflow run id");

    let step = rt.on_agent_event(
        &MemberId::new("builder"),
        AgentEvent::Exited {
            code: Some(0),
            ok: true,
        },
    );

    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::WorkflowRunUpdated { run }
            if run.id == run_id && run.status == WorkflowRunStatus::Done
    )));
    assert!(
        step.events
            .iter()
            .any(|e| matches!(e, RuntimeEvent::TurnFinished { .. }))
    );
}

#[test]
fn verify_workflow_records_successful_check() {
    let dir = std::env::temp_dir().join(format!("asterline-verify-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut rt = runtime_in_workspace(dir.clone());

    rt.on_ui_command(UiCommand::RunWorkflow {
        goal: "ship the parser".to_string(),
    });
    let step = rt.on_ui_command(UiCommand::VerifyWorkflow {
        run_id: None,
        command: Some("printf verified".to_string()),
    });

    assert_eq!(step.verify_actions.len(), 1);
    let action = &step.verify_actions[0];
    assert_eq!(action.command, "printf verified");
    assert_eq!(action.workspace, dir);
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::WorkflowRunUpdated { run }
            if run.status == WorkflowRunStatus::Verifying
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
        RuntimeEvent::WorkflowRunUpdated { run }
            if run.status == WorkflowRunStatus::Done
                && run.verification.as_ref().is_some_and(|v| {
                    v.ok && v.command == "printf verified" && v.summary == "verified"
                })
    )));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn verify_workflow_can_target_an_older_run() {
    let dir = std::env::temp_dir().join(format!("asterline-verify-target-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut rt = runtime_in_workspace(dir.clone());

    let first = rt.on_ui_command(UiCommand::RunWorkflow {
        goal: "ship parser".to_string(),
    });
    let first_id = first
        .events
        .iter()
        .find_map(|e| match e {
            RuntimeEvent::WorkflowRunUpdated { run } => Some(run.id),
            _ => None,
        })
        .expect("first run id");
    let second = rt.on_ui_command(UiCommand::RunWorkflow {
        goal: "refactor ui".to_string(),
    });
    let second_id = second
        .events
        .iter()
        .find_map(|e| match e {
            RuntimeEvent::WorkflowRunUpdated { run } => Some(run.id),
            _ => None,
        })
        .expect("second run id");

    let verify = rt.on_ui_command(UiCommand::VerifyWorkflow {
        run_id: Some(first_id),
        command: Some("printf first".to_string()),
    });

    assert_eq!(verify.verify_actions.len(), 1);
    assert_eq!(verify.verify_actions[0].run_id, first_id);
    assert_ne!(verify.verify_actions[0].run_id, second_id);
    assert!(verify.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::WorkflowRunUpdated { run }
            if run.id == first_id && run.status == WorkflowRunStatus::Verifying
    )));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn continue_workflow_resumes_failed_run() {
    let mut rt = runtime();
    let step = rt.on_ui_command(UiCommand::RunWorkflow {
        goal: "ship the parser".to_string(),
    });
    let run_id = step
        .events
        .iter()
        .find_map(|event| match event {
            RuntimeEvent::WorkflowRunUpdated { run } => Some(run.id),
            _ => None,
        })
        .expect("workflow run id");
    rt.on_agent_event(
        &MemberId::new("builder"),
        AgentEvent::Exited {
            code: Some(0),
            ok: true,
        },
    );
    let verify = rt.on_ui_command(UiCommand::VerifyWorkflow {
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

    let step = rt.on_ui_command(UiCommand::ContinueWorkflow {
        run_id: Some(run_id),
        note: Some("fix verification".to_string()),
    });

    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::WorkflowRunUpdated { run }
            if run.id == run_id && run.status == WorkflowRunStatus::Running && run.attempt == 2
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
        RuntimeEvent::WorkflowRunUpdated { run }
            if run.id == run_id && run.status == WorkflowRunStatus::Done
                && run.attempt == 2
            && run.verification.is_none()
    )));
}

#[test]
fn workflow_note_and_block_update_timeline() {
    let mut rt = runtime();
    let step = rt.on_ui_command(UiCommand::RunWorkflow {
        goal: "ship the parser".to_string(),
    });
    let run_id = step
        .events
        .iter()
        .find_map(|event| match event {
            RuntimeEvent::WorkflowRunUpdated { run } => Some(run.id),
            _ => None,
        })
        .expect("workflow run id");

    let step = rt.on_ui_command(UiCommand::NoteWorkflow {
        run_id: Some(run_id),
        note: "waiting for API docs".to_string(),
    });
    assert!(step.actions.is_empty());
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::WorkflowRunUpdated { run }
            if run.id == run_id
                && run.status == WorkflowRunStatus::Running
                && run.events.last().is_some_and(|event| {
                    event.kind == "note"
                        && event.detail.as_deref() == Some("waiting for API docs")
                })
    )));

    let step = rt.on_ui_command(UiCommand::BlockWorkflow {
        run_id: Some(run_id),
        reason: "missing API token".to_string(),
    });
    assert!(step.actions.is_empty());
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::WorkflowRunUpdated { run }
            if run.id == run_id
                && run.status == WorkflowRunStatus::Blocked
                && run.events.last().is_some_and(|event| {
                    event.kind == "blocked"
                        && event.detail.as_deref() == Some("missing API token")
                })
    )));
}

#[test]
fn workflow_steps_update_checklist_without_running_agents() {
    let mut rt = runtime();
    let step = rt.on_ui_command(UiCommand::RunWorkflow {
        goal: "ship the parser".to_string(),
    });
    let run_id = step
        .events
        .iter()
        .find_map(|event| match event {
            RuntimeEvent::WorkflowRunUpdated { run } => Some(run.id),
            _ => None,
        })
        .expect("workflow run id");

    let step = rt.on_ui_command(UiCommand::AddWorkflowStep {
        run_id: Some(run_id),
        owner: None,
        title: "write parser tests".to_string(),
    });
    assert!(step.actions.is_empty());
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::WorkflowRunUpdated { run }
            if run.id == run_id
                && run.steps.len() == 1
                && run.steps[0].status == WorkflowStepStatus::Todo
                && run.steps[0].owner.is_none()
                && run.steps[0].title == "write parser tests"
    )));

    let step = rt.on_ui_command(UiCommand::AssignWorkflowStep {
        run_id: Some(run_id),
        step: 1,
        owner: Some(MemberId::new("reviewer")),
    });
    assert!(step.actions.is_empty());
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::WorkflowRunUpdated { run }
            if run.id == run_id
                && run.steps[0].owner == Some(MemberId::new("reviewer"))
                && run.events.last().is_some_and(|event| event.kind == "step_assigned")
    )));

    let step = rt.on_ui_command(UiCommand::UpdateWorkflowStep {
        run_id: Some(run_id),
        step: 1,
        status: WorkflowStepStatus::Done,
        note: Some("covered lexer edge cases".to_string()),
    });
    assert!(step.actions.is_empty());
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::WorkflowRunUpdated { run }
            if run.id == run_id
                && run.steps[0].status == WorkflowStepStatus::Done
                && run.steps[0].note.as_deref() == Some("covered lexer edge cases")
                && run.events.last().is_some_and(|event| event.kind == "step_updated")
    )));

    rt.on_ui_command(UiCommand::AddWorkflowStep {
        run_id: Some(run_id),
        owner: None,
        title: "obsolete duplicate".to_string(),
    });
    let step = rt.on_ui_command(UiCommand::RenameWorkflowStep {
        run_id: Some(run_id),
        step: 2,
        title: "document parser setup".to_string(),
    });
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::WorkflowRunUpdated { run }
            if run.id == run_id
                && run.steps[1].title == "document parser setup"
                && run.events.last().is_some_and(|event| event.kind == "step_renamed")
    )));

    let step = rt.on_ui_command(UiCommand::RemoveWorkflowStep {
        run_id: Some(run_id),
        step: 1,
    });
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::WorkflowRunUpdated { run }
            if run.id == run_id
                && run.steps.len() == 1
                && run.steps[0].number == 1
                && run.steps[0].title == "document parser setup"
                && run.events.last().is_some_and(|event| event.kind == "step_removed")
    )));

    let step = rt.on_ui_command(UiCommand::AssignWorkflowStep {
        run_id: Some(run_id),
        step: 1,
        owner: None,
    });
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::WorkflowRunUpdated { run }
            if run.id == run_id && run.steps[0].owner.is_none()
    )));
}

#[test]
fn agent_workflow_step_envelope_updates_active_workflow_checklist() {
    let mut rt = runtime();
    let step = rt.on_ui_command(UiCommand::RunWorkflow {
        goal: "ship the parser".to_string(),
    });
    let run_id = step
        .events
        .iter()
        .find_map(|event| match event {
            RuntimeEvent::WorkflowRunUpdated { run } => Some(run.id),
            _ => None,
        })
        .expect("workflow run id");

    let step = rt.on_agent_event(
        &MemberId::new("builder"),
        AgentEvent::MessageCompleted(
            r#"@@workflow_step {"action":"add","owner":"builder","title":"Write parser tests"}"#
                .to_string(),
        ),
    );
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::WorkflowRunUpdated { run }
            if run.id == run_id
                && run.steps.len() == 1
                && run.steps[0].status == WorkflowStepStatus::Todo
                && run.steps[0].owner == Some(MemberId::new("builder"))
                && run.steps[0].title == "Write parser tests"
    )));

    let step = rt.on_agent_event(
        &MemberId::new("builder"),
        AgentEvent::MessageCompleted(
            r#"@@workflow_step {"action":"assign","step":1,"owner":"reviewer"}"#.to_string(),
        ),
    );
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::WorkflowRunUpdated { run }
            if run.id == run_id && run.steps[0].owner == Some(MemberId::new("reviewer"))
    )));

    let step = rt.on_agent_event(
        &MemberId::new("builder"),
        AgentEvent::MessageCompleted(
            r#"@@workflow_step {"action":"done","step":1,"note":"Covered edge cases"}"#.to_string(),
        ),
    );
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::WorkflowRunUpdated { run }
            if run.id == run_id
                && run.steps[0].status == WorkflowStepStatus::Done
                && run.steps[0].note.as_deref() == Some("Covered edge cases")
    )));

    let step = rt.on_agent_event(
        &MemberId::new("builder"),
        AgentEvent::MessageCompleted(
            r#"@@workflow_step {"action":"rename","step":1,"title":"Write parser coverage tests"}"#
                .to_string(),
        ),
    );
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::WorkflowRunUpdated { run }
            if run.id == run_id && run.steps[0].title == "Write parser coverage tests"
    )));

    rt.on_agent_event(
        &MemberId::new("builder"),
        AgentEvent::MessageCompleted(
            r#"@@workflow_step {"action":"add","title":"Temporary duplicate"}"#.to_string(),
        ),
    );
    let step = rt.on_agent_event(
        &MemberId::new("builder"),
        AgentEvent::MessageCompleted(r#"@@workflow_step {"action":"remove","step":2}"#.to_string()),
    );
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::WorkflowRunUpdated { run }
            if run.id == run_id
                && run.steps.len() == 1
                && run.steps[0].title == "Write parser coverage tests"
    )));
}

#[test]
fn agent_workflow_step_envelope_outside_workflow_is_ignored() {
    let mut rt = runtime();
    rt.on_ui_command(user("@builder hello"));

    let step = rt.on_agent_event(
        &MemberId::new("builder"),
        AgentEvent::MessageCompleted(
            r#"@@workflow_step {"action":"add","title":"Write parser tests"}"#.to_string(),
        ),
    );

    assert!(
        !step
            .events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::WorkflowRunUpdated { .. }))
    );
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Notice(text)
            if text.contains("ignored workflow step update: no active workflow run")
    )));
}

#[test]
fn failed_verification_is_not_overwritten_by_later_exit() {
    let dir = std::env::temp_dir().join(format!("asterline-verify-fail-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut rt = runtime_in_workspace(dir.clone());

    let step = rt.on_ui_command(UiCommand::RunWorkflow {
        goal: "ship the parser".to_string(),
    });
    let run_id = step
        .events
        .iter()
        .find_map(|e| match e {
            RuntimeEvent::WorkflowRunUpdated { run } => Some(run.id),
            _ => None,
        })
        .expect("workflow run id");
    let verify = rt.on_ui_command(UiCommand::VerifyWorkflow {
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
        RuntimeEvent::WorkflowRunUpdated { run }
            if run.id == run_id && run.status == WorkflowRunStatus::Done
    )));
    assert_eq!(
        rt.store.latest_workflow_run().unwrap().unwrap().status,
        WorkflowRunStatus::Failed
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

// --- M3 review mode / M4 lead + roundtable --------------------------------

use crate::domain::mode::CollabMode;
use crate::runtime::mode_prompts::{
    LEAD_PLAN_HINT, MODERATOR_HINT, REVIEW_PROTOCOL_HINT, ROUNDTABLE_HINT,
};

fn run_mode(task: &str) -> UiCommand {
    UiCommand::RunMode {
        mode: CollabMode::Review,
        task: task.to_string(),
        overrides: vec![],
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

fn latest_run(rt: &TeamRuntime) -> WorkflowRunSummary {
    rt.store.latest_workflow_run().unwrap().expect("run exists")
}

fn find_run_id(step: &RuntimeStep) -> WorkflowRunId {
    step.events
        .iter()
        .find_map(|e| match e {
            RuntimeEvent::WorkflowRunUpdated { run } => Some(run.id),
            _ => None,
        })
        .expect("workflow run id")
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
        RuntimeEvent::WorkflowRunUpdated { run }
            if run.id == run_id && run.status == WorkflowRunStatus::Done
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
        RuntimeEvent::WorkflowRunUpdated { run }
            if run.status == WorkflowRunStatus::Verifying
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
        RuntimeEvent::WorkflowRunUpdated { run }
            if run.id == run_id && run.status == WorkflowRunStatus::Done
    )));

    // Session freed.
    let step = rt.on_ui_command(run_mode("next"));
    assert!(step.actions.iter().any(|a| a.member == builder));

    std::fs::remove_dir_all(&dir).ok();
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
    assert_eq!(run.status, WorkflowRunStatus::Running);
}

#[test]
fn review_max_iterations_blocks() {
    let mut rt = runtime();
    let builder = MemberId::new("builder");
    let reviewer = MemberId::new("reviewer");

    let step = rt.on_ui_command(UiCommand::RunMode {
        mode: CollabMode::Review,
        task: "tight loop".to_string(),
        overrides: vec![("max_iterations".to_string(), "1".to_string())],
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
        RuntimeEvent::WorkflowRunUpdated { run }
            if run.id == run_id && run.status == WorkflowRunStatus::Blocked
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
        RuntimeEvent::WorkflowRunUpdated { run }
            if run.id == run_id && run.status == WorkflowRunStatus::Blocked
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
    let run = rt.store.workflow_run(run_id).unwrap();
    assert_eq!(run.status, WorkflowRunStatus::Blocked);
    assert!(
        !step.events.iter().any(|e| matches!(
            e,
            RuntimeEvent::WorkflowRunUpdated { run }
                if run.id == run_id && run.status == WorkflowRunStatus::Done
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
        RuntimeEvent::WorkflowRunUpdated { run }
            if run.id == run_id && run.status == WorkflowRunStatus::Blocked
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
    assert_eq!(
        rt.store.workflow_run(run_id).unwrap().status,
        WorkflowRunStatus::Running
    );
    drop(rt);

    let store = SqliteStore::open(&path).unwrap();
    let _rt = TeamRuntime::new(team(), store).with_approvals(false);
    drop(_rt);

    let store = SqliteStore::open(&path).unwrap();
    let run = store.workflow_run(run_id).unwrap();
    assert_eq!(run.status, WorkflowRunStatus::Blocked);
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
    assert_eq!(
        rt.store.workflow_run(run_id).unwrap().status,
        WorkflowRunStatus::Blocked
    );

    let step = rt.on_ui_command(UiCommand::ContinueWorkflow {
        run_id: Some(run_id),
        note: None,
    });
    assert!(
        step.actions.iter().any(|a| a.member == builder),
        "continue should re-dispatch the building phase: {:?}",
        step.actions.iter().map(|a| &a.member).collect::<Vec<_>>()
    );
    let run = rt.store.workflow_run(run_id).unwrap();
    assert_eq!(run.status, WorkflowRunStatus::Running);
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

// --- M4 lead + roundtable helpers ----------------------------------------

fn lead_team() -> TeamConfig {
    let mut config = TeamConfig::new("lead-team", "/tmp/ws")
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

fn lead_runtime() -> TeamRuntime {
    TeamRuntime::new(lead_team(), SqliteStore::in_memory().unwrap()).with_approvals(false)
}

fn run_lead(task: &str) -> UiCommand {
    UiCommand::RunMode {
        mode: CollabMode::Lead,
        task: task.to_string(),
        overrides: vec![],
    }
}

fn run_roundtable(task: &str, overrides: Vec<(String, String)>) -> UiCommand {
    UiCommand::RunMode {
        mode: CollabMode::Roundtable,
        task: task.to_string(),
        overrides,
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
fn lead_dispatches_owned_todo_steps() {
    let mut rt = lead_runtime();
    let planner = MemberId::new("planner");
    let builder = MemberId::new("builder");
    let reviewer = MemberId::new("reviewer");

    let step = rt.on_ui_command(run_lead("ship the feature"));
    assert!(
        step.actions.iter().any(|a| {
            a.member == planner
                && a.prompt.contains(LEAD_PLAN_HINT)
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
         @@workflow_step {\"action\":\"add\",\"owner\":\"builder\",\"title\":\"Implement core\"}\n\
         @@workflow_step {\"action\":\"add\",\"owner\":\"reviewer\",\"title\":\"Write tests\"}",
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
        reviewer_action.prompt.contains("step #2"),
        "reviewer owns step 2: {}",
        reviewer_action.prompt
    );
    assert!(
        !reviewer_action.prompt.contains("step #1"),
        "reviewer should not see builder step"
    );

    let run = rt.store.workflow_run(run_id).unwrap();
    assert!(
        run.steps
            .iter()
            .all(|s| s.status == WorkflowStepStatus::Doing),
        "owned todos should be Doing: {:?}",
        run.steps
    );
}

#[test]
fn lead_empty_checklist_nudges_then_blocks() {
    let mut rt = lead_runtime();
    let planner = MemberId::new("planner");

    let step = rt.on_ui_command(run_lead("empty plan"));
    let run_id = find_run_id(&step);

    let step = complete_ok(&mut rt, &planner, "I thought about it but wrote nothing");
    assert!(
        step.actions
            .iter()
            .any(|a| a.member == planner && a.prompt.contains(LEAD_PLAN_HINT)),
        "empty checklist should nudge the leader"
    );

    let step = complete_ok(&mut rt, &planner, "still nothing useful");
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::WorkflowRunUpdated { run }
            if run.id == run_id && run.status == WorkflowRunStatus::Blocked
    )));
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::Notice(text) if text.contains("no actionable plan")
    )));
}

#[test]
fn lead_unfinished_steps_return_to_leader() {
    let mut rt = lead_runtime();
    let planner = MemberId::new("planner");
    let builder = MemberId::new("builder");

    rt.on_ui_command(run_lead("partial work"));
    complete_ok(
        &mut rt,
        &planner,
        "@@workflow_step {\"action\":\"add\",\"owner\":\"builder\",\"title\":\"Do the thing\"}",
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
    assert_eq!(run.mode.as_ref().unwrap().state.phase, "leading");
}

#[test]
fn lead_all_done_enters_review_and_approve_finishes() {
    let mut rt = lead_runtime();
    let planner = MemberId::new("planner");
    let builder = MemberId::new("builder");
    let reviewer = MemberId::new("reviewer");

    let step = rt.on_ui_command(run_lead("finish path"));
    let run_id = find_run_id(&step);

    complete_ok(
        &mut rt,
        &planner,
        "@@workflow_step {\"action\":\"add\",\"owner\":\"builder\",\"title\":\"Implement core\"}\n\
         @@workflow_step {\"action\":\"add\",\"owner\":\"reviewer\",\"title\":\"Write docs\"}",
    );

    let step = complete_all(
        &mut rt,
        &[
            (
                builder,
                "@@workflow_step {\"action\":\"done\",\"step\":1}\ncore done",
            ),
            (
                reviewer.clone(),
                "@@workflow_step {\"action\":\"done\",\"step\":2}\ndocs done",
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
        "reviewer should get lead review with step titles: {:?}",
        step.actions.iter().map(|a| &a.prompt).collect::<Vec<_>>()
    );

    let step = complete_ok(
        &mut rt,
        &reviewer,
        "@@review {\"verdict\":\"approve\",\"summary\":\"looks good\"}",
    );
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::WorkflowRunUpdated { run }
            if run.id == run_id && run.status == WorkflowRunStatus::Done
    )));
}

#[test]
fn lead_request_changes_returns_to_leader() {
    let mut rt = lead_runtime();
    let planner = MemberId::new("planner");
    let builder = MemberId::new("builder");
    let reviewer = MemberId::new("reviewer");

    rt.on_ui_command(run_lead("needs changes"));
    complete_ok(
        &mut rt,
        &planner,
        "@@workflow_step {\"action\":\"add\",\"owner\":\"builder\",\"title\":\"Build it\"}",
    );
    complete_ok(
        &mut rt,
        &builder,
        "@@workflow_step {\"action\":\"done\",\"step\":1}\ndone",
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
    assert_eq!(run.mode.as_ref().unwrap().state.phase, "leading");
}

#[test]
fn roundtable_runs_rounds_then_moderates() {
    let mut rt = lead_runtime();
    let planner = MemberId::new("planner");
    let builder = MemberId::new("builder");
    let reviewer = MemberId::new("reviewer");

    let step = rt.on_ui_command(run_roundtable("pick an architecture", vec![]));
    assert_eq!(
        step.actions.len(),
        3,
        "round 1 should dispatch all participants: {:?}",
        step.actions.iter().map(|a| &a.member).collect::<Vec<_>>()
    );
    assert!(
        step.actions
            .iter()
            .all(|a| a.prompt.contains(ROUNDTABLE_HINT)),
        "round 1 prompts should include ROUNDTABLE_HINT"
    );
    let run_id = find_run_id(&step);

    let step = complete_all(
        &mut rt,
        &[
            (planner.clone(), "Planner says use monolith"),
            (builder.clone(), "Builder says use microservices"),
            (reviewer.clone(), "Reviewer says hybrid"),
        ],
    );
    assert_eq!(
        step.actions.len(),
        3,
        "round 2 should dispatch all three: {:?}",
        step.actions.iter().map(|a| &a.member).collect::<Vec<_>>()
    );
    for action in &step.actions {
        assert!(
            action.prompt.contains(ROUNDTABLE_HINT),
            "digest should keep ROUNDTABLE_HINT"
        );
        // Digests contain OTHER members' texts, not own.
        match action.member.as_str() {
            "planner" => {
                assert!(action.prompt.contains("Builder says use microservices"));
                assert!(action.prompt.contains("Reviewer says hybrid"));
                assert!(!action.prompt.contains("Planner says use monolith"));
            }
            "builder" => {
                assert!(action.prompt.contains("Planner says use monolith"));
                assert!(action.prompt.contains("Reviewer says hybrid"));
                assert!(!action.prompt.contains("Builder says use microservices"));
            }
            "reviewer" => {
                assert!(action.prompt.contains("Planner says use monolith"));
                assert!(action.prompt.contains("Builder says use microservices"));
                assert!(!action.prompt.contains("Reviewer says hybrid"));
            }
            other => panic!("unexpected member {other}"),
        }
    }

    let step = complete_all(
        &mut rt,
        &[
            (planner.clone(), "still monolith"),
            (builder.clone(), "still micro"),
            (reviewer.clone(), "still hybrid"),
        ],
    );
    assert!(
        step.actions.iter().any(|a| {
            a.member == planner
                && a.prompt.contains(MODERATOR_HINT)
                && (a.prompt.contains("still monolith")
                    || a.prompt.contains("still micro")
                    || a.prompt.contains("still hybrid")
                    || a.prompt.contains("Planner")
                    || a.prompt.contains("Builder"))
        }),
        "moderator should get synthesis prompt with transcripts: {:?}",
        step.actions.iter().map(|a| &a.prompt).collect::<Vec<_>>()
    );

    let step = complete_ok(&mut rt, &planner, "Converge on hybrid.");
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::WorkflowRunUpdated { run }
            if run.id == run_id && run.status == WorkflowRunStatus::Done
    )));
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::Notice(text) if text.contains("roundtable") && text.contains("finished")
    )));
}

#[test]
fn roundtable_without_moderator_finishes() {
    let mut config = TeamConfig::new("rt-no-mod", "/tmp/ws")
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
    config.default_target = Some(DefaultTarget::Member(MemberId::new("alice")));
    let mut rt = TeamRuntime::new(config, SqliteStore::in_memory().unwrap()).with_approvals(false);

    let alice = MemberId::new("alice");
    let bob = MemberId::new("bob");
    let step = rt.on_ui_command(run_roundtable(
        "quick chat",
        vec![("rounds".to_string(), "1".to_string())],
    ));
    let run_id = find_run_id(&step);
    assert_eq!(step.actions.len(), 2);

    let step = complete_all(&mut rt, &[(alice, "Alice view"), (bob, "Bob view")]);
    assert!(
        step.actions.is_empty(),
        "no moderator → no further dispatch: {:?}",
        step.actions.iter().map(|a| &a.member).collect::<Vec<_>>()
    );
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::WorkflowRunUpdated { run }
            if run.id == run_id && run.status == WorkflowRunStatus::Done
    )));
}

#[test]
fn roundtable_single_participant_refused() {
    let mut rt = lead_runtime();
    let step = rt.on_ui_command(run_roundtable(
        "solo",
        vec![("participants".to_string(), "builder".to_string())],
    ));
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::Notice(text) if text.contains("at least two participants")
    )));
    assert!(step.actions.is_empty());
}

#[test]
fn lead_resume_after_abort_redispatches_leader() {
    let mut rt = lead_runtime();
    let planner = MemberId::new("planner");

    let step = rt.on_ui_command(run_lead("resume plan"));
    let run_id = find_run_id(&step);
    rt.on_ui_command(UiCommand::Cancel { member: None });
    let _ = rt.on_agent_event(
        &planner,
        AgentEvent::Exited {
            code: None,
            ok: false,
        },
    );
    assert_eq!(
        rt.store.workflow_run(run_id).unwrap().status,
        WorkflowRunStatus::Blocked
    );

    let step = rt.on_ui_command(UiCommand::ContinueWorkflow {
        run_id: Some(run_id),
        note: None,
    });
    assert!(
        step.actions.iter().any(|a| a.member == planner),
        "continue should re-dispatch the leader: {:?}",
        step.actions.iter().map(|a| &a.member).collect::<Vec<_>>()
    );
    let run = rt.store.workflow_run(run_id).unwrap();
    assert_eq!(run.status, WorkflowRunStatus::Running);
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

    let step = rt.on_ui_command(UiCommand::ContinueWorkflow {
        run_id: Some(run_id),
        note: None,
    });

    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::Notice(text) if text.contains("left the roster") && text.contains("reviewer")
    )));
    assert!(step.actions.is_empty(), "no dispatch to a missing member");
    assert_eq!(
        rt.store.workflow_run(run_id).unwrap().status,
        WorkflowRunStatus::Blocked,
        "the run stays blocked instead of half-resuming"
    );
}

#[test]
fn manual_verify_on_active_mode_run_is_refused() {
    let mut rt = runtime();
    let step = rt.on_ui_command(run_mode("review this"));
    let run_id = find_run_id(&step);

    let step = rt.on_ui_command(UiCommand::VerifyWorkflow {
        run_id: Some(run_id),
        command: Some("true".to_string()),
    });

    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::Notice(text) if text.contains("active mode run")
    )));
    assert!(step.verify_actions.is_empty());
}
