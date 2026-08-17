use super::super::*;
use super::*;

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
            && member.system_prompt.as_deref().unwrap_or("").contains("Asterline team skill")
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
    assert!(step.actions[0].prompt.contains("Asterline team skill"));
    assert!(!step.actions[0].prompt.contains("$asterline-team"));
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
    assert!(step.actions[0].prompt.contains("Asterline team skill"));
    assert!(!step.actions[0].prompt.contains("$asterline-team"));

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
