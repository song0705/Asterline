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
use serde::{Deserialize, Serialize};

use crate::domain::event::{
    AgentSessionId, ApprovalDecision, ApprovalId, ChatItem, ConversationSummary, LogEntry,
    LogLevel, MessageId, ModeRunStatus, RunEventSummary, RunId, RunStatus, RunStepStatus,
    RunStepSummary, RunSummary, RunVerification, TurnId,
};
use crate::domain::mode::{CollabMode, ModeStatusSummary};
use crate::domain::team::{BackendKind, MemberId, TeamConfig};

pub type Result<T> = result::Result<T, rusqlite::Error>;

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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StoredConversationSession {
    pub member: MemberId,
    pub backend: BackendKind,
    pub session_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConversationSnapshot {
    pub team: TeamConfig,
    pub sessions: Vec<StoredConversationSession>,
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
        self.create_schema()
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

            CREATE TABLE IF NOT EXISTS runtime_state (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS conversation_snapshots (
                conversation_id INTEGER PRIMARY KEY,
                team_json       TEXT NOT NULL,
                sessions_json   TEXT NOT NULL,
                updated_at      TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY(conversation_id) REFERENCES conversations(id)
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

            CREATE TABLE IF NOT EXISTS runs (
                id                   INTEGER PRIMARY KEY AUTOINCREMENT,
                conversation_id      INTEGER NOT NULL DEFAULT 0,
                goal                 TEXT NOT NULL,
                status               TEXT NOT NULL,
                coordinator          TEXT,
                verification_command TEXT,
                verification_ok      INTEGER,
                verification_summary TEXT,
                created_at           TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                updated_at           TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                attempt              INTEGER NOT NULL DEFAULT 1,
                mode                 TEXT,
                mode_state           TEXT
            );

            CREATE TABLE IF NOT EXISTS run_events (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id     INTEGER NOT NULL,
                attempt    INTEGER NOT NULL,
                kind       TEXT NOT NULL,
                title      TEXT NOT NULL,
                detail     TEXT,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );

            CREATE INDEX IF NOT EXISTS run_events_run_idx
                ON run_events (run_id, id);

            CREATE TABLE IF NOT EXISTS run_steps (
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

            CREATE INDEX IF NOT EXISTS run_steps_run_idx
                ON run_steps (run_id, position);
            "#,
        )?;
        let columns = {
            let mut stmt = self.conn.prepare("PRAGMA table_info(runs)")?;
            stmt.query_map([], |row| row.get::<_, String>(1))?
                .collect::<Result<Vec<_>>>()?
        };
        if !columns.iter().any(|column| column == "conversation_id") {
            self.conn.execute(
                "ALTER TABLE runs
                 ADD COLUMN conversation_id INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS runs_conversation_idx
             ON runs (conversation_id, id)",
            [],
        )?;
        Ok(())
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

    /// Persist a reviewer verdict as a chat row (`kind = "verdict"`).
    pub fn record_verdict(
        &self,
        turn: TurnId,
        member: &MemberId,
        approve: bool,
        summary: &str,
    ) -> Result<MessageId> {
        self.insert_message(MessageRow {
            turn: Some(turn),
            kind: "verdict",
            member: Some(member),
            text: Some(summary),
            ok: Some(approve),
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
        self.replay_chat_for(self.conversation.get())
    }

    /// Rebuild one saved conversation's transcript in insertion order.
    pub fn replay_chat_for(&self, conversation: i64) -> Result<Vec<ChatItem>> {
        let mut stmt = self.conn.prepare(
            "SELECT kind, member_id, display_name, backend, text, name, summary, ok, targets
             FROM messages WHERE conversation_id = ?1 ORDER BY id ASC",
        )?;
        let rows = stmt.query_map(params![conversation], map_chat_item)?;
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
        let selected: Option<i64> = self
            .conn
            .query_row(
                "SELECT c.id
                 FROM runtime_state s
                 JOIN conversations c ON c.id = CAST(s.value AS INTEGER)
                 WHERE s.key = 'active_conversation'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(id) = selected {
            return Ok(id);
        }
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
        let _ = self.conn.execute(
            "INSERT INTO runtime_state (key, value)
             VALUES ('active_conversation', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![id.to_string()],
        );
    }

    pub fn active_conversation(&self) -> i64 {
        self.conversation.get()
    }

    /// Save the roster and native backend sessions belonging to the active chat.
    pub fn save_conversation_snapshot(
        &self,
        team: &TeamConfig,
        sessions: &[StoredConversationSession],
    ) -> Result<()> {
        let team_json = serde_json::to_string(team)
            .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?;
        let sessions_json = serde_json::to_string(sessions)
            .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?;
        self.conn.execute(
            "INSERT INTO conversation_snapshots
                (conversation_id, team_json, sessions_json, updated_at)
             VALUES (?1, ?2, ?3, CURRENT_TIMESTAMP)
             ON CONFLICT(conversation_id) DO UPDATE SET
                team_json = excluded.team_json,
                sessions_json = excluded.sessions_json,
                updated_at = CURRENT_TIMESTAMP",
            params![self.conversation.get(), team_json, sessions_json],
        )?;
        Ok(())
    }

    /// Load the roster and native backend sessions saved with one chat.
    pub fn conversation_snapshot(&self, conversation: i64) -> Result<Option<ConversationSnapshot>> {
        let row: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT team_json, sessions_json
                 FROM conversation_snapshots WHERE conversation_id = ?1",
                params![conversation],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        row.map(|(team_json, sessions_json)| {
            let team = serde_json::from_str(&team_json).map_err(|err| {
                rusqlite::Error::FromSqlConversionFailure(0, Type::Text, Box::new(err))
            })?;
            let sessions = serde_json::from_str(&sessions_json).map_err(|err| {
                rusqlite::Error::FromSqlConversionFailure(1, Type::Text, Box::new(err))
            })?;
            Ok(ConversationSnapshot { team, sessions })
        })
        .transpose()
    }

    /// Saved chats other than the active one, newest first.
    ///
    /// Only conversations with a roster/session snapshot are selectable:
    /// restoring a transcript without its members would violate `/resume`.
    pub fn resumable_conversations(&self) -> Result<Vec<ConversationSummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT c.id,
                    c.created_at,
                    COALESCE(
                        (SELECT text FROM messages
                         WHERE conversation_id = c.id AND kind = 'user'
                         ORDER BY id ASC LIMIT 1),
                        ''
                    ),
                    (SELECT COUNT(*) FROM messages WHERE conversation_id = c.id),
                    s.team_json
             FROM conversations c
             JOIN conversation_snapshots s ON s.conversation_id = c.id
             WHERE c.id != ?1
             ORDER BY c.id DESC",
        )?;
        let rows = stmt.query_map(params![self.conversation.get()], |row| {
            let preview: String = row.get(2)?;
            let team_json: String = row.get(4)?;
            let member_count = serde_json::from_str::<TeamConfig>(&team_json)
                .map_err(|err| {
                    rusqlite::Error::FromSqlConversionFailure(4, Type::Text, Box::new(err))
                })?
                .members
                .len();
            Ok(ConversationSummary {
                id: row.get(0)?,
                created_at: row.get(1)?,
                preview: if preview.trim().is_empty() {
                    "(empty chat)".to_string()
                } else {
                    preview
                },
                message_count: row.get::<_, i64>(3)? as usize,
                member_count,
            })
        })?;
        rows.collect()
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

    pub fn clear_sessions(&self) -> Result<()> {
        self.conn.execute("DELETE FROM agent_sessions", [])?;
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

    // --- runs ---------------------------------------------------

    pub fn create_run(&self, goal: &str, coordinator: Option<&MemberId>) -> Result<RunSummary> {
        self.conn.execute(
            "INSERT INTO runs (conversation_id, goal, status, coordinator)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                self.active_conversation(),
                goal,
                RunStatus::Running.as_str(),
                coordinator.map(MemberId::as_str)
            ],
        )?;
        let id = RunId(self.conn.last_insert_rowid() as u64);
        self.record_run_event(id, "started", "Started run", Some(goal))?;
        self.run(id)
    }

    /// Start a collaboration-mode run with initial `mode` + `mode_state`.
    pub fn create_mode_run(
        &self,
        goal: &str,
        coordinator: Option<&MemberId>,
        mode: CollabMode,
        mode_state: &str,
    ) -> Result<RunSummary> {
        self.conn.execute(
            "INSERT INTO runs
                (conversation_id, goal, status, coordinator, mode, mode_state)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                self.active_conversation(),
                goal,
                RunStatus::Running.as_str(),
                coordinator.map(MemberId::as_str),
                mode.as_str(),
                mode_state
            ],
        )?;
        let id = RunId(self.conn.last_insert_rowid() as u64);
        self.record_run_event(id, "started", "Started run", Some(goal))?;
        self.run(id)
    }

    /// Test helper: write a raw `runs.mode` string that may not parse as [`CollabMode`].
    #[cfg(test)]
    pub(crate) fn insert_run_with_raw_mode(
        &self,
        goal: &str,
        coordinator: Option<&MemberId>,
        mode: &str,
        mode_state: Option<&str>,
        status: RunStatus,
    ) -> Result<RunSummary> {
        self.conn.execute(
            "INSERT INTO runs
                (conversation_id, goal, status, coordinator, mode, mode_state)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                self.active_conversation(),
                goal,
                status.as_str(),
                coordinator.map(MemberId::as_str),
                mode,
                mode_state
            ],
        )?;
        let id = RunId(self.conn.last_insert_rowid() as u64);
        self.run(id)
    }

    /// Persist an updated `mode_state` blob without recording a timeline event.
    pub fn update_run_mode_state(&self, id: RunId, mode_state: &str) -> Result<RunSummary> {
        self.conn.execute(
            "UPDATE runs SET mode_state = ?1, updated_at = CURRENT_TIMESTAMP WHERE id = ?2",
            params![mode_state, id.0 as i64],
        )?;
        self.run(id)
    }

    /// Raw `mode_state` JSON for a run, if any.
    pub fn run_mode_state(&self, id: RunId) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT mode_state FROM runs WHERE id = ?1",
                params![id.0 as i64],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map(|opt| opt.flatten())
    }

    /// Record a verdict on a mode run's event timeline.
    pub fn record_run_verdict_event(&self, id: RunId, approve: bool, summary: &str) -> Result<()> {
        let title = if approve {
            "Review approved"
        } else {
            "Changes requested"
        };
        let detail = if summary.is_empty() {
            None
        } else {
            Some(summary)
        };
        self.record_run_event(id, "verdict", title, detail)
    }

    /// Record one accepted private brainstorm ballot in the run timeline.
    pub fn record_brainstorm_vote_event(
        &self,
        id: RunId,
        voter: &MemberId,
        ranked: &[String],
    ) -> Result<()> {
        self.record_run_event(
            id,
            "vote",
            "Brainstorm ballot",
            Some(&format!("@{voter}: {}", ranked.join(" > "))),
        )
    }

    /// Ids of in-flight mode runs (`running`/`verifying` with a non-null mode).
    pub fn running_mode_runs(&self) -> Result<Vec<RunId>> {
        let mut stmt = self.conn.prepare(
            "SELECT id FROM runs
              WHERE conversation_id = ?1
                AND status IN ('running', 'verifying')
                AND mode IS NOT NULL
              ORDER BY id ASC",
        )?;
        let rows = stmt.query_map(params![self.active_conversation()], |row| {
            Ok(RunId(row.get::<_, i64>(0)? as u64))
        })?;
        rows.collect()
    }

    pub fn update_run_status(&self, id: RunId, status: RunStatus) -> Result<RunSummary> {
        self.conn.execute(
            "UPDATE runs SET status = ?1, updated_at = CURRENT_TIMESTAMP WHERE id = ?2",
            params![status.as_str(), id.0 as i64],
        )?;
        let (kind, title) = run_status_event(status);
        self.record_run_event(id, kind, title, None)?;
        self.run(id)
    }

    pub fn set_run_verification(
        &self,
        id: RunId,
        command: &str,
        ok: bool,
        summary: &str,
    ) -> Result<RunSummary> {
        self.conn.execute(
            "UPDATE runs
             SET status = ?1,
                 verification_command = ?2,
                 verification_ok = ?3,
                 verification_summary = ?4,
                 updated_at = CURRENT_TIMESTAMP
             WHERE id = ?5",
            params![
                if ok {
                    RunStatus::Done.as_str()
                } else {
                    RunStatus::Failed.as_str()
                },
                command,
                ok as i64,
                summary,
                id.0 as i64
            ],
        )?;
        self.record_run_event(
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
        self.run(id)
    }

    pub fn continue_run(&self, id: RunId, note: Option<&str>) -> Result<RunSummary> {
        self.conn.execute(
            "UPDATE runs
             SET status = ?1,
                 attempt = attempt + 1,
                 verification_command = NULL,
                 verification_ok = NULL,
                 verification_summary = NULL,
                 updated_at = CURRENT_TIMESTAMP
             WHERE id = ?2",
            params![RunStatus::Running.as_str(), id.0 as i64],
        )?;
        self.record_run_event(id, "continued", "Continued run", note)?;
        self.run(id)
    }

    pub fn add_run_note(&self, id: RunId, note: &str) -> Result<RunSummary> {
        self.conn.execute(
            "UPDATE runs SET updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
            params![id.0 as i64],
        )?;
        self.record_run_event(id, "note", "User note", Some(note))?;
        self.run(id)
    }

    pub fn block_run(&self, id: RunId, reason: &str) -> Result<RunSummary> {
        self.conn.execute(
            "UPDATE runs
             SET status = ?1,
                 updated_at = CURRENT_TIMESTAMP
             WHERE id = ?2",
            params![RunStatus::Blocked.as_str(), id.0 as i64],
        )?;
        self.record_run_event(id, "blocked", "Run blocked", Some(reason))?;
        self.run(id)
    }

    pub fn add_run_step(
        &self,
        id: RunId,
        owner: Option<&MemberId>,
        title: &str,
    ) -> Result<RunSummary> {
        let inserted = self.conn.execute(
            "INSERT INTO run_steps (run_id, position, status, owner, title)
             SELECT id,
                    (
                        SELECT COALESCE(MAX(position), 0) + 1
                          FROM run_steps
                         WHERE run_id = ?1
                    ),
                    ?2,
                    ?3,
                    ?4
              FROM runs
             WHERE id = ?1",
            params![
                id.0 as i64,
                RunStepStatus::Todo.as_str(),
                owner.map(MemberId::as_str),
                title
            ],
        )?;
        if inserted == 0 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        self.conn.execute(
            "UPDATE runs SET updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
            params![id.0 as i64],
        )?;
        let detail = match owner {
            Some(owner) => format!("@{owner}: {title}"),
            None => title.to_string(),
        };
        self.record_run_event(id, "step_added", "Step added", Some(&detail))?;
        self.run(id)
    }

    pub fn update_run_step(
        &self,
        id: RunId,
        number: u32,
        status: RunStepStatus,
        note: Option<&str>,
    ) -> Result<RunSummary> {
        let title: String = self.conn.query_row(
            "SELECT title FROM run_steps WHERE run_id = ?1 AND position = ?2",
            params![id.0 as i64, number as i64],
            |row| row.get(0),
        )?;
        let note_value = note.filter(|note| !note.trim().is_empty());
        self.conn.execute(
            "UPDATE run_steps
             SET status = ?1,
                 note = ?2,
                 updated_at = CURRENT_TIMESTAMP
             WHERE run_id = ?3 AND position = ?4",
            params![status.as_str(), note_value, id.0 as i64, number as i64],
        )?;
        self.conn.execute(
            "UPDATE runs SET updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
            params![id.0 as i64],
        )?;
        let detail = match note_value {
            Some(note) => format!("#{number} {}: {title}\n{note}", status.as_str()),
            None => format!("#{number} {}: {title}", status.as_str()),
        };
        self.record_run_event(id, "step_updated", "Step updated", Some(&detail))?;
        self.run(id)
    }

    pub fn rename_run_step(&self, id: RunId, number: u32, title: &str) -> Result<RunSummary> {
        let old_title: String = self.conn.query_row(
            "SELECT title FROM run_steps WHERE run_id = ?1 AND position = ?2",
            params![id.0 as i64, number as i64],
            |row| row.get(0),
        )?;
        self.conn.execute(
            "UPDATE run_steps
             SET title = ?1,
                 updated_at = CURRENT_TIMESTAMP
             WHERE run_id = ?2 AND position = ?3",
            params![title, id.0 as i64, number as i64],
        )?;
        self.conn.execute(
            "UPDATE runs SET updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
            params![id.0 as i64],
        )?;
        self.record_run_event(
            id,
            "step_renamed",
            "Step renamed",
            Some(&format!("#{number}: {old_title}\n{title}")),
        )?;
        self.run(id)
    }

    pub fn remove_run_step(&self, id: RunId, number: u32) -> Result<RunSummary> {
        let title: String = self.conn.query_row(
            "SELECT title FROM run_steps WHERE run_id = ?1 AND position = ?2",
            params![id.0 as i64, number as i64],
            |row| row.get(0),
        )?;
        self.conn.execute(
            "DELETE FROM run_steps WHERE run_id = ?1 AND position = ?2",
            params![id.0 as i64, number as i64],
        )?;
        self.conn.execute(
            "UPDATE run_steps
             SET position = position - 1,
                 updated_at = CURRENT_TIMESTAMP
             WHERE run_id = ?1 AND position > ?2",
            params![id.0 as i64, number as i64],
        )?;
        self.conn.execute(
            "UPDATE runs SET updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
            params![id.0 as i64],
        )?;
        self.record_run_event(
            id,
            "step_removed",
            "Step removed",
            Some(&format!("#{number}: {title}")),
        )?;
        self.run(id)
    }

    pub fn assign_run_step(
        &self,
        id: RunId,
        number: u32,
        owner: Option<&MemberId>,
    ) -> Result<RunSummary> {
        let title: String = self.conn.query_row(
            "SELECT title FROM run_steps WHERE run_id = ?1 AND position = ?2",
            params![id.0 as i64, number as i64],
            |row| row.get(0),
        )?;
        self.conn.execute(
            "UPDATE run_steps
             SET owner = ?1,
                 updated_at = CURRENT_TIMESTAMP
             WHERE run_id = ?2 AND position = ?3",
            params![owner.map(MemberId::as_str), id.0 as i64, number as i64],
        )?;
        self.conn.execute(
            "UPDATE runs SET updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
            params![id.0 as i64],
        )?;
        let detail = match owner {
            Some(owner) => format!("#{number} @{owner}: {title}"),
            None => format!("#{number} unassigned: {title}"),
        };
        self.record_run_event(id, "step_assigned", "Step assigned", Some(&detail))?;
        self.run(id)
    }

    pub fn latest_run(&self) -> Result<Option<RunSummary>> {
        let run = self
            .conn
            .query_row(
                "SELECT id, goal, status, coordinator, verification_command, verification_ok, verification_summary, created_at, updated_at, attempt, mode, mode_state
                 FROM runs
                 WHERE conversation_id = ?1
                 ORDER BY id DESC LIMIT 1",
                params![self.active_conversation()],
                map_run,
            )
            .optional()?;
        run.map(|run| self.with_run_events(run)).transpose()
    }

    pub fn recent_runs(&self, limit: usize) -> Result<Vec<RunSummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, goal, status, coordinator, verification_command, verification_ok, verification_summary, created_at, updated_at, attempt, mode, mode_state
             FROM runs
             WHERE conversation_id = ?1
             ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![self.active_conversation(), limit as i64], map_run)?;
        let mut runs = rows.collect::<Result<Vec<_>>>()?;
        runs.reverse();
        runs.into_iter()
            .map(|run| self.with_run_events(run))
            .collect()
    }

    pub fn run(&self, id: RunId) -> Result<RunSummary> {
        let run = self.conn.query_row(
            "SELECT id, goal, status, coordinator, verification_command, verification_ok, verification_summary, created_at, updated_at, attempt, mode, mode_state
             FROM runs WHERE id = ?1",
            params![id.0 as i64],
            map_run,
        )?;
        self.with_run_events(run)
    }

    fn record_run_event(
        &self,
        id: RunId,
        kind: &str,
        title: &str,
        detail: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO run_events (run_id, attempt, kind, title, detail)
             SELECT id, attempt, ?2, ?3, ?4 FROM runs WHERE id = ?1",
            params![id.0 as i64, kind, title, detail],
        )?;
        Ok(())
    }

    fn with_run_events(&self, mut run: RunSummary) -> Result<RunSummary> {
        run.events = self.run_events(run.id, 8)?;
        run.steps = self.run_steps(run.id, 12)?;
        Ok(run)
    }

    fn run_events(&self, id: RunId, limit: usize) -> Result<Vec<RunEventSummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT kind, title, detail, created_at, attempt
             FROM (
                 SELECT id, kind, title, detail, created_at, attempt
                   FROM run_events
                  WHERE run_id = ?1
                  ORDER BY id DESC
                  LIMIT ?2
             )
             ORDER BY id ASC",
        )?;
        let rows = stmt.query_map(params![id.0 as i64, limit as i64], |row| {
            Ok(RunEventSummary {
                kind: row.get(0)?,
                title: row.get(1)?,
                detail: row.get(2)?,
                created_at: row.get(3)?,
                attempt: row.get::<_, i64>(4)? as u32,
            })
        })?;
        rows.collect()
    }

    fn run_steps(&self, id: RunId, limit: usize) -> Result<Vec<RunStepSummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT position, status, owner, title, note, updated_at
               FROM run_steps
              WHERE run_id = ?1
              ORDER BY position ASC
              LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![id.0 as i64, limit as i64], |row| {
            Ok(RunStepSummary {
                number: row.get::<_, i64>(0)? as u32,
                status: RunStepStatus::parse(&row.get::<_, String>(1)?),
                owner: row.get::<_, Option<String>>(2)?.map(MemberId::new),
                title: row.get(3)?,
                note: row.get(4)?,
                updated_at: row.get(5)?,
            })
        })?;
        rows.collect()
    }

    /// All checklist steps for a run (no LIMIT), ordered by position.
    pub fn run_steps_all(&self, id: RunId) -> Result<Vec<RunStepSummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT position, status, owner, title, note, updated_at
               FROM run_steps
              WHERE run_id = ?1
              ORDER BY position ASC",
        )?;
        let rows = stmt.query_map(params![id.0 as i64], |row| {
            Ok(RunStepSummary {
                number: row.get::<_, i64>(0)? as u32,
                status: RunStepStatus::parse(&row.get::<_, String>(1)?),
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
        "verdict" => ChatItem::Verdict {
            member: MemberId::new(member_id.unwrap_or_default()),
            approve: ok.map(|v| v != 0).unwrap_or(false),
            summary: text.unwrap_or_default(),
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

fn map_run(row: &Row<'_>) -> rusqlite::Result<RunSummary> {
    let command: Option<String> = row.get(4)?;
    let ok: Option<i64> = row.get(5)?;
    let summary: Option<String> = row.get(6)?;
    let mode_col: Option<String> = row.get(10)?;
    let mode_state_col: Option<String> = row.get(11)?;
    let (mode, legacy_mode) = match mode_col {
        Some(raw) => match CollabMode::parse(&raw) {
            Some(mode) => {
                let state = mode_state_col
                    .as_deref()
                    .and_then(|json| serde_json::from_str::<ModeStatusSummary>(json).ok())
                    .unwrap_or_default();
                (Some(ModeRunStatus { mode, state }), None)
            }
            None => (None, Some(raw)),
        },
        None => (None, None),
    };
    Ok(RunSummary {
        id: RunId(row.get::<_, i64>(0)? as u64),
        goal: row.get(1)?,
        status: RunStatus::parse(&row.get::<_, String>(2)?),
        coordinator: row.get::<_, Option<String>>(3)?.map(MemberId::new),
        verification: match (command, ok, summary) {
            (Some(command), Some(ok), Some(summary)) => Some(RunVerification {
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
        mode,
        legacy_mode,
    })
}

fn run_status_event(status: RunStatus) -> (&'static str, &'static str) {
    match status {
        RunStatus::Planned => ("planned", "Run planned"),
        RunStatus::Running => ("running", "Run running"),
        RunStatus::Verifying => ("verifying", "Started verification"),
        RunStatus::Done => ("done", "Work finished"),
        RunStatus::Failed => ("failed", "Run failed"),
        RunStatus::Blocked => ("blocked", "Run blocked"),
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
