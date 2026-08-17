//! Export/sync Asterline chat history into Claude Code's native session storage
//! (`~/.claude/projects/<munged-cwd>/<session-id>.jsonl`) so that `claude --resume` or `claude -r`
//! in the project workspace directly displays and resumes the conversation.
//!
//! Disabled by default. Can be enabled by setting `ASTERLINE_SYNC_CLAUDE_SESSION=1` in the environment
//! or by manual invocation (`/export` or `/export claude`).

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;

use crate::domain::{config, event::ChatItem};

/// Environment variable that enables auto-syncing Asterline sessions to Claude Code JSONL.
pub const SYNC_CLAUDE_SESSION_ENV: &str = "ASTERLINE_SYNC_CLAUDE_SESSION";

/// Check if Claude session syncing is enabled. Disabled by default (false).
pub fn is_sync_enabled() -> bool {
    std::env::var(SYNC_CLAUDE_SESSION_ENV)
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Project directory for `cwd` under a Claude projects root.
pub fn projects_dir_for(root: &Path, cwd: &str) -> PathBuf {
    root.join(munge_cwd(cwd))
}

/// Replace every character that is not `[A-Za-z0-9]` with `-`.
pub fn munge_cwd(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// The platform user profile's `.claude/projects` directory.
pub fn default_projects_root() -> Option<PathBuf> {
    config::user_home_dir().map(|home| home.join(".claude").join("projects"))
}

/// Ensure the session ID string is a valid RFC 4122 UUID so Claude's scanner accepts the filename.
pub fn canonical_claude_uuid(session_id: &str) -> String {
    if uuid::Uuid::parse_str(session_id).is_ok() {
        session_id.to_string()
    } else {
        uuid_deterministic(0, session_id)
    }
}

/// Export chat items into Claude Code's native JSONL format under the project's Claude storage.
pub fn export_chat_items_to_claude_jsonl(
    workspace: &Path,
    session_id: &str,
    items: &[ChatItem],
) -> io::Result<PathBuf> {
    let Some(root) = default_projects_root() else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "cannot determine user home directory for Claude projects",
        ));
    };
    export_chat_items_to_root(&root, workspace, session_id, items)
}

