//! SQLite event-source store.
//!
//! The chat transcript is persisted as an ordered `messages` log (user, agent,
//! tool, route, notice, error rows) which the TUI replays on startup. Raw
//! backend JSON goes to `stream_events`; diagnostics to `logs`; resumable
//! backend session ids to `agent_sessions`. The runtime always writes here
//! before emitting the corresponding UI event, so history survives a crash.

use std::cell::Cell;
use std::path::Path;
use std::{io, result};

use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::domain::event::{
    AgentSessionId, ApprovalDecision, ApprovalId, ChatItem, LogEntry, LogLevel, MessageId, TurnId,
    WorkflowRunEventSummary, WorkflowRunId, WorkflowRunStatus, WorkflowRunSummary,
    WorkflowStepStatus, WorkflowStepSummary, WorkflowVerification,
};
use crate::domain::team::{BackendKind, MemberId, TeamConfig};

pub type Result<T> = result::Result<T, rusqlite::Error>;

/// Current event-source schema version. Bump this and add a migration arm in
/// [`SqliteStore::migrate`] whenever the schema changes.
const SCHEMA_VERSION: i64 = 8;

/// A pending approval row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredApproval {
    pub id: ApprovalId,
    pub turn: Option<TurnId>,
    pub member: Option<MemberId>,
    pub action: String,
    pub body: String,
    pub decision: String,
}

#[derive(Debug)]
pub struct SqliteStore {
    conn: Connection,
    /// The conversation new rows are written to / replayed from. `/new` bumps it
    /// to a fresh conversation so the transcript starts clean.
    conversation: Cell<i64>,
}

