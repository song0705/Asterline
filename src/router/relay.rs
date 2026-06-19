//! Structured relay guard.
//!
//! Bounds automatic agent-to-agent relays so two agents cannot loop forever.
//! Counting is keyed by `(turn, sender)` so each member gets an independent
//! budget within a turn, and a new user turn starts fresh.

use std::collections::HashMap;

use crate::domain::event::TurnId;
use crate::domain::team::{DEFAULT_MAX_AUTO_RELAYS, MemberId};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RelayDecision {
    Continue { count: u32 },
    Pause { count: u32 },
}

#[derive(Clone, Debug)]
pub struct RelayGuard {
    max_auto_relays: u32,
    counts: HashMap<(TurnId, MemberId), u32>,
}

impl RelayGuard {
    pub fn new(max_auto_relays: u32) -> Self {
        Self {
            max_auto_relays,
            counts: HashMap::new(),
        }
    }

    pub fn with_default_limit() -> Self {
        Self::new(DEFAULT_MAX_AUTO_RELAYS)
    }

    /// Record one automatic relay initiated by `sender` during `turn`.
    /// Returns [`RelayDecision::Pause`] once the sender exceeds the limit.
    pub fn record_auto_relay(&mut self, turn: TurnId, sender: &MemberId) -> RelayDecision {
        let count = self
            .counts
            .entry((turn, sender.clone()))
            .or_insert(0);
        *count = count.saturating_add(1);

        if *count > self.max_auto_relays {
            RelayDecision::Pause { count: *count }
        } else {
            RelayDecision::Continue { count: *count }
        }
    }

    /// Forget all counters for a finished turn.
    pub fn reset_turn(&mut self, turn: TurnId) {
        self.counts.retain(|(t, _), _| *t != turn);
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

    fn member(id: &str) -> MemberId {
        MemberId::new(id)
    }

    #[test]
    fn continues_until_limit_then_pauses() {
        let mut guard = RelayGuard::new(3);
        let turn = TurnId(1);
        let builder = member("builder");

        for expected in 1..=3 {
            assert_eq!(
                guard.record_auto_relay(turn, &builder),
                RelayDecision::Continue { count: expected }
            );
        }
        assert_eq!(
            guard.record_auto_relay(turn, &builder),
            RelayDecision::Pause { count: 4 }
        );
    }

    #[test]
    fn counts_are_independent_per_member_and_turn() {
        let mut guard = RelayGuard::new(1);
        let turn1 = TurnId(1);
        let turn2 = TurnId(2);

        assert_eq!(
            guard.record_auto_relay(turn1, &member("a")),
            RelayDecision::Continue { count: 1 }
        );
        // Different member, same turn: independent budget.
        assert_eq!(
            guard.record_auto_relay(turn1, &member("b")),
            RelayDecision::Continue { count: 1 }
        );
        // Same member, different turn: independent budget.
        assert_eq!(
            guard.record_auto_relay(turn2, &member("a")),
            RelayDecision::Continue { count: 1 }
        );
        // Same member + turn again: now over the limit.
        assert_eq!(
            guard.record_auto_relay(turn1, &member("a")),
            RelayDecision::Pause { count: 2 }
        );
    }

    #[test]
    fn reset_turn_clears_only_that_turn() {
        let mut guard = RelayGuard::new(1);
        let turn1 = TurnId(1);
        let turn2 = TurnId(2);
        guard.record_auto_relay(turn1, &member("a"));
        guard.record_auto_relay(turn2, &member("a"));

        guard.reset_turn(turn1);

        // turn1 reset -> back to 1; turn2 untouched -> now 2 -> pause.
        assert_eq!(
            guard.record_auto_relay(turn1, &member("a")),
            RelayDecision::Continue { count: 1 }
        );
        assert_eq!(
            guard.record_auto_relay(turn2, &member("a")),
            RelayDecision::Pause { count: 2 }
        );
    }
}
