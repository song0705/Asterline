//! Parse composer text into a submission: a runtime command, a drawer to open,
//! an approval decision, or help. Supports slash commands and `@member` prefixes.

use crate::domain::event::{ApprovalDecision, MessageTarget, RunId, RunStepStatus, UiCommand};
use crate::domain::mode::TerminalMode;
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
    /// Search the transcript (`/find`); empty query clears the search.
    FindInChat(String),
    /// Show help.
    Help,
    /// Reject invalid command syntax while leaving the draft untouched.
    Invalid(String),
    /// Non-empty message text without an explicit target prefix.
    NeedsTarget,
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

    Submission::NeedsTarget
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
        "team" if arg.is_empty() => Submission::Drawer(Drawer::Team),
        "runs" if arg.is_empty() => Submission::Drawer(Drawer::Runs),
        "logs" if arg.is_empty() => Submission::Drawer(Drawer::Logs),
        "diff" if arg.is_empty() => Submission::Drawer(Drawer::Diff),
        "skills" if arg.is_empty() => Submission::Drawer(Drawer::Skills),
        "new" if arg.is_empty() => Submission::Runtime(UiCommand::NewSession),
        "resume" if arg.is_empty() => Submission::Runtime(UiCommand::RequestResume),
        "abort" if arg.is_empty() => Submission::Runtime(UiCommand::Cancel { member: None }),
        "retry" if arg.is_empty() => Submission::Runtime(UiCommand::Retry),
        "approve" if arg.is_empty() => Submission::ApproveFirst(ApprovalDecision::Approve),
        "reject" if arg.is_empty() => Submission::ApproveFirst(ApprovalDecision::Reject),
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
        "mode" => parse_mode_selector(arg),
        "find" => Submission::FindInChat(arg.to_string()),
        "continue" => {
            let (first, rest) = split_first_word(arg);
            let (run_id, note) = if let Some(run_id) = parse_run_id(first) {
                (Some(run_id), (!rest.is_empty()).then(|| rest.to_string()))
            } else {
                (None, (!arg.is_empty()).then(|| arg.to_string()))
            };
            Submission::Runtime(UiCommand::ContinueRun { run_id, note })
        }
        "note" => {
            let (first, rest) = split_first_word(arg);
            let (run_id, note) = if let Some(run_id) = parse_run_id(first) {
                (Some(run_id), rest)
            } else {
                (None, arg)
            };
            if note.is_empty() {
                Submission::Help
            } else {
                Submission::Runtime(UiCommand::NoteRun {
                    run_id,
                    note: note.to_string(),
                })
            }
        }
        "block" => {
            let (first, rest) = split_first_word(arg);
            let (run_id, reason) = if let Some(run_id) = parse_run_id(first) {
                (Some(run_id), rest)
            } else {
                (None, arg)
            };
            if reason.is_empty() {
                Submission::Help
            } else {
                Submission::Runtime(UiCommand::BlockRun {
                    run_id,
                    reason: reason.to_string(),
                })
            }
        }
        "verify" => {
            let (first, rest) = split_first_word(arg);
            let (run_id, command) = if let Some(run_id) = parse_run_id(first) {
                (Some(run_id), (!rest.is_empty()).then(|| rest.to_string()))
            } else {
                (None, (!arg.is_empty()).then(|| arg.to_string()))
            };
            Submission::Runtime(UiCommand::VerifyRun { run_id, command })
        }
        "step" => parse_step_command(arg),
        "focus" => {
            let (member, _) = split_first_word(arg);
            if member.is_empty() {
                Submission::Help
            } else {
                Submission::Drawer(Drawer::MemberLogs(MemberId::new(member)))
            }
        }
        "help" if arg.is_empty() => Submission::Help,
        "team" | "runs" | "logs" | "diff" | "skills" | "new" | "resume" | "abort" | "retry"
        | "approve" | "reject" | "help" => {
            Submission::Invalid(format!("/{cmd} does not accept arguments; draft kept"))
        }
        _ => Submission::Help,
    }
}

fn parse_mode_selector(arg: &str) -> Submission {
    let (selected, extra) = split_first_word(arg);
    if !extra.is_empty() {
        return Submission::Help;
    }
    TerminalMode::parse(selected).map_or(Submission::Help, |mode| {
        Submission::Runtime(UiCommand::SetMode { mode })
    })
}

