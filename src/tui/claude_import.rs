//! Import messages from a Claude Code session transcript after an interactive
//! attach.
//!
//! Claude Code stores sessions at
//! `~/.claude/projects/<munged-cwd>/<session-id>.jsonl`, one JSON object per
//! line. When the user attaches (`claude --resume <id>`), chats, and exits,
//! new turns land in that file — or Claude may fork into a new session id whose
//! file replays the prior history plus the new turns. The original transcript
//! uses a row-count delta; a fork must prove lineage by copying a snapshotted
//! message UUID before any of its new rows are imported.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::Value;

use crate::domain::{config, event::ImportedMessage};
use crate::tui::attach::AttachOutcome;
use crate::tui::import_io;

/// Clock-skew grace: import rows whose timestamp is up to this much earlier
/// than the attach start.
const CLOCK_SKEW: Duration = Duration::from_secs(2);
const MAX_CANDIDATE_FILES: usize = 10_000;
const MAX_CANDIDATE_SCAN_ENTRIES: usize = 50_000;

/// A snapshot taken before launching the interactive session, used to import
/// only the messages added while attached.
pub struct ClaudeSnapshot {
    /// Workspace cwd used to locate `~/.claude/projects/<munged-cwd>/`.
    cwd: String,
    /// Session file located up front (when the session id is known).
    path: Option<PathBuf>,
    /// Message rows already present in `path` before the attach.
    before: usize,
    /// UUID of the last importable row in the original session. A copied UUID
    /// proves that another transcript is a fork instead of an unrelated
    /// concurrent session in the same project directory.
    lineage_anchor: Option<String>,
    /// When the attach started, to spot newly written fork session files.
    started: SystemTime,
}

/// Load every importable message from a bound Claude session file.
pub(crate) fn messages_for_session(session_id: &str, cwd: &str) -> Vec<ImportedMessage> {
    let Some(root) = default_projects_root() else {
        return Vec::new();
    };
    let Some(path) = claude_session_path(&projects_dir_for(&root, cwd), session_id) else {
        return Vec::new();
    };
    if !path.is_file() {
        return Vec::new();
    }
    let mut items = Vec::new();
    let mut retained_bytes = 0_usize;
    import_io::for_each_json_value(&path, |value| {
        let Some(message) = classify_row(&value) else {
            return true;
        };
        import_io::push_imported_bounded(&mut items, &mut retained_bytes, message)
    });
    items
}

/// Snapshot the Claude session for `session_id` (if any) before attaching.
pub fn snapshot(session_id: Option<&str>, cwd: &str) -> ClaudeSnapshot {
    match default_projects_root() {
        Some(root) => snapshot_with_root(&root, session_id, cwd),
        None => ClaudeSnapshot {
            cwd: cwd.to_string(),
            path: None,
            before: 0,
            lineage_anchor: None,
            started: SystemTime::now(),
        },
    }
}

/// After the attach exits, return the messages added during it.
pub fn imported_since(snapshot: ClaudeSnapshot) -> Vec<ImportedMessage> {
    imported_attach_since(snapshot).items
}

/// Return both the messages added while attached and the session identity when
/// the transcript itself proves it. Fresh sessions are safe when their caller
/// supplied Claude with a session UUID; unnamed sessions are never guessed
/// from files that merely appeared beside the project transcript.
pub(crate) fn imported_attach_since(snapshot: ClaudeSnapshot) -> AttachOutcome {
    let Some(root) = default_projects_root() else {
        return AttachOutcome::default();
    };
    imported_attach_since_with_root(snapshot, &root)
}

/// The platform user profile's `.claude/projects` directory (may not exist).
fn default_projects_root() -> Option<PathBuf> {
    config::user_home_dir().map(|home| home.join(".claude").join("projects"))
}

/// Project directory for `cwd` under a Claude projects root.
///
/// `root` is typically `$HOME/.claude/projects`; tests pass a fixture root.
fn projects_dir_for(root: &Path, cwd: &str) -> PathBuf {
    root.join(munge_cwd(cwd))
}

