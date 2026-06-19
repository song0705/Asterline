use std::{io, path::Path};

use rusqlite::{Connection, OptionalExtension, Row, params, types::Type};

use crate::{router::envelope::TeamMessage, types::AgentId};

#[derive(Debug)]
pub struct SqliteStore {
    conn: Connection,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredInterAgentMessage {
    pub id: i64,
    pub thread_id: String,
    pub from: AgentId,
    pub to: AgentId,
    pub kind: String,
    pub body: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredApproval {
    pub id: i64,
    pub thread_id: String,
    pub action_kind: String,
    pub body: String,
    pub decision: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredTerminalEvent {
    pub id: i64,
    pub agent: AgentId,
    pub stream: String,
    pub body: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredMessage {
    pub id: i64,
    pub session_id: Option<String>,
    pub route_from: String,
    pub route_to: String,
    pub body: String,
}

impl SqliteStore {
    pub fn open(path: impl AsRef<Path>) -> rusqlite::Result<Self> {
        let store = Self {
            conn: Connection::open(path)?,
        };
        store.initialize()?;
        Ok(store)
    }

    pub fn in_memory() -> rusqlite::Result<Self> {
        let store = Self {
            conn: Connection::open_in_memory()?,
        };
        store.initialize()?;
        Ok(store)
    }

    pub fn initialize(&self) -> rusqlite::Result<()> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS agents (
                id TEXT PRIMARY KEY,
                status TEXT NOT NULL,
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );

            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );

            CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT,
                route_from TEXT NOT NULL,
                route_to TEXT NOT NULL,
                body TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );

            CREATE TABLE IF NOT EXISTS terminal_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                agent_id TEXT NOT NULL,
                stream TEXT NOT NULL,
                body TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );

            CREATE TABLE IF NOT EXISTS inter_agent_messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                thread_id TEXT NOT NULL,
                from_agent TEXT NOT NULL,
                to_agent TEXT NOT NULL,
                kind TEXT NOT NULL,
                body TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );

