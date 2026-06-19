//! In-memory cache of each member's resumable backend session id, kept in sync
//! with the `agent_sessions` table so the runtime can resume without a DB hit.

use std::collections::HashMap;

use crate::domain::event::AgentSessionId;
use crate::domain::team::MemberId;
use crate::store::sqlite::SqliteStore;

#[derive(Debug, Default)]
pub struct SessionRegistry {
    sessions: HashMap<MemberId, AgentSessionId>,
}

impl SessionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load any persisted sessions for the given members.
    pub fn from_store(store: &SqliteStore, members: &[MemberId]) -> Self {
        let mut sessions = HashMap::new();
        for member in members {
            if let Ok(Some(session)) = store.session_for(member) {
                sessions.insert(member.clone(), session);
            }
        }
        Self { sessions }
    }

    pub fn get(&self, member: &MemberId) -> Option<AgentSessionId> {
        self.sessions.get(member).cloned()
    }

    pub fn set(&mut self, member: MemberId, session: AgentSessionId) {
        self.sessions.insert(member, session);
    }

    pub fn clear(&mut self, member: &MemberId) {
        self.sessions.remove(member);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::team::BackendKind;

    #[test]
    fn loads_and_caches_sessions() {
        let store = SqliteStore::in_memory().unwrap();
        let builder = MemberId::new("builder");
        store
            .upsert_session(
                &builder,
                BackendKind::Codex,
                &AgentSessionId("t-1".to_string()),
            )
            .unwrap();

        let mut registry = SessionRegistry::from_store(&store, std::slice::from_ref(&builder));
        assert_eq!(
            registry.get(&builder),
            Some(AgentSessionId("t-1".to_string()))
        );

        registry.set(builder.clone(), AgentSessionId("t-2".to_string()));
        assert_eq!(
            registry.get(&builder),
            Some(AgentSessionId("t-2".to_string()))
        );

        registry.clear(&builder);
        assert_eq!(registry.get(&builder), None);
    }
}
