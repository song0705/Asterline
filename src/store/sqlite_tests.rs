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
fn legacy_runs_are_attached_to_a_real_conversation() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE runs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            goal TEXT NOT NULL,
            status TEXT NOT NULL
        );
        INSERT INTO runs (goal, status) VALUES ('legacy run', 'running');",
    )
    .unwrap();
    let store = SqliteStore {
        conn,
        conversation: Cell::new(0),
    };

    store.create_schema().unwrap();

    let conversation: i64 = store
        .conn
        .query_row(
            "SELECT conversation_id FROM runs WHERE goal = 'legacy run'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(conversation > 0);
    assert_eq!(
        store
            .conn
            .query_row("SELECT COUNT(*) FROM conversations", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        1_i64
    );
}

#[test]
fn legacy_pending_approvals_are_rejected_and_scoped_during_migration() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE approvals (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            turn_id INTEGER,
            member_id TEXT,
            action TEXT NOT NULL,
            body TEXT NOT NULL,
            decision TEXT NOT NULL DEFAULT 'pending',
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );
        INSERT INTO approvals (action, body, decision)
        VALUES ('git', 'git push', 'pending'),
               ('shell', 'echo done', 'approved');",
    )
    .unwrap();
    let store = SqliteStore {
        conn,
        conversation: Cell::new(0),
    };

    store.create_schema().unwrap();

    let rows: Vec<(i64, String)> = store
        .conn
        .prepare("SELECT conversation_id, decision FROM approvals ORDER BY id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>>>()
        .unwrap();
    assert!(rows.iter().all(|(conversation, _)| *conversation > 0));
    assert_eq!(rows[0].1, "rejected");
    assert_eq!(rows[1].1, "approved");
}

#[test]
fn already_migrated_zero_conversation_rows_are_repaired_idempotently() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE runs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            conversation_id INTEGER NOT NULL DEFAULT 0,
            goal TEXT NOT NULL,
            status TEXT NOT NULL
        );
        CREATE TABLE approvals (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            conversation_id INTEGER NOT NULL DEFAULT 0,
            action TEXT NOT NULL,
            body TEXT NOT NULL,
            decision TEXT NOT NULL DEFAULT 'pending'
        );
        INSERT INTO runs (goal, status) VALUES ('orphan run', 'running');
        INSERT INTO approvals (action, body) VALUES ('shell', 'echo unsafe');",
    )
    .unwrap();
    let store = SqliteStore {
        conn,
        conversation: Cell::new(0),
    };

    store.create_schema().unwrap();
    store.create_schema().unwrap();

    let run_conversation: i64 = store
        .conn
        .query_row("SELECT conversation_id FROM runs", [], |row| row.get(0))
        .unwrap();
    let (approval_conversation, decision): (i64, String) = store
        .conn
        .query_row(
            "SELECT conversation_id, decision FROM approvals",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(run_conversation > 0);
    assert_eq!(approval_conversation, run_conversation);
    assert_eq!(decision, "rejected");
}

#[test]
fn legacy_conversation_snapshot_defaults_to_normal_mode() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE conversation_snapshots (
            conversation_id INTEGER PRIMARY KEY,
            team_json TEXT NOT NULL,
            sessions_json TEXT NOT NULL,
            updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );",
    )
    .unwrap();
    let team = TeamConfig::new("saved", "/tmp/ws").with_member(TeamMember::new(
        "builder",
        "Builder",
        BackendKind::Codex,
        "build",
    ));
    conn.execute(
        "INSERT INTO conversation_snapshots (
            conversation_id, team_json, sessions_json
         ) VALUES (1, ?1, '[]')",
        params![serde_json::to_string(&team).unwrap()],
    )
    .unwrap();
    let store = SqliteStore {
        conn,
        conversation: Cell::new(0),
    };

    store.create_schema().unwrap();

    let snapshot = store.conversation_snapshot(1).unwrap().unwrap();
    assert_eq!(snapshot.mode, TerminalMode::Normal);
}