/// Core implementation taking an explicit projects root (testable).
pub fn export_chat_items_to_root(
    root: &Path,
    workspace: &Path,
    session_id: &str,
    items: &[ChatItem],
) -> io::Result<PathBuf> {
    let canonical_uuid = canonical_claude_uuid(session_id);
    let cwd_str = workspace.to_string_lossy();
    let project_dir = projects_dir_for(root, &cwd_str);
    fs::create_dir_all(&project_dir)?;
    let target_file = project_dir.join(format!("{canonical_uuid}.jsonl"));

    let timestamp = iso_timestamp_now();
    let mut lines = Vec::new();

    // Standard session header lines required by Claude Code CLI
    let mode_obj = json!({
        "type": "mode",
        "mode": "normal",
        "sessionId": canonical_uuid
    });
    lines.push(serde_json::to_string(&mode_obj).unwrap_or_default());

    let perm_obj = json!({
        "type": "permission-mode",
        "permissionMode": "bypassPermissions",
        "sessionId": canonical_uuid
    });
    lines.push(serde_json::to_string(&perm_obj).unwrap_or_default());

    let mut parent_uuid: Option<String> = None;
    let mut last_prompt = String::new();
    let mut last_uuid = String::new();

    for (idx, item) in items.iter().enumerate() {
        let current_uuid = uuid_deterministic(idx, &canonical_uuid);
        match item {
            ChatItem::User { body, .. } => {
                if !body.trim().is_empty() {
                    last_prompt = body.trim().to_string();
                }
                let line_obj = json!({
                    "parentUuid": parent_uuid,
                    "isSidechain": false,
                    "type": "user",
                    "message": {
                        "role": "user",
                        "content": body
                    },
                    "uuid": current_uuid,
                    "timestamp": timestamp,
                    "permissionMode": "bypassPermissions",
                    "origin": { "kind": "human" },
                    "promptSource": "typed",
                    "userType": "external",
                    "entrypoint": "cli",
                    "cwd": cwd_str,
                    "sessionId": canonical_uuid,
                    "version": "2.1.233",
                    "gitBranch": "main"
                });
                lines.push(serde_json::to_string(&line_obj).unwrap_or_default());
                parent_uuid = Some(current_uuid.clone());
                last_uuid = current_uuid;
            }
            ChatItem::Agent { text, .. } => {
                let formatted_text = text.clone();
                let line_obj = json!({
                    "parentUuid": parent_uuid,
                    "isSidechain": false,
                    "type": "assistant",
                    "message": {
                        "model": "claude-code",
                        "role": "assistant",
                        "content": [
                            {
                                "type": "text",
                                "text": formatted_text
                            }
                        ],
                        "stop_reason": "end_turn",
                        "usage": { "input_tokens": 1, "output_tokens": 1 }
                    },
                    "uuid": current_uuid,
                    "timestamp": timestamp,
                    "userType": "external",
                    "entrypoint": "cli",
                    "cwd": cwd_str,
                    "sessionId": canonical_uuid,
                    "version": "2.1.233",
                    "gitBranch": "main"
                });
                lines.push(serde_json::to_string(&line_obj).unwrap_or_default());
                parent_uuid = Some(current_uuid.clone());
                last_uuid = current_uuid;
            }
            ChatItem::Tool {
                name,
                summary,
                detail,
                ..
            } => {
                let tool_text = format!("⚒ Tool: {name} - {summary}\n{detail}");
                let line_obj = json!({
                    "parentUuid": parent_uuid,
                    "isSidechain": false,
                    "type": "assistant",
                    "message": {
                        "model": "claude-code",
                        "role": "assistant",
                        "content": [
                            {
                                "type": "text",
                                "text": tool_text
                            }
                        ],
                        "stop_reason": "end_turn",
                        "usage": { "input_tokens": 1, "output_tokens": 1 }
                    },
                    "uuid": current_uuid,
                    "timestamp": timestamp,
                    "userType": "external",
                    "entrypoint": "cli",
                    "cwd": cwd_str,
                    "sessionId": canonical_uuid,
                    "version": "2.1.233",
                    "gitBranch": "main"
                });
                lines.push(serde_json::to_string(&line_obj).unwrap_or_default());
                parent_uuid = Some(current_uuid.clone());
                last_uuid = current_uuid;
            }
            ChatItem::Diff { files, .. } => {
                let mut diff_text = String::from("📁 Changes:\n");
                for f in files {
                    diff_text.push_str(&format!("- {} ({})\n", f.path, f.kind));
                }
                let line_obj = json!({
                    "parentUuid": parent_uuid,
                    "isSidechain": false,
                    "type": "assistant",
                    "message": {
                        "model": "claude-code",
                        "role": "assistant",
                        "content": [
                            {
                                "type": "text",
                                "text": diff_text
                            }
                        ],
                        "stop_reason": "end_turn",
                        "usage": { "input_tokens": 1, "output_tokens": 1 }
                    },
                    "uuid": current_uuid,
                    "timestamp": timestamp,
                    "userType": "external",
                    "entrypoint": "cli",
                    "cwd": cwd_str,
                    "sessionId": canonical_uuid,
                    "version": "2.1.233",
                    "gitBranch": "main"
                });
                lines.push(serde_json::to_string(&line_obj).unwrap_or_default());
                parent_uuid = Some(current_uuid.clone());
                last_uuid = current_uuid;
            }
            ChatItem::Thinking { .. }
            | ChatItem::Route { .. }
            | ChatItem::Notice { .. }
            | ChatItem::Error { .. }
            | ChatItem::Verdict { .. } => {}
        }
    }

    if !last_prompt.is_empty() && !last_uuid.is_empty() {
        let last_prompt_obj = json!({
            "type": "last-prompt",
            "lastPrompt": last_prompt,
            "leafUuid": last_uuid,
            "sessionId": canonical_uuid,
        });
        lines.push(serde_json::to_string(&last_prompt_obj).unwrap_or_default());
    }

    let mut file = File::create(&target_file)?;
    for line in lines {
        writeln!(file, "{line}")?;
    }

    Ok(target_file)
}