/// Replace every character that is not `[A-Za-z0-9]` with `-`.
fn munge_cwd(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn snapshot_with_root(root: &Path, session_id: Option<&str>, cwd: &str) -> ClaudeSnapshot {
    let project = projects_dir_for(root, cwd);
    let path = session_id.and_then(|id| claude_session_path(&project, id));
    let (before, lineage_anchor) = path
        .as_deref()
        .map(classified_snapshot)
        .unwrap_or((0, None));
    ClaudeSnapshot {
        cwd: cwd.to_string(),
        path,
        before,
        lineage_anchor,
        started: SystemTime::now(),
    }
}

/// Claude session ids are used as a filename suffix. Keep the lookup to the
/// normal UUID-like identifiers emitted by the CLI; never let a persisted or
/// streamed id introduce a path component on either Unix or Windows.
fn claude_session_path(project: &Path, session_id: &str) -> Option<PathBuf> {
    let valid = !session_id.is_empty()
        && session_id.len() <= 256
        && session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    valid.then(|| project.join(format!("{session_id}.jsonl")))
}

#[cfg(test)]
fn imported_since_with_root(snapshot: ClaudeSnapshot, root: &Path) -> Vec<ImportedMessage> {
    imported_attach_since_with_root(snapshot, root).items
}

fn imported_attach_since_with_root(snapshot: ClaudeSnapshot, root: &Path) -> AttachOutcome {
    let project = projects_dir_for(root, &snapshot.cwd);
    if let Some(path) = snapshot.path.as_deref() {
        let original = import_original_delta(path, snapshot.before);
        if original.saw_delta {
            return AttachOutcome {
                items: original.items,
                session: session_id_from_path(path),
                notice: None,
            };
        }
    }

    if let Some(anchor) = snapshot.lineage_anchor.as_deref() {
        let mut fork_delta = None;
        for path in candidate_files(&snapshot, &project) {
            if snapshot.path.as_ref() == Some(&path) {
                continue;
            }
            let candidate = import_fork_delta(&path, anchor);
            if !candidate.saw_delta {
                continue;
            }
            if fork_delta.is_some() {
                // Multiple descendants are ambiguous: do not guess which branch
                // the interactive CLI used.
                return AttachOutcome {
                    notice: Some(
                        "could not safely identify the forked Claude session; its attached transcript was not imported"
                            .to_string(),
                    ),
                    ..AttachOutcome::default()
                };
            }
            fork_delta = Some((candidate.items, path));
        }
        return fork_delta.map_or_else(AttachOutcome::default, |(items, path)| AttachOutcome {
            items,
            session: session_id_from_path(&path),
            notice: None,
        });
    }

    // An unnamed native session has no pre-existing id or lineage anchor. A
    // pre/post directory diff cannot prove which local CLI created a new file,
    // so do not import or claim it. Asterline's fresh Claude attach supplies
    // `--session-id`, so it takes the deterministic path above instead.
    AttachOutcome {
        notice: Some(
            "could not identify an unnamed Claude session, so its transcript was not imported"
                .to_string(),
        ),
        ..AttachOutcome::default()
    }
}

struct ImportDelta {
    items: Vec<ImportedMessage>,
    saw_delta: bool,
}

fn import_original_delta(path: &Path, before: usize) -> ImportDelta {
    let mut items = Vec::new();
    let mut retained_bytes = 0_usize;
    let mut classified_index = 0_usize;
    let mut saw_delta = false;
    import_io::for_each_json_value(path, |value| {
        let Some(message) = classify_row(&value) else {
            return true;
        };
        let index = classified_index;
        classified_index = classified_index.saturating_add(1);
        if index < before {
            return true;
        }
        saw_delta = true;
        import_io::push_imported_bounded(&mut items, &mut retained_bytes, message)
    });
    ImportDelta { items, saw_delta }
}

fn import_fork_delta(path: &Path, anchor: &str) -> ImportDelta {
    let mut items = Vec::new();
    let mut retained_bytes = 0_usize;
    let mut anchor_seen = false;
    let mut saw_delta = false;
    import_io::for_each_json_value(path, |value| {
        let Some(message) = classify_row(&value) else {
            return true;
        };
        if !anchor_seen {
            anchor_seen = value.get("uuid").and_then(Value::as_str) == Some(anchor);
            return true;
        }
        saw_delta = true;
        import_io::push_imported_bounded(&mut items, &mut retained_bytes, message)
    });
    ImportDelta { items, saw_delta }
}

fn session_id_from_path(path: &Path) -> Option<crate::domain::event::AgentSessionId> {
    let stem = path.file_stem()?.to_str()?;
    import_io::safe_session_id(stem)
}

/// Candidate session files: the snapshot path (if present) plus every `.jsonl`
/// in the project dir whose mtime is within the clock-skew grace before
/// `started`. Original path first, then others sorted by mtime ascending.
/// Deduplicated.
fn candidate_files(snapshot: &ClaudeSnapshot, project_dir: &Path) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut candidates = Vec::new();

    if let Some(ref path) = snapshot.path
        && path.is_file()
        && seen.insert(path.clone())
    {
        candidates.push(path.clone());
    }

    let candidate_threshold = snapshot
        .started
        .checked_sub(CLOCK_SKEW)
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let mut others: Vec<(SystemTime, PathBuf)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(project_dir) {
        for entry in entries.flatten().take(MAX_CANDIDATE_SCAN_ENTRIES) {
            if seen.len() >= MAX_CANDIDATE_FILES {
                break;
            }
            let path = entry.path();
            if !entry.file_type().is_ok_and(|kind| kind.is_file())
                || path
                    .extension()
                    .is_none_or(|e| !e.to_string_lossy().eq_ignore_ascii_case("jsonl"))
            {
                continue;
            }
            let Some(mtime) = modified(&path) else {
                continue;
            };
            if mtime < candidate_threshold {
                continue;
            }
            if seen.insert(path.clone()) {
                others.push((mtime, path));
            }
        }
    }
    others.sort_by_key(|(m, _)| *m);
    candidates.extend(others.into_iter().map(|(_, p)| p));
    candidates
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

/// One classified chat row (test inspection only).
#[cfg(test)]
struct ClassifiedRow {
    msg: ImportedMessage,
}

fn classified_snapshot(path: &Path) -> (usize, Option<String>) {
    let mut count = 0_usize;
    let mut lineage_anchor = None;
    import_io::for_each_json_value(path, |value| {
        if classify_row(&value).is_some() {
            count = count.saturating_add(1);
            if let Some(uuid) = value.get("uuid").and_then(Value::as_str) {
                lineage_anchor = Some(uuid.to_string());
            }
        }
        true
    });
    (count, lineage_anchor)
}

#[cfg(test)]
fn parse_classified(path: &Path) -> Vec<ClassifiedRow> {
    let mut out = Vec::new();
    import_io::for_each_json_value(path, |value| {
        let Some(msg) = classify_row(&value) else {
            return true;
        };
        out.push(ClassifiedRow { msg });
        true
    });
    out
}

/// Classify a single JSONL row into an importable chat message, or `None` if
/// it is meta / sidechain / empty / injected.
fn classify_row(value: &Value) -> Option<ImportedMessage> {
    if value.get("isSidechain").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let row_type = value.get("type").and_then(Value::as_str)?;
    match row_type {
        "user" => {
            let text = import_io::imported_text(user_text(value.get("message")?.get("content")?)?)?;
            if text.is_empty() || is_injected_user_meta(&text) {
                return None;
            }
            Some(ImportedMessage {
                from_user: true,
                text,
            })
        }
        "assistant" => {
            let text =
                import_io::imported_text(assistant_text(value.get("message")?.get("content")?)?)?;
            if text.is_empty() {
                return None;
            }
            Some(ImportedMessage {
                from_user: false,
                text,
            })
        }
        _ => None,
    }
}

fn is_injected_user_meta(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with("<command-name>")
        || t.starts_with("<local-command-stdout")
        || t.starts_with("Caveat:")
}

/// User content: string as-is, or array of blocks (skip row if any `tool_result`;
/// otherwise concatenate `text` blocks).
fn user_text(content: &Value) -> Option<String> {
    match content {
        Value::String(s) => Some(s.trim().to_string()),
        Value::Array(blocks) => {
            if blocks
                .iter()
                .any(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
            {
                return None;
            }
            let text = concat_text_blocks(blocks);
            Some(text)
        }
        _ => None,
    }
}

/// Assistant content: concatenate `text` blocks (ignore `thinking` / `tool_use`).
fn assistant_text(content: &Value) -> Option<String> {
    match content {
        Value::String(s) => Some(s.trim().to_string()),
        Value::Array(blocks) => Some(concat_text_blocks(blocks)),
        _ => None,
    }
}

fn concat_text_blocks(blocks: &[Value]) -> String {
    let mut parts = Vec::new();
    for block in blocks {
        if block.get("type").and_then(Value::as_str) != Some("text") {
            continue;
        }
        if let Some(text) = block.get("text").and_then(Value::as_str) {
            parts.push(text.to_string());
        }
    }
    parts.join("\n").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ast-claude-import-{}-{}-{}",
            name,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_session(root: &Path, cwd: &str, session_id: &str, lines: &[&str]) -> PathBuf {
        let dir = projects_dir_for(root, cwd);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{session_id}.jsonl"));
        std::fs::write(&path, lines.join("\n")).unwrap();
        path
    }

    fn user_line(text: &str, ts: &str) -> String {
        format!(
            r#"{{"type":"user","isSidechain":false,"timestamp":"{ts}","message":{{"role":"user","content":"{text}"}}}}"#
        )
    }

    fn assistant_line(text: &str, ts: &str) -> String {
        format!(
            r#"{{"type":"assistant","isSidechain":false,"timestamp":"{ts}","message":{{"role":"assistant","content":[{{"type":"text","text":"{text}"}}]}}}}"#
        )
    }

    fn user_line_with_uuid(text: &str, ts: &str, uuid: &str) -> String {
        format!(
            r#"{{"type":"user","uuid":"{uuid}","isSidechain":false,"timestamp":"{ts}","message":{{"role":"user","content":"{text}"}}}}"#
        )
    }

    fn assistant_line_with_uuid(text: &str, ts: &str, uuid: &str) -> String {
        format!(
            r#"{{"type":"assistant","uuid":"{uuid}","isSidechain":false,"timestamp":"{ts}","message":{{"role":"assistant","content":[{{"type":"text","text":"{text}"}}]}}}}"#
        )
    }

    #[test]
    fn munge_cwd_replaces_non_alnum() {
        assert_eq!(munge_cwd("/Users/x/proj.name"), "-Users-x-proj-name");
        assert_eq!(munge_cwd(r"C:\Users\Ada\repo"), "C--Users-Ada-repo");
        assert_eq!(munge_cwd("/tmp/你好"), "-tmp---");
        assert_eq!(
            munge_cwd("/Users/pys/project/git/engine/Asterline"),
            "-Users-pys-project-git-engine-Asterline"
        );
    }

    #[test]
    fn projects_dir_for_joins_munged_cwd() {
        let root = Path::new("/fake/claude/projects");
        assert_eq!(
            projects_dir_for(root, "/Users/x/proj"),
            PathBuf::from("/fake/claude/projects/-Users-x-proj")
        );
    }

    #[test]
    fn session_lookup_rejects_path_like_ids() {
        let project = Path::new("/fake/claude/projects/project");
        for invalid in [
            "",
            "../secret",
            "..\\secret",
            "/absolute",
            "has space",
            "id.jsonl",
        ] {
            assert!(
                claude_session_path(project, invalid).is_none(),
                "{invalid:?}"
            );
        }
        assert_eq!(
            claude_session_path(project, "session-abc_123"),
            Some(project.join("session-abc_123.jsonl"))
        );
    }

    #[test]
    fn classify_skips_sidechain() {
        let v: Value = serde_json::from_str(
            r#"{"type":"user","isSidechain":true,"message":{"content":"hi"}}"#,
        )
        .unwrap();
        assert!(classify_row(&v).is_none());
    }

    #[test]
    fn classify_user_string_content() {
        let v: Value = serde_json::from_str(
            r#"{"type":"user","isSidechain":false,"message":{"content":"hello world"}}"#,
        )
        .unwrap();
        assert_eq!(
            classify_row(&v),
            Some(ImportedMessage {
                from_user: true,
                text: "hello world".to_string()
            })
        );
    }

    #[test]
    fn classify_user_array_with_tool_result_skipped() {
        let v: Value = serde_json::from_str(
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"ok"},{"type":"text","text":"x"}]}}"#,
        )
        .unwrap();
        assert!(classify_row(&v).is_none());
    }

    #[test]
    fn classify_user_injected_meta_skipped() {
        for text in [
            "<command-name>/help</command-name>",
            "<local-command-stdout>out</local-command-stdout>",
            "Caveat: The messages below were generated",
        ] {
            let v = serde_json::json!({
                "type": "user",
                "message": { "content": text }
            });
            assert!(classify_row(&v).is_none(), "expected skip for {text:?}");
        }
    }

    #[test]
    fn classify_assistant_text_concat() {
        let v: Value = serde_json::from_str(
            r#"{"type":"assistant","message":{"content":[
                {"type":"thinking","thinking":"secret"},
                {"type":"text","text":"Hello "},
                {"type":"tool_use","name":"Bash"},
                {"type":"text","text":"world"}
            ]}}"#,
        )
        .unwrap();
        assert_eq!(
            classify_row(&v),
            Some(ImportedMessage {
                from_user: false,
                text: "Hello \nworld".to_string()
            })
        );
    }

    #[test]
    fn classify_meta_types_skipped() {
        for t in [
            "file-history-snapshot",
            "summary",
            "system",
            "queue-operation",
            "attachment",
        ] {
            let v = serde_json::json!({ "type": t, "message": { "content": "x" } });
            assert!(classify_row(&v).is_none(), "expected skip for type {t}");
        }
    }

    #[test]
    fn parser_drains_oversized_rows_and_skips_oversized_messages() {
        let root = fixture_root("large-row");
        let path = root.join("large.jsonl");
        let mut fixture = "x".repeat(import_io::MAX_JSONL_LINE_BYTES + 1);
        fixture.push('\n');
        fixture.push_str(&user_line(
            &"y".repeat(import_io::MAX_IMPORTED_MESSAGE_BYTES + 1),
            "2099-01-01T00:00:00Z",
        ));
        fixture.push('\n');
        fixture.push_str(&user_line("still parsed", "2099-01-01T00:00:00Z"));
        std::fs::write(&path, fixture).unwrap();

        let parsed = parse_classified(&path);

        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].msg.text, "still parsed");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn snapshot_import_round_trip_uses_original_row_count() {
        let root = fixture_root("roundtrip");
        let cwd = "/tmp/ws-claude";
        let sid = "sess-round";
        let old1 = user_line("old user", "2020-01-01T00:00:00Z");
        let old2 = assistant_line("old assistant", "2020-01-01T00:00:01Z");
        write_session(&root, cwd, sid, &[&old1, &old2]);

        let snap = snapshot_with_root(&root, Some(sid), cwd);
        assert_eq!(snap.before, 2);
        assert!(snap.path.as_ref().is_some_and(|p| p.is_file()));

        let new1 = user_line("new user", "2099-01-01T00:00:00Z");
        let new2 = assistant_line("new assistant", "2099-01-01T00:00:01Z");
        let path = snap.path.clone().unwrap();
        let mut content = std::fs::read_to_string(&path).unwrap();
        content.push('\n');
        content.push_str(&new1);
        content.push('\n');
        content.push_str(&new2);
        std::fs::write(&path, content).unwrap();

        let imported = imported_since_with_root(snap, &root);
        assert_eq!(
            imported,
            vec![
                ImportedMessage {
                    from_user: true,
                    text: "new user".to_string()
                },
                ImportedMessage {
                    from_user: false,
                    text: "new assistant".to_string()
                },
            ]
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn unnamed_fresh_attach_never_guesses_a_claude_session() {
        let root = fixture_root("fresh-attach");
        let snapshot = snapshot_with_root(&root, None, "/tmp/ws-fresh-claude");
        let imported = imported_attach_since_with_root(snapshot, &root);
        assert!(imported.items.is_empty());
        assert!(imported.session.is_none());
        assert!(imported.notice.is_some());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn supplied_fresh_session_id_imports_and_binds_without_guessing() {
        let root = fixture_root("supplied-fresh-id");
        let cwd = "/tmp/ws-supplied-fresh-id";
        let session_id = "3e2f3488-c08a-4d09-9cac-fc64f632a590";
        let snapshot = snapshot_with_root(&root, Some(session_id), cwd);
        let user = user_line("new user", "2099-06-01T00:00:00Z");
        let assistant = assistant_line("new assistant", "2099-06-01T00:00:01Z");
        write_session(&root, cwd, session_id, &[&user, &assistant]);

        let imported = imported_attach_since_with_root(snapshot, &root);
        assert_eq!(
            imported.session,
            Some(crate::domain::event::AgentSessionId(session_id.to_string()))
        );
        assert_eq!(
            imported.items,
            vec![
                ImportedMessage {
                    from_user: true,
                    text: "new user".to_string(),
                },
                ImportedMessage {
                    from_user: false,
                    text: "new assistant".to_string(),
                },
            ]
        );
        assert!(imported.notice.is_none());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn fork_session_imports_only_rows_after_lineage_anchor() {
        let root = fixture_root("fork");
        let cwd = "/tmp/ws-fork";
        let old_sid = "sess-old";
        let old1 = user_line_with_uuid("prior user", "2020-06-01T12:00:00Z", "old-user");
        let old2 =
            assistant_line_with_uuid("prior assistant", "2020-06-01T12:00:01Z", "lineage-anchor");
        write_session(&root, cwd, old_sid, &[&old1, &old2]);

        let snap = snapshot_with_root(&root, Some(old_sid), cwd);
        assert_eq!(snap.before, 2);

        // Forked session file: full replay of old history + new turns.
        let new_sid = "sess-forked";
        let new1 = user_line("while attached", "2099-06-01T12:00:00Z");
        let new2 = assistant_line("fork reply", "2099-06-01T12:00:01Z");
        write_session(&root, cwd, new_sid, &[&old1, &old2, &new1, &new2]);

        let imported = imported_since_with_root(snap, &root);
        assert_eq!(
            imported,
            vec![
                ImportedMessage {
                    from_user: true,
                    text: "while attached".to_string()
                },
                ImportedMessage {
                    from_user: false,
                    text: "fork reply".to_string()
                },
            ]
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn unrelated_concurrent_session_in_same_cwd_is_not_imported() {
        let root = fixture_root("unrelated");
        let cwd = "/tmp/ws-unrelated";
        let original =
            user_line_with_uuid("original history", "2020-06-01T12:00:00Z", "lineage-anchor");
        write_session(&root, cwd, "sess-original", &[&original]);
        let snap = snapshot_with_root(&root, Some("sess-original"), cwd);

        let unrelated = user_line_with_uuid(
            "must not cross sessions",
            "2099-06-01T12:00:00Z",
            "unrelated-message",
        );
        write_session(&root, cwd, "sess-unrelated", &[&unrelated]);

        assert!(imported_since_with_root(snap, &root).is_empty());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn fork_lineage_wins_over_unrelated_concurrent_session() {
        let root = fixture_root("fork-with-unrelated");
        let cwd = "/tmp/ws-fork-with-unrelated";
        let old1 = user_line_with_uuid("old user", "2020-06-01T12:00:00Z", "old-user");
        let anchor =
            assistant_line_with_uuid("old assistant", "2020-06-01T12:00:01Z", "lineage-anchor");
        write_session(&root, cwd, "sess-original", &[&old1, &anchor]);
        let snap = snapshot_with_root(&root, Some("sess-original"), cwd);

        let fork_new =
            user_line_with_uuid("from proven fork", "2099-06-01T12:00:00Z", "fork-message");
        write_session(&root, cwd, "sess-fork", &[&old1, &anchor, &fork_new]);
        let unrelated = user_line_with_uuid(
            "must not cross sessions",
            "2099-06-01T12:00:01Z",
            "unrelated-message",
        );
        write_session(&root, cwd, "sess-unrelated", &[&unrelated]);

        assert_eq!(
            imported_since_with_root(snap, &root),
            vec![ImportedMessage {
                from_user: true,
                text: "from proven fork".to_string(),
            }]
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn multiple_fork_descendants_are_treated_as_ambiguous() {
        let root = fixture_root("ambiguous-forks");
        let cwd = "/tmp/ws-ambiguous-forks";
        let anchor =
            user_line_with_uuid("original history", "2020-06-01T12:00:00Z", "lineage-anchor");
        write_session(&root, cwd, "sess-original", &[&anchor]);
        let snap = snapshot_with_root(&root, Some("sess-original"), cwd);

        let fork_a = user_line_with_uuid("fork a", "2099-06-01T12:00:00Z", "fork-a-message");
        let fork_b = user_line_with_uuid("fork b", "2099-06-01T12:00:01Z", "fork-b-message");
        write_session(&root, cwd, "sess-fork-a", &[&anchor, &fork_a]);
        write_session(&root, cwd, "sess-fork-b", &[&anchor, &fork_b]);

        assert!(imported_since_with_root(snap, &root).is_empty());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn count_fallback_imports_untimestamped_rows_after_before() {
        let root = fixture_root("count-fallback");
        let cwd = "/tmp/ws-count";
        let sid = "sess-count";
        let dir = projects_dir_for(&root, cwd);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{sid}.jsonl"));

        let lines = [
            r#"{"type":"user","message":{"content":"already there"}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"old reply"}]}}"#,
            r#"{"type":"user","message":{"content":"typed while attached"}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"fresh reply"}]}}"#,
        ];
        std::fs::write(&path, lines.join("\n")).unwrap();

        // before = 2 (the two pre-existing classified rows).
        let snap = ClaudeSnapshot {
            cwd: cwd.to_string(),
            path: Some(path),
            before: 2,
            lineage_anchor: None,
            started: SystemTime::now(),
        };

        let imported = imported_since_with_root(snap, &root);
        assert_eq!(
            imported,
            vec![
                ImportedMessage {
                    from_user: true,
                    text: "typed while attached".to_string()
                },
                ImportedMessage {
                    from_user: false,
                    text: "fresh reply".to_string()
                },
            ]
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn consecutive_duplicate_texts_deduped_across_candidates() {
        let root = fixture_root("dedup");
        let cwd = "/tmp/ws-dedup";
        let old_sid = "sess-a";
        let new_line = user_line("same text", "2099-01-01T00:00:00Z");
        write_session(&root, cwd, old_sid, &[]);

        let snap = snapshot_with_root(&root, Some(old_sid), cwd);
        // Append to original and also write a forked file with the same new text.
        let path = snap.path.clone().unwrap();
        std::fs::write(&path, &new_line).unwrap();
        write_session(&root, cwd, "sess-b", &[&new_line]);

        let imported = imported_since_with_root(snap, &root);
        assert_eq!(
            imported,
            vec![ImportedMessage {
                from_user: true,
                text: "same text".to_string()
            }]
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn attached_claude_import_stops_at_aggregate_item_budget() {
        let root = fixture_root("aggregate-budget");
        let cwd = "/tmp/ws-budget";
        let sid = "sess-budget";
        let lines = (0..import_io::MAX_IMPORTED_ITEMS + 25)
            .map(|index| user_line(&format!("message-{index}"), "2099-01-01T00:00:00Z"))
            .collect::<Vec<_>>();
        let refs = lines.iter().map(String::as_str).collect::<Vec<_>>();
        let path = write_session(&root, cwd, sid, &refs);
        let snapshot = ClaudeSnapshot {
            cwd: cwd.to_string(),
            path: Some(path),
            before: 0,
            lineage_anchor: None,
            started: SystemTime::UNIX_EPOCH,
        };

        let imported = imported_since_with_root(snapshot, &root);

        assert_eq!(imported.len(), import_io::MAX_IMPORTED_ITEMS);
        assert_eq!(imported.last().unwrap().text, "message-999");
        std::fs::remove_dir_all(root).ok();
    }
}