#[test]
fn conversation_snapshots_drive_resume_list_and_restore_data() {
    let store = store();
    let first = store.create_conversation().unwrap();
    store.set_conversation(first).unwrap();
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
    store
        .save_conversation_snapshot(&team, &sessions, TerminalMode::Review)
        .unwrap();
    let turn = store.create_turn().unwrap();
    store
        .record_user(turn, &[MemberId::new("builder")], "restore this exact chat")
        .unwrap();

    let second = store.create_conversation().unwrap();
    store.set_conversation(second).unwrap();
    store
        .save_conversation_snapshot(&team, &[], TerminalMode::Normal)
        .unwrap();

    let choices = store.resumable_conversations().unwrap();
    assert_eq!(choices.len(), 1);
    assert_eq!(choices[0].id, first);
    assert_eq!(choices[0].preview, "restore this exact chat");
    assert_eq!(choices[0].message_count, 1);
    assert_eq!(choices[0].member_count, 1);

    let restored = store.conversation_snapshot(first).unwrap().unwrap();
    assert_eq!(restored.team, team);
    assert_eq!(restored.sessions, sessions);
    assert_eq!(restored.mode, TerminalMode::Review);
    assert_eq!(
        store.replay_chat_for(first).unwrap(),
        vec![ChatItem::User {
            body: "restore this exact chat".to_string()
        }]
    );
    store.set_conversation(first).unwrap();
    assert_eq!(store.current_conversation().unwrap(), first);
}

#[test]
fn current_conversation_also_updates_the_in_memory_selection() {
    let store = store();
    let conversation = store.current_conversation().unwrap();

    assert_eq!(store.active_conversation(), conversation);
    assert!(conversation > 0);
}

#[test]
fn selecting_a_missing_conversation_is_rejected_without_changing_selection() {
    let store = store();
    let conversation = store.current_conversation().unwrap();

    assert!(matches!(
        store.set_conversation(i64::MAX),
        Err(rusqlite::Error::QueryReturnedNoRows)
    ));
    assert_eq!(store.active_conversation(), conversation);
    assert_eq!(store.current_conversation().unwrap(), conversation);
}