/// Automatically sync chat items to Claude session JSONL if the feature is enabled.
pub fn sync_if_enabled(
    workspace: &Path,
    session_id: Option<&str>,
    items: &[ChatItem],
) -> Option<PathBuf> {
    if !is_sync_enabled() {
        return None;
    }
    let session_id = session_id.filter(|s| !s.trim().is_empty())?;
    export_chat_items_to_claude_jsonl(workspace, session_id, items).ok()
}

fn uuid_deterministic(index: usize, session_id: &str) -> String {
    let hash = session_id
        .bytes()
        .fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64));
    format!(
        "{:08x}-a851-49ca-8420-{:012x}",
        index as u32,
        (index as u64).wrapping_add(hash)
    )
}

fn iso_timestamp_now() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let millis = now.subsec_millis();
    // Format approximate ISO 8601 string: 2026-08-17T12:00:00.000Z
    format!("{secs}.{millis:03}Z")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::event::ChatItem;
    use crate::domain::team::{BackendKind, MemberId};

    #[test]
    fn sync_is_disabled_by_default() {
        unsafe {
            std::env::remove_var(SYNC_CLAUDE_SESSION_ENV);
        }
        assert!(!is_sync_enabled());
    }

    #[test]
    fn munge_cwd_replaces_non_alphanumeric() {
        assert_eq!(
            munge_cwd("/Users/pys/project/Asterline"),
            "-Users-pys-project-Asterline"
        );
    }

    #[test]
    fn canonical_claude_uuid_ensures_valid_uuid() {
        let valid = "6f4cc9b9-f603-4f1f-8973-141a816b99f0";
        assert_eq!(canonical_claude_uuid(valid), valid);

        let custom = "sess-1234";
        let canonical = canonical_claude_uuid(custom);
        assert!(uuid::Uuid::parse_str(&canonical).is_ok());
    }

    #[test]
    fn exports_chat_items_to_valid_claude_jsonl() {
        let temp =
            std::env::temp_dir().join(format!("ast-claude-export-test-{}", std::process::id()));
        let workspace = PathBuf::from("/Users/test/my-project");
        let session_id = "6f4cc9b9-f603-4f1f-8973-141a816b99f0";

        let items = vec![
            ChatItem::User {
                body: "Hello from Asterline".to_string(),
                targets: vec![MemberId::new("builder")],
                interrupted: vec![],
            },
            ChatItem::Agent {
                member: MemberId::new("builder"),
                display_name: "Builder".to_string(),
                backend: BackendKind::Claude,
                text: "Here is the plan for the project.".to_string(),
            },
        ];

        let path = export_chat_items_to_root(&temp, &workspace, session_id, &items).unwrap();
        assert!(path.exists());
        assert!(
            path.to_string_lossy()
                .ends_with("6f4cc9b9-f603-4f1f-8973-141a816b99f0.jsonl")
        );

        let content = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert!(lines.len() >= 5); // mode, permission-mode, user, assistant, last-prompt

        let mode_val: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(mode_val["type"], "mode");

        let user_val: serde_json::Value = serde_json::from_str(lines[2]).unwrap();
        assert_eq!(user_val["type"], "user");
        assert_eq!(user_val["sessionId"], session_id);
        assert_eq!(user_val["origin"]["kind"], "human");
        assert_eq!(user_val["promptSource"], "typed");

        let agent_val: serde_json::Value = serde_json::from_str(lines[3]).unwrap();
        assert_eq!(agent_val["type"], "assistant");

        let last_val: serde_json::Value = serde_json::from_str(lines[4]).unwrap();
        assert_eq!(last_val["type"], "last-prompt");
        assert_eq!(last_val["lastPrompt"], "Hello from Asterline");

        // Clean up
        let _ = fs::remove_dir_all(&temp);
    }
}
