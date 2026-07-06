//! Lenient parser for agent envelopes.
//!
//! Agent output may contain zero or more envelope lines interleaved with normal
//! text. Parsing never loses content: a malformed envelope is reported as a
//! warning and its original line is kept in the visible text.
//!
//! ```text
//! @@team_message {"to":"builder","body":"please implement the migration"}
//! @@team_message {"to":["builder","reviewer"],"body":"implement and review"}
//! @@team_message {"to":"all","body":"let's agree on the data model first"}
//! @@team_member {"display_name":"QA","backend":"codex","role":"tests"}
//! @@workflow_step {"action":"add","owner":"builder","title":"Write parser tests"}
//! @@workflow_step {"action":"done","step":1,"note":"Covered lexer edge cases"}
//! @@workflow_step {"action":"assign","step":2,"owner":"reviewer"}
//! ```

use std::path::PathBuf;

use serde::Deserialize;

use crate::domain::event::{RouteTo, TeamMessage, WorkflowStepRequest, WorkflowStepStatus};
use crate::domain::team::{
    BackendKind, Effort, MemberId, PermissionMode, SandboxPolicy, SessionPolicy, TeamMember,
    derived_member_id,
};

const TEAM_MESSAGE_PREFIX: &str = "@@team_message";
const TEAM_MEMBER_PREFIX: &str = "@@team_member";
const WORKFLOW_STEP_PREFIX: &str = "@@workflow_step";

/// The result of scanning one agent message for envelopes.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ParsedAgentOutput {
    /// Text with successfully-parsed envelope lines removed.
    pub visible_text: String,
    /// Envelopes parsed from the output, in order.
    pub messages: Vec<TeamMessage>,
    /// New teammate requests parsed from the output, in order.
    pub members: Vec<TeamMember>,
    /// Workflow checklist mutations parsed from the output, in order.
    pub workflow_steps: Vec<WorkflowStepRequest>,
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
struct TeamMemberRaw {
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    backend: BackendKind,
    role: String,
    #[serde(default)]
    cwd: Option<PathBuf>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    system_prompt: Option<String>,
    #[serde(default)]
    sandbox: SandboxPolicy,
    #[serde(default)]
    permission_mode: Option<PermissionMode>,
    #[serde(default)]
    allowed_tools: Vec<String>,
    #[serde(default)]
    session_policy: SessionPolicy,
    #[serde(default)]
    effort: Option<Effort>,
}

#[derive(Deserialize)]
struct WorkflowStepRaw {
    action: String,
    #[serde(default)]
    step: Option<u32>,
    #[serde(default)]
    owner: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    note: Option<String>,
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
    let mut members = Vec::new();
    let mut workflow_steps = Vec::new();
    let mut warnings = Vec::new();

    for line in text.lines() {
        match envelope_payload(line, TEAM_MESSAGE_PREFIX) {
            Some(payload) => match parse_team_message(payload) {
                Ok(message) => messages.push(message),
                Err(warning) => {
                    warnings.push(warning);
                    kept_lines.push(line);
                }
            },
            None => match envelope_payload(line, TEAM_MEMBER_PREFIX) {
                Some(payload) => match parse_team_member(payload) {
                    Ok(member) => members.push(member),
                    Err(warning) => {
                        warnings.push(warning);
                        kept_lines.push(line);
                    }
                },
                None => match envelope_payload(line, WORKFLOW_STEP_PREFIX) {
                    Some(payload) => match parse_workflow_step(payload) {
                        Ok(request) => workflow_steps.push(request),
                        Err(warning) => {
                            warnings.push(warning);
                            kept_lines.push(line);
                        }
                    },
                    None => kept_lines.push(line),
                },
            },
        }
    }

    ParsedAgentOutput {
        visible_text: kept_lines.join("\n").trim().to_string(),
        messages,
        members,
        workflow_steps,
        warnings,
    }
}

/// If `line` is an envelope, return the JSON payload after the prefix.
fn envelope_payload<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    let rest = line.trim_start().strip_prefix(prefix)?;
    if rest.is_empty() || rest.starts_with(char::is_whitespace) || rest.starts_with('{') {
        Some(rest.trim())
    } else {
        None
    }
}

fn parse_team_message(payload: &str) -> Result<TeamMessage, String> {
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

fn parse_team_member(payload: &str) -> Result<TeamMember, String> {
    let raw: TeamMemberRaw = serde_json::from_str(payload)
        .map_err(|err| format!("invalid @@team_member envelope: {err}"))?;
    if let Some(action) = raw.action.as_deref()
        && action != "add"
    {
        return Err(format!(
            "unsupported @@team_member action: {action} (only add is supported)"
        ));
    }

    let role = raw.role.trim();
    if role.is_empty() {
        return Err("@@team_member role must be non-empty".to_string());
    }
    let display_name = raw
        .display_name
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty());
    let id = match raw.id {
        Some(id) => {
            let id = id.trim();
            if id.is_empty() || id.chars().any(char::is_whitespace) {
                return Err("@@team_member id must be a non-empty token".to_string());
            }
            MemberId::new(id)
        }
        None => {
            let Some(display_name) = display_name.as_deref() else {
                return Err("@@team_member needs id or display_name".to_string());
            };
            derived_member_id(display_name, raw.backend.as_str())
        }
    };
    let display_name = display_name.unwrap_or_else(|| id.to_string());

    Ok(TeamMember {
        id,
        display_name,
        backend: raw.backend,
        role: role.to_string(),
        cwd: raw.cwd,
        model: raw.model,
        system_prompt: raw.system_prompt,
        sandbox: raw.sandbox,
        permission_mode: raw.permission_mode,
        allowed_tools: raw.allowed_tools,
        session_policy: raw.session_policy,
        effort: raw.effort,
    })
}