impl SqliteStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let store = Self {
            conn: Connection::open(path)?,
            conversation: Cell::new(0),
        };
        store.initialize()?;
        Ok(store)
    }

    pub fn in_memory() -> Result<Self> {
        let store = Self {
            conn: Connection::open_in_memory()?,
            conversation: Cell::new(0),
        };
        store.initialize()?;
        Ok(store)
    }

    fn initialize(&self) -> Result<()> {
        self.conn.execute_batch("PRAGMA journal_mode = WAL;")?;
        let version: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version < SCHEMA_VERSION {
            self.migrate(version)?;
            self.conn
                .execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))?;
        }
        Ok(())
    }

    /// Bring a database at schema `from` up to [`SCHEMA_VERSION`].
    ///
    /// The pre-v1 prototype wrote an incompatible `messages`/`approvals` schema
    /// plus a handful of tables the event-source design replaced. Its rows are
    /// not convertible to the new model, so when that schema is detected the
    /// legacy tables are dropped and rebuilt. A brand-new database has no such
    /// tables, so the same path simply creates the current schema.
    fn migrate(&self, from: i64) -> Result<()> {
        if from == 0 && self.has_legacy_schema()? {
            self.conn.execute_batch(
                r#"
                DROP TABLE IF EXISTS messages;
                DROP TABLE IF EXISTS approvals;
                DROP TABLE IF EXISTS agents;
                DROP TABLE IF EXISTS sessions;
                DROP TABLE IF EXISTS inter_agent_messages;
                DROP TABLE IF EXISTS terminal_events;
                "#,
            )?;
        }
        self.create_schema()?;
        // v1 -> v2: introduce conversations so `/new` can start a clean chat.
        // Backfill existing messages into a single conversation.
        if from == 1 {
            if !self.has_column("messages", "conversation_id")? {
                self.conn
                    .execute_batch("ALTER TABLE messages ADD COLUMN conversation_id INTEGER;")?;
            }
            self.conn
                .execute("INSERT INTO conversations DEFAULT VALUES", [])?;
            let id = self.conn.last_insert_rowid();
            self.conn.execute(
                "UPDATE messages SET conversation_id = ?1 WHERE conversation_id IS NULL",
                params![id],
            )?;
        }
        // v2 -> v3: Gemini CLI was replaced by Agy. Keep old transcripts,
        // roster snapshots, and session rows readable under the new backend.
        self.conn.execute_batch(
            r#"
            UPDATE messages SET backend = 'agy' WHERE backend = 'gemini';
            UPDATE team_members SET backend = 'agy' WHERE backend = 'gemini';
            UPDATE agent_sessions SET backend = 'agy' WHERE backend = 'gemini';
            "#,
        )?;
        if !self.has_column("workflow_runs", "attempt")? {
            self.conn.execute_batch(
                "ALTER TABLE workflow_runs ADD COLUMN attempt INTEGER NOT NULL DEFAULT 1;",
            )?;
        }
        if from < 6 {
            self.conn.execute_batch(
                r#"
                INSERT INTO workflow_run_events (run_id, attempt, kind, title, detail, created_at)
                SELECT id,
                       attempt,
                       'imported',
                       'Imported existing run',
                       status,
                       updated_at
                  FROM workflow_runs
                 WHERE NOT EXISTS (
                       SELECT 1 FROM workflow_run_events
                        WHERE workflow_run_events.run_id = workflow_runs.id
                 );
                "#,
            )?;
        }
        if from < 7 {
            self.conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS workflow_run_steps (
                    id         INTEGER PRIMARY KEY AUTOINCREMENT,
                    run_id     INTEGER NOT NULL,
                    position   INTEGER NOT NULL,
                    status     TEXT NOT NULL,
                    owner      TEXT,
                    title      TEXT NOT NULL,
                    note       TEXT,
                    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                );

                CREATE INDEX IF NOT EXISTS workflow_run_steps_run_idx
                    ON workflow_run_steps (run_id, position);
                "#,
            )?;
        }
        if from < 8 && !self.has_column("workflow_run_steps", "owner")? {
            self.conn
                .execute_batch("ALTER TABLE workflow_run_steps ADD COLUMN owner TEXT;")?;
        }
        Ok(())
    }

    fn has_column(&self, table: &str, column: &str) -> Result<bool> {
        let mut stmt = self.conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let columns: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>>>()?;
        Ok(columns.iter().any(|name| name == column))
    }

    /// True when the database holds the pre-v1 prototype schema, detected by a
    /// `messages` table that predates the event-source `kind` column.
    fn has_legacy_schema(&self) -> Result<bool> {
        let messages_exists = self
            .conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'messages'",
                [],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !messages_exists {
            return Ok(false);
        }
        let mut stmt = self.conn.prepare("PRAGMA table_info(messages)")?;
        let columns: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>>>()?;
        Ok(!columns.iter().any(|name| name == "kind"))
    }

    fn create_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS teams (
                id         INTEGER PRIMARY KEY,
                name       TEXT NOT NULL,
                workspace  TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );

            CREATE TABLE IF NOT EXISTS team_members (
                id           INTEGER PRIMARY KEY,
                team_id      INTEGER NOT NULL,
                member_id    TEXT NOT NULL,
                display_name TEXT NOT NULL,
                backend      TEXT NOT NULL,
                role         TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS agent_sessions (
                member_id  TEXT PRIMARY KEY,
                backend    TEXT NOT NULL,
                session_id TEXT NOT NULL,
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );

            CREATE TABLE IF NOT EXISTS turns (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );

            CREATE TABLE IF NOT EXISTS conversations (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );

            CREATE TABLE IF NOT EXISTS messages (
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

            CREATE TABLE IF NOT EXISTS stream_events (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                member_id  TEXT NOT NULL,
                payload    TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );

            CREATE TABLE IF NOT EXISTS approvals (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                turn_id    INTEGER,
                member_id  TEXT,
                action     TEXT NOT NULL,
                body       TEXT NOT NULL,
                decision   TEXT NOT NULL DEFAULT 'pending',
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );

            CREATE TABLE IF NOT EXISTS logs (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                level      TEXT NOT NULL,
                source     TEXT NOT NULL,
                message    TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );

            CREATE TABLE IF NOT EXISTS workflow_runs (
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

            CREATE TABLE IF NOT EXISTS workflow_run_events (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id     INTEGER NOT NULL,
                attempt    INTEGER NOT NULL,
                kind       TEXT NOT NULL,
                title      TEXT NOT NULL,
                detail     TEXT,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );

            CREATE INDEX IF NOT EXISTS workflow_run_events_run_idx
                ON workflow_run_events (run_id, id);

            CREATE TABLE IF NOT EXISTS workflow_run_steps (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id     INTEGER NOT NULL,
                position   INTEGER NOT NULL,
                status     TEXT NOT NULL,
                owner      TEXT,
                title      TEXT NOT NULL,
                note       TEXT,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );

            CREATE INDEX IF NOT EXISTS workflow_run_steps_run_idx
                ON workflow_run_steps (run_id, position);
            "#,
        )
    }

    // --- roster snapshot -------------------------------------------------

    /// Persist a snapshot of the team roster (for inspection; the in-memory
    /// config remains the source of truth).
    pub fn upsert_team(&self, config: &TeamConfig) -> Result<()> {
        self.conn.execute("DELETE FROM team_members", [])?;
        self.conn.execute("DELETE FROM teams", [])?;
        self.conn.execute(
            "INSERT INTO teams (id, name, workspace) VALUES (1, ?1, ?2)",
            params![config.name, config.workspace.display().to_string()],
        )?;
        for member in &config.members {
            self.conn.execute(
                "INSERT INTO team_members (team_id, member_id, display_name, backend, role)
                 VALUES (1, ?1, ?2, ?3, ?4)",
                params![
                    member.id.as_str(),
                    member.display_name,
                    member.backend.as_str(),
                    member.role
                ],
            )?;
        }
        Ok(())
    }

    // --- turns -----------------------------------------------------------

    pub fn create_turn(&self) -> Result<TurnId> {
        self.conn.execute("INSERT INTO turns DEFAULT VALUES", [])?;
        Ok(TurnId(self.conn.last_insert_rowid() as u64))
    }

    // --- chat messages ---------------------------------------------------

    pub fn record_user(&self, turn: TurnId, targets: &[MemberId], body: &str) -> Result<MessageId> {
        self.insert_message(MessageRow {
            turn: Some(turn),
            kind: "user",
            text: Some(body),
            targets: Some(&member_csv(targets)),
            ..MessageRow::default()
        })
    }

    pub fn record_agent(
        &self,
        turn: TurnId,
        member: &MemberId,
        display_name: &str,
        backend: BackendKind,
        text: &str,
    ) -> Result<MessageId> {
        self.insert_message(MessageRow {
            turn: Some(turn),
            kind: "agent",
            member: Some(member),
            display_name: Some(display_name),
            backend: Some(backend.as_str()),
            text: Some(text),
            ..MessageRow::default()
        })
    }

    pub fn record_tool(
        &self,
        turn: TurnId,
        member: &MemberId,
        name: &str,
        summary: &str,
        detail: &str,
        ok: Option<bool>,
    ) -> Result<MessageId> {
        self.insert_message(MessageRow {
            turn: Some(turn),
            kind: "tool",
            member: Some(member),
            name: Some(name),
            summary: Some(summary),
            text: Some(detail),
            ok,
            ..MessageRow::default()
        })
    }

    pub fn record_route(
        &self,
        turn: TurnId,
        from: &MemberId,
        to: &[String],
        body: &str,
    ) -> Result<MessageId> {
        self.insert_message(MessageRow {
            turn: Some(turn),
            kind: "route",
            member: Some(from),
            text: Some(body),
            targets: Some(&to.join(",")),
            ..MessageRow::default()
        })
    }

    pub fn record_diff(
        &self,
        turn: TurnId,
        member: &MemberId,
        files: &[(String, String)],
    ) -> Result<MessageId> {
        let encoded = files
            .iter()
            .map(|(path, kind)| format!("{kind}\t{path}"))
            .collect::<Vec<_>>()
            .join("\n");
        self.insert_message(MessageRow {
            turn: Some(turn),
            kind: "diff",
            member: Some(member),
            text: Some(&encoded),
            ..MessageRow::default()
        })
    }

    pub fn record_notice(&self, turn: Option<TurnId>, text: &str) -> Result<MessageId> {
        self.insert_message(MessageRow {
            turn,
            kind: "notice",
            text: Some(text),
            ..MessageRow::default()
        })
    }

    pub fn record_error(
        &self,
        turn: Option<TurnId>,
        member: Option<&MemberId>,
        message: &str,
    ) -> Result<MessageId> {
        self.insert_message(MessageRow {
            turn,
            kind: "error",
            member,
            text: Some(message),
            ..MessageRow::default()
        })
    }

    fn insert_message(&self, row: MessageRow<'_>) -> Result<MessageId> {
        self.conn.execute(
            "INSERT INTO messages
                (conversation_id, turn_id, kind, member_id, display_name, backend, text, name, summary, ok, targets)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                self.conversation.get(),
                row.turn.map(|t| t.0 as i64),
                row.kind,
                row.member.map(MemberId::as_str),
                row.display_name,
                row.backend,
                row.text,
                row.name,
                row.summary,
                row.ok.map(|v| v as i64),
                row.targets,
            ],
        )?;
        Ok(MessageId(self.conn.last_insert_rowid() as u64))
    }

    /// Rebuild the current conversation's transcript in insertion order.
    pub fn replay_chat(&self) -> Result<Vec<ChatItem>> {
        let mut stmt = self.conn.prepare(
            "SELECT kind, member_id, display_name, backend, text, name, summary, ok, targets
             FROM messages WHERE conversation_id = ?1 ORDER BY id ASC",
        )?;
        let rows = stmt.query_map(params![self.conversation.get()], map_chat_item)?;
        let mut items = Vec::new();
        for item in rows {
            if let Some(item) = item? {
                items.push(item);
            }
        }
        Ok(items)
    }

    // --- conversations ---------------------------------------------------

    /// The active conversation (latest existing, creating one if none yet).
    pub fn current_conversation(&self) -> Result<i64> {
        let latest: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM conversations ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        match latest {
            Some(id) => Ok(id),
            None => self.create_conversation(),
        }
    }

    /// Start a new conversation and return its id.
    pub fn create_conversation(&self) -> Result<i64> {
        self.conn
            .execute("INSERT INTO conversations DEFAULT VALUES", [])?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Set the conversation new rows are written to / replayed from.
    pub fn set_conversation(&self, id: i64) {
        self.conversation.set(id);
    }

    pub fn message_count(&self) -> Result<i64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
    }

    // --- raw stream events & logs ---------------------------------------

    pub fn record_stream_event(&self, member: &MemberId, payload: &str) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO stream_events (member_id, payload) VALUES (?1, ?2)",
            params![member.as_str(), payload],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn stream_event_count(&self) -> Result<i64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM stream_events", [], |row| row.get(0))
    }

    pub fn record_log(&self, entry: &LogEntry) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO logs (level, source, message) VALUES (?1, ?2, ?3)",
            params![entry.level.as_str(), entry.source, entry.message],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Most recent `limit` log entries, oldest-first.
    pub fn recent_logs(&self, limit: usize) -> Result<Vec<LogEntry>> {
        let mut stmt = self
            .conn
            .prepare("SELECT level, source, message FROM logs ORDER BY id DESC LIMIT ?1")?;
        let rows = stmt.query_map(params![limit as i64], |row| {
            Ok(LogEntry {
                level: parse_log_level(&row.get::<_, String>(0)?),
                source: row.get(1)?,
                message: row.get(2)?,
            })
        })?;
        let mut entries = rows.collect::<Result<Vec<_>>>()?;
        entries.reverse();
        Ok(entries)
    }

    // --- sessions --------------------------------------------------------

    pub fn upsert_session(
        &self,
        member: &MemberId,
        backend: BackendKind,
        session: &AgentSessionId,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO agent_sessions (member_id, backend, session_id, updated_at)
             VALUES (?1, ?2, ?3, CURRENT_TIMESTAMP)
             ON CONFLICT(member_id) DO UPDATE SET
                backend = excluded.backend,
                session_id = excluded.session_id,
                updated_at = CURRENT_TIMESTAMP",
            params![member.as_str(), backend.as_str(), session.as_str()],
        )?;
        Ok(())
    }

    pub fn session_for(&self, member: &MemberId) -> Result<Option<AgentSessionId>> {
        self.conn
            .query_row(
                "SELECT session_id FROM agent_sessions WHERE member_id = ?1",
                params![member.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map(|opt| opt.map(AgentSessionId))
    }

    /// Forget a member's resumable session so the next run starts fresh.
    pub fn delete_session(&self, member: &MemberId) -> Result<()> {
        self.conn.execute(
            "DELETE FROM agent_sessions WHERE member_id = ?1",
            params![member.as_str()],
        )?;
        Ok(())
    }

    // --- approvals -------------------------------------------------------

    pub fn insert_approval(
        &self,
        turn: Option<TurnId>,
        member: Option<&MemberId>,
        action: &str,
        body: &str,
    ) -> Result<ApprovalId> {
        self.conn.execute(
            "INSERT INTO approvals (turn_id, member_id, action, body, decision)
             VALUES (?1, ?2, ?3, ?4, 'pending')",
            params![
                turn.map(|t| t.0 as i64),
                member.map(MemberId::as_str),
                action,
                body
            ],
        )?;
        Ok(ApprovalId(self.conn.last_insert_rowid() as u64))
    }

    pub fn pending_approvals(&self) -> Result<Vec<StoredApproval>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, turn_id, member_id, action, body, decision
             FROM approvals WHERE decision = 'pending' ORDER BY id ASC",
        )?;
        let rows = stmt.query_map([], map_approval)?;
        rows.collect()
    }

    pub fn resolve_approval(&self, id: ApprovalId, decision: ApprovalDecision) -> Result<bool> {
        let updated = self.conn.execute(
            "UPDATE approvals SET decision = ?1 WHERE id = ?2 AND decision = 'pending'",
            params![decision.as_str(), id.0 as i64],
        )?;
        Ok(updated == 1)
    }

    // --- workflow runs ---------------------------------------------------

    pub fn create_workflow_run(
        &self,
        goal: &str,
        coordinator: Option<&MemberId>,
    ) -> Result<WorkflowRunSummary> {
        self.conn.execute(
            "INSERT INTO workflow_runs (goal, status, coordinator)
             VALUES (?1, ?2, ?3)",
            params![
                goal,
                WorkflowRunStatus::Running.as_str(),
                coordinator.map(MemberId::as_str)
            ],
        )?;
        let id = WorkflowRunId(self.conn.last_insert_rowid() as u64);
        self.record_workflow_event(id, "started", "Started workflow", Some(goal))?;
        self.workflow_run(id)
    }

    pub fn update_workflow_status(
        &self,
        id: WorkflowRunId,
        status: WorkflowRunStatus,
    ) -> Result<WorkflowRunSummary> {
        self.conn.execute(
            "UPDATE workflow_runs SET status = ?1, updated_at = CURRENT_TIMESTAMP WHERE id = ?2",
            params![status.as_str(), id.0 as i64],
        )?;
        let (kind, title) = workflow_status_event(status);
        self.record_workflow_event(id, kind, title, None)?;
        self.workflow_run(id)
    }

    pub fn set_workflow_verification(
        &self,
        id: WorkflowRunId,
        command: &str,
        ok: bool,
        summary: &str,
    ) -> Result<WorkflowRunSummary> {
        self.conn.execute(
            "UPDATE workflow_runs
             SET status = ?1,
                 verification_command = ?2,
                 verification_ok = ?3,
                 verification_summary = ?4,
                 updated_at = CURRENT_TIMESTAMP
             WHERE id = ?5",
            params![
                if ok {
                    WorkflowRunStatus::Done.as_str()
                } else {
                    WorkflowRunStatus::Failed.as_str()
                },
                command,
                ok as i64,
                summary,
                id.0 as i64
            ],
        )?;
        self.record_workflow_event(
            id,
            if ok {
                "verification_passed"
            } else {
                "verification_failed"
            },
            if ok {
                "Verification passed"
            } else {
                "Verification failed"
            },
            Some(&format!("{command}\n{summary}")),
        )?;
        self.workflow_run(id)
    }

    pub fn continue_workflow_run(
        &self,
        id: WorkflowRunId,
        note: Option<&str>,
    ) -> Result<WorkflowRunSummary> {
        self.conn.execute(
            "UPDATE workflow_runs
             SET status = ?1,
                 attempt = attempt + 1,
                 verification_command = NULL,
                 verification_ok = NULL,
                 verification_summary = NULL,
                 updated_at = CURRENT_TIMESTAMP
             WHERE id = ?2",
            params![WorkflowRunStatus::Running.as_str(), id.0 as i64],
        )?;
        self.record_workflow_event(id, "continued", "Continued workflow", note)?;
        self.workflow_run(id)
    }

    pub fn add_workflow_note(&self, id: WorkflowRunId, note: &str) -> Result<WorkflowRunSummary> {
        self.conn.execute(
            "UPDATE workflow_runs SET updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
            params![id.0 as i64],
        )?;
        self.record_workflow_event(id, "note", "User note", Some(note))?;
        self.workflow_run(id)
    }

    pub fn block_workflow_run(
        &self,
        id: WorkflowRunId,
        reason: &str,
    ) -> Result<WorkflowRunSummary> {
        self.conn.execute(
            "UPDATE workflow_runs
             SET status = ?1,
                 updated_at = CURRENT_TIMESTAMP
             WHERE id = ?2",
            params![WorkflowRunStatus::Blocked.as_str(), id.0 as i64],
        )?;
        self.record_workflow_event(id, "blocked", "Workflow blocked", Some(reason))?;
        self.workflow_run(id)
    }

    pub fn add_workflow_step(
        &self,
        id: WorkflowRunId,
        owner: Option<&MemberId>,
        title: &str,
    ) -> Result<WorkflowRunSummary> {
        let inserted = self.conn.execute(
            "INSERT INTO workflow_run_steps (run_id, position, status, owner, title)
             SELECT id,
                    (
                        SELECT COALESCE(MAX(position), 0) + 1
                          FROM workflow_run_steps
                         WHERE run_id = ?1
                    ),
                    ?2,
                    ?3,
                    ?4
              FROM workflow_runs
             WHERE id = ?1",
            params![
                id.0 as i64,
                WorkflowStepStatus::Todo.as_str(),
                owner.map(MemberId::as_str),
                title
            ],
        )?;
        if inserted == 0 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        self.conn.execute(
            "UPDATE workflow_runs SET updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
            params![id.0 as i64],
        )?;
        let detail = match owner {
            Some(owner) => format!("@{owner}: {title}"),
            None => title.to_string(),
        };
        self.record_workflow_event(id, "step_added", "Step added", Some(&detail))?;
        self.workflow_run(id)
    }

    pub fn update_workflow_step(
        &self,
        id: WorkflowRunId,
        number: u32,
        status: WorkflowStepStatus,
        note: Option<&str>,
    ) -> Result<WorkflowRunSummary> {
        let title: String = self.conn.query_row(
            "SELECT title FROM workflow_run_steps WHERE run_id = ?1 AND position = ?2",
            params![id.0 as i64, number as i64],
            |row| row.get(0),
        )?;
        let note_value = note.filter(|note| !note.trim().is_empty());
        self.conn.execute(
            "UPDATE workflow_run_steps
             SET status = ?1,
                 note = ?2,
                 updated_at = CURRENT_TIMESTAMP
             WHERE run_id = ?3 AND position = ?4",
            params![status.as_str(), note_value, id.0 as i64, number as i64],
        )?;
        self.conn.execute(
            "UPDATE workflow_runs SET updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
            params![id.0 as i64],
        )?;
        let detail = match note_value {
            Some(note) => format!("#{number} {}: {title}\n{note}", status.as_str()),
            None => format!("#{number} {}: {title}", status.as_str()),
        };
        self.record_workflow_event(id, "step_updated", "Step updated", Some(&detail))?;
        self.workflow_run(id)
    }

    pub fn rename_workflow_step(
        &self,
        id: WorkflowRunId,
        number: u32,
        title: &str,
    ) -> Result<WorkflowRunSummary> {
        let old_title: String = self.conn.query_row(
            "SELECT title FROM workflow_run_steps WHERE run_id = ?1 AND position = ?2",
            params![id.0 as i64, number as i64],
            |row| row.get(0),
        )?;
        self.conn.execute(
            "UPDATE workflow_run_steps
             SET title = ?1,
                 updated_at = CURRENT_TIMESTAMP
             WHERE run_id = ?2 AND position = ?3",
            params![title, id.0 as i64, number as i64],
        )?;
        self.conn.execute(
            "UPDATE workflow_runs SET updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
            params![id.0 as i64],
        )?;
        self.record_workflow_event(
            id,
            "step_renamed",
            "Step renamed",
            Some(&format!("#{number}: {old_title}\n{title}")),
        )?;
        self.workflow_run(id)
    }

    pub fn remove_workflow_step(
        &self,
        id: WorkflowRunId,
        number: u32,
    ) -> Result<WorkflowRunSummary> {
        let title: String = self.conn.query_row(
            "SELECT title FROM workflow_run_steps WHERE run_id = ?1 AND position = ?2",
            params![id.0 as i64, number as i64],
            |row| row.get(0),
        )?;
        self.conn.execute(
            "DELETE FROM workflow_run_steps WHERE run_id = ?1 AND position = ?2",
            params![id.0 as i64, number as i64],
        )?;
        self.conn.execute(
            "UPDATE workflow_run_steps
             SET position = position - 1,
                 updated_at = CURRENT_TIMESTAMP
             WHERE run_id = ?1 AND position > ?2",
            params![id.0 as i64, number as i64],
        )?;
        self.conn.execute(
            "UPDATE workflow_runs SET updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
            params![id.0 as i64],
        )?;
        self.record_workflow_event(
            id,
            "step_removed",
            "Step removed",
            Some(&format!("#{number}: {title}")),
        )?;
        self.workflow_run(id)
    }

    pub fn assign_workflow_step(
        &self,
        id: WorkflowRunId,
        number: u32,
        owner: Option<&MemberId>,
    ) -> Result<WorkflowRunSummary> {
        let title: String = self.conn.query_row(
            "SELECT title FROM workflow_run_steps WHERE run_id = ?1 AND position = ?2",
            params![id.0 as i64, number as i64],
            |row| row.get(0),
        )?;
        self.conn.execute(
            "UPDATE workflow_run_steps
             SET owner = ?1,
                 updated_at = CURRENT_TIMESTAMP
             WHERE run_id = ?2 AND position = ?3",
            params![owner.map(MemberId::as_str), id.0 as i64, number as i64],
        )?;
        self.conn.execute(
            "UPDATE workflow_runs SET updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
            params![id.0 as i64],
        )?;
        let detail = match owner {
            Some(owner) => format!("#{number} @{owner}: {title}"),
            None => format!("#{number} unassigned: {title}"),
        };
        self.record_workflow_event(id, "step_assigned", "Step assigned", Some(&detail))?;
        self.workflow_run(id)
    }

    pub fn latest_workflow_run(&self) -> Result<Option<WorkflowRunSummary>> {
        let run = self
            .conn
            .query_row(
                "SELECT id, goal, status, coordinator, verification_command, verification_ok, verification_summary, created_at, updated_at, attempt
                 FROM workflow_runs ORDER BY id DESC LIMIT 1",
                [],
                map_workflow_run,
            )
            .optional()?;
        run.map(|run| self.with_workflow_events(run)).transpose()
    }

    pub fn recent_workflow_runs(&self, limit: usize) -> Result<Vec<WorkflowRunSummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, goal, status, coordinator, verification_command, verification_ok, verification_summary, created_at, updated_at, attempt
             FROM workflow_runs ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], map_workflow_run)?;
        let mut runs = rows.collect::<Result<Vec<_>>>()?;
        runs.reverse();
        runs.into_iter()
            .map(|run| self.with_workflow_events(run))
            .collect()
    }

    pub fn workflow_run(&self, id: WorkflowRunId) -> Result<WorkflowRunSummary> {
        let run = self.conn.query_row(
            "SELECT id, goal, status, coordinator, verification_command, verification_ok, verification_summary, created_at, updated_at, attempt
             FROM workflow_runs WHERE id = ?1",
            params![id.0 as i64],
            map_workflow_run,
        )?;
        self.with_workflow_events(run)
    }

    fn record_workflow_event(
        &self,
        id: WorkflowRunId,
        kind: &str,
        title: &str,
        detail: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO workflow_run_events (run_id, attempt, kind, title, detail)
             SELECT id, attempt, ?2, ?3, ?4 FROM workflow_runs WHERE id = ?1",
            params![id.0 as i64, kind, title, detail],
        )?;
        Ok(())
    }

    fn with_workflow_events(&self, mut run: WorkflowRunSummary) -> Result<WorkflowRunSummary> {
        run.events = self.workflow_run_events(run.id, 8)?;
        run.steps = self.workflow_run_steps(run.id, 12)?;
        Ok(run)
    }

    fn workflow_run_events(
        &self,
        id: WorkflowRunId,
        limit: usize,
    ) -> Result<Vec<WorkflowRunEventSummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT kind, title, detail, created_at, attempt
             FROM (
                 SELECT id, kind, title, detail, created_at, attempt
                   FROM workflow_run_events
                  WHERE run_id = ?1
                  ORDER BY id DESC
                  LIMIT ?2
             )
             ORDER BY id ASC",
        )?;
        let rows = stmt.query_map(params![id.0 as i64, limit as i64], |row| {
            Ok(WorkflowRunEventSummary {
                kind: row.get(0)?,
                title: row.get(1)?,
                detail: row.get(2)?,
                created_at: row.get(3)?,
                attempt: row.get::<_, i64>(4)? as u32,
            })
        })?;
        rows.collect()
    }

    fn workflow_run_steps(
        &self,
        id: WorkflowRunId,
        limit: usize,
    ) -> Result<Vec<WorkflowStepSummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT position, status, owner, title, note, updated_at
               FROM workflow_run_steps
              WHERE run_id = ?1
              ORDER BY position ASC
              LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![id.0 as i64, limit as i64], |row| {
            Ok(WorkflowStepSummary {
                number: row.get::<_, i64>(0)? as u32,
                status: WorkflowStepStatus::parse(&row.get::<_, String>(1)?),
                owner: row.get::<_, Option<String>>(2)?.map(MemberId::new),
                title: row.get(3)?,
                note: row.get(4)?,
                updated_at: row.get(5)?,
            })
        })?;
        rows.collect()
    }
}