#[test]
fn create_and_select_conversation_rolls_back_together() {
    let store = store();
    let original = store.current_conversation().unwrap();
    store
        .conn
        .execute_batch(
            "CREATE TRIGGER fail_conversation_selection
             BEFORE UPDATE ON runtime_state
             WHEN NEW.key = 'active_conversation'
             BEGIN SELECT RAISE(ABORT, 'selection unavailable'); END;",
        )
        .unwrap();

    assert!(store.create_and_set_conversation().is_err());

    let conversations: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM conversations", [], |row| row.get(0))
        .unwrap();
    assert_eq!(conversations, 1);
    assert_eq!(store.active_conversation(), original);
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
fn pending_approval_cleanup_is_scoped_to_the_active_conversation() {
    let store = store();
    let first = store.create_conversation().unwrap();
    store.set_conversation(first).unwrap();
    let first_id = store
        .insert_approval(None, None, "git", "git status")
        .unwrap();

    let second = store.create_conversation().unwrap();
    store.set_conversation(second).unwrap();
    store
        .insert_approval(None, None, "shell", "run command")
        .unwrap();

    assert_eq!(
        store
            .reject_pending_approvals_for_active_conversation()
            .unwrap(),
        1
    );
    assert!(store.pending_approvals().unwrap().is_empty());
    store.set_conversation(first).unwrap();
    assert_eq!(store.pending_approvals().unwrap().len(), 1);
    assert!(
        store
            .resolve_approval(first_id, ApprovalDecision::Reject)
            .unwrap()
    );
}

#[test]
fn compound_run_writes_roll_back_when_timeline_insert_fails() {
    let store = store();
    store
        .conn
        .execute_batch(
            "CREATE TRIGGER fail_run_event
             BEFORE INSERT ON run_events
             BEGIN SELECT RAISE(ABORT, 'timeline unavailable'); END;",
        )
        .unwrap();

    assert!(store.create_run("must be atomic", None).is_err());
    let run_count: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM runs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(run_count, 0, "run insert must roll back with its event");

    store
        .conn
        .execute_batch("DROP TRIGGER fail_run_event")
        .unwrap();
    let run = store.create_run("status must be atomic", None).unwrap();
    store
        .conn
        .execute_batch(
            "CREATE TRIGGER fail_status_event
             BEFORE INSERT ON run_events
             WHEN NEW.kind = 'verifying'
             BEGIN SELECT RAISE(ABORT, 'status event unavailable'); END;",
        )
        .unwrap();

    assert!(
        store
            .update_run_status(run.id, RunStatus::Verifying)
            .is_err()
    );
    let unchanged = store.run(run.id).unwrap();
    assert_eq!(unchanged.status, RunStatus::Running);
    assert_eq!(unchanged.events.len(), 1);
}

#[test]
fn roster_replacement_rolls_back_when_a_member_insert_fails() {
    let store = store();
    let original = TeamConfig::new("original", "/tmp/original")
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
    store.upsert_team(&original).unwrap();
    store
        .conn
        .execute_batch(
            "CREATE TRIGGER fail_broken_member
             BEFORE INSERT ON team_members
             WHEN NEW.member_id = 'broken'
             BEGIN SELECT RAISE(ABORT, 'member unavailable'); END;",
        )
        .unwrap();
    let replacement = TeamConfig::new("replacement", "/tmp/replacement").with_member(
        TeamMember::new("broken", "Broken", BackendKind::Grok, "test"),
    );

    assert!(store.upsert_team(&replacement).is_err());
    let team_name: String = store
        .conn
        .query_row("SELECT name FROM teams WHERE id = 1", [], |row| row.get(0))
        .unwrap();
    let members: Vec<String> = store
        .conn
        .prepare("SELECT member_id FROM team_members ORDER BY member_id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<Vec<_>>>()
        .unwrap();
    assert_eq!(team_name, "original");
    assert_eq!(members, vec!["builder", "reviewer"]);
}

#[test]
fn busy_timeout_waits_for_a_short_competing_writer() {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "asterline-busy-timeout-{}-{unique}.sqlite3",
        std::process::id()
    ));
    let store = SqliteStore::open(&path).unwrap();
    let locked_path = path.clone();
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
    let holder = std::thread::spawn(move || {
        let conn = Connection::open(locked_path).unwrap();
        conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        ready_tx.send(()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(150));
        conn.execute_batch("COMMIT").unwrap();
    });
    ready_rx.recv().unwrap();

    let started = std::time::Instant::now();
    store.create_turn().unwrap();
    assert!(started.elapsed() >= std::time::Duration::from_millis(75));

    holder.join().unwrap();
    drop(store);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(format!("{}-wal", path.display()));
    let _ = std::fs::remove_file(format!("{}-shm", path.display()));
}

#[test]
fn foreign_keys_remain_enabled_after_reopening_the_store() {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "asterline-foreign-keys-{}-{unique}.sqlite3",
        std::process::id()
    ));

    for _ in 0..2 {
        let store = SqliteStore::open(&path).unwrap();
        let enabled: i64 = store
            .conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        assert_eq!(enabled, 1);
        assert!(
            store
                .conn
                .execute(
                    "INSERT INTO conversation_snapshots
                        (conversation_id, team_json, sessions_json, mode)
                     VALUES (999, '{}', '[]', 'normal')",
                    [],
                )
                .is_err()
        );
    }

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(format!("{}-wal", path.display()));
    let _ = std::fs::remove_file(format!("{}-shm", path.display()));
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
    store.set_conversation(first).unwrap();
    let first_run = store.create_run("first chat run", None).unwrap();

    let second = store.create_conversation().unwrap();
    store.set_conversation(second).unwrap();
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
    assert!(matches!(
        store.update_run_status(first_run.id, RunStatus::Done),
        Err(rusqlite::Error::QueryReturnedNoRows)
    ));
    assert!(matches!(
        store.add_run_note(first_run.id, "wrong chat"),
        Err(rusqlite::Error::QueryReturnedNoRows)
    ));
    assert!(matches!(
        store.add_run_step(first_run.id, None, "wrong chat step"),
        Err(rusqlite::Error::QueryReturnedNoRows)
    ));
    let unchanged = store.run(first_run.id).unwrap();
    assert_eq!(unchanged.status, RunStatus::Running);
    assert_eq!(unchanged.events.len(), 1);
    assert!(unchanged.steps.is_empty());

    store.set_conversation(first).unwrap();
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