            CREATE TABLE IF NOT EXISTS approvals (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                thread_id TEXT NOT NULL,
                action_kind TEXT NOT NULL,
                body TEXT NOT NULL,
                decision TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );
            "#,
        )
    }

    pub fn insert_inter_agent_message(
        &self,
        thread_id: &str,
        from: AgentId,
        message: &TeamMessage,
    ) -> rusqlite::Result<i64> {
        self.conn.execute(
            r#"
            INSERT INTO inter_agent_messages (thread_id, from_agent, to_agent, kind, body)
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
            params![
                thread_id,
                from.as_str(),
                message.to.as_str(),
                message.kind,
                message.body
            ],
        )?;

        Ok(self.conn.last_insert_rowid())
    }

    pub fn inter_agent_message(
        &self,
        id: i64,
    ) -> rusqlite::Result<Option<StoredInterAgentMessage>> {
        self.conn
            .query_row(
                r#"
                SELECT id, thread_id, from_agent, to_agent, kind, body
                FROM inter_agent_messages
                WHERE id = ?1
                "#,
                params![id],
                map_inter_agent_message,
            )
            .optional()
    }

    pub fn inter_agent_message_count(&self) -> rusqlite::Result<i64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM inter_agent_messages", [], |row| {
                row.get(0)
            })
    }

    pub fn insert_approval(
        &self,
        thread_id: &str,
        action_kind: &str,
        body: &str,
    ) -> rusqlite::Result<i64> {
        self.conn.execute(
            r#"
            INSERT INTO approvals (thread_id, action_kind, body, decision)
            VALUES (?1, ?2, ?3, 'pending')
            "#,
            params![thread_id, action_kind, body],
        )?;

        Ok(self.conn.last_insert_rowid())
    }

    pub fn approval(&self, id: i64) -> rusqlite::Result<Option<StoredApproval>> {
        self.conn
            .query_row(
                r#"
                SELECT id, thread_id, action_kind, body, decision
                FROM approvals
                WHERE id = ?1
                "#,
                params![id],
                |row| {
                    Ok(StoredApproval {
                        id: row.get(0)?,
                        thread_id: row.get(1)?,
                        action_kind: row.get(2)?,
                        body: row.get(3)?,
                        decision: row.get(4)?,
                    })
                },
            )
            .optional()
    }

    pub fn approval_count(&self) -> rusqlite::Result<i64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM approvals", [], |row| row.get(0))
    }

    pub fn pending_approvals(&self) -> rusqlite::Result<Vec<StoredApproval>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT id, thread_id, action_kind, body, decision
            FROM approvals
            WHERE decision = 'pending'
            ORDER BY id ASC
            "#,
        )?;
        let rows = stmt.query_map([], map_approval)?;

        rows.collect()
    }

    pub fn pending_approval_count(&self) -> rusqlite::Result<i64> {
        self.conn.query_row(
            "SELECT COUNT(*) FROM approvals WHERE decision = 'pending'",
            [],
            |row| row.get(0),
        )
    }

    pub fn set_approval_decision(&self, id: i64, decision: &str) -> rusqlite::Result<bool> {
        let updated = self.conn.execute(
            r#"
            UPDATE approvals
            SET decision = ?1
            WHERE id = ?2 AND decision = 'pending'
            "#,
            params![decision, id],
        )?;

        Ok(updated == 1)
    }

    pub fn insert_terminal_event(
        &self,
        agent: AgentId,
        stream: &str,
        body: &str,
    ) -> rusqlite::Result<i64> {
        self.conn.execute(
            r#"
            INSERT INTO terminal_events (agent_id, stream, body)
            VALUES (?1, ?2, ?3)
            "#,
            params![agent.as_str(), stream, body],
        )?;

        Ok(self.conn.last_insert_rowid())
    }

    pub fn terminal_event(&self, id: i64) -> rusqlite::Result<Option<StoredTerminalEvent>> {
        self.conn
            .query_row(
                r#"
                SELECT id, agent_id, stream, body
                FROM terminal_events
                WHERE id = ?1
                "#,
                params![id],
                |row| {
                    Ok(StoredTerminalEvent {
                        id: row.get(0)?,
                        agent: read_agent(row, 1)?,
                        stream: row.get(2)?,
                        body: row.get(3)?,
                    })
                },
            )
            .optional()
    }

    pub fn terminal_event_count(&self) -> rusqlite::Result<i64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM terminal_events", [], |row| row.get(0))
    }

    pub fn terminal_events(&self) -> rusqlite::Result<Vec<StoredTerminalEvent>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT id, agent_id, stream, body
            FROM terminal_events
            ORDER BY id ASC
            "#,
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(StoredTerminalEvent {
                id: row.get(0)?,
                agent: read_agent(row, 1)?,
                stream: row.get(2)?,
                body: row.get(3)?,
            })
        })?;

        rows.collect()
    }

    pub fn insert_message(
        &self,
        session_id: Option<&str>,
        route_from: &str,
        route_to: &str,
        body: &str,
    ) -> rusqlite::Result<i64> {
        self.conn.execute(
            r#"
            INSERT INTO messages (session_id, route_from, route_to, body)
            VALUES (?1, ?2, ?3, ?4)
            "#,
            params![session_id, route_from, route_to, body],
        )?;

        Ok(self.conn.last_insert_rowid())
    }

    pub fn messages(&self) -> rusqlite::Result<Vec<StoredMessage>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT id, session_id, route_from, route_to, body
            FROM messages
            ORDER BY id ASC
            "#,
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(StoredMessage {
                id: row.get(0)?,
                session_id: row.get(1)?,
                route_from: row.get(2)?,
                route_to: row.get(3)?,
                body: row.get(4)?,
            })
        })?;

        rows.collect()
    }

    pub fn message_count(&self) -> rusqlite::Result<i64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
    }
}

fn map_inter_agent_message(row: &Row<'_>) -> rusqlite::Result<StoredInterAgentMessage> {
    Ok(StoredInterAgentMessage {
        id: row.get(0)?,
        thread_id: row.get(1)?,
        from: read_agent(row, 2)?,
        to: read_agent(row, 3)?,
        kind: row.get(4)?,
        body: row.get(5)?,
    })
}

fn map_approval(row: &Row<'_>) -> rusqlite::Result<StoredApproval> {
    Ok(StoredApproval {
        id: row.get(0)?,
        thread_id: row.get(1)?,
        action_kind: row.get(2)?,
        body: row.get(3)?,
        decision: row.get(4)?,
    })
}

