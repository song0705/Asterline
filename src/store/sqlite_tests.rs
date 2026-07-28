use super::*;
use crate::domain::team::{BackendKind, TeamConfig, TeamMember};

fn store() -> SqliteStore {
    SqliteStore::in_memory().expect("store initializes")
}

#[test]
fn existing_run_table_is_migrated_for_conversation_scoping() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE runs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            goal TEXT NOT NULL,
            status TEXT NOT NULL
        );",
    )
    .unwrap();
    let store = SqliteStore {
        conn,
        conversation: Cell::new(0),
    };

    store.create_schema().unwrap();

    let columns = {
        let mut stmt = store.conn.prepare("PRAGMA table_info(runs)").unwrap();
        stmt.query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap()
    };
    assert!(columns.iter().any(|column| column == "conversation_id"));
}

#[test]
fn conversation_snapshots_drive_resume_list_and_restore_data() {
    let store = store();
    let first = store.create_conversation().unwrap();
    store.set_conversation(first);
    let team = TeamConfig::new("saved", "/tmp/ws").with_member(TeamMember::new(
        "builder",
        "Builder",
        BackendKind::Codex,
        "build",
    ));
    let sessions = vec![StoredConversationSession {
        member: MemberId::new("builder"),
        backend: BackendKind::Codex,
        session_id: "codex-session-1".to_string(),
    }];
    store.save_conversation_snapshot(&team, &sessions).unwrap();
    let turn = store.create_turn().unwrap();
    store
        .record_user(turn, &[MemberId::new("builder")], "restore this exact chat")
        .unwrap();

    let second = store.create_conversation().unwrap();
    store.set_conversation(second);
    store.save_conversation_snapshot(&team, &[]).unwrap();

    let choices = store.resumable_conversations().unwrap();
    assert_eq!(choices.len(), 1);
    assert_eq!(choices[0].id, first);
    assert_eq!(choices[0].preview, "restore this exact chat");
    assert_eq!(choices[0].message_count, 1);
    assert_eq!(choices[0].member_count, 1);

    let restored = store.conversation_snapshot(first).unwrap().unwrap();
    assert_eq!(restored.team, team);
    assert_eq!(restored.sessions, sessions);
    assert_eq!(
        store.replay_chat_for(first).unwrap(),
        vec![ChatItem::User {
            body: "restore this exact chat".to_string()
        }]
    );
    store.set_conversation(first);
    assert_eq!(store.current_conversation().unwrap(), first);
}

#[test]
fn replays_chat_in_insertion_order() {
    let store = store();
    let turn = store.create_turn().unwrap();
    let builder = MemberId::new("builder");
    let reviewer = MemberId::new("reviewer");

    store
        .record_user(turn, std::slice::from_ref(&builder), "build the parser")
        .unwrap();
    store
        .record_agent(turn, &builder, "Builder", BackendKind::Codex, "on it")
        .unwrap();
    store
        .record_tool(turn, &builder, "shell", "cargo test", "ok", Some(true))
        .unwrap();
    store
        .record_route(turn, &builder, &["reviewer".to_string()], "please review")
        .unwrap();
    store
        .record_agent(
            turn,
            &reviewer,
            "Reviewer",
            BackendKind::Claude,
            "looks good",
        )
        .unwrap();

    let items = store.replay_chat().unwrap();
    assert_eq!(items.len(), 5);
    assert_eq!(
        items[0],
        ChatItem::User {
            body: "build the parser".to_string()
        }
    );
    assert!(matches!(
        &items[1],
        ChatItem::Agent { backend: BackendKind::Codex, text, .. } if text == "on it"
    ));
    assert!(matches!(
        &items[2],
        ChatItem::Tool { ok: Some(true), summary, detail, .. }
            if summary == "cargo test" && detail == "ok"
    ));
    assert!(matches!(
        &items[3],
        ChatItem::Route { to, .. } if to == &vec!["reviewer".to_string()]
    ));
    assert!(matches!(
        &items[4],
        ChatItem::Agent {
            backend: BackendKind::Claude,
            ..
        }
    ));
}

#[test]
fn error_and_notice_round_trip() {
    let store = store();
    store.record_notice(None, "relay paused").unwrap();
    store
        .record_error(None, Some(&MemberId::new("builder")), "process failed")
        .unwrap();

    let items = store.replay_chat().unwrap();
    assert_eq!(
        items[0],
        ChatItem::Notice {
            text: "relay paused".to_string()
        }
    );
    assert_eq!(
        items[1],
        ChatItem::Error {
            member: Some(MemberId::new("builder")),
            message: "process failed".to_string()
        }
    );
}

