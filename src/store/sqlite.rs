//! SQLite event-source store.
//!
//! The chat transcript is persisted as an ordered `messages` log (user, agent,
//! tool, route, notice, error rows) which the TUI replays on startup. Raw
//! backend JSON goes to `stream_events`; diagnostics to `logs`; resumable
//! backend session ids to `agent_sessions`. The runtime always writes here
//! before emitting the corresponding UI event, so history survives a crash.

use std::path::Path;
use std::{io, result};

use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::domain::event::{
    AgentSessionId, ApprovalDecision, ApprovalId, ChatItem, LogEntry, LogLevel, MessageId, TurnId,
};
use crate::domain::team::{BackendKind, MemberId, TeamConfig};

pub type Result<T> = result::Result<T, rusqlite::Error>;

/// Current event-source schema version. Bump this and add a migration arm in
/// [`SqliteStore::migrate`] whenever the schema changes.
const SCHEMA_VERSION: i64 = 1;

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
}

impl SqliteStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let store = Self {
            conn: Connection::open(path)?,
        };
        store.initialize()?;
        Ok(store)
    }

    pub fn in_memory() -> Result<Self> {
        let store = Self {
            conn: Connection::open_in_memory()?,
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
        self.create_schema()
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

            CREATE TABLE IF NOT EXISTS messages (
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
        ok: Option<bool>,
    ) -> Result<MessageId> {
        self.insert_message(MessageRow {
            turn: Some(turn),
            kind: "tool",
            member: Some(member),
            name: Some(name),
            summary: Some(summary),
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
                (turn_id, kind, member_id, display_name, backend, text, name, summary, ok, targets)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
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

    /// Rebuild the chat transcript in insertion order for TUI replay.
    pub fn replay_chat(&self) -> Result<Vec<ChatItem>> {
        let mut stmt = self.conn.prepare(
            "SELECT kind, member_id, display_name, backend, text, name, summary, ok, targets
             FROM messages ORDER BY id ASC",
        )?;
        let rows = stmt.query_map([], map_chat_item)?;
        let mut items = Vec::new();
        for item in rows {
            if let Some(item) = item? {
                items.push(item);
            }
        }
        Ok(items)
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

fn read_backend(value: Option<&str>) -> rusqlite::Result<BackendKind> {
    let value = value.unwrap_or("");
    BackendKind::try_from(value).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            Type::Text,
            Box::new(io::Error::new(io::ErrorKind::InvalidData, err)),
        )
    })
}

#[cfg(test)]
mod tests {
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
            .record_tool(turn, &builder, "shell", "cargo test", Some(true))
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
            ChatItem::Tool { ok: Some(true), summary, .. } if summary == "cargo test"
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
}
