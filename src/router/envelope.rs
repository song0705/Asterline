//! Lenient parser for `@@team_message` agent-to-agent envelopes.
//!
//! Agent output may contain zero or more envelope lines interleaved with normal
//! text. Parsing never loses content: a malformed envelope is reported as a
//! warning and its original line is kept in the visible text.
//!
//! ```text
//! @@team_message {"to":"builder","body":"please implement the migration"}
//! @@team_message {"to":["builder","reviewer"],"body":"implement and review"}
//! @@team_message {"to":"all","body":"let's agree on the data model first"}
//! ```

use serde::Deserialize;

use crate::domain::event::{RouteTo, TeamMessage};

const ENVELOPE_PREFIX: &str = "@@team_message";

/// The result of scanning one agent message for envelopes.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ParsedAgentOutput {
    /// Text with successfully-parsed envelope lines removed.
    pub visible_text: String,
    /// Envelopes parsed from the output, in order.
    pub messages: Vec<TeamMessage>,
    /// Human-readable warnings for malformed envelopes (kept in the logs drawer).
    pub warnings: Vec<String>,
}

#[derive(Deserialize)]
struct EnvelopeRaw {
    to: ToField,
    #[serde(default)]
    kind: Option<String>,
    body: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ToField {
    One(String),
    Many(Vec<String>),
}

impl ToField {
    fn into_route_targets(self) -> Vec<RouteTo> {
        let raw = match self {
            Self::One(value) => vec![value],
            Self::Many(values) => values,
        };
        raw.into_iter()
            .map(|value| {
                if value.eq_ignore_ascii_case("all") {
                    RouteTo::All
                } else {
                    RouteTo::Member(value)
                }
            })
            .collect()
    }
}

/// Scan one agent message for `@@team_message` envelopes.
pub fn parse_agent_output(text: &str) -> ParsedAgentOutput {
    let mut kept_lines = Vec::new();
    let mut messages = Vec::new();
    let mut warnings = Vec::new();

    for line in text.lines() {
        match envelope_payload(line) {
            Some(payload) => match parse_envelope(payload) {
                Ok(message) => messages.push(message),
                Err(warning) => {
                    warnings.push(warning);
                    kept_lines.push(line);
                }
            },
            None => kept_lines.push(line),
        }
    }

    ParsedAgentOutput {
        visible_text: kept_lines.join("\n").trim().to_string(),
        messages,
        warnings,
    }
}

/// If `line` is an envelope, return the JSON payload after the prefix.
fn envelope_payload(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix(ENVELOPE_PREFIX)?;
    if rest.is_empty() || rest.starts_with(char::is_whitespace) || rest.starts_with('{') {
        Some(rest.trim())
    } else {
        None
    }
}

fn parse_envelope(payload: &str) -> Result<TeamMessage, String> {
    let raw: EnvelopeRaw = serde_json::from_str(payload)
        .map_err(|err| format!("invalid @@team_message envelope: {err}"))?;
    let to = raw.to.into_route_targets();
    if to.is_empty() {
        return Err("@@team_message envelope has no target".to_string());
    }
    Ok(TeamMessage {
        to,
        kind: raw.kind,
        body: raw.body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_string_target() {
        let parsed =
            parse_agent_output(r#"@@team_message {"to":"builder","body":"do the thing"}"#);

        assert_eq!(
            parsed.messages,
            vec![TeamMessage {
                to: vec![RouteTo::Member("builder".to_string())],
                kind: None,
                body: "do the thing".to_string(),
            }]
        );
        assert_eq!(parsed.visible_text, "");
        assert!(parsed.warnings.is_empty());
    }

    #[test]
    fn keeps_surrounding_text_and_strips_envelope_line() {
        let parsed = parse_agent_output(
            "Working on it.\n@@team_message {\"to\":\"reviewer\",\"kind\":\"question\",\"body\":\"ok?\"}\nDone.",
        );

        assert_eq!(parsed.visible_text, "Working on it.\nDone.");
        assert_eq!(parsed.messages.len(), 1);
        assert_eq!(parsed.messages[0].kind.as_deref(), Some("question"));
    }

    #[test]
    fn parses_array_target() {
        let parsed = parse_agent_output(
            r#"@@team_message {"to":["builder","reviewer"],"body":"split work"}"#,
        );

        assert_eq!(
            parsed.messages[0].to,
            vec![
                RouteTo::Member("builder".to_string()),
                RouteTo::Member("reviewer".to_string())
            ]
        );
    }

    #[test]
    fn maps_all_keyword_case_insensitively() {
        let parsed = parse_agent_output(r#"@@team_message {"to":"ALL","body":"sync up"}"#);
        assert_eq!(parsed.messages[0].to, vec![RouteTo::All]);
    }

    #[test]
    fn parses_multiple_envelopes() {
        let parsed = parse_agent_output(
            "@@team_message {\"to\":\"a\",\"body\":\"one\"}\n@@team_message {\"to\":\"b\",\"body\":\"two\"}",
        );
        assert_eq!(parsed.messages.len(), 2);
        assert_eq!(parsed.messages[1].body, "two");
    }

    #[test]
    fn malformed_envelope_warns_and_keeps_line() {
        let parsed = parse_agent_output(r#"@@team_message {"to":"a""#);

        assert!(parsed.messages.is_empty());
        assert_eq!(parsed.warnings.len(), 1);
        assert!(parsed.visible_text.contains("@@team_message"));
    }

    #[test]
    fn empty_target_warns_and_keeps_line() {
        let parsed = parse_agent_output(r#"@@team_message {"to":[],"body":"x"}"#);

        assert!(parsed.messages.is_empty());
        assert_eq!(parsed.warnings.len(), 1);
    }

    #[test]
    fn plain_text_has_no_messages() {
        let parsed = parse_agent_output("just some normal output\nwith two lines");

        assert!(parsed.messages.is_empty());
        assert!(parsed.warnings.is_empty());
        assert_eq!(parsed.visible_text, "just some normal output\nwith two lines");
    }

    #[test]
    fn envelope_without_space_after_prefix_still_parses() {
        let parsed = parse_agent_output(r#"@@team_message{"to":"a","body":"hi"}"#);
        assert_eq!(parsed.messages.len(), 1);
    }
}