#[test]
fn verdict_message_and_run_event_round_trip() {
    use crate::domain::mode::CollabMode;

    let store = store();
    let turn = store.create_turn().unwrap();
    let reviewer = MemberId::new("reviewer");
    store
        .record_verdict(turn, &reviewer, true, "looks solid")
        .unwrap();

    let items = store.replay_chat().unwrap();
    assert!(items.iter().any(|item| matches!(
        item,
        ChatItem::Verdict {
            member,
            approve: true,
            summary
        } if member == &reviewer && summary == "looks solid"
    )));

    let run = store
        .create_mode_run(
            "review task",
            Some(&MemberId::new("builder")),
            CollabMode::Review,
            r#"{"phase":"reviewing","iteration":1,"max_iterations":3}"#,
        )
        .unwrap();
    store
        .record_run_verdict_event(run.id, false, "needs tests")
        .unwrap();
    let loaded = store.run(run.id).unwrap();
    assert!(
        loaded.events.iter().any(|event| {
            event.kind == "verdict"
                && event.title == "Changes requested"
                && event.detail.as_deref() == Some("needs tests")
        }),
        "verdict event missing: {:?}",
        loaded.events
    );

    store.record_run_verdict_event(run.id, true, "").unwrap();
    let loaded = store.run(run.id).unwrap();
    assert!(loaded.events.iter().any(|event| {
        event.kind == "verdict" && event.title == "Review approved" && event.detail.is_none()
    }));

    let state = store.run_mode_state(run.id).unwrap();
    assert!(state.is_some_and(|s| s.contains("reviewing")));
}

#[test]
fn sessions_upsert_and_resolve() {
    let store = store();
    let builder = MemberId::new("builder");
    assert_eq!(store.session_for(&builder).unwrap(), None);

    store
        .upsert_session(
            &builder,
            BackendKind::Codex,
            &AgentSessionId("thread-1".to_string()),
        )
        .unwrap();
    assert_eq!(
        store.session_for(&builder).unwrap(),
        Some(AgentSessionId("thread-1".to_string()))
    );

    store
        .upsert_session(
            &builder,
            BackendKind::Codex,
            &AgentSessionId("thread-2".to_string()),
        )
        .unwrap();
    assert_eq!(
        store.session_for(&builder).unwrap(),
        Some(AgentSessionId("thread-2".to_string()))
    );
}

#[test]
fn approvals_list_and_resolve() {
    let store = store();
    let turn = store.create_turn().unwrap();
    let id = store
        .insert_approval(
            Some(turn),
            Some(&MemberId::new("builder")),
            "git",
            "git push",
        )
        .unwrap();

    let pending = store.pending_approvals().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, id);
    assert_eq!(pending[0].action, "git");

    assert!(
        store
            .resolve_approval(id, ApprovalDecision::Approve)
            .unwrap()
    );
    assert!(
        !store
            .resolve_approval(id, ApprovalDecision::Reject)
            .unwrap()
    );
    assert!(store.pending_approvals().unwrap().is_empty());
}

