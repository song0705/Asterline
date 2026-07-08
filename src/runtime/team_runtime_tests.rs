use super::*;
use crate::domain::team::{BackendKind, DefaultTarget, TeamMember};

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
fn team_message_routes_to_another_member() {
    let mut rt = runtime();
    rt.on_ui_command(user("plan it"));
    let builder = MemberId::new("builder");

    let step = rt.on_agent_event(
        &builder,
        AgentEvent::MessageCompleted(
            r#"@@team_message {"to":"reviewer","body":"please review"}"#.to_string(),
        ),
    );

    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::Route { to, .. } if to == &vec!["reviewer".to_string()]
    )));
    // The relay is dispatched to the reviewer.
    assert!(
        step.actions
            .iter()
            .any(|a| a.member == MemberId::new("reviewer"))
    );
    assert!(step.actions[0].prompt.contains("please review"));
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
