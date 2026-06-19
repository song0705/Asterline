use std::collections::HashMap;

pub const DEFAULT_MAX_AUTO_RELAYS: u8 = 5;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RelayDecision {
    Continue { count: u8 },
    Pause { count: u8 },
}

#[derive(Clone, Debug)]
pub struct RelayGuard {
    max_auto_relays: u8,
    counts_by_thread: HashMap<String, u8>,
}

impl RelayGuard {
    pub fn new(max_auto_relays: u8) -> Self {
        Self {
            max_auto_relays,
            counts_by_thread: HashMap::new(),
        }
    }

    pub fn with_default_limit() -> Self {
        Self::new(DEFAULT_MAX_AUTO_RELAYS)
    }

    pub fn record_auto_relay(&mut self, thread_id: impl Into<String>) -> RelayDecision {
        let count = self.counts_by_thread.entry(thread_id.into()).or_insert(0);
        *count = count.saturating_add(1);

        if *count > self.max_auto_relays {
            RelayDecision::Pause { count: *count }
        } else {
            RelayDecision::Continue { count: *count }
        }
    }

    pub fn reset_thread(&mut self, thread_id: &str) {
        self.counts_by_thread.remove(thread_id);
    }
}

impl Default for RelayGuard {
    fn default() -> Self {
        Self::with_default_limit()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pauses_after_default_auto_relay_limit() {
        let mut guard = RelayGuard::default();

        for expected in 1..=DEFAULT_MAX_AUTO_RELAYS {
            assert_eq!(
                guard.record_auto_relay("thread-1"),
                RelayDecision::Continue { count: expected }
            );
        }

        assert_eq!(
            guard.record_auto_relay("thread-1"),
            RelayDecision::Pause {
                count: DEFAULT_MAX_AUTO_RELAYS + 1
            }
        );
    }

    #[test]
    fn tracks_threads_independently() {
        let mut guard = RelayGuard::new(1);

        assert_eq!(
            guard.record_auto_relay("thread-a"),
            RelayDecision::Continue { count: 1 }
        );
        assert_eq!(
            guard.record_auto_relay("thread-b"),
            RelayDecision::Continue { count: 1 }
        );
        assert_eq!(
            guard.record_auto_relay("thread-a"),
            RelayDecision::Pause { count: 2 }
        );
    }
}
