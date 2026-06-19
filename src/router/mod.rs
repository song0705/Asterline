//! Message routing: agent-to-agent envelope parsing, the relay guard, and
//! resolution of route targets against a team roster.

pub mod envelope;
pub mod relay;

pub use envelope::{ParsedAgentOutput, parse_agent_output};
pub use relay::{RelayDecision, RelayGuard};

use crate::domain::event::RouteTo;
use crate::domain::team::{MemberId, TeamConfig};

/// Outcome of resolving a set of [`RouteTo`] targets against a roster.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ResolvedTargets {
    /// Concrete member ids, de-duplicated and in first-seen order.
    pub members: Vec<MemberId>,
    /// Target strings that matched no member id or display name.
    pub unknown: Vec<String>,
}

/// Resolve `targets` against `config`. `RouteTo::All` expands to every member.
/// `exclude` (typically the sender) is never included in the result, so an
/// agent broadcasting to `all` does not message itself.
pub fn resolve_targets(
    config: &TeamConfig,
    targets: &[RouteTo],
    exclude: Option<&MemberId>,
) -> ResolvedTargets {
    let mut members: Vec<MemberId> = Vec::new();
    let mut unknown: Vec<String> = Vec::new();

    let mut push_member = |id: MemberId| {
        if Some(&id) == exclude {
            return;
        }
        if !members.contains(&id) {
            members.push(id);
        }
    };

    for target in targets {
        match target {
            RouteTo::All => {
                for member in &config.members {
                    push_member(member.id.clone());
                }
            }
            RouteTo::Member(name) => match config.find(name) {
                Some(member) => push_member(member.id.clone()),
                None => {
                    if !unknown.contains(name) {
                        unknown.push(name.clone());
                    }
                }
            },
        }
    }

    ResolvedTargets { members, unknown }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::team::{BackendKind, TeamMember};

    fn team() -> TeamConfig {
        TeamConfig::new("t", "/tmp/ws")
            .with_member(TeamMember::new(
                "builder",
                "Builder",
                BackendKind::Codex,
                "implementation",
            ))
            .with_member(TeamMember::new(
                "reviewer",
                "Reviewer",
                BackendKind::Claude,
                "review",
            ))
    }

    #[test]
    fn resolves_named_member() {
        let resolved = resolve_targets(
            &team(),
            &[RouteTo::Member("reviewer".to_string())],
            None,
        );
        assert_eq!(resolved.members, vec![MemberId::new("reviewer")]);
        assert!(resolved.unknown.is_empty());
    }

    #[test]
    fn resolves_by_display_name() {
        let resolved = resolve_targets(
            &team(),
            &[RouteTo::Member("Builder".to_string())],
            None,
        );
        assert_eq!(resolved.members, vec![MemberId::new("builder")]);
    }

    #[test]
    fn all_expands_to_everyone_except_sender() {
        let sender = MemberId::new("builder");
        let resolved = resolve_targets(&team(), &[RouteTo::All], Some(&sender));
        assert_eq!(resolved.members, vec![MemberId::new("reviewer")]);
    }

    #[test]
    fn unknown_target_is_reported() {
        let resolved = resolve_targets(
            &team(),
            &[RouteTo::Member("ghost".to_string())],
            None,
        );
        assert!(resolved.members.is_empty());
        assert_eq!(resolved.unknown, vec!["ghost".to_string()]);
    }

    #[test]
    fn duplicate_targets_are_deduplicated() {
        let resolved = resolve_targets(
            &team(),
            &[
                RouteTo::Member("builder".to_string()),
                RouteTo::Member("Builder".to_string()),
                RouteTo::All,
            ],
            None,
        );
        assert_eq!(
            resolved.members,
            vec![MemberId::new("builder"), MemberId::new("reviewer")]
        );
    }
}
