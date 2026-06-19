use serde::Deserialize;

use crate::types::AgentId;

const TEAM_MESSAGE_PREFIX: &str = "@@team_message ";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TeamMessage {
    pub to: AgentId,
    pub kind: String,
    pub body: String,
}

#[derive(Debug, Eq, PartialEq)]
pub enum ParseError {
    InvalidJson(String),
}

#[derive(Deserialize)]
struct TeamMessageEnvelope {
    to: AgentId,
    kind: String,
    body: String,
}

pub fn parse_team_message(output: &str) -> Result<Option<TeamMessage>, ParseError> {
    let Some(payload) = output.trim().strip_prefix(TEAM_MESSAGE_PREFIX) else {
        return Ok(None);
    };

    let envelope: TeamMessageEnvelope =
        serde_json::from_str(payload).map_err(|err| ParseError::InvalidJson(err.to_string()))?;

    Ok(Some(TeamMessage {
        to: envelope.to,
        kind: envelope.kind,
        body: envelope.body,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_team_message_envelope() {
        let parsed = parse_team_message(
            r#"@@team_message {"to":"claude","kind":"question","body":"write tests first?"}"#,
        )
        .expect("valid envelope should parse");

        assert_eq!(
            parsed,
            Some(TeamMessage {
                to: AgentId::Claude,
                kind: "question".to_string(),
                body: "write tests first?".to_string(),
            })
        );
    }

    #[test]
    fn ignores_plain_output() {
        let parsed = parse_team_message("normal model output").expect("plain output is valid");

        assert_eq!(parsed, None);
    }

    #[test]
    fn rejects_malformed_envelope_json() {
        let parsed = parse_team_message(r#"@@team_message {"to":"claude""#);

        assert!(matches!(parsed, Err(ParseError::InvalidJson(_))));
    }
}