fn parse_step_command(arg: &str) -> Submission {
    let (action, rest) = split_first_word(arg);
    match action {
        "add" => {
            let (first, rest_after_first) = split_first_word(rest);
            let (run_id, title_input) = if let Some(run_id) = parse_run_id(first) {
                (Some(run_id), rest_after_first)
            } else {
                (None, rest)
            };
            let (owner, title) = split_optional_owner(title_input);
            if title.is_empty() {
                Submission::Help
            } else {
                Submission::Runtime(UiCommand::AddRunStep {
                    run_id,
                    owner,
                    title: title.to_string(),
                })
            }
        }
        "todo" | "doing" | "done" | "block" | "blocked" => {
            let Some(status) = parse_run_step_status(action) else {
                return Submission::Help;
            };
            let (first, rest_after_first) = split_first_word(rest);
            let (run_id, number_text, note) = if let Some(run_id) = parse_run_id(first) {
                let (number, note) = split_first_word(rest_after_first);
                (Some(run_id), number, note)
            } else {
                let (number, note) = split_first_word(rest);
                (None, number, note)
            };
            let Ok(step) = number_text.parse::<u32>() else {
                return Submission::Help;
            };
            if step == 0 {
                return Submission::Help;
            }
            Submission::Runtime(UiCommand::UpdateRunStep {
                run_id,
                step,
                status,
                note: (!note.is_empty()).then(|| note.to_string()),
            })
        }
        "rename" | "edit" => {
            let (first, rest_after_first) = split_first_word(rest);
            let (run_id, number_text, title) = if let Some(run_id) = parse_run_id(first) {
                let (number, title) = split_first_word(rest_after_first);
                (Some(run_id), number, title)
            } else {
                let (number, title) = split_first_word(rest);
                (None, number, title)
            };
            let Ok(step) = number_text.parse::<u32>() else {
                return Submission::Help;
            };
            if step == 0 || title.is_empty() {
                return Submission::Help;
            }
            Submission::Runtime(UiCommand::RenameRunStep {
                run_id,
                step,
                title: title.to_string(),
            })
        }
        "remove" | "delete" | "drop" => {
            let (first, rest_after_first) = split_first_word(rest);
            let (run_id, number_text) = if let Some(run_id) = parse_run_id(first) {
                let (number, _) = split_first_word(rest_after_first);
                (Some(run_id), number)
            } else {
                let (number, _) = split_first_word(rest);
                (None, number)
            };
            let Ok(step) = number_text.parse::<u32>() else {
                return Submission::Help;
            };
            if step == 0 {
                return Submission::Help;
            }
            Submission::Runtime(UiCommand::RemoveRunStep { run_id, step })
        }
        "assign" | "owner" => {
            let (first, rest_after_first) = split_first_word(rest);
            let (run_id, number_text, owner_text) = if let Some(run_id) = parse_run_id(first) {
                let (number, owner) = split_first_word(rest_after_first);
                (Some(run_id), number, owner)
            } else {
                let (number, owner) = split_first_word(rest);
                (None, number, owner)
            };
            let Ok(step) = number_text.parse::<u32>() else {
                return Submission::Help;
            };
            let Some(owner) = parse_owner_arg(owner_text) else {
                return Submission::Help;
            };
            if step == 0 {
                return Submission::Help;
            }
            Submission::Runtime(UiCommand::AssignRunStep {
                run_id,
                step,
                owner: Some(owner),
            })
        }
        "unassign" | "clear-owner" | "clear_owner" => {
            let (first, rest_after_first) = split_first_word(rest);
            let (run_id, number_text) = if let Some(run_id) = parse_run_id(first) {
                let (number, _) = split_first_word(rest_after_first);
                (Some(run_id), number)
            } else {
                let (number, _) = split_first_word(rest);
                (None, number)
            };
            let Ok(step) = number_text.parse::<u32>() else {
                return Submission::Help;
            };
            if step == 0 {
                return Submission::Help;
            }
            Submission::Runtime(UiCommand::AssignRunStep {
                run_id,
                step,
                owner: None,
            })
        }
        _ => Submission::Help,
    }
}

fn split_optional_owner(input: &str) -> (Option<MemberId>, &str) {
    let (first, rest) = split_first_word(input);
    parse_prefixed_owner_arg(first)
        .map(|owner| (Some(owner), rest))
        .unwrap_or((None, input))
}

