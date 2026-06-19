pub mod envelope;
pub mod relay;

use crate::router::envelope::{TeamMessage, parse_team_message};
use crate::types::AgentId;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RoutedEvent {
    InterAgent { from: AgentId, message: TeamMessage },
    VisibleOutput { from: AgentId, body: String },
}

pub fn route_agent_output(
    from: AgentId,
    output: &str,
) -> Result<RoutedEvent, envelope::ParseError> {
    match parse_team_message(output)? {
        Some(message) => Ok(RoutedEvent::InterAgent { from, message }),
        None => Ok(RoutedEvent::VisibleOutput {
            from,
            body: output.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_structured_team_message_as_inter_agent_event() {
        let output =
            r#"@@team_message {"to":"claude","kind":"question","body":"should we test first?"}"#;

        let routed = route_agent_output(AgentId::Codex, output).expect("message should parse");

        assert_eq!(
            routed,
            RoutedEvent::InterAgent {
                from: AgentId::Codex,
                message: TeamMessage {
                    to: AgentId::Claude,
                    kind: "question".to_string(),
                    body: "should we test first?".to_string(),
                },
            }
        );
    }

    #[test]
    fn routes_plain_output_as_visible_event() {
        let routed = route_agent_output(AgentId::Claude, "plain progress")
            .expect("plain output should route");

        assert_eq!(
            routed,
            RoutedEvent::VisibleOutput {
                from: AgentId::Claude,
                body: "plain progress".to_string(),
            }
        );
    }
}
