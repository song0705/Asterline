use super::super::*;
use super::*;

#[test]
fn plan_reviewer_approves_before_builder_dispatch() {
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
    let reviewer_action = step
        .actions
        .iter()
        .find(|a| a.member == reviewer)
        .expect("reviewer RunAction");
    assert!(
        !step.actions.iter().any(|a| a.member == builder),
        "the Builder must not receive work before the plan is approved: {:?}",
        step.actions.iter().map(|a| &a.prompt).collect::<Vec<_>>()
    );
    assert!(
        reviewer_action
            .prompt
            .contains("plan review before implementation"),
        "Reviewer should receive the plan, not an implementation step: {}",
        reviewer_action.prompt
    );
    assert!(
        reviewer_action.prompt.contains("Implement core")
            && reviewer_action.prompt.contains("Write tests"),
        "Reviewer should see the full checklist: {}",
        reviewer_action.prompt
    );

    let step = complete_ok(
        &mut rt,
        &reviewer,
        "@@review {\"verdict\":\"approve\",\"summary\":\"ready to build\"}",
    );
    let builder_action = step
        .actions
        .iter()
        .find(|a| a.member == builder)
        .expect("Builder RunAction after plan approval");
    assert!(
        builder_action.prompt.contains("step #1") && builder_action.prompt.contains("step #2"),
        "the configured Builder should receive the entire approved checklist: {}",
        builder_action.prompt
    );
    assert!(
        builder_action
            .prompt
            .contains(r#"@@team_message {"to":"planner","kind":"reply""#),
        "Builder must report completion to the planning lead"
    );

    let run = rt.store.run(run_id).unwrap();
    assert!(
        run.steps
            .iter()
            .all(|s| s.status == RunStepStatus::Doing && s.owner.as_ref() == Some(&builder)),
        "the entire approved plan should be assigned to the configured Builder: {:?}",
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
    let mut config = plan_team();
    config.modes.plan = Some(PlanModeConfig {
        builder: Some(MemberId::new("builder")),
        reviewer: Some(MemberId::new("reviewer")),
        ..PlanModeConfig::default()
    });
    let mut rt = TeamRuntime::new(config, store).with_approvals(false);
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
fn plan_unfinished_steps_get_one_owner_nudge_before_replanning() {
    let mut rt = plan_runtime();
    let planner = MemberId::new("planner");
    let builder = MemberId::new("builder");
    let reviewer = MemberId::new("reviewer");

    rt.on_ui_command(run_plan("partial work"));
    complete_ok(
        &mut rt,
        &planner,
        "@@run_step {\"action\":\"add\",\"owner\":\"builder\",\"title\":\"Do the thing\"}",
    );
    complete_ok(
        &mut rt,
        &reviewer,
        "@@review {\"verdict\":\"approve\",\"summary\":\"ready\"}",
    );

    let step = complete_ok(&mut rt, &builder, "I worked but forgot to mark done");
    assert!(
        step.actions.iter().any(|a| {
            a.member == builder
                && a.prompt.contains("Do the thing")
                && a.prompt.contains("@@run_step")
        }),
        "owner should receive one checklist nudge: {:?}",
        step.actions.iter().map(|a| &a.prompt).collect::<Vec<_>>()
    );
    let run = latest_run(&rt);
    assert_eq!(run.mode.as_ref().unwrap().state.iteration, 1);
    assert_eq!(run.mode.as_ref().unwrap().state.phase, "executing");

    let step = complete_ok(&mut rt, &builder, "I still forgot to mark it done");
    assert!(step.actions.iter().any(|a| {
        a.member == planner && (a.prompt.contains("Do the thing") || a.prompt.contains("#1"))
    }));
    let run = latest_run(&rt);
    assert_eq!(run.mode.as_ref().unwrap().state.iteration, 2);
    assert_eq!(run.mode.as_ref().unwrap().state.phase, "planning");
}

#[test]
fn plan_builder_completion_finishes_after_plan_approval() {
    let mut rt = plan_runtime();
    let planner = MemberId::new("planner");
    let builder = MemberId::new("builder");
    let reviewer = MemberId::new("reviewer");

    let step = rt.on_ui_command(run_plan("finish path"));
    let run_id = find_run_id(&step);

    let step = complete_ok(
        &mut rt,
        &planner,
        "@@run_step {\"action\":\"add\",\"owner\":\"builder\",\"title\":\"Implement core\"}\n\
         @@run_step {\"action\":\"add\",\"owner\":\"reviewer\",\"title\":\"Write docs\"}",
    );
    assert!(step.actions.iter().any(|a| {
        a.member == reviewer && a.prompt.contains("plan review before implementation")
    }));

    let step = complete_ok(
        &mut rt,
        &reviewer,
        "@@review {\"verdict\":\"approve\",\"summary\":\"ready to build\"}",
    );
    assert!(
        step.actions.iter().any(|a| {
            a.member == builder
                && a.prompt.contains("Implement core")
                && a.prompt.contains("Write docs")
        }),
        "Builder should get the approved plan: {:?}",
        step.actions.iter().map(|a| &a.prompt).collect::<Vec<_>>()
    );

    let step = complete_ok(
        &mut rt,
        &builder,
        "@@run_step {\"action\":\"done\",\"step\":1}\n\
         @@run_step {\"action\":\"done\",\"step\":2}\ncompleted",
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
    let reviewer = MemberId::new("reviewer");

    rt.on_ui_command(run_plan("needs changes"));
    complete_ok(
        &mut rt,
        &planner,
        "@@run_step {\"action\":\"add\",\"owner\":\"builder\",\"title\":\"Build it\"}",
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
fn plan_without_builder_is_rejected_before_leader_dispatch() {
    let mut rt =
        TeamRuntime::new(plan_team(), SqliteStore::in_memory().unwrap()).with_approvals(false);

    let step = rt.on_ui_command(run_plan("produce a plan"));

    assert!(step.actions.is_empty());
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Notice(text) if text == "plan mode needs a builder"
    )));
    assert!(rt.store.latest_run().unwrap().is_none());
}

#[test]
fn plan_without_reviewer_dispatches_to_the_required_builder() {
    let mut config = plan_team();
    config.modes.plan = Some(PlanModeConfig {
        builder: Some(MemberId::new("builder")),
        ..PlanModeConfig::default()
    });
    let mut rt = TeamRuntime::new(config, SqliteStore::in_memory().unwrap()).with_approvals(false);
    let planner = MemberId::new("planner");
    let builder = MemberId::new("builder");

    rt.on_ui_command(run_plan("skip plan review"));
    let step = complete_ok(
        &mut rt,
        &planner,
        "@@run_step {\"action\":\"add\",\"title\":\"Implement\"}",
    );

    assert!(step.actions.iter().any(|action| action.member == builder));
    assert!(
        !step
            .actions
            .iter()
            .any(|action| action.member == MemberId::new("reviewer"))
    );
}

#[test]
fn plan_manual_execution_waits_for_approval() {
    let mut config = plan_team();
    config.modes.plan = Some(PlanModeConfig {
        builder: Some(MemberId::new("builder")),
        auto_execute: Some(false),
        ..PlanModeConfig::default()
    });
    let mut rt = TeamRuntime::new(config, SqliteStore::in_memory().unwrap()).with_approvals(false);
    let planner = MemberId::new("planner");
    let builder = MemberId::new("builder");

    let started = rt.on_ui_command(run_plan("confirm before build"));
    let run_id = find_run_id(&started);
    let waiting = complete_ok(
        &mut rt,
        &planner,
        "@@run_step {\"action\":\"add\",\"title\":\"Implement\"}",
    );
    let approval = waiting
        .events
        .iter()
        .find_map(|event| match event {
            RuntimeEvent::ApprovalRequested { id, action, .. } if action == "plan_execution" => {
                Some(*id)
            }
            _ => None,
        })
        .expect("manual plan execution should require approval");
    assert!(waiting.actions.is_empty());
    assert_eq!(
        rt.store.run(run_id).unwrap().mode.unwrap().state.phase,
        "awaiting_execution"
    );

    let approved = rt.on_ui_command(UiCommand::Approve {
        id: approval,
        decision: ApprovalDecision::Approve,
    });
    assert!(
        approved
            .actions
            .iter()
            .any(|action| action.member == builder)
    );
    assert_eq!(
        rt.store.run(run_id).unwrap().mode.unwrap().state.phase,
        "executing"
    );
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
        ChatItem::User { body, .. } if body == topic
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
fn brainstorm_accepts_case_insensitive_candidate_ids() {
    let mut rt = plan_runtime();
    let planner = MemberId::new("planner");
    let run_id = enter_fallback_voting(&mut rt);

    let accepted = rt.on_agent_event(
        &planner,
        AgentEvent::MessageCompleted(
            "@@brainstorm_vote {\"ranked\":[\"r1-a#1\",\"r1-b#1\"]}".to_string(),
        ),
    );

    assert!(!accepted.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Notice(text) if text.contains("unknown brainstorm candidate")
    )));
    assert_eq!(rt.mode_sessions[&run_id].vote_count, 1);
    assert_eq!(rt.mode_sessions[&run_id].votes[0].ranked[0], "R1-A#1");
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
            (
                planner.clone(),
                "@@brainstorm_card {\"title\":\"one\",\"operation\":\"seed\",\"proposal\":\"p1\",\"mechanism\":\"m\",\"sources\":[]}\n\
                 @@brainstorm_card {\"title\":\"two\",\"operation\":\"seed\",\"proposal\":\"p2\",\"mechanism\":\"m\",\"sources\":[]}",
            ),
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
    // Two structured cards plus two free-text batches.  Resume must retain
    // the same card-count semantics as live generation.
    assert_eq!(rt.mode_sessions[&run_id].idea_count, 4);
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