/// Builder for a `messages` row; unused fields stay `None`.
#[derive(Default)]
struct MessageRow<'a> {
    turn: Option<TurnId>,
    kind: &'a str,
    member: Option<&'a MemberId>,
    display_name: Option<&'a str>,
    backend: Option<&'a str>,
    text: Option<&'a str>,
    name: Option<&'a str>,
    summary: Option<&'a str>,
    ok: Option<bool>,
    targets: Option<&'a str>,
}

fn member_csv(ids: &[MemberId]) -> String {
    ids.iter()
        .map(MemberId::as_str)
        .collect::<Vec<_>>()
        .join(",")
}

fn split_targets(value: Option<String>) -> Vec<String> {
    value
        .filter(|s| !s.is_empty())
        .map(|s| s.split(',').map(str::to_string).collect())
        .unwrap_or_default()
}

fn parse_log_level(value: &str) -> LogLevel {
    match value {
        "debug" => LogLevel::Debug,
        "warn" => LogLevel::Warn,
        "error" => LogLevel::Error,
        _ => LogLevel::Info,
    }
}

fn map_chat_item(row: &Row<'_>) -> rusqlite::Result<Option<ChatItem>> {
    let kind: String = row.get(0)?;
    let member_id: Option<String> = row.get(1)?;
    let display_name: Option<String> = row.get(2)?;
    let backend: Option<String> = row.get(3)?;
    let text: Option<String> = row.get(4)?;
    let name: Option<String> = row.get(5)?;
    let summary: Option<String> = row.get(6)?;
    let ok: Option<i64> = row.get(7)?;
    let targets: Option<String> = row.get(8)?;

    let item = match kind.as_str() {
        "user" => ChatItem::User {
            body: text.unwrap_or_default(),
        },
        "agent" => ChatItem::Agent {
            member: MemberId::new(member_id.unwrap_or_default()),
            display_name: display_name.unwrap_or_default(),
            backend: read_backend(backend.as_deref())?,
            text: text.unwrap_or_default(),
        },
        "tool" => ChatItem::Tool {
            member: MemberId::new(member_id.unwrap_or_default()),
            name: name.unwrap_or_default(),
            summary: summary.unwrap_or_default(),
            detail: text.unwrap_or_default(),
            ok: ok.map(|v| v != 0),
        },
        "route" => ChatItem::Route {
            from: MemberId::new(member_id.unwrap_or_default()),
            to: split_targets(targets),
            body: text.unwrap_or_default(),
        },
        "diff" => ChatItem::Diff {
            member: MemberId::new(member_id.unwrap_or_default()),
            files: text
                .unwrap_or_default()
                .lines()
                .filter_map(|line| {
                    let mut parts = line.splitn(2, '\t');
                    let kind = parts.next()?.to_string();
                    let path = parts.next()?.to_string();
                    Some((path, kind))
                })
                .collect(),
        },
        "notice" => ChatItem::Notice {
            text: text.unwrap_or_default(),
        },
        "error" => ChatItem::Error {
            member: member_id.map(MemberId::new),
            message: text.unwrap_or_default(),
        },
        _ => return Ok(None),
    };
    Ok(Some(item))
}

