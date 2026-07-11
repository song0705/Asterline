use super::*;
use crate::domain::team::{BackendKind, TeamMember};

fn store() -> SqliteStore {
    SqliteStore::in_memory().expect("store initializes")
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
fn workflow_runs_record_status_and_verification() {
    let store = store();
    let builder = MemberId::new("builder");

    let run = store
        .create_workflow_run("ship the parser", Some(&builder))
        .unwrap();
    assert_eq!(run.status, WorkflowRunStatus::Running);
    assert_eq!(run.coordinator, Some(builder));
    assert_eq!(run.attempt, 1);
    assert_eq!(run.events.len(), 1);
    assert_eq!(run.events[0].kind, "started");

    let run = store
        .update_workflow_status(run.id, WorkflowRunStatus::Verifying)
        .unwrap();
    assert_eq!(run.status, WorkflowRunStatus::Verifying);
    assert_eq!(run.events.last().unwrap().kind, "verifying");

    let run = store
        .set_workflow_verification(run.id, "cargo test", true, "ok")
        .unwrap();
    assert_eq!(run.status, WorkflowRunStatus::Done);
    let verification = run.verification.expect("verification saved");
    assert_eq!(verification.command, "cargo test");
    assert!(verification.ok);
    assert_eq!(verification.summary, "ok");
    assert_eq!(run.events.last().unwrap().kind, "verification_passed");
    assert_eq!(
        run.events.last().unwrap().detail.as_deref(),
        Some("cargo test\nok")
    );

    let run = store
        .continue_workflow_run(run.id, Some("fix follow-up"))
        .unwrap();
    assert_eq!(run.status, WorkflowRunStatus::Running);
    assert_eq!(run.attempt, 2);
    assert_eq!(run.verification, None);
    assert_eq!(run.events.last().unwrap().kind, "continued");
    assert_eq!(
        run.events.last().unwrap().detail.as_deref(),
        Some("fix follow-up")
    );
    assert_eq!(run.events.last().unwrap().attempt, 2);

    let run = store
        .add_workflow_note(run.id, "waiting for design input")
        .unwrap();
    assert_eq!(run.status, WorkflowRunStatus::Running);
    assert_eq!(run.events.last().unwrap().kind, "note");
    assert_eq!(
        run.events.last().unwrap().detail.as_deref(),
        Some("waiting for design input")
    );

    let run = store
        .add_workflow_step(run.id, None, "parse config")
        .unwrap();
    assert_eq!(run.steps.len(), 1);
    assert_eq!(run.steps[0].number, 1);
    assert_eq!(run.steps[0].status, WorkflowStepStatus::Todo);
    assert_eq!(run.steps[0].owner, None);
    assert_eq!(run.steps[0].title, "parse config");
    assert_eq!(run.events.last().unwrap().kind, "step_added");

    let reviewer = MemberId::new("reviewer");
    let run = store
        .assign_workflow_step(run.id, 1, Some(&reviewer))
        .unwrap();
    assert_eq!(run.steps[0].owner, Some(reviewer.clone()));
    assert_eq!(run.events.last().unwrap().kind, "step_assigned");
    assert_eq!(
        run.events.last().unwrap().detail.as_deref(),
        Some("#1 @reviewer: parse config")
    );

    let run = store
        .update_workflow_step(
            run.id,
            1,
            WorkflowStepStatus::Done,
            Some("covered by config tests"),
        )
        .unwrap();
    assert_eq!(run.steps[0].status, WorkflowStepStatus::Done);
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
        .add_workflow_step(run.id, Some(&reviewer), "obsolete duplicate")
        .unwrap();
    assert_eq!(run.steps.len(), 2);
    assert_eq!(run.steps[1].owner, Some(reviewer));

    let run = store
        .rename_workflow_step(run.id, 2, "document config")
        .unwrap();
    assert_eq!(run.steps[1].title, "document config");
    assert_eq!(run.events.last().unwrap().kind, "step_renamed");

    let run = store.remove_workflow_step(run.id, 1).unwrap();
    assert_eq!(run.steps.len(), 1);
    assert_eq!(run.steps[0].number, 1);
    assert_eq!(run.steps[0].title, "document config");
    assert_eq!(run.events.last().unwrap().kind, "step_removed");

    let run = store.assign_workflow_step(run.id, 1, None).unwrap();
    assert_eq!(run.steps[0].owner, None);
    assert_eq!(
        run.events.last().unwrap().detail.as_deref(),
        Some("#1 unassigned: document config")
    );

    let run = store
        .block_workflow_run(run.id, "missing API token")
        .unwrap();
    assert_eq!(run.status, WorkflowRunStatus::Blocked);
    assert_eq!(run.events.last().unwrap().kind, "blocked");
    assert_eq!(
        run.events.last().unwrap().detail.as_deref(),
        Some("missing API token")
    );

    assert_eq!(store.latest_workflow_run().unwrap().unwrap().id, run.id);
    assert_eq!(store.recent_workflow_runs(10).unwrap().len(), 1);
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
fn migrates_v1_to_conversations_and_preserves_messages() {
    let dir = std::env::temp_dir().join(format!("asterline-v2-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("v1.sqlite3");
    let _ = std::fs::remove_file(&path);

    // A v1 database: event-source `messages` (has `kind`) but no
    // conversation scoping, stamped user_version = 1.
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            r#"
                CREATE TABLE messages (
                    id           INTEGER PRIMARY KEY AUTOINCREMENT,
                    turn_id      INTEGER,
                    kind         TEXT NOT NULL,
                    member_id    TEXT,
                    display_name TEXT,
                    backend      TEXT,
                    text         TEXT,
                    name         TEXT,
                    summary      TEXT,
                    ok           INTEGER,
                    targets      TEXT,
                    created_at   TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                );
                INSERT INTO messages (kind, text) VALUES ('user', 'older message');
                PRAGMA user_version = 1;
                "#,
        )
        .unwrap();
    }

    let store = SqliteStore::open(&path).unwrap();
    let version: i64 = store
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);

    // The pre-v2 message is backfilled into the (now current) conversation.
    let conversation = store.current_conversation().unwrap();
    store.set_conversation(conversation);
    let items = store.replay_chat().unwrap();
    assert_eq!(
        items,
        vec![ChatItem::User {
            body: "older message".to_string()
        }]
    );

    // A new chat starts an empty transcript; the old one is untouched.
    let next = store.create_conversation().unwrap();
    store.set_conversation(next);
    assert!(store.replay_chat().unwrap().is_empty());

    drop(store);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn migrates_v2_gemini_backend_rows_to_agy() {
    let dir = std::env::temp_dir().join(format!("asterline-v3-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("v2.sqlite3");
    let _ = std::fs::remove_file(&path);

    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            r#"
                CREATE TABLE conversations (
                    id         INTEGER PRIMARY KEY AUTOINCREMENT,
                    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                );
                CREATE TABLE messages (
                    id              INTEGER PRIMARY KEY AUTOINCREMENT,
                    conversation_id INTEGER,
                    turn_id         INTEGER,
                    kind            TEXT NOT NULL,
                    member_id       TEXT,
                    display_name    TEXT,
                    backend         TEXT,
                    text            TEXT,
                    name            TEXT,
                    summary         TEXT,
                    ok              INTEGER,
                    targets         TEXT,
                    created_at      TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                );
                INSERT INTO conversations DEFAULT VALUES;
                INSERT INTO messages (conversation_id, kind, member_id, display_name, backend, text)
                    VALUES (1, 'agent', 'researcher', 'Researcher', 'gemini', 'old reply');
                PRAGMA user_version = 2;
                "#,
        )
        .unwrap();
    }

    let store = SqliteStore::open(&path).unwrap();
    let version: i64 = store
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    let conversation = store.current_conversation().unwrap();
    store.set_conversation(conversation);
    let items = store.replay_chat().unwrap();
    assert!(matches!(
        &items[0],
        ChatItem::Agent {
            backend: BackendKind::Agy,
            text,
            ..
        } if text == "old reply"
    ));

    drop(store);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn migrates_v3_to_workflow_runs_schema() {
    let dir = std::env::temp_dir().join(format!("asterline-v4-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("v3.sqlite3");
    let _ = std::fs::remove_file(&path);

    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            r#"
                CREATE TABLE conversations (
                    id         INTEGER PRIMARY KEY AUTOINCREMENT,
                    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                );
                CREATE TABLE messages (
                    id              INTEGER PRIMARY KEY AUTOINCREMENT,
                    conversation_id INTEGER,
                    turn_id         INTEGER,
                    kind            TEXT NOT NULL,
                    member_id       TEXT,
                    display_name    TEXT,
                    backend         TEXT,
                    text            TEXT,
                    name            TEXT,
                    summary         TEXT,
                    ok              INTEGER,
                    targets         TEXT,
                    created_at      TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                );
                INSERT INTO conversations DEFAULT VALUES;
                INSERT INTO messages (conversation_id, kind, text)
                    VALUES (1, 'user', 'older v3 message');
                PRAGMA user_version = 3;
                "#,
        )
        .unwrap();
    }

    let store = SqliteStore::open(&path).unwrap();
    let version: i64 = store
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);

    let run = store
        .create_workflow_run("verify migration", Some(&MemberId::new("builder")))
        .unwrap();
    assert_eq!(run.status, WorkflowRunStatus::Running);

    let conversation = store.current_conversation().unwrap();
    store.set_conversation(conversation);
    assert_eq!(
        store.replay_chat().unwrap(),
        vec![ChatItem::User {
            body: "older v3 message".to_string()
        }]
    );

    drop(store);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn migrates_v4_to_workflow_attempts_and_events() {
    let dir = std::env::temp_dir().join(format!("asterline-v6-v4-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("v4.sqlite3");
    let _ = std::fs::remove_file(&path);

    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            r#"
                CREATE TABLE workflow_runs (
                    id                   INTEGER PRIMARY KEY AUTOINCREMENT,
                    goal                 TEXT NOT NULL,
                    status               TEXT NOT NULL,
                    coordinator          TEXT,
                    verification_command TEXT,
                    verification_ok      INTEGER,
                    verification_summary TEXT,
                    created_at           TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    updated_at           TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                );
                INSERT INTO workflow_runs (goal, status, coordinator)
                    VALUES ('ship parser', 'done', 'builder');
                PRAGMA user_version = 4;
                "#,
        )
        .unwrap();
    }

    let store = SqliteStore::open(&path).unwrap();
    let version: i64 = store
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    let run = store.latest_workflow_run().unwrap().unwrap();
    assert_eq!(run.attempt, 1);
    assert_eq!(run.events.len(), 1);
    assert_eq!(run.events[0].kind, "imported");
    assert_eq!(run.events[0].detail.as_deref(), Some("done"));
    assert!(run.steps.is_empty());

    drop(store);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn migrates_v5_to_workflow_events() {
    let dir = std::env::temp_dir().join(format!("asterline-v6-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("v5.sqlite3");
    let _ = std::fs::remove_file(&path);

    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            r#"
                CREATE TABLE workflow_runs (
                    id                   INTEGER PRIMARY KEY AUTOINCREMENT,
                    goal                 TEXT NOT NULL,
                    status               TEXT NOT NULL,
                    coordinator          TEXT,
                    verification_command TEXT,
                    verification_ok      INTEGER,
                    verification_summary TEXT,
                    created_at           TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    updated_at           TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    attempt              INTEGER NOT NULL DEFAULT 1
                );
                INSERT INTO workflow_runs (goal, status, coordinator, attempt)
                    VALUES ('ship parser', 'failed', 'builder', 3);
                PRAGMA user_version = 5;
                "#,
        )
        .unwrap();
    }

    let store = SqliteStore::open(&path).unwrap();
    let version: i64 = store
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    let run = store.latest_workflow_run().unwrap().unwrap();
    assert_eq!(run.attempt, 3);
    assert_eq!(run.events.len(), 1);
    assert_eq!(run.events[0].kind, "imported");
    assert_eq!(run.events[0].attempt, 3);
    assert_eq!(run.events[0].detail.as_deref(), Some("failed"));
    assert!(run.steps.is_empty());

    drop(store);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn migrates_v6_to_workflow_steps() {
    let dir = std::env::temp_dir().join(format!("asterline-v7-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("v6.sqlite3");
    let _ = std::fs::remove_file(&path);

    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            r#"
                CREATE TABLE workflow_runs (
                    id                   INTEGER PRIMARY KEY AUTOINCREMENT,
                    goal                 TEXT NOT NULL,
                    status               TEXT NOT NULL,
                    coordinator          TEXT,
                    verification_command TEXT,
                    verification_ok      INTEGER,
                    verification_summary TEXT,
                    created_at           TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    updated_at           TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    attempt              INTEGER NOT NULL DEFAULT 1
                );
                CREATE TABLE workflow_run_events (
                    id         INTEGER PRIMARY KEY AUTOINCREMENT,
                    run_id     INTEGER NOT NULL,
                    attempt    INTEGER NOT NULL,
                    kind       TEXT NOT NULL,
                    title      TEXT NOT NULL,
                    detail     TEXT,
                    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                );
                INSERT INTO workflow_runs (goal, status, coordinator, attempt)
                    VALUES ('ship parser', 'running', 'builder', 1);
                PRAGMA user_version = 6;
                "#,
        )
        .unwrap();
    }

    let store = SqliteStore::open(&path).unwrap();
    let version: i64 = store
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    let run = store
        .add_workflow_step(WorkflowRunId(1), None, "write parser tests")
        .unwrap();
    assert_eq!(run.steps.len(), 1);
    assert_eq!(run.steps[0].title, "write parser tests");

    drop(store);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn migrates_v7_to_workflow_step_owner() {
    let dir = std::env::temp_dir().join(format!("asterline-v8-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("v7.sqlite3");
    let _ = std::fs::remove_file(&path);

    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            r#"
                CREATE TABLE workflow_runs (
                    id                   INTEGER PRIMARY KEY AUTOINCREMENT,
                    goal                 TEXT NOT NULL,
                    status               TEXT NOT NULL,
                    coordinator          TEXT,
                    verification_command TEXT,
                    verification_ok      INTEGER,
                    verification_summary TEXT,
                    created_at           TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    updated_at           TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    attempt              INTEGER NOT NULL DEFAULT 1
                );
                CREATE TABLE workflow_run_events (
                    id         INTEGER PRIMARY KEY AUTOINCREMENT,
                    run_id     INTEGER NOT NULL,
                    attempt    INTEGER NOT NULL,
                    kind       TEXT NOT NULL,
                    title      TEXT NOT NULL,
                    detail     TEXT,
                    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                );
                CREATE TABLE workflow_run_steps (
                    id         INTEGER PRIMARY KEY AUTOINCREMENT,
                    run_id     INTEGER NOT NULL,
                    position   INTEGER NOT NULL,
                    status     TEXT NOT NULL,
                    title      TEXT NOT NULL,
                    note       TEXT,
                    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                );
                INSERT INTO workflow_runs (goal, status, coordinator, attempt)
                    VALUES ('ship parser', 'running', 'builder', 1);
                INSERT INTO workflow_run_steps (run_id, position, status, title)
                    VALUES (1, 1, 'todo', 'write parser tests');
                PRAGMA user_version = 7;
                "#,
        )
        .unwrap();
    }

    let store = SqliteStore::open(&path).unwrap();
    let version: i64 = store
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    assert!(store.has_column("workflow_run_steps", "owner").unwrap());

    let owner = MemberId::new("builder");
    let run = store
        .assign_workflow_step(WorkflowRunId(1), 1, Some(&owner))
        .unwrap();
    assert_eq!(run.steps[0].owner, Some(owner));

    drop(store);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn migrates_legacy_prototype_schema_then_persists() {
    let dir = std::env::temp_dir().join(format!("asterline-migrate-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("legacy.sqlite3");
    let _ = std::fs::remove_file(&path);

    // Simulate a pre-v1 prototype database: an incompatible `messages`
    // schema (no `kind` column), a legacy `approvals` (`action_kind`), and
    // dead prototype tables. `user_version` stays at the default 0.
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            r#"
                CREATE TABLE messages (
                    id         INTEGER PRIMARY KEY AUTOINCREMENT,
                    session_id TEXT,
                    route_from TEXT NOT NULL,
                    route_to   TEXT NOT NULL,
                    body       TEXT NOT NULL
                );
                CREATE TABLE approvals (id INTEGER PRIMARY KEY, action_kind TEXT NOT NULL);
                CREATE TABLE agents (id INTEGER PRIMARY KEY);
                CREATE TABLE sessions (id INTEGER PRIMARY KEY);
                CREATE TABLE inter_agent_messages (id INTEGER PRIMARY KEY);
                CREATE TABLE terminal_events (id INTEGER PRIMARY KEY);
                INSERT INTO messages (route_from, route_to, body) VALUES ('a', 'b', 'old');
                "#,
        )
        .unwrap();
    }

    // Opening through the store migrates the legacy schema in place. The
    // unconvertible prototype rows are dropped (replay is empty, not an
    // error) and the version is stamped.
    let store = SqliteStore::open(&path).unwrap();
    assert!(store.replay_chat().unwrap().is_empty());
    let version: i64 = store
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);

    // New writes round-trip through the rebuilt event-source schema — the
    // exact path that was silently failing before the migration.
    let turn = store.create_turn().unwrap();
    let builder = MemberId::new("builder");
    store
        .record_user(turn, std::slice::from_ref(&builder), "hi")
        .unwrap();
    store
        .record_agent(turn, &builder, "Builder", BackendKind::Codex, "on it")
        .unwrap();
    let items = store.replay_chat().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(
        items[0],
        ChatItem::User {
            body: "hi".to_string()
        }
    );

    // A second open is a clean no-op (already at SCHEMA_VERSION).
    drop(store);
    let reopened = SqliteStore::open(&path).unwrap();
    assert_eq!(reopened.replay_chat().unwrap().len(), 2);

    drop(reopened);
    std::fs::remove_dir_all(&dir).ok();
}
