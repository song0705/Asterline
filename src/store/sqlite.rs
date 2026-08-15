//! SQLite event-source store.
//!
//! The chat transcript is persisted as an ordered `messages` log (user, agent,
//! tool, route, notice, error rows) which the TUI replays on startup. Raw
//! backend JSON goes to `stream_events`; diagnostics to `logs`; resumable
//! backend session ids to `agent_sessions`. The runtime always writes here
//! before emitting the corresponding UI event, so history survives a crash.

use std::cell::Cell;
use std::path::Path;
use std::time::Duration;
use std::{io, result};

use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use crate::domain::event::{
    AgentSessionId, ApprovalDecision, ApprovalId, ChatItem, ConversationSummary, LogEntry,
    LogLevel, MessageId, ModeRunStatus, RunEventSummary, RunId, RunStatus, RunStepStatus,
    RunStepSummary, RunSummary, RunVerification, TurnId,
};
use crate::domain::mode::{CollabMode, ModeStatusSummary, TerminalMode};
use crate::domain::team::{BackendKind, MemberId, TeamConfig};

pub type Result<T> = result::Result<T, rusqlite::Error>;

const REPLAY_MAX_ITEMS: usize = 10_000;
const REPLAY_MAX_BYTES: usize = 16 * 1024 * 1024;
const REPLAY_ITEM_OVERHEAD: usize = 64;
const REPLAY_TRUNCATION_NOTICE: &str =
    "Earlier history was omitted to keep chat replay within memory limits.";
