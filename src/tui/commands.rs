//! Parse composer text into a submission: a runtime command, a drawer to open,
//! an approval decision, or help. Supports slash commands and `@member` prefixes.

use crate::domain::event::{ApprovalDecision, MessageTarget, UiCommand};
use crate::domain::team::MemberId;
use crate::tui::drawers::Drawer;

/// What submitting the composer should do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Submission {
    /// Send a command to the runtime.
    Runtime(UiCommand),
    /// Open a drawer (a local UI action).
    Drawer(Drawer),
    /// Approve (true) or reject (false) the first pending approval.
    ApproveFirst(ApprovalDecision),
    /// Show help.
    Help,
    /// Nothing to do (blank input).
    Empty,
}

/// Parse the composer text.
pub fn parse(input: &str) -> Submission {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Submission::Empty;
    }

    if let Some(rest) = trimmed.strip_prefix('/') {
        return parse_slash(rest);
    }
    if let Some(rest) = trimmed.strip_prefix('@') {
        let (member, body) = split_first_word(rest);
        if member.is_empty() || body.is_empty() {
            return Submission::Empty;
        }
        return Submission::Runtime(UiCommand::UserMessage {
            target: MessageTarget::Member(MemberId::new(member)),
            body: body.to_string(),
        });
    }

    Submission::Runtime(UiCommand::UserMessage {
        target: MessageTarget::Default,
        body: trimmed.to_string(),
    })
}

fn parse_slash(rest: &str) -> Submission {
    let (cmd, arg) = split_first_word(rest);
    match cmd {
        "ask" => {
            let (member, body) = split_first_word(arg);
            if member.is_empty() || body.is_empty() {
                Submission::Help
            } else {
                Submission::Runtime(UiCommand::UserMessage {
                    target: MessageTarget::Member(MemberId::new(member)),
                    body: body.to_string(),
                })
            }
        }
        "all" => {
            if arg.is_empty() {
                Submission::Help
            } else {
                Submission::Runtime(UiCommand::UserMessage {
                    target: MessageTarget::All,
                    body: arg.to_string(),
                })
            }
        }
        "team" | "status" | "sessions" => Submission::Drawer(Drawer::Team),
        "logs" => Submission::Drawer(Drawer::Logs),
        "abort" => Submission::Runtime(UiCommand::Cancel { member: None }),
        "retry" => Submission::Runtime(UiCommand::Retry),
        "approve" => Submission::ApproveFirst(ApprovalDecision::Approve),
        "reject" => Submission::ApproveFirst(ApprovalDecision::Reject),
        "help" => Submission::Help,
        _ => Submission::Help,
    }
}

fn split_first_word(s: &str) -> (&str, &str) {
    let s = s.trim_start();
    match s.find(char::is_whitespace) {
        Some(idx) => (&s[..idx], s[idx..].trim()),
        None => (s, ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_targets_default() {
        assert_eq!(
            parse("build the parser"),
            Submission::Runtime(UiCommand::UserMessage {
                target: MessageTarget::Default,
                body: "build the parser".to_string(),
            })
        );
    }

    #[test]
    fn at_prefix_targets_member() {
        assert_eq!(
            parse("@reviewer please check"),
            Submission::Runtime(UiCommand::UserMessage {
                target: MessageTarget::Member(MemberId::new("reviewer")),
                body: "please check".to_string(),
            })
        );
    }

    #[test]
    fn ask_command_targets_member() {
        assert_eq!(
            parse("/ask builder implement it"),
            Submission::Runtime(UiCommand::UserMessage {
                target: MessageTarget::Member(MemberId::new("builder")),
                body: "implement it".to_string(),
            })
        );
    }

    #[test]
    fn all_command_broadcasts() {
        assert_eq!(
            parse("/all status?"),
            Submission::Runtime(UiCommand::UserMessage {
                target: MessageTarget::All,
                body: "status?".to_string(),
            })
        );
    }

    #[test]
    fn drawer_and_control_commands() {
        assert_eq!(parse("/logs"), Submission::Drawer(Drawer::Logs));
        assert_eq!(parse("/team"), Submission::Drawer(Drawer::Team));
        assert_eq!(
            parse("/abort"),
            Submission::Runtime(UiCommand::Cancel { member: None })
        );
        assert_eq!(parse("/retry"), Submission::Runtime(UiCommand::Retry));
        assert_eq!(
            parse("/approve"),
            Submission::ApproveFirst(ApprovalDecision::Approve)
        );
    }

    #[test]
    fn blank_is_empty_and_unknown_slash_is_help() {
        assert_eq!(parse("   "), Submission::Empty);
        assert_eq!(parse("/wat"), Submission::Help);
        assert_eq!(parse("/ask builder"), Submission::Help);
    }
}
