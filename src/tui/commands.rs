//! Parse composer text into a submission: a runtime command, a drawer to open,
//! an approval decision, or help. Supports slash commands and `@member` prefixes.

use crate::domain::event::{ApprovalDecision, MessageTarget, UiCommand};
use crate::domain::team::{Effort, MemberId};
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
        let target = if member == "all" {
            MessageTarget::All
        } else {
            MessageTarget::Member(MemberId::new(member))
        };
        return Submission::Runtime(UiCommand::UserMessage {
            target,
            body: trimmed.to_string(),
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
                let target = if member == "all" {
                    MessageTarget::All
                } else {
                    MessageTarget::Member(MemberId::new(member))
                };
                Submission::Runtime(UiCommand::UserMessage {
                    target,
                    body: format!("@{} {}", member, body),
                })
            }
        }
        "all" => {
            if arg.is_empty() {
                Submission::Help
            } else {
                Submission::Runtime(UiCommand::UserMessage {
                    target: MessageTarget::All,
                    body: format!("@all {}", arg),
                })
            }
        }
        "team" | "status" | "sessions" => Submission::Drawer(Drawer::Team),
        "logs" => Submission::Drawer(Drawer::Logs),
        "diff" => Submission::Drawer(Drawer::Diff),
        "new" => Submission::Runtime(UiCommand::NewSession),
        "abort" => Submission::Runtime(UiCommand::Cancel { member: None }),
        "retry" => Submission::Runtime(UiCommand::Retry),
        "approve" => Submission::ApproveFirst(ApprovalDecision::Approve),
        "reject" => Submission::ApproveFirst(ApprovalDecision::Reject),
        "effort" => {
            let (member, level) = split_first_word(arg);
            match Effort::parse(level) {
                Some(effort) if !member.is_empty() => Submission::Runtime(UiCommand::SetEffort {
                    member: MemberId::new(member),
                    effort,
                }),
                _ => Submission::Help,
            }
        }
        "workflow" => {
            if arg.is_empty() {
                Submission::Help
            } else {
                Submission::Runtime(UiCommand::RunWorkflow {
                    goal: arg.to_string(),
                })
            }
        }
        "focus" => {
            let (member, _) = split_first_word(arg);
            if member.is_empty() {
                Submission::Help
            } else {
                Submission::Drawer(Drawer::MemberLogs(MemberId::new(member)))
            }
        }
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
                body: "@reviewer please check".to_string(),
            })
        );
    }

    #[test]
    fn ask_command_targets_member() {
        assert_eq!(
            parse("/ask builder implement it"),
            Submission::Runtime(UiCommand::UserMessage {
                target: MessageTarget::Member(MemberId::new("builder")),
                body: "@builder implement it".to_string(),
            })
        );
    }

    #[test]
    fn ask_all_command_broadcasts() {
        assert_eq!(
            parse("/ask all implement it"),
            Submission::Runtime(UiCommand::UserMessage {
                target: MessageTarget::All,
                body: "@all implement it".to_string(),
            })
        );
    }

    #[test]
    fn all_command_broadcasts() {
        assert_eq!(
            parse("/all status?"),
            Submission::Runtime(UiCommand::UserMessage {
                target: MessageTarget::All,
                body: "@all status?".to_string(),
            })
        );
    }

    #[test]
    fn drawer_and_control_commands() {
        assert_eq!(parse("/logs"), Submission::Drawer(Drawer::Logs));
        assert_eq!(parse("/team"), Submission::Drawer(Drawer::Team));
        assert_eq!(parse("/diff"), Submission::Drawer(Drawer::Diff));
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

    #[test]
    fn effort_command_sets_member_effort() {
        assert_eq!(
            parse("/effort builder high"),
            Submission::Runtime(UiCommand::SetEffort {
                member: MemberId::new("builder"),
                effort: Effort::High,
            })
        );
        assert_eq!(parse("/effort builder"), Submission::Help);
        assert_eq!(parse("/effort builder bogus"), Submission::Help);
    }

    #[test]
    fn workflow_and_focus_commands() {
        assert_eq!(
            parse("/workflow build a parser"),
            Submission::Runtime(UiCommand::RunWorkflow {
                goal: "build a parser".to_string(),
            })
        );
        assert_eq!(
            parse("/focus reviewer"),
            Submission::Drawer(Drawer::MemberLogs(MemberId::new("reviewer")))
        );
        assert_eq!(parse("/workflow"), Submission::Help);
        assert_eq!(parse("/focus"), Submission::Help);
    }

    #[test]
    fn new_session_command() {
        assert_eq!(parse("/new"), Submission::Runtime(UiCommand::NewSession));
    }
}