const MAX_PERSISTED_LOGS: usize = 4_000;

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
    pub mode: TerminalMode,
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
        // Another Asterline process (or an external SQLite reader) may hold a
        // short write lock. Wait for it instead of failing the runtime on the
        // first SQLITE_BUSY response.
        self.conn.busy_timeout(Duration::from_secs(5))?;
        self.conn
            .execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")?;
        self.create_schema()
    }

    /// Run one compound mutation atomically. `unchecked_transaction` accepts
    /// `&self`; SqliteStore is owned by the single runtime thread, so nested
    /// transactions cannot occur through the public runtime path.
    fn transactional<T>(&self, op: impl FnOnce() -> Result<T>) -> Result<T> {
        let transaction = self.conn.unchecked_transaction()?;
        let value = op()?;
        transaction.commit()?;
        Ok(value)
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
                mode            TEXT NOT NULL DEFAULT 'normal',
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
                conversation_id INTEGER NOT NULL DEFAULT 0,
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
        let migrate_runs = !self.has_column("runs", "conversation_id")?;
        let migrate_approvals = !self.has_column("approvals", "conversation_id")?;
        // Repair conversation_id = 0 every time, not only while adding the
        // column. Older builds could have completed the column migration but
        // left those legacy rows permanently outside every conversation.
        self.transactional(|| {
            if migrate_runs {
                self.conn.execute(
                    "ALTER TABLE runs
                     ADD COLUMN conversation_id INTEGER NOT NULL DEFAULT 0",
                    [],
                )?;
            }
            if migrate_approvals {
                self.conn.execute(
                    "ALTER TABLE approvals
                     ADD COLUMN conversation_id INTEGER NOT NULL DEFAULT 0",
                    [],
                )?;
            }
            self.conn.execute(
                "INSERT INTO conversations (created_at)
                 SELECT CURRENT_TIMESTAMP
                 WHERE NOT EXISTS (SELECT 1 FROM conversations)
                   AND (EXISTS (SELECT 1 FROM runs WHERE conversation_id = 0)
                        OR EXISTS (SELECT 1 FROM approvals WHERE conversation_id = 0))",
                [],
            )?;
            let active_conversation = "COALESCE(
                    (SELECT c.id
                       FROM runtime_state s
                       JOIN conversations c ON c.id = CAST(s.value AS INTEGER)
                      WHERE s.key = 'active_conversation'),
                    (SELECT id FROM conversations ORDER BY id DESC LIMIT 1),
                    0
                )";
            self.conn.execute(
                &format!(
                    "UPDATE runs SET conversation_id = {active_conversation}
                     WHERE conversation_id = 0"
                ),
                [],
            )?;
            self.conn.execute(
                &format!(
                    "UPDATE approvals
                     SET conversation_id = {active_conversation},
                         decision = CASE
                             WHEN decision = 'pending' THEN 'rejected'
                             ELSE decision
                         END
                     WHERE conversation_id = 0"
                ),
                [],
            )?;
            Ok(())
        })?;
        if !self.has_column("conversation_snapshots", "mode")? {
            self.conn.execute(
                "ALTER TABLE conversation_snapshots
                 ADD COLUMN mode TEXT NOT NULL DEFAULT 'normal'",
                [],
            )?;
        }
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS runs_conversation_idx
             ON runs (conversation_id, id)",
            [],
        )?;
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS approvals_conversation_idx
             ON approvals (conversation_id, id)",
            [],
        )?;
        Ok(())
    }

    fn has_column(&self, table: &str, column: &str) -> Result<bool> {
        let mut stmt = self.conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let columns = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>>>()?;
        Ok(columns.iter().any(|name| name == column))
    }

    // --- roster snapshot -------------------------------------------------

    /// Persist a snapshot of the team roster (for inspection; the in-memory
    /// config remains the source of truth).
    pub fn upsert_team(&self, config: &TeamConfig) -> Result<()> {
        self.transactional(|| self.replace_team_rows(config))
    }

    fn replace_team_rows(&self, config: &TeamConfig) -> Result<()> {
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

    /// Persist the exact source text for structured agent controls. These rows
    /// are intentionally omitted from chat replay; the visible agent message
    /// and the resulting route/run/verdict rows remain the rendered history.
    pub fn record_agent_control_source(
        &self,
        turn: TurnId,
        member: &MemberId,
        text: &str,
    ) -> Result<MessageId> {
        self.insert_message(MessageRow {
            turn: Some(turn),
            kind: "agent_control",
            member: Some(member),
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
        ok: bool,
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
            ok: Some(ok),
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
             FROM messages
             WHERE conversation_id = ?1
               AND kind IN ('user', 'agent', 'tool', 'route', 'diff', 'notice', 'error', 'verdict')
             ORDER BY id DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            params![conversation, (REPLAY_MAX_ITEMS + 1) as i64],
            map_chat_item,
        )?;
        let mut items = Vec::with_capacity(REPLAY_MAX_ITEMS);
        let mut retained_bytes = 0_usize;
        let mut truncated = false;
        for item in rows {
            if let Some(item) = item? {
                if items.len() == REPLAY_MAX_ITEMS {
                    truncated = true;
                    break;
                }
                let item_bytes = chat_item_replay_bytes(&item);
                if retained_bytes.saturating_add(item_bytes) > REPLAY_MAX_BYTES {
                    truncated = true;
                    break;
                }
                retained_bytes = retained_bytes.saturating_add(item_bytes);
                items.push(item);
            }
        }
        if truncated {
            let notice = ChatItem::Notice {
                text: REPLAY_TRUNCATION_NOTICE.to_string(),
            };
            let notice_bytes = chat_item_replay_bytes(&notice);
            while !items.is_empty()
                && (items.len() == REPLAY_MAX_ITEMS
                    || retained_bytes.saturating_add(notice_bytes) > REPLAY_MAX_BYTES)
            {
                if let Some(removed) = items.pop() {
                    retained_bytes =
                        retained_bytes.saturating_sub(chat_item_replay_bytes(&removed));
                }
            }
            items.reverse();
            items.insert(0, notice);
        } else {
            items.reverse();
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
            self.conversation.set(id);
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
            Some(id) => {
                self.set_conversation(id)?;
                Ok(id)
            }
            None => self.create_and_set_conversation(),
        }
    }

    /// Start a new conversation and return its id.
    pub fn create_conversation(&self) -> Result<i64> {
        self.conn
            .execute("INSERT INTO conversations DEFAULT VALUES", [])?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Create and select a conversation as one durable mutation.
    pub fn create_and_set_conversation(&self) -> Result<i64> {
        let id = self.transactional(|| {
            self.conn
                .execute("INSERT INTO conversations DEFAULT VALUES", [])?;
            let id = self.conn.last_insert_rowid();
            self.write_active_conversation(id)?;
            Ok(id)
        })?;
        self.conversation.set(id);
        Ok(id)
    }

    /// Start a new chat and clear all resumable backend sessions as one durable
    /// mutation. The in-memory selection changes only after the transaction
    /// commits, so callers can keep their current chat on any failure.
    pub fn create_fresh_conversation(&self, team: &TeamConfig, mode: TerminalMode) -> Result<i64> {
        let (team_json, sessions_json) = serialize_conversation_snapshot(team, &[])?;
        let id = self.transactional(|| {
            self.conn
                .execute("INSERT INTO conversations DEFAULT VALUES", [])?;
            let id = self.conn.last_insert_rowid();
            self.write_active_conversation(id)?;
            self.replace_session_rows(&[])?;
            self.write_conversation_snapshot(id, &team_json, &sessions_json, mode)?;
            Ok(id)
        })?;
        self.conversation.set(id);
        Ok(id)
    }

    /// Set the conversation new rows are written to / replayed from.
    pub fn set_conversation(&self, id: i64) -> Result<()> {
        self.write_active_conversation(id)?;
        self.conversation.set(id);
        Ok(())
    }

    fn write_active_conversation(&self, id: i64) -> Result<()> {
        let updated = self.conn.execute(
            "INSERT INTO runtime_state (key, value)
             SELECT 'active_conversation', CAST(id AS TEXT)
             FROM conversations
             WHERE id = ?1
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![id],
        )?;
        if updated == 1 {
            Ok(())
        } else {
            Err(rusqlite::Error::QueryReturnedNoRows)
        }
    }

    pub fn active_conversation(&self) -> i64 {
        self.conversation.get()
    }

    /// Restore the active conversation's roster-scoped configuration while
    /// retaining the already-resolved launch workspace (including a CLI
    /// override). Call this before constructing runners and TeamRuntime.
    pub fn restore_active_team_config(&self, launch: &TeamConfig) -> Result<TeamConfig> {
        let conversation = self.current_conversation()?;
        let Some(snapshot) = self.conversation_snapshot(conversation)? else {
            return Ok(launch.clone());
        };
        let mut restored = snapshot.team;
        restored.workspace = launch.workspace.clone();
        restored.validate().map_err(|err| {
            rusqlite::Error::ToSqlConversionFailure(Box::new(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid active conversation team: {err}"),
            )))
        })?;
        Ok(restored)
    }

    /// Save the roster and native backend sessions belonging to the active chat.
    pub fn save_conversation_snapshot(
        &self,
        team: &TeamConfig,
        sessions: &[StoredConversationSession],
        mode: TerminalMode,
    ) -> Result<()> {
        let (team_json, sessions_json) = serialize_conversation_snapshot(team, sessions)?;
        self.write_conversation_snapshot(
            self.active_conversation(),
            &team_json,
            &sessions_json,
            mode,
        )
    }

    fn write_conversation_snapshot(
        &self,
        conversation: i64,
        team_json: &str,
        sessions_json: &str,
        mode: TerminalMode,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO conversation_snapshots
                (conversation_id, team_json, sessions_json, mode, updated_at)
             VALUES (?1, ?2, ?3, ?4, CURRENT_TIMESTAMP)
             ON CONFLICT(conversation_id) DO UPDATE SET
                team_json = excluded.team_json,
                sessions_json = excluded.sessions_json,
                mode = excluded.mode,
                updated_at = CURRENT_TIMESTAMP",
            params![conversation, team_json, sessions_json, mode.as_str()],
        )?;
        Ok(())
    }

    /// Atomically persist the SQLite side of a live roster replacement.
    pub fn replace_runtime_team_state(
        &self,
        roster: &TeamConfig,
        snapshot_team: &TeamConfig,
        sessions: &[StoredConversationSession],
        mode: TerminalMode,
        approvals_to_reject: &[ApprovalId],
    ) -> Result<()> {
        let (team_json, sessions_json) = serialize_conversation_snapshot(snapshot_team, sessions)?;
        self.transactional(|| {
            self.reject_pending_approval_rows(approvals_to_reject)?;
            self.replace_team_rows(roster)?;
            self.replace_session_rows(sessions)?;
            self.write_conversation_snapshot(
                self.active_conversation(),
                &team_json,
                &sessions_json,
                mode,
            )
        })
    }

    /// Atomically activate a restored conversation and all of its live state.
    pub fn activate_runtime_team_state(
        &self,
        conversation: i64,
        roster: &TeamConfig,
        snapshot_team: &TeamConfig,
        sessions: &[StoredConversationSession],
        mode: TerminalMode,
    ) -> Result<usize> {
        let (team_json, sessions_json) = serialize_conversation_snapshot(snapshot_team, sessions)?;
        let rejected = self.transactional(|| {
            self.write_active_conversation(conversation)?;
            let rejected = self.conn.execute(
                "UPDATE approvals
                 SET decision = 'rejected'
                 WHERE conversation_id = ?1 AND decision = 'pending'",
                params![conversation],
            )?;
            self.replace_team_rows(roster)?;
            self.replace_session_rows(sessions)?;
            self.write_conversation_snapshot(conversation, &team_json, &sessions_json, mode)?;
            Ok(rejected)
        })?;
        self.conversation.set(conversation);
        Ok(rejected)
    }

    /// Load the roster and native backend sessions saved with one chat.
    pub fn conversation_snapshot(&self, conversation: i64) -> Result<Option<ConversationSnapshot>> {
        let row: Option<(String, String, String)> = self
            .conn
            .query_row(
                "SELECT team_json, sessions_json, mode
                 FROM conversation_snapshots WHERE conversation_id = ?1",
                params![conversation],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        row.map(|(team_json, sessions_json, mode)| {
            let team = serde_json::from_str(&team_json).map_err(|err| {
                rusqlite::Error::FromSqlConversionFailure(0, Type::Text, Box::new(err))
            })?;
            let sessions = serde_json::from_str(&sessions_json).map_err(|err| {
                rusqlite::Error::FromSqlConversionFailure(1, Type::Text, Box::new(err))
            })?;
            let mode = TerminalMode::parse(&mode).unwrap_or_default();
            Ok(ConversationSnapshot {
                team,
                sessions,
                mode,
            })
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
                         WHERE conversation_id = c.id
                           AND kind = 'user'
                           AND TRIM(text) != ''
                           -- Older Codex attach imports could persist this
                           -- native bootstrap block as a user message. It is
                           -- not a useful conversation title; pick the first
                           -- actual user request instead.
                           AND LOWER(LTRIM(text)) NOT LIKE '<recommended_plugins>%'
                         ORDER BY id ASC LIMIT 1),
                        ''
                    ),
                    (SELECT COUNT(*) FROM messages
                     WHERE conversation_id = c.id AND kind != 'agent_control'),
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
        let entry = entry.clone().bounded();
        self.conn.execute(
            "INSERT INTO logs (level, source, message) VALUES (?1, ?2, ?3)",
            params![entry.level.as_str(), entry.source, entry.message],
        )?;
        let id = self.conn.last_insert_rowid();
        self.conn.execute(
            "DELETE FROM logs WHERE id NOT IN (SELECT id FROM logs ORDER BY id DESC LIMIT ?1)",
            params![MAX_PERSISTED_LOGS as i64],
        )?;
        Ok(id)
    }

    /// Most recent `limit` log entries, oldest-first.
    pub fn recent_logs(&self, limit: usize) -> Result<Vec<LogEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT level, substr(source, 1, 256), substr(message, 1, 4096) \
                 FROM logs ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |row| {
            Ok(LogEntry {
                level: parse_log_level(&row.get::<_, String>(0)?),
                source: row.get(1)?,
                message: row.get(2)?,
            }
            .bounded())
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

    /// Load a resumable session only when it belongs to the member's current
    /// backend. Native session ids are backend-scoped and must not cross an
    /// adapter change that reused the same member id.
    pub fn session_for_backend(
        &self,
        member: &MemberId,
        backend: BackendKind,
    ) -> Result<Option<AgentSessionId>> {
        self.conn
            .query_row(
                "SELECT session_id FROM agent_sessions
                 WHERE member_id = ?1 AND backend = ?2",
                params![member.as_str(), backend.as_str()],
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

    fn replace_session_rows(&self, sessions: &[StoredConversationSession]) -> Result<()> {
        self.conn.execute("DELETE FROM agent_sessions", [])?;
        for session in sessions {
            self.conn.execute(
                "INSERT INTO agent_sessions (member_id, backend, session_id, updated_at)
                 VALUES (?1, ?2, ?3, CURRENT_TIMESTAMP)",
                params![
                    session.member.as_str(),
                    session.backend.as_str(),
                    session.session_id
                ],
            )?;
        }
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
            "INSERT INTO approvals (
                 conversation_id, turn_id, member_id, action, body, decision
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'pending')",
            params![
                self.active_conversation(),
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
             FROM approvals
             WHERE conversation_id = ?1 AND decision = 'pending'
             ORDER BY id ASC",
        )?;
        let rows = stmt.query_map(params![self.active_conversation()], map_approval)?;
        rows.collect()
    }

    /// Pending approvals cannot be resumed after a process restart because the
    /// in-memory dispatch context (targets and wrapped prompts) is gone. Reject
    /// them explicitly so they do not remain actionable-looking orphan rows.
    pub fn reject_pending_approvals_for_active_conversation(&self) -> Result<usize> {
        self.conn.execute(
            "UPDATE approvals
             SET decision = 'rejected'
             WHERE conversation_id = ?1 AND decision = 'pending'",
            params![self.active_conversation()],
        )
    }

    /// Atomically reject a known set of pending approvals in the active chat.
    pub fn reject_pending_approvals(&self, ids: &[ApprovalId]) -> Result<usize> {
        self.transactional(|| self.reject_pending_approval_rows(ids))
    }

    fn reject_pending_approval_rows(&self, ids: &[ApprovalId]) -> Result<usize> {
        let mut updated = 0;
        for id in ids {
            updated += self.conn.execute(
                "UPDATE approvals
                 SET decision = 'rejected'
                 WHERE id = ?1 AND conversation_id = ?2 AND decision = 'pending'",
                params![id.0 as i64, self.active_conversation()],
            )?;
        }
        if updated != ids.len() {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        Ok(updated)
    }

    pub fn resolve_approval(&self, id: ApprovalId, decision: ApprovalDecision) -> Result<bool> {
        let updated = self.conn.execute(
            "UPDATE approvals
             SET decision = ?1
             WHERE id = ?2 AND conversation_id = ?3 AND decision = 'pending'",
            params![decision.as_str(), id.0 as i64, self.active_conversation()],
        )?;
        Ok(updated == 1)
    }

    // --- runs ---------------------------------------------------

    pub fn create_run(&self, goal: &str, coordinator: Option<&MemberId>) -> Result<RunSummary> {
        let id = self.transactional(|| {
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
            Ok(id)
        })?;
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
        let id = self.transactional(|| {
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
            Ok(id)
        })?;
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
        self.write_run_mode_state(id, mode_state)?;
        self.run(id)
    }

    fn write_run_mode_state(&self, id: RunId, mode_state: &str) -> Result<()> {
        self.ensure_active_run(id)?;
        self.conn.execute(
            "UPDATE runs SET mode_state = ?1, updated_at = CURRENT_TIMESTAMP WHERE id = ?2",
            params![mode_state, id.0 as i64],
        )?;
        Ok(())
    }

    /// Raw `mode_state` JSON for a run, if any.
    pub fn run_mode_state(&self, id: RunId) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT mode_state FROM runs WHERE id = ?1 AND conversation_id = ?2",
                params![id.0 as i64, self.active_conversation()],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map(|opt| opt.flatten())
    }

    /// Record a verdict on a mode run's event timeline.
    pub fn record_run_verdict_event(&self, id: RunId, approve: bool, summary: &str) -> Result<()> {
        self.ensure_active_run(id)?;
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

    /// Atomically commit every durable representation of an accepted mode
    /// verdict. The runtime must not expose or act on the verdict before this
    /// transaction succeeds.
    pub fn commit_mode_verdict(
        &self,
        turn: TurnId,
        member: &MemberId,
        id: RunId,
        approve: bool,
        summary: &str,
        mode_state: &str,
    ) -> Result<MessageId> {
        self.transactional(|| {
            self.ensure_active_run(id)?;
            let message = self.record_verdict(turn, member, approve, summary)?;
            self.record_run_verdict_event(id, approve, summary)?;
            self.write_run_mode_state(id, mode_state)?;
            Ok(message)
        })
    }

    /// Record one accepted private brainstorm ballot in the run timeline.
    pub fn record_brainstorm_vote_event(
        &self,
        id: RunId,
        voter: &MemberId,
        ranked: &[String],
    ) -> Result<()> {
        self.ensure_active_run(id)?;
        self.record_run_event(
            id,
            "vote",
            "Brainstorm ballot",
            Some(&format!("@{voter}: {}", ranked.join(" > "))),
        )
    }

    /// Atomically record an accepted private ballot and the mode state that
    /// contains it. This prevents a timeline-only ballot or an untracked state
    /// mutation when either write fails.
    pub fn commit_brainstorm_vote(
        &self,
        id: RunId,
        voter: &MemberId,
        ranked: &[String],
        mode_state: &str,
    ) -> Result<()> {
        self.transactional(|| {
            self.ensure_active_run(id)?;
            self.record_brainstorm_vote_event(id, voter, ranked)?;
            self.write_run_mode_state(id, mode_state)
        })
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
        self.transactional(|| {
            self.ensure_active_run(id)?;
            self.conn.execute(
                "UPDATE runs SET status = ?1, updated_at = CURRENT_TIMESTAMP WHERE id = ?2",
                params![status.as_str(), id.0 as i64],
            )?;
            let (kind, title) = run_status_event(status);
            self.record_run_event(id, kind, title, None)
        })?;
        self.run(id)
    }

    pub fn set_run_verification(
        &self,
        id: RunId,
        command: &str,
        ok: bool,
        summary: &str,
    ) -> Result<RunSummary> {
        self.transactional(|| {
            self.ensure_active_run(id)?;
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
            )
        })?;
        self.run(id)
    }

    pub fn cancel_run_verification(
        &self,
        id: RunId,
        command: &str,
        summary: &str,
    ) -> Result<RunSummary> {
        self.transactional(|| {
            self.ensure_active_run(id)?;
            self.conn.execute(
                "UPDATE runs
                 SET status = ?1,
                     verification_command = ?2,
                     verification_ok = 0,
                     verification_summary = ?3,
                     updated_at = CURRENT_TIMESTAMP
                 WHERE id = ?4",
                params![RunStatus::Blocked.as_str(), command, summary, id.0 as i64],
            )?;
            self.record_run_event(
                id,
                "verification_cancelled",
                "Verification cancelled",
                Some(&format!("{command}\n{summary}")),
            )
        })?;
        self.run(id)
    }

    pub fn continue_run(&self, id: RunId, note: Option<&str>) -> Result<RunSummary> {
        self.transactional(|| {
            self.ensure_active_run(id)?;
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
            self.record_run_event(id, "continued", "Continued run", note)
        })?;
        self.run(id)
    }

    pub fn add_run_note(&self, id: RunId, note: &str) -> Result<RunSummary> {
        self.transactional(|| {
            self.ensure_active_run(id)?;
            self.conn.execute(
                "UPDATE runs SET updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
                params![id.0 as i64],
            )?;
            self.record_run_event(id, "note", "User note", Some(note))
        })?;
        self.run(id)
    }

    pub fn block_run(&self, id: RunId, reason: &str) -> Result<RunSummary> {
        self.transactional(|| {
            self.ensure_active_run(id)?;
            self.conn.execute(
                "UPDATE runs
                 SET status = ?1,
                     updated_at = CURRENT_TIMESTAMP
                 WHERE id = ?2",
                params![RunStatus::Blocked.as_str(), id.0 as i64],
            )?;
            self.record_run_event(id, "blocked", "Run blocked", Some(reason))
        })?;
        self.run(id)
    }

    pub fn add_run_step(
        &self,
        id: RunId,
        owner: Option<&MemberId>,
        title: &str,
    ) -> Result<RunSummary> {
        let detail = match owner {
            Some(owner) => format!("@{owner}: {title}"),
            None => title.to_string(),
        };
        self.transactional(|| {
            self.ensure_active_run(id)?;
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
            self.record_run_event(id, "step_added", "Step added", Some(&detail))
        })?;
        self.run(id)
    }

    pub fn update_run_step(
        &self,
        id: RunId,
        number: u32,
        status: RunStepStatus,
        note: Option<&str>,
    ) -> Result<RunSummary> {
        let note_value = note.filter(|note| !note.trim().is_empty());
        self.transactional(|| {
            self.ensure_active_run(id)?;
            let title: String = self.conn.query_row(
                "SELECT title FROM run_steps WHERE run_id = ?1 AND position = ?2",
                params![id.0 as i64, number as i64],
                |row| row.get(0),
            )?;
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
            self.record_run_event(id, "step_updated", "Step updated", Some(&detail))
        })?;
        self.run(id)
    }

    pub fn rename_run_step(&self, id: RunId, number: u32, title: &str) -> Result<RunSummary> {
        self.transactional(|| {
            self.ensure_active_run(id)?;
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
            )
        })?;
        self.run(id)
    }

    pub fn remove_run_step(&self, id: RunId, number: u32) -> Result<RunSummary> {
        self.transactional(|| {
            self.ensure_active_run(id)?;
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
            )
        })?;
        self.run(id)
    }

    pub fn assign_run_step(
        &self,
        id: RunId,
        number: u32,
        owner: Option<&MemberId>,
    ) -> Result<RunSummary> {
        self.transactional(|| {
            self.ensure_active_run(id)?;
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
            self.record_run_event(id, "step_assigned", "Step assigned", Some(&detail))
        })?;
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

    /// Load a run only when it belongs to the currently selected chat.
    pub fn active_run(&self, id: RunId) -> Result<RunSummary> {
        let run = self.conn.query_row(
            "SELECT id, goal, status, coordinator, verification_command, verification_ok, verification_summary, created_at, updated_at, attempt, mode, mode_state
             FROM runs WHERE id = ?1 AND conversation_id = ?2",
            params![id.0 as i64, self.active_conversation()],
            map_run,
        )?;
        self.with_run_events(run)
    }

    fn ensure_active_run(&self, id: RunId) -> Result<()> {
        self.conn.query_row(
            "SELECT 1 FROM runs WHERE id = ?1 AND conversation_id = ?2",
            params![id.0 as i64, self.active_conversation()],
            |_| Ok(()),
        )
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

fn serialize_conversation_snapshot(
    team: &TeamConfig,
    sessions: &[StoredConversationSession],
) -> Result<(String, String)> {
    let team_json = serde_json::to_string(team)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?;
    let sessions_json = serde_json::to_string(sessions)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?;
    Ok((team_json, sessions_json))
}

fn split_targets(value: Option<String>) -> Vec<String> {
    value
        .filter(|s| !s.is_empty())
        .map(|s| s.split(',').map(str::to_string).collect())
        .unwrap_or_default()
}

fn chat_item_replay_bytes(item: &ChatItem) -> usize {
    let payload = match item {
        ChatItem::User { body, .. } => body.len(),
        ChatItem::Agent {
            member,
            display_name,
            text,
            ..
        } => member.as_str().len() + display_name.len() + text.len(),
        ChatItem::Tool {
            member,
            name,
            summary,
            detail,
            ..
        } => member.as_str().len() + name.len() + summary.len() + detail.len(),
        ChatItem::Diff { member, files, .. } => {
            member.as_str().len()
                + files
                    .iter()
                    .map(|(path, kind)| path.len() + kind.len())
                    .sum::<usize>()
        }
        ChatItem::Route { from, to, body } => {
            from.as_str().len() + to.iter().map(String::len).sum::<usize>() + body.len()
        }
        ChatItem::Notice { text } => text.len(),
        ChatItem::Error { member, message } => {
            member.as_ref().map_or(0, |member| member.as_str().len()) + message.len()
        }
        ChatItem::Verdict {
            member, summary, ..
        } => member.as_str().len() + summary.len(),
    };
    REPLAY_ITEM_OVERHEAD.saturating_add(payload)
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
            targets: split_targets(targets)
                .into_iter()
                .map(MemberId::new)
                .collect(),
            interrupted: Vec::new(),
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
            ok: ok.map(|value| value != 0).unwrap_or(true),
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
