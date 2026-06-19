use crate::types::{AgentId, Participant, RouteTarget};

pub const TARGETS: [RouteTarget; 5] = [
    RouteTarget::new(Participant::You, Participant::Team),
    RouteTarget::new(Participant::You, Participant::Agent(AgentId::Codex)),
    RouteTarget::new(Participant::You, Participant::Agent(AgentId::Claude)),
    RouteTarget::new(
        Participant::Agent(AgentId::Codex),
        Participant::Agent(AgentId::Claude),
    ),
    RouteTarget::new(
        Participant::Agent(AgentId::Claude),
        Participant::Agent(AgentId::Codex),
    ),
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetSelector {
    selected: usize,
}

impl TargetSelector {
    pub fn new() -> Self {
        Self { selected: 0 }
    }

    pub fn selected(&self) -> RouteTarget {
        TARGETS[self.selected]
    }

    pub fn selected_label(&self) -> String {
        self.selected().to_string()
    }

    pub fn next(&mut self) -> RouteTarget {
        self.selected = (self.selected + 1) % TARGETS.len();
        self.selected()
    }
}

impl Default for TargetSelector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_selector_cycles_through_plan_targets() {
        let mut selector = TargetSelector::new();

        assert_eq!(
            selector.selected(),
            RouteTarget::new(Participant::You, Participant::Team)
        );
        assert_eq!(
            selector.next(),
            RouteTarget::new(Participant::You, Participant::Agent(AgentId::Codex))
        );
        assert_eq!(
            selector.next(),
            RouteTarget::new(Participant::You, Participant::Agent(AgentId::Claude))
        );
    }
}
