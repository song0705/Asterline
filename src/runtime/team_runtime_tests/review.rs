use super::super::*;
use super::*;

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
    assert!(
        step.events.iter().any(|e| matches!(
            e,
            RuntimeEvent::Notice(text)
                if text.contains("review run-") && text.contains("started")
        )),
        "start notice should name the review run: {:?}",
        step.events
    );

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
        "review mode does not schedule a verify command"
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
fn review_approve_does_not_auto_verify() {
    let dir =
        std::env::temp_dir().join(format!("asterline-review-no-verify-{}", std::process::id()));
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
        step.verify_actions.is_empty(),
        "review approve must not run a workspace check"
    );
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::RunUpdated { run }
            if run.status == RunStatus::Done
    )));

    let step = rt.on_ui_command(run_mode("next"));
    assert!(step.actions.iter().any(|a| a.member == builder));

    std::fs::remove_dir_all(&dir).ok();
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
fn review_hint_is_appended_to_reviewer_prompt() {
    let mut rt = runtime();
    rt.config.modes.review = Some(ReviewModeConfig {
        reviewer_hint: Some("look at the parser tests".to_string()),
        ..ReviewModeConfig::default()
    });
    let builder = MemberId::new("builder");
    let reviewer = MemberId::new("reviewer");
    rt.on_ui_command(run_mode("use a hint"));
    let step = complete_ok(&mut rt, &builder, "done");
    assert!(
        step.actions.iter().any(|a| {
            a.member == reviewer
                && a.prompt.contains("Reviewer note from the user")
                && a.prompt.contains("look at the parser tests")
        }),
        "reviewer prompt should include the optional hint: {:?}",
        step.actions.iter().map(|a| &a.prompt).collect::<Vec<_>>()
    );
    let step = complete_ok(
        &mut rt,
        &reviewer,
        "@@review {\"verdict\":\"approve\",\"summary\":\"ok\"}",
    );
    assert!(step.verify_actions.is_empty());
}

#[test]
fn plan_builder_completion_finishes_without_engine_verify() {
    let mut rt = plan_runtime();
    let planner = MemberId::new("planner");
    let builder = MemberId::new("builder");
    let reviewer = MemberId::new("reviewer");
    rt.on_ui_command(run_plan("ship plan"));
    complete_ok(
        &mut rt,
        &planner,
        "@@run_step {\"action\":\"add\",\"owner\":\"builder\",\"title\":\"Do it\"}",
    );
    complete_ok(
        &mut rt,
        &reviewer,
        "@@review {\"verdict\":\"approve\",\"summary\":\"plan is ready\"}",
    );
    let step = complete_ok(
        &mut rt,
        &builder,
        "@@run_step {\"action\":\"done\",\"step\":1}\ndone",
    );
    assert!(step.verify_actions.is_empty());
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::RunUpdated { run } if run.status == RunStatus::Done
    )));
}

#[test]
fn plan_progress_prompt_includes_blocked_step_note() {
    let mut rt = plan_runtime();
    let planner = MemberId::new("planner");
    let builder = MemberId::new("builder");
    let reviewer = MemberId::new("reviewer");
    rt.on_ui_command(run_plan("note fidelity"));
    complete_ok(
        &mut rt,
        &planner,
        "@@run_step {\"action\":\"add\",\"owner\":\"builder\",\"title\":\"Do the thing\"}",
    );
    complete_ok(
        &mut rt,
        &reviewer,
        "@@review {\"verdict\":\"approve\",\"summary\":\"plan is ready\"}",
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
                && a.prompt.contains("configured Builder")
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
    let reviewer = MemberId::new("reviewer");

    rt.on_ui_command(run_plan("owner fail replan"));
    complete_ok(
        &mut rt,
        &planner,
        "@@run_step {\"action\":\"add\",\"owner\":\"builder\",\"title\":\"Fragile work\"}",
    );
    complete_ok(
        &mut rt,
        &reviewer,
        "@@review {\"verdict\":\"approve\",\"summary\":\"plan is ready\"}",
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
        builder: Some(MemberId::new("builder")),
        reviewer: Some(MemberId::new("reviewer")),
        max_iterations: Some(1),
        ..PlanModeConfig::default()
    });
    let planner = MemberId::new("planner");
    let builder = MemberId::new("builder");
    let reviewer = MemberId::new("reviewer");

    let step = rt.on_ui_command(run_plan("owner fail exhaust"));
    let run_id = find_run_id(&step);
    complete_ok(
        &mut rt,
        &planner,
        "@@run_step {\"action\":\"add\",\"owner\":\"builder\",\"title\":\"Fragile work\"}",
    );
    complete_ok(
        &mut rt,
        &reviewer,
        "@@review {\"verdict\":\"approve\",\"summary\":\"plan is ready\"}",
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
    let reviewer = MemberId::new("reviewer");

    let step = rt.on_ui_command(run_plan("abort mid execute"));
    let run_id = find_run_id(&step);
    complete_ok(
        &mut rt,
        &planner,
        "@@run_step {\"action\":\"add\",\"owner\":\"builder\",\"title\":\"Work\"}",
    );
    complete_ok(
        &mut rt,
        &reviewer,
        "@@review {\"verdict\":\"approve\",\"summary\":\"plan is ready\"}",
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
    assert!(
        step.events.iter().any(|e| matches!(
            e,
            RuntimeEvent::Notice(text)
                if text.contains("no structured @@review verdict")
                    && text.contains("request_changes")
        )),
        "treating free text as request_changes should be announced: {:?}",
        step.events
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
fn mode_dispatch_ignores_prompt_keywords() {
    let mut rt = TeamRuntime::new(team(), SqliteStore::in_memory().unwrap());
    let step = rt.on_ui_command(run_mode("run git status"));
    assert!(
        !step
            .events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::ApprovalRequested { .. }))
    );
    assert!(
        step.actions
            .iter()
            .any(|action| action.member == MemberId::new("builder")),
        "review dispatch must start without a prompt-keyword gate"
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
fn explicit_member_message_stays_routable_during_an_active_mode_run() {
    let mut rt = runtime();
    let reviewer = MemberId::new("reviewer");
    rt.on_ui_command(run_mode("review this change"));

    let step = rt.on_ui_command(UiCommand::UserMessage {
        target: MessageTarget::Member(reviewer.clone()),
        body: "@reviewer focus on the error path".to_string(),
    });

    assert!(step.actions.iter().any(|action| {
        action.member == reviewer && action.prompt.contains("focus on the error path")
    }));
    assert!(!step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Notice(text) if text.contains("already active")
    )));
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