#[test]
fn runs_record_status_and_verification() {
    let store = store();
    let builder = MemberId::new("builder");

    let run = store.create_run("ship the parser", Some(&builder)).unwrap();
    assert_eq!(run.status, RunStatus::Running);
    assert_eq!(run.coordinator, Some(builder));
    assert_eq!(run.attempt, 1);
    assert_eq!(run.events.len(), 1);
    assert_eq!(run.events[0].kind, "started");

    let run = store
        .update_run_status(run.id, RunStatus::Verifying)
        .unwrap();
    assert_eq!(run.status, RunStatus::Verifying);
    assert_eq!(run.events.last().unwrap().kind, "verifying");

    let run = store
        .set_run_verification(run.id, "cargo test", true, "ok")
        .unwrap();
    assert_eq!(run.status, RunStatus::Done);
    let verification = run.verification.expect("verification saved");
    assert_eq!(verification.command, "cargo test");
    assert!(verification.ok);
    assert_eq!(verification.summary, "ok");
    assert_eq!(run.events.last().unwrap().kind, "verification_passed");
    assert_eq!(
        run.events.last().unwrap().detail.as_deref(),
        Some("cargo test\nok")
    );

    let run = store.continue_run(run.id, Some("fix follow-up")).unwrap();
    assert_eq!(run.status, RunStatus::Running);
    assert_eq!(run.attempt, 2);
    assert_eq!(run.verification, None);
    assert_eq!(run.events.last().unwrap().kind, "continued");
    assert_eq!(
        run.events.last().unwrap().detail.as_deref(),
        Some("fix follow-up")
    );
    assert_eq!(run.events.last().unwrap().attempt, 2);

    let run = store
        .add_run_note(run.id, "waiting for design input")
        .unwrap();
    assert_eq!(run.status, RunStatus::Running);
    assert_eq!(run.events.last().unwrap().kind, "note");
    assert_eq!(
        run.events.last().unwrap().detail.as_deref(),
        Some("waiting for design input")
    );

    let run = store.add_run_step(run.id, None, "parse config").unwrap();
    assert_eq!(run.steps.len(), 1);
    assert_eq!(run.steps[0].number, 1);
    assert_eq!(run.steps[0].status, RunStepStatus::Todo);
    assert_eq!(run.steps[0].owner, None);
    assert_eq!(run.steps[0].title, "parse config");
    assert_eq!(run.events.last().unwrap().kind, "step_added");

    let reviewer = MemberId::new("reviewer");
    let run = store.assign_run_step(run.id, 1, Some(&reviewer)).unwrap();
    assert_eq!(run.steps[0].owner, Some(reviewer.clone()));
    assert_eq!(run.events.last().unwrap().kind, "step_assigned");
    assert_eq!(
        run.events.last().unwrap().detail.as_deref(),
        Some("#1 @reviewer: parse config")
    );

    let run = store
        .update_run_step(
            run.id,
            1,
            RunStepStatus::Done,
            Some("covered by config tests"),
        )
        .unwrap();
    assert_eq!(run.steps[0].status, RunStepStatus::Done);
    assert_eq!(
        run.steps[0].note.as_deref(),
        Some("covered by config tests")
    );
    assert_eq!(run.events.last().unwrap().kind, "step_updated");
    assert_eq!(
        run.events.last().unwrap().detail.as_deref(),
        Some("#1 done: parse config\ncovered by config tests")
    );

    let run = store
        .add_run_step(run.id, Some(&reviewer), "obsolete duplicate")
        .unwrap();
    assert_eq!(run.steps.len(), 2);
    assert_eq!(run.steps[1].owner, Some(reviewer));

    let run = store.rename_run_step(run.id, 2, "document config").unwrap();
    assert_eq!(run.steps[1].title, "document config");
    assert_eq!(run.events.last().unwrap().kind, "step_renamed");

    let run = store.remove_run_step(run.id, 1).unwrap();
    assert_eq!(run.steps.len(), 1);
    assert_eq!(run.steps[0].number, 1);
    assert_eq!(run.steps[0].title, "document config");
    assert_eq!(run.events.last().unwrap().kind, "step_removed");

    let run = store.assign_run_step(run.id, 1, None).unwrap();
    assert_eq!(run.steps[0].owner, None);
    assert_eq!(
        run.events.last().unwrap().detail.as_deref(),
        Some("#1 unassigned: document config")
    );

    let run = store.block_run(run.id, "missing API token").unwrap();
    assert_eq!(run.status, RunStatus::Blocked);
    assert_eq!(run.events.last().unwrap().kind, "blocked");
    assert_eq!(
        run.events.last().unwrap().detail.as_deref(),
        Some("missing API token")
    );

    assert_eq!(store.latest_run().unwrap().unwrap().id, run.id);
    assert_eq!(store.recent_runs(10).unwrap().len(), 1);
}

#[test]
fn runs_are_scoped_to_the_active_conversation() {
    let store = store();
    let first = store.create_conversation().unwrap();
    store.set_conversation(first);
    let first_run = store.create_run("first chat run", None).unwrap();

    let second = store.create_conversation().unwrap();
    store.set_conversation(second);
    let second_run = store.create_run("second chat run", None).unwrap();

    assert_eq!(
        store
            .recent_runs(10)
            .unwrap()
            .into_iter()
            .map(|run| run.id)
            .collect::<Vec<_>>(),
        vec![second_run.id]
    );
    assert_eq!(
        store.latest_run().unwrap().map(|run| run.id),
        Some(second_run.id)
    );

    store.set_conversation(first);
    assert_eq!(
        store
            .recent_runs(10)
            .unwrap()
            .into_iter()
            .map(|run| run.id)
            .collect::<Vec<_>>(),
        vec![first_run.id]
    );
    assert_eq!(
        store.latest_run().unwrap().map(|run| run.id),
        Some(first_run.id)
    );
}