fn parse_workflow_step(payload: &str) -> Result<WorkflowStepRequest, String> {
    let raw: WorkflowStepRaw = serde_json::from_str(payload)
        .map_err(|err| format!("invalid @@workflow_step envelope: {err}"))?;
    match raw.action.as_str() {
        "add" => {
            let title = raw
                .title
                .map(|title| title.trim().to_string())
                .filter(|title| !title.is_empty())
                .ok_or_else(|| "@@workflow_step add needs title".to_string())?;
            let owner = parse_optional_step_owner(raw.owner)?;
            Ok(WorkflowStepRequest::Add { owner, title })
        }
        "todo" | "doing" | "done" | "block" | "blocked" => {
            let step = raw
                .step
                .filter(|step| *step > 0)
                .ok_or_else(|| "@@workflow_step update needs positive step".to_string())?;
            let status = match raw.action.as_str() {
                "todo" => WorkflowStepStatus::Todo,
                "doing" => WorkflowStepStatus::Doing,
                "done" => WorkflowStepStatus::Done,
                "block" | "blocked" => WorkflowStepStatus::Blocked,
                _ => unreachable!(),
            };
            let note = raw
                .note
                .map(|note| note.trim().to_string())
                .filter(|note| !note.is_empty());
            Ok(WorkflowStepRequest::Update { step, status, note })
        }
        "rename" | "edit" => {
            let step = raw
                .step
                .filter(|step| *step > 0)
                .ok_or_else(|| "@@workflow_step rename needs positive step".to_string())?;
            let title = raw
                .title
                .map(|title| title.trim().to_string())
                .filter(|title| !title.is_empty())
                .ok_or_else(|| "@@workflow_step rename needs title".to_string())?;
            Ok(WorkflowStepRequest::Rename { step, title })
        }
        "remove" | "delete" | "drop" => {
            let step = raw
                .step
                .filter(|step| *step > 0)
                .ok_or_else(|| "@@workflow_step remove needs positive step".to_string())?;
            Ok(WorkflowStepRequest::Remove { step })
        }
        "assign" => {
            let step = raw
                .step
                .filter(|step| *step > 0)
                .ok_or_else(|| "@@workflow_step assign needs positive step".to_string())?;
            let owner = parse_required_step_owner(raw.owner)?;
            Ok(WorkflowStepRequest::Assign {
                step,
                owner: Some(owner),
            })
        }
        "unassign" | "clear_owner" | "clear-owner" => {
            let step = raw
                .step
                .filter(|step| *step > 0)
                .ok_or_else(|| "@@workflow_step unassign needs positive step".to_string())?;
            Ok(WorkflowStepRequest::Assign { step, owner: None })
        }
        action => Err(format!(
            "unsupported @@workflow_step action: {action} (use add, todo, doing, done, block, rename, remove, assign, or unassign)"
        )),
    }
}

fn parse_optional_step_owner(owner: Option<String>) -> Result<Option<MemberId>, String> {
    owner.map(parse_step_owner).transpose()
}

fn parse_required_step_owner(owner: Option<String>) -> Result<MemberId, String> {
    let Some(owner) = owner else {
        return Err("@@workflow_step assign needs owner".to_string());
    };
    parse_step_owner(owner)
}