fn map_approval(row: &Row<'_>) -> rusqlite::Result<StoredApproval> {
    Ok(StoredApproval {
        id: ApprovalId(row.get::<_, i64>(0)? as u64),
        turn: row.get::<_, Option<i64>>(1)?.map(|v| TurnId(v as u64)),
        member: row.get::<_, Option<String>>(2)?.map(MemberId::new),
        action: row.get(3)?,
        body: row.get(4)?,
        decision: row.get(5)?,
    })
}

fn map_workflow_run(row: &Row<'_>) -> rusqlite::Result<WorkflowRunSummary> {
    let command: Option<String> = row.get(4)?;
    let ok: Option<i64> = row.get(5)?;
    let summary: Option<String> = row.get(6)?;
    Ok(WorkflowRunSummary {
        id: WorkflowRunId(row.get::<_, i64>(0)? as u64),
        goal: row.get(1)?,
        status: WorkflowRunStatus::parse(&row.get::<_, String>(2)?),
        coordinator: row.get::<_, Option<String>>(3)?.map(MemberId::new),
        verification: match (command, ok, summary) {
            (Some(command), Some(ok), Some(summary)) => Some(WorkflowVerification {
                command,
                ok: ok != 0,
                summary,
            }),
            _ => None,
        },
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
        attempt: row.get::<_, i64>(9)? as u32,
        events: Vec::new(),
        steps: Vec::new(),
    })
}

fn workflow_status_event(status: WorkflowRunStatus) -> (&'static str, &'static str) {
    match status {
        WorkflowRunStatus::Planned => ("planned", "Workflow planned"),
        WorkflowRunStatus::Running => ("running", "Workflow running"),
        WorkflowRunStatus::Verifying => ("verifying", "Started verification"),
        WorkflowRunStatus::Done => ("done", "Work finished"),
        WorkflowRunStatus::Failed => ("failed", "Workflow failed"),
        WorkflowRunStatus::Blocked => ("blocked", "Workflow blocked"),
    }
}

fn read_backend(value: Option<&str>) -> rusqlite::Result<BackendKind> {
    let value = value.unwrap_or("");
    if value == "gemini" {
        return Ok(BackendKind::Agy);
    }
    BackendKind::try_from(value).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            Type::Text,
            Box::new(io::Error::new(io::ErrorKind::InvalidData, err)),
        )
    })
}

#[cfg(test)]
#[path = "sqlite_tests.rs"]
mod tests;