fn read_agent(row: &Row<'_>, index: usize) -> rusqlite::Result<AgentId> {
    let value: String = row.get(index)?;
    AgentId::try_from(value.as_str()).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            Type::Text,
            Box::new(io::Error::new(io::ErrorKind::InvalidData, err)),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_inter_agent_message_in_sqlite() {
        let store = SqliteStore::in_memory().expect("store should initialize");
        let message = TeamMessage {
            to: AgentId::Claude,
            kind: "question".to_string(),
            body: "should we write tests first?".to_string(),
        };

        let id = store
            .insert_inter_agent_message("thread-1", AgentId::Codex, &message)
            .expect("message should insert");
        let saved = store
            .inter_agent_message(id)
            .expect("message should query")
            .expect("message should exist");

        assert_eq!(
            saved,
            StoredInterAgentMessage {
                id,
                thread_id: "thread-1".to_string(),
                from: AgentId::Codex,
                to: AgentId::Claude,
                kind: "question".to_string(),
                body: "should we write tests first?".to_string(),
            }
        );
        assert_eq!(store.inter_agent_message_count().unwrap(), 1);
    }

    #[test]
    fn records_pending_approval_in_sqlite() {
        let store = SqliteStore::in_memory().expect("store should initialize");

        let id = store
            .insert_approval("thread-1", "git", "run git status")
            .expect("approval should insert");
        let saved = store
            .approval(id)
            .expect("approval should query")
            .expect("approval should exist");

        assert_eq!(
            saved,
            StoredApproval {
                id,
                thread_id: "thread-1".to_string(),
                action_kind: "git".to_string(),
                body: "run git status".to_string(),
                decision: "pending".to_string(),
            }
        );
        assert_eq!(store.approval_count().unwrap(), 1);
    }

    #[test]
    fn lists_and_decides_pending_approvals() {
        let store = SqliteStore::in_memory().expect("store should initialize");
        let first = store
            .insert_approval("thread-1", "git", "run git status")
            .expect("approval should insert");
        let second = store
            .insert_approval("thread-2", "shell", "run `cargo test`")
            .expect("approval should insert");

        let pending = store
            .pending_approvals()
            .expect("pending approvals should query");
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].id, first);
        assert_eq!(pending[1].id, second);
        assert_eq!(store.pending_approval_count().unwrap(), 2);

        assert!(
            store
                .set_approval_decision(first, "approved")
                .expect("approval should update")
        );
        assert!(
            !store
                .set_approval_decision(first, "rejected")
                .expect("decided approval should not update again")
        );

        let saved = store
            .approval(first)
            .expect("approval should query")
            .expect("approval should exist");
        assert_eq!(saved.decision, "approved");
        assert_eq!(store.pending_approval_count().unwrap(), 1);
    }

    #[test]
    fn records_terminal_event_in_sqlite() {
        let store = SqliteStore::in_memory().expect("store should initialize");

        let id = store
            .insert_terminal_event(AgentId::Codex, "stdout", "{\"type\":\"turn.started\"}")
            .expect("terminal event should insert");
        let saved = store
            .terminal_event(id)
            .expect("terminal event should query")
            .expect("terminal event should exist");

        assert_eq!(
            saved,
            StoredTerminalEvent {
                id,
                agent: AgentId::Codex,
                stream: "stdout".to_string(),
                body: "{\"type\":\"turn.started\"}".to_string(),
            }
        );
        assert_eq!(store.terminal_event_count().unwrap(), 1);
        assert_eq!(store.terminal_events().unwrap(), vec![saved]);
    }

    #[test]
    fn records_visible_message_in_sqlite() {
        let store = SqliteStore::in_memory().expect("store should initialize");

        let id = store
            .insert_message(Some("thread-1"), "You", "Team", "plan this")
            .expect("message should insert");
        let messages = store.messages().expect("messages should query");

        assert_eq!(
            messages,
            vec![StoredMessage {
                id,
                session_id: Some("thread-1".to_string()),
                route_from: "You".to_string(),
                route_to: "Team".to_string(),
                body: "plan this".to_string(),
            }]
        );
        assert_eq!(store.message_count().unwrap(), 1);
    }
}