#[test]
fn stream_events_and_logs_are_recorded() {
    let store = store();
    store
        .record_stream_event(&MemberId::new("builder"), r#"{"type":"thread.started"}"#)
        .unwrap();
    assert_eq!(store.stream_event_count().unwrap(), 1);

    store
        .record_log(&LogEntry::warn("builder", "stderr noise"))
        .unwrap();
    store
        .record_log(&LogEntry::error("runtime", "boom"))
        .unwrap();
    let logs = store.recent_logs(10).unwrap();
    assert_eq!(logs.len(), 2);
    assert_eq!(logs[0].message, "stderr noise");
    assert_eq!(logs[1].level, LogLevel::Error);
}

#[test]
fn upsert_team_snapshots_roster() {
    let store = store();
    let config = TeamConfig::new("mixed", "/tmp/ws")
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
    store.upsert_team(&config).unwrap();
    // Idempotent: a second snapshot replaces, not appends.
    store.upsert_team(&config).unwrap();

    let count: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM team_members", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 2);
}

#[test]
fn diff_round_trips_through_replay() {
    let store = store();
    let turn = store.create_turn().unwrap();
    let files = vec![
        ("src/a.rs".to_string(), "update".to_string()),
        ("src/b.rs".to_string(), "add".to_string()),
    ];
    store
        .record_diff(turn, &MemberId::new("builder"), &files)
        .unwrap();

    let items = store.replay_chat().unwrap();
    assert!(matches!(
        &items[0],
        ChatItem::Diff { files: f, .. } if *f == files
    ));
}

#[test]
fn mode_run_round_trips_and_filters_running() {
    use crate::domain::mode::CollabMode;

    let store = store();
    let builder = MemberId::new("builder");
    let state = r#"{"phase":"build","iteration":1,"max_iterations":3}"#;
    let run = store
        .create_mode_run(
            "review the parser",
            Some(&builder),
            CollabMode::Review,
            state,
        )
        .unwrap();

    assert_eq!(run.mode.as_ref().map(|m| m.mode), Some(CollabMode::Review));
    let mode = run.mode.as_ref().expect("mode present");
    assert_eq!(mode.state.phase, "build");
    assert_eq!(mode.state.iteration, 1);
    assert_eq!(mode.state.max_iterations, 3);

    let updated_state = r#"{"phase":"review","iteration":2,"max_iterations":3}"#;
    let updated = store.update_run_mode_state(run.id, updated_state).unwrap();
    assert_eq!(updated.mode.as_ref().unwrap().state.phase, "review");
    assert_eq!(updated.mode.as_ref().unwrap().state.iteration, 2);

    // Plain runs without mode are excluded.
    let plain = store.create_run("plain plan", Some(&builder)).unwrap();
    assert_eq!(plain.mode, None);
    assert_eq!(plain.legacy_mode, None);

    let running = store.running_mode_runs().unwrap();
    assert_eq!(running, vec![run.id]);

    store.update_run_status(run.id, RunStatus::Done).unwrap();
    assert!(store.running_mode_runs().unwrap().is_empty());

    // Verifying mode runs still count as in-flight.
    let verifying = store
        .create_mode_run(
            "plan the release",
            Some(&builder),
            CollabMode::Plan,
            r#"{"phase":"verify"}"#,
        )
        .unwrap();
    store
        .update_run_status(verifying.id, RunStatus::Verifying)
        .unwrap();
    assert_eq!(store.running_mode_runs().unwrap(), vec![verifying.id]);
}

#[test]
fn legacy_roundtable_mode_is_preserved_as_legacy_mode() {
    let store = store();
    let builder = MemberId::new("builder");
    let run = store
        .insert_run_with_raw_mode(
            "old discussion",
            Some(&builder),
            "roundtable",
            Some(r#"{"phase":"rounds","round":1,"rounds":2}"#),
            RunStatus::Done,
        )
        .unwrap();
    assert_eq!(run.mode, None);
    assert_eq!(run.legacy_mode.as_deref(), Some("roundtable"));

    let listed = store.recent_runs(10).unwrap();
    assert!(
        listed
            .iter()
            .any(|r| r.id == run.id && r.legacy_mode.as_deref() == Some("roundtable")),
        "legacy run should still appear in the runs list: {listed:?}"
    );
}
