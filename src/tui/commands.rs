//! Parse composer text into a submission: a runtime command, a drawer to open,
//! an approval decision, or help. Supports slash commands and `@member` prefixes.

use crate::domain::event::{ApprovalDecision, MessageTarget, RunId, RunStepStatus, UiCommand};
use crate::domain::mode::TerminalMode;
use crate::domain::team::MemberId;
use crate::tui::drawers::Drawer;

/// What submitting the composer should do.
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(clippy::large_enum_variant)] // Runtime commands stay unboxed at the UI boundary.
pub enum Submission {
    /// Exit the Asterline TUI and begin normal runtime shutdown.
    Exit,
    /// Open one member's native interactive CLI session.
    Attach { member: MemberId },
    /// A targeted slash invocation resolved only against the target backend's
    /// discovered prompt-invocable skills before any prompt is sent to a
    /// noninteractive backend runner.
    TargetedSlash { member: MemberId, body: String },
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
        if let Some(submission) = parse_targeted_slash(member, body) {
            return submission;
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

/// `@member` / `@all` / `/ask member` / `/all` typed with no message body.
/// Lets an image-only send keep an explicit target.
pub fn parse_target_only(input: &str) -> Option<MessageTarget> {
    let trimmed = input.trim();
    if let Some(rest) = trimmed.strip_prefix('@') {
        let (member, body) = split_first_word(rest);
        if !member.is_empty() && body.is_empty() {
            return Some(target_from_member_token(member));
        }
    }
    if let Some(rest) = trimmed.strip_prefix("/ask") {
        let (member, body) = split_first_word(rest);
        if !member.is_empty() && body.is_empty() {
            return Some(target_from_member_token(member));
        }
    }
    if trimmed == "/all" {
        return Some(MessageTarget::All);
    }
    None
}

fn target_from_member_token(member: &str) -> MessageTarget {
    if member == "all" {
        MessageTarget::All
    } else {
        MessageTarget::Member(MemberId::new(member))
    }
}

fn parse_slash(rest: &str) -> Submission {
    let (cmd, arg) = split_first_word(rest);
    match cmd {
        "ask" => {
            let (member, body) = split_first_word(arg);
            if member.is_empty() || body.is_empty() {
                Submission::Help
            } else if let Some(submission) = parse_targeted_slash(member, body) {
                submission
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
            } else if arg.trim_start().starts_with('/') {
                Submission::Invalid(
                    "slash commands need one member; use @member /command (draft kept)".to_string(),
                )
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
        "attach" => {
            let (member, extra) = split_first_word(arg);
            if member.is_empty() {
                Submission::Help
            } else if member == "all" {
                Submission::Invalid("/attach needs one member; use /attach <member>".to_string())
            } else if !extra.is_empty() {
                Submission::Invalid("/attach does not accept arguments; draft kept".to_string())
            } else {
                Submission::Attach {
                    member: MemberId::new(member),
                }
            }
        }
        // Both spellings intentionally perform the same durable reset. Keep
        // `/new` for muscle memory and accept `/clear` when it is submitted
        // directly (rather than only after completion rewrites it).
        "new" | "clear" if arg.is_empty() => Submission::Runtime(UiCommand::NewSession),
        "resume" if arg.is_empty() => Submission::Runtime(UiCommand::RequestResume),
        "exit" if arg.is_empty() => Submission::Exit,
        "retry" if arg.is_empty() => Submission::Runtime(UiCommand::Retry),
        "approve" if arg.is_empty() => Submission::ApproveFirst(ApprovalDecision::Approve),
        "reject" if arg.is_empty() => Submission::ApproveFirst(ApprovalDecision::Reject),
        "mode" if arg.is_empty() => Submission::Drawer(Drawer::Mode),
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
            let (member, extra) = split_first_word(arg);
            if member.is_empty() {
                Submission::Help
            } else if !extra.is_empty() {
                Submission::Invalid(
                    "/focus accepts exactly one member; trailing arguments were not used; draft kept"
                        .to_string(),
                )
            } else {
                Submission::Drawer(Drawer::MemberLogs(MemberId::new(member)))
            }
        }
        "help" if arg.is_empty() => Submission::Help,
        "team" | "runs" | "logs" | "diff" | "new" | "clear" | "resume" | "exit" | "retry"
        | "approve" | "reject" | "help" => {
            Submission::Invalid(format!("/{cmd} does not accept arguments; draft kept"))
        }
        _ => Submission::Help,
    }
}

/// Parse a slash command aimed at one explicit member. Returning `None` means
/// `body` is ordinary prompt text, not a slash command. Both `@member …` and
/// `/ask member …` use this path so the latter cannot bypass the native-session
/// and discovered-skill safeguards.
fn parse_targeted_slash(member: &str, body: &str) -> Option<Submission> {
    if !body.trim_start().starts_with('/') {
        return None;
    }
    if let Some(rest) = targeted_command_rest(body, "attach") {
        return Some(match (member, rest.is_empty()) {
            ("all", _) => {
                Submission::Invalid("/attach needs one member; use @member /attach".to_string())
            }
            (_, true) => Submission::Attach {
                member: MemberId::new(member),
            },
            _ => Submission::Invalid("/attach does not accept arguments; draft kept".to_string()),
        });
    }
    Some(if member == "all" {
        Submission::Invalid(
            "slash commands need one member; use @member /<discovered-skill> or /attach <member> (draft kept)"
                .to_string(),
        )
    } else {
        Submission::TargetedSlash {
            member: MemberId::new(member),
            body: body.to_string(),
        }
    })
}

fn targeted_command_rest<'a>(body: &'a str, command: &str) -> Option<&'a str> {
    let rest = body.strip_prefix('/')?.strip_prefix(command)?;
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None;
    }
    Some(rest.trim())
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
            let (run_id, number_text, extra) = if let Some(run_id) = parse_run_id(first) {
                let (number, extra) = split_first_word(rest_after_first);
                (Some(run_id), number, extra)
            } else {
                let (number, extra) = split_first_word(rest);
                (None, number, extra)
            };
            let Ok(step) = number_text.parse::<u32>() else {
                return Submission::Help;
            };
            if step == 0 {
                return Submission::Help;
            }
            if !extra.is_empty() {
                return Submission::Invalid(format!(
                    "/step {action} does not accept trailing arguments; draft kept"
                ));
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
            let (run_id, number_text, extra) = if let Some(run_id) = parse_run_id(first) {
                let (number, extra) = split_first_word(rest_after_first);
                (Some(run_id), number, extra)
            } else {
                let (number, extra) = split_first_word(rest);
                (None, number, extra)
            };
            let Ok(step) = number_text.parse::<u32>() else {
                return Submission::Help;
            };
            if step == 0 {
                return Submission::Help;
            }
            if !extra.is_empty() {
                return Submission::Invalid(format!(
                    "/step {action} does not accept trailing arguments; draft kept"
                ));
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
    fn target_only_accepts_member_or_all_without_a_body() {
        assert_eq!(
            parse_target_only("@builder"),
            Some(MessageTarget::Member(MemberId::new("builder")))
        );
        assert_eq!(parse_target_only("@all"), Some(MessageTarget::All));
        assert_eq!(
            parse_target_only("/ask reviewer"),
            Some(MessageTarget::Member(MemberId::new("reviewer")))
        );
        assert_eq!(parse_target_only("/all"), Some(MessageTarget::All));
        assert_eq!(parse_target_only("@builder look"), None);
        assert_eq!(parse_target_only(""), None);
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
        assert_eq!(parse("/team "), Submission::Drawer(Drawer::Team));
        assert_eq!(parse("/diff"), Submission::Drawer(Drawer::Diff));
        assert_eq!(parse("/exit"), Submission::Exit);
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
            "/new extra",
            "/clear extra",
            "/resume extra",
            "/exit extra",
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
    fn fixed_arity_commands_reject_unused_trailing_text() {
        for command in [
            "/focus reviewer accidental",
            "/step remove 2 accidental",
            "/step remove run-12 2 accidental",
            "/step delete 2 accidental",
            "/step unassign 3 accidental",
            "/step clear-owner run-12 3 accidental",
        ] {
            assert!(
                matches!(parse(command), Submission::Invalid(message) if message.contains("trailing")),
                "{command} must not silently discard input"
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
    fn model_is_not_a_composer_control() {
        assert_eq!(parse("/model"), Submission::Help);
        assert_eq!(parse("/model builder gpt-5.6-sol"), Submission::Help);
        assert_eq!(
            parse("@builder /model gpt-5.6-sol"),
            Submission::TargetedSlash {
                member: MemberId::new("builder"),
                body: "/model gpt-5.6-sol".to_string(),
            }
        );
    }

    #[test]
    fn targeted_slashes_are_resolved_before_reaching_a_backend_prompt() {
        assert_eq!(
            parse("@builder /attach"),
            Submission::Attach {
                member: MemberId::new("builder"),
            }
        );
        assert_eq!(
            parse("/attach builder"),
            Submission::Attach {
                member: MemberId::new("builder"),
            }
        );
        assert_eq!(
            parse("@builder /unrecognized with args"),
            Submission::TargetedSlash {
                member: MemberId::new("builder"),
                body: "/unrecognized with args".to_string(),
            }
        );
        assert_eq!(
            parse("/ask builder /fast"),
            Submission::TargetedSlash {
                member: MemberId::new("builder"),
                body: "/fast".to_string(),
            }
        );
        assert_eq!(
            parse("/ask builder /attach"),
            Submission::Attach {
                member: MemberId::new("builder"),
            }
        );
        for input in ["@all /attach", "/ask all /fast", "/all /fast"] {
            assert!(
                matches!(parse(input), Submission::Invalid(message) if message.contains("one member")),
                "{input} must not broadcast a native-looking slash command"
            );
        }
    }

    #[test]
    fn attach_rejects_missing_or_extra_arguments() {
        assert!(matches!(parse("/attach"), Submission::Help));
        assert!(matches!(
            parse("/attach builder extra"),
            Submission::Invalid(_)
        ));
        assert!(matches!(
            parse("@builder /attach extra"),
            Submission::Invalid(_)
        ));
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
    fn new_and_clear_both_start_a_fresh_session() {
        assert_eq!(parse("/new"), Submission::Runtime(UiCommand::NewSession));
        assert_eq!(parse("/clear"), Submission::Runtime(UiCommand::NewSession));
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
        assert_eq!(parse("/mode"), Submission::Drawer(Drawer::Mode));
        assert_eq!(parse("/mode review fix parser"), Submission::Help);
    }

    #[test]
    fn removed_skills_command_falls_back_to_help() {
        assert_eq!(parse("/skills"), Submission::Help);
        assert_eq!(parse("/skill"), Submission::Help);
    }

    #[test]
    fn removed_abort_command_falls_back_to_help() {
        assert_eq!(parse("/abort"), Submission::Help);
        assert_eq!(parse("/abort extra"), Submission::Help);
    }
}