fn parse_prefixed_owner_arg(input: &str) -> Option<MemberId> {
    input.trim().strip_prefix('@').and_then(parse_owner_arg)
}

fn parse_owner_arg(input: &str) -> Option<MemberId> {
    let owner = input.trim().trim_start_matches('@');
    if owner.is_empty()
        || owner.eq_ignore_ascii_case("none")
        || owner.eq_ignore_ascii_case("unassigned")
        || owner.chars().any(char::is_whitespace)
    {
        None
    } else {
        Some(MemberId::new(owner))
    }
}

fn parse_run_step_status(value: &str) -> Option<RunStepStatus> {
    match value {
        "todo" => Some(RunStepStatus::Todo),
        "doing" => Some(RunStepStatus::Doing),
        "done" => Some(RunStepStatus::Done),
        "block" | "blocked" => Some(RunStepStatus::Blocked),
        _ => None,
    }
}

fn split_first_word(s: &str) -> (&str, &str) {
    let s = s.trim_start();
    match s.find(char::is_whitespace) {
        Some(idx) => (&s[..idx], s[idx..].trim()),
        None => (s, ""),
    }
}

fn parse_run_id(value: &str) -> Option<RunId> {
    let raw = value.strip_prefix("run-")?;
    raw.parse::<u64>().ok().map(RunId)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_requires_an_explicit_target_prefix() {
        assert_eq!(parse("build the parser"), Submission::NeedsTarget);
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
        assert_eq!(parse("/runs"), Submission::Drawer(Drawer::Runs));
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
    fn no_argument_commands_reject_trailing_text() {
        for command in [
            "/team extra",
            "/runs extra",
            "/logs extra",
            "/diff extra",
            "/skills extra",
            "/new extra",
            "/resume extra",
            "/abort extra",
            "/retry extra",
            "/approve extra",
            "/reject extra",
            "/help extra",
        ] {
            assert!(
                matches!(parse(command), Submission::Invalid(message) if message.contains("does not accept arguments")),
                "{command}"
            );
        }
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
    fn plan_and_focus_commands() {
        assert_eq!(parse("/plan build a parser"), Submission::Help);
        assert_eq!(
            parse("/focus reviewer"),
            Submission::Drawer(Drawer::MemberLogs(MemberId::new("reviewer")))
        );
        assert_eq!(
            parse("/continue"),
            Submission::Runtime(UiCommand::ContinueRun {
                run_id: None,
                note: None
            })
        );
        assert_eq!(
            parse("/continue run-12 fix verification"),
            Submission::Runtime(UiCommand::ContinueRun {
                run_id: Some(RunId(12)),
                note: Some("fix verification".to_string())
            })
        );
        assert_eq!(parse("/cont unblock deployment"), Submission::Help);
        assert_eq!(
            parse("/note run-12 waiting for product signoff"),
            Submission::Runtime(UiCommand::NoteRun {
                run_id: Some(RunId(12)),
                note: "waiting for product signoff".to_string()
            })
        );
        assert_eq!(
            parse("/note checkpoint saved"),
            Submission::Runtime(UiCommand::NoteRun {
                run_id: None,
                note: "checkpoint saved".to_string()
            })
        );
        assert_eq!(
            parse("/block run-12 missing credentials"),
            Submission::Runtime(UiCommand::BlockRun {
                run_id: Some(RunId(12)),
                reason: "missing credentials".to_string()
            })
        );
        assert_eq!(
            parse("/step add write parser tests"),
            Submission::Runtime(UiCommand::AddRunStep {
                run_id: None,
                owner: None,
                title: "write parser tests".to_string()
            })
        );
        assert_eq!(
            parse("/step add run-12 wire verification"),
            Submission::Runtime(UiCommand::AddRunStep {
                run_id: Some(RunId(12)),
                owner: None,
                title: "wire verification".to_string()
            })
        );
        assert_eq!(
            parse("/step add run-12 @builder wire verification"),
            Submission::Runtime(UiCommand::AddRunStep {
                run_id: Some(RunId(12)),
                owner: Some(MemberId::new("builder")),
                title: "wire verification".to_string()
            })
        );
        assert_eq!(
            parse("/step doing run-12 2 waiting on reviewer"),
            Submission::Runtime(UiCommand::UpdateRunStep {
                run_id: Some(RunId(12)),
                step: 2,
                status: RunStepStatus::Doing,
                note: Some("waiting on reviewer".to_string())
            })
        );
        assert_eq!(
            parse("/step done 1"),
            Submission::Runtime(UiCommand::UpdateRunStep {
                run_id: None,
                step: 1,
                status: RunStepStatus::Done,
                note: None
            })
        );
        assert_eq!(
            parse("/step rename run-12 2 document setup"),
            Submission::Runtime(UiCommand::RenameRunStep {
                run_id: Some(RunId(12)),
                step: 2,
                title: "document setup".to_string()
            })
        );
        assert_eq!(
            parse("/step remove 3"),
            Submission::Runtime(UiCommand::RemoveRunStep {
                run_id: None,
                step: 3
            })
        );
        assert_eq!(
            parse("/step assign run-12 3 reviewer"),
            Submission::Runtime(UiCommand::AssignRunStep {
                run_id: Some(RunId(12)),
                step: 3,
                owner: Some(MemberId::new("reviewer"))
            })
        );
        assert_eq!(
            parse("/step unassign 3"),
            Submission::Runtime(UiCommand::AssignRunStep {
                run_id: None,
                step: 3,
                owner: None
            })
        );
        assert_eq!(parse("/step add"), Submission::Help);
        assert_eq!(parse("/step done 0"), Submission::Help);
        assert_eq!(parse("/step done nope"), Submission::Help);
        assert_eq!(parse("/step rename 2"), Submission::Help);
        assert_eq!(parse("/step remove 0"), Submission::Help);
        assert_eq!(parse("/step assign 2"), Submission::Help);
        assert_eq!(parse("/note"), Submission::Help);
        assert_eq!(parse("/block run-12"), Submission::Help);
        assert_eq!(parse("/plan"), Submission::Help);
        assert_eq!(parse("/focus"), Submission::Help);
    }

    #[test]
    fn removed_mode_commands_do_not_bypass_mode_selection() {
        for command in [
            "/review fix it",
            "/plan goal",
            "/lead goal",
            "/roundtable topic",
            "/rt topic",
        ] {
            assert_eq!(parse(command), Submission::Help);
        }
        assert_eq!(
            parse("/find needle"),
            Submission::FindInChat("needle".to_string())
        );
        assert_eq!(parse("/find"), Submission::FindInChat(String::new()));
    }

    #[test]
    fn verify_command_runs_default_or_explicit_check() {
        assert_eq!(
            parse("/verify"),
            Submission::Runtime(UiCommand::VerifyRun {
                run_id: None,
                command: None
            })
        );
        assert_eq!(
            parse("/verify cargo test -q"),
            Submission::Runtime(UiCommand::VerifyRun {
                run_id: None,
                command: Some("cargo test -q".to_string())
            })
        );
        assert_eq!(
            parse("/verify run-12 cargo test -q"),
            Submission::Runtime(UiCommand::VerifyRun {
                run_id: Some(RunId(12)),
                command: Some("cargo test -q".to_string())
            })
        );
        assert_eq!(
            parse("/verify run-12"),
            Submission::Runtime(UiCommand::VerifyRun {
                run_id: Some(RunId(12)),
                command: None
            })
        );
    }

    #[test]
    fn new_session_command() {
        assert_eq!(parse("/new"), Submission::Runtime(UiCommand::NewSession));
        assert_eq!(parse("/clear"), Submission::Help);
    }

    #[test]
    fn resume_opens_saved_chat_picker() {
        assert_eq!(
            parse("/resume"),
            Submission::Runtime(UiCommand::RequestResume)
        );
        assert!(matches!(parse("/resume 3"), Submission::Invalid(_)));
    }

    #[test]
    fn mode_command_selects_normal_and_collaboration_modes() {
        for (text, mode) in [
            ("/mode normal", TerminalMode::Normal),
            ("/mode review", TerminalMode::Review),
            ("/mode plan", TerminalMode::Plan),
            ("/mode brainstorm", TerminalMode::Brainstorm),
            ("/mode team", TerminalMode::Team),
        ] {
            assert_eq!(
                parse(text),
                Submission::Runtime(UiCommand::SetMode { mode })
            );
        }
        assert_eq!(parse("/mode"), Submission::Help);
        assert_eq!(parse("/mode review fix parser"), Submission::Help);
    }

    #[test]
    fn skills_command_opens_picker() {
        assert_eq!(parse("/skills"), Submission::Drawer(Drawer::Skills));
        assert_eq!(parse("/skill"), Submission::Help);
    }
}