fn parse_step_owner(owner: String) -> Result<MemberId, String> {
    let owner = owner.trim().trim_start_matches('@');
    if owner.is_empty() || owner.chars().any(char::is_whitespace) {
        return Err("@@workflow_step owner must be a non-empty member token".to_string());
    }
    Ok(MemberId::new(owner))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_string_target() {
        let parsed = parse_agent_output(r#"@@team_message {"to":"builder","body":"do the thing"}"#);

        assert_eq!(
            parsed.messages,
            vec![TeamMessage {
                to: vec![RouteTo::Member("builder".to_string())],
                kind: None,
                body: "do the thing".to_string(),
            }]
        );
        assert!(parsed.members.is_empty());
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
        assert_eq!(
            parsed.visible_text,
            "just some normal output\nwith two lines"
        );
    }

    #[test]
    fn envelope_without_space_after_prefix_still_parses() {
        let parsed = parse_agent_output(r#"@@team_message{"to":"a","body":"hi"}"#);
        assert_eq!(parsed.messages.len(), 1);
    }

    #[test]
    fn parses_team_member_add_request() {
        let parsed = parse_agent_output(
            r#"@@team_member {"id":"qa","display_name":"QA","backend":"codex","role":"tests","model":"gpt-5-codex","effort":"high"}"#,
        );

        assert!(parsed.messages.is_empty());
        assert_eq!(parsed.members.len(), 1);
        assert_eq!(parsed.members[0].id, MemberId::new("qa"));
        assert_eq!(parsed.members[0].display_name, "QA");
        assert_eq!(parsed.members[0].backend, BackendKind::Codex);
        assert_eq!(parsed.members[0].role, "tests");
        assert_eq!(parsed.members[0].model.as_deref(), Some("gpt-5-codex"));
        assert_eq!(parsed.members[0].effort, Some(Effort::High));
        assert_eq!(parsed.visible_text, "");
    }

    #[test]
    fn parses_workflow_step_add_request() {
        let parsed =
            parse_agent_output(r#"@@workflow_step {"action":"add","title":"Write parser tests"}"#);

        assert!(parsed.messages.is_empty());
        assert!(parsed.members.is_empty());
        assert_eq!(
            parsed.workflow_steps,
            vec![WorkflowStepRequest::Add {
                owner: None,
                title: "Write parser tests".to_string()
            }]
        );
        assert_eq!(parsed.visible_text, "");
        assert!(parsed.warnings.is_empty());
    }

    #[test]
    fn parses_workflow_step_owner_requests() {
        let parsed = parse_agent_output(
            "@@workflow_step {\"action\":\"add\",\"owner\":\"builder\",\"title\":\"Write parser tests\"}\n@@workflow_step {\"action\":\"assign\",\"step\":2,\"owner\":\"@reviewer\"}\n@@workflow_step {\"action\":\"unassign\",\"step\":3}",
        );

        assert_eq!(
            parsed.workflow_steps,
            vec![
                WorkflowStepRequest::Add {
                    owner: Some(MemberId::new("builder")),
                    title: "Write parser tests".to_string()
                },
                WorkflowStepRequest::Assign {
                    step: 2,
                    owner: Some(MemberId::new("reviewer"))
                },
                WorkflowStepRequest::Assign {
                    step: 3,
                    owner: None
                }
            ]
        );
        assert!(parsed.warnings.is_empty());
    }

    #[test]
    fn parses_workflow_step_update_request() {
        let parsed = parse_agent_output(
            r#"@@workflow_step {"action":"done","step":2,"note":"Covered edge cases"}"#,
        );

        assert_eq!(
            parsed.workflow_steps,
            vec![WorkflowStepRequest::Update {
                step: 2,
                status: WorkflowStepStatus::Done,
                note: Some("Covered edge cases".to_string())
            }]
        );
        assert!(parsed.warnings.is_empty());
    }

    #[test]
    fn parses_workflow_step_rename_and_remove_requests() {
        let parsed = parse_agent_output(
            "@@workflow_step {\"action\":\"rename\",\"step\":2,\"title\":\"Document setup\"}\n@@workflow_step {\"action\":\"remove\",\"step\":3}",
        );

        assert_eq!(
            parsed.workflow_steps,
            vec![
                WorkflowStepRequest::Rename {
                    step: 2,
                    title: "Document setup".to_string()
                },
                WorkflowStepRequest::Remove { step: 3 }
            ]
        );
        assert!(parsed.warnings.is_empty());
    }

    #[test]
    fn malformed_workflow_step_warns_and_keeps_line() {
        let parsed = parse_agent_output(r#"@@workflow_step {"action":"done","step":0}"#);

        assert!(parsed.workflow_steps.is_empty());
        assert_eq!(parsed.warnings.len(), 1);
        assert!(parsed.visible_text.contains("@@workflow_step"));
    }

    #[test]
    fn team_member_defaults_display_name_to_id() {
        let parsed =
            parse_agent_output(r#"@@team_member {"id":"qa","backend":"agy","role":"research"}"#);

        assert_eq!(parsed.members[0].display_name, "qa");
        assert_eq!(parsed.members[0].backend, BackendKind::Agy);
    }

    #[test]
    fn team_member_derives_id_from_display_name() {
        let parsed = parse_agent_output(
            r#"@@team_member {"display_name":"QA Lead","backend":"codex","role":"tests"}"#,
        );

        assert!(parsed.warnings.is_empty());
        assert_eq!(parsed.members[0].id, MemberId::new("qa-lead"));
        assert_eq!(parsed.members[0].display_name, "QA Lead");
    }

    #[test]
    fn malformed_team_member_warns_and_keeps_line() {
        let parsed =
            parse_agent_output(r#"@@team_member {"id":"qa","backend":"bogus","role":"x"}"#);

        assert!(parsed.members.is_empty());
        assert_eq!(parsed.warnings.len(), 1);
        assert!(parsed.visible_text.contains("@@team_member"));
    }

    #[test]
    fn unsupported_team_member_action_warns_and_keeps_line() {
        let parsed = parse_agent_output(
            r#"@@team_member {"action":"delete","id":"qa","backend":"codex","role":"x"}"#,
        );

        assert!(parsed.members.is_empty());
        assert_eq!(parsed.warnings.len(), 1);
        assert!(parsed.visible_text.contains("@@team_member"));
    }
}
