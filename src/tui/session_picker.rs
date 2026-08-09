//! Backend session-history discovery and selection state for the Team editor.

use std::cmp::Reverse;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::Value;

use crate::domain::{config, team::BackendKind};
use crate::tui::import_io;

const MAX_METADATA_LINES: usize = 256;
const MAX_SESSION_SCAN_DEPTH: usize = 8;
const MAX_SESSION_SCAN_ENTRIES: usize = 50_000;
const MAX_SESSION_FILES: usize = 10_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionEntry {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) project: String,
    updated_at: SystemTime,
}

impl SessionEntry {
    pub(crate) fn age(&self) -> String {
        let elapsed = SystemTime::now()
            .duration_since(self.updated_at)
            .unwrap_or(Duration::ZERO);
        match elapsed.as_secs() {
            0..=59 => "now".to_string(),
            seconds @ 60..=3_599 => format!("{}m", seconds / 60),
            seconds @ 3_600..=86_399 => format!("{}h", seconds / 3_600),
            seconds => format!("{}d", seconds / 86_400),
        }
    }

    #[cfg(test)]
    pub(crate) fn fixture(id: &str, title: &str, project: &str) -> Self {
        Self {
            id: id.to_string(),
            title: title.to_string(),
            project: project.to_string(),
            updated_at: SystemTime::UNIX_EPOCH,
        }
    }
}

#[derive(Debug)]
pub(crate) struct SessionPicker {
    backend: BackendKind,
    entries: Vec<SessionEntry>,
    filtered: Vec<usize>,
    selected: usize,
    query: String,
    error: Option<String>,
}

impl SessionPicker {
    pub(crate) fn discover(backend: BackendKind, cwd: &Path) -> Self {
        let root = provider_root(backend);
        let (mut entries, error) = match root {
            Some(root) => (discover_in(backend, &root), None),
            None => (
                Vec::new(),
                Some(format!(
                    "{} session history is unavailable · Esc then e for manual id",
                    backend.as_str()
                )),
            ),
        };
        retain_current_project(&mut entries, cwd);
        entries.sort_by_key(|entry| Reverse(entry.updated_at));
        let filtered = (0..entries.len()).collect();
        Self {
            backend,
            entries,
            filtered,
            selected: 0,
            query: String::new(),
            error,
        }
    }

    pub(crate) fn backend(&self) -> BackendKind {
        self.backend
    }

    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    pub(crate) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub(crate) fn visible_len(&self) -> usize {
        self.filtered.len()
    }

    pub(crate) fn selected(&self) -> usize {
        self.selected
    }

    pub(crate) fn selected_entry(&self) -> Option<&SessionEntry> {
        self.filtered
            .get(self.selected)
            .and_then(|index| self.entries.get(*index))
    }

    pub(crate) fn window(&self, height: usize) -> (usize, Vec<&SessionEntry>) {
        let start = window_start(self.filtered.len(), self.selected, height);
        let entries = self.filtered[start..self.filtered.len().min(start + height)]
            .iter()
            .filter_map(|index| self.entries.get(*index))
            .collect();
        (start, entries)
    }

    pub(crate) fn up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub(crate) fn down(&mut self) {
        if self.selected + 1 < self.filtered.len() {
            self.selected += 1;
        }
    }

    pub(crate) fn page_up(&mut self, height: usize) {
        self.selected = self.selected.saturating_sub(height);
    }

    pub(crate) fn page_down(&mut self, height: usize) {
        self.selected = self
            .selected
            .saturating_add(height)
            .min(self.filtered.len().saturating_sub(1));
    }

    pub(crate) fn push_query(&mut self, ch: char) {
        self.query.push(ch);
        self.refilter();
    }

    pub(crate) fn pop_query(&mut self) {
        self.query.pop();
        self.refilter();
    }

    pub(crate) fn clear_query(&mut self) {
        self.query.clear();
        self.refilter();
    }

    fn refilter(&mut self) {
        let needle = self.query.to_lowercase();
        self.filtered = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                needle.is_empty()
                    || entry.id.to_lowercase().contains(&needle)
                    || entry.title.to_lowercase().contains(&needle)
                    || entry.project.to_lowercase().contains(&needle)
            })
            .map(|(index, _)| index)
            .collect();
        self.selected = self.selected.min(self.filtered.len().saturating_sub(1));
    }

    #[cfg(test)]
    pub(crate) fn from_entries(backend: BackendKind, entries: Vec<SessionEntry>) -> Self {
        let filtered = (0..entries.len()).collect();
        Self {
            backend,
            entries,
            filtered,
            selected: 0,
            query: String::new(),
            error: None,
        }
    }
}

fn retain_current_project(entries: &mut Vec<SessionEntry>, cwd: &Path) {
    entries.retain(|entry| config::paths_equivalent(Path::new(&entry.project), cwd));
}

fn window_start(len: usize, selected: usize, height: usize) -> usize {
    if height == 0 || len <= height {
        0
    } else {
        selected.saturating_sub(height / 2).min(len - height)
    }
}

fn provider_root(backend: BackendKind) -> Option<PathBuf> {
    let user_home = config::user_home_dir();
    let codex_home = config::codex_home_dir();
    provider_root_from(backend, user_home.as_deref(), codex_home.as_deref())
}

fn provider_root_from(
    backend: BackendKind,
    user_home: Option<&Path>,
    codex_home: Option<&Path>,
) -> Option<PathBuf> {
    match backend {
        BackendKind::Codex => Some(codex_home?.join("sessions")),
        BackendKind::Claude => Some(user_home?.join(".claude").join("projects")),
        BackendKind::Grok => Some(user_home?.join(".grok").join("sessions")),
        BackendKind::Agy => None,
    }
}

fn discover_in(backend: BackendKind, root: &Path) -> Vec<SessionEntry> {
    match backend {
        BackendKind::Codex => files_below(root, |path| {
            path.file_name()
                .is_some_and(|name| os_starts_and_ends(name, "rollout-", ".jsonl"))
        })
        .into_iter()
        .filter_map(|path| parse_codex(&path))
        .collect(),
        BackendKind::Claude => files_below(root, |path| {
            path.extension().is_some_and(|ext| os_eq(ext, "jsonl"))
                && !path
                    .components()
                    .any(|part| os_eq(part.as_os_str(), "subagents"))
                && !path
                    .file_stem()
                    .is_some_and(|stem| os_starts_with(stem, "agent-"))
        })
        .into_iter()
        .filter_map(|path| parse_claude(&path))
        .collect(),
        BackendKind::Grok => files_below(root, |path| {
            path.file_name()
                .is_some_and(|name| os_eq(name, "summary.json"))
        })
        .into_iter()
        .filter_map(|path| parse_grok(&path))
        .collect(),
        BackendKind::Agy => Vec::new(),
    }
}

fn os_eq(value: &OsStr, expected: &str) -> bool {
    if cfg!(windows) {
        value.to_string_lossy().eq_ignore_ascii_case(expected)
    } else {
        value == OsStr::new(expected)
    }
}

fn os_starts_with(value: &OsStr, prefix: &str) -> bool {
    let value = value.to_string_lossy();
    if cfg!(windows) {
        value
            .to_ascii_lowercase()
            .starts_with(&prefix.to_ascii_lowercase())
    } else {
        value.starts_with(prefix)
    }
}

fn os_starts_and_ends(value: &OsStr, prefix: &str, suffix: &str) -> bool {
    let value = value.to_string_lossy();
    if cfg!(windows) {
        let value = value.to_ascii_lowercase();
        value.starts_with(&prefix.to_ascii_lowercase())
            && value.ends_with(&suffix.to_ascii_lowercase())
    } else {
        value.starts_with(prefix) && value.ends_with(suffix)
    }
}

fn files_below(root: &Path, accept: impl Fn(&Path) -> bool + Copy) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut entries_remaining = MAX_SESSION_SCAN_ENTRIES;
    files_below_inner(root, 0, &mut entries_remaining, &mut files, accept);
    files
}

fn files_below_inner(
    root: &Path,
    depth: usize,
    entries_remaining: &mut usize,
    files: &mut Vec<PathBuf>,
    accept: impl Fn(&Path) -> bool + Copy,
) {
    if depth > MAX_SESSION_SCAN_DEPTH || *entries_remaining == 0 || files.len() >= MAX_SESSION_FILES
    {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        if *entries_remaining == 0 || files.len() >= MAX_SESSION_FILES {
            break;
        }
        *entries_remaining -= 1;
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            files_below_inner(&path, depth + 1, entries_remaining, files, accept);
        } else if kind.is_file() && accept(&path) {
            files.push(path);
        }
    }
}

fn parse_codex(path: &Path) -> Option<SessionEntry> {
    let mut id = None;
    let mut project = None;
    let mut title = None;
    import_io::for_each_json_value_limit(path, MAX_METADATA_LINES, |value| {
        if value["type"] == "session_meta" {
            let payload = &value["payload"];
            id = string_at(payload, "id")
                .or_else(|| string_at(payload, "session_id"))
                .map(str::to_string);
            project = string_at(payload, "cwd").map(str::to_string);
        } else if title.is_none() && value["type"] == "response_item" {
            title = value.get("payload").and_then(codex_user_text);
        }
        !(id.is_some() && project.is_some() && title.is_some())
    });
    Some(SessionEntry {
        id: id?,
        title: title.unwrap_or_else(|| "Untitled session".to_string()),
        project: project.unwrap_or_else(|| "unknown".to_string()),
        updated_at: modified(path),
    })
}

fn codex_user_text(payload: &Value) -> Option<String> {
    if payload["type"] != "message" || payload["role"] != "user" {
        return None;
    }
    payload["content"]
        .as_array()?
        .iter()
        .filter_map(|part| string_at(part, "text"))
        .find_map(clean_title)
}

fn parse_claude(path: &Path) -> Option<SessionEntry> {
    let mut id = None;
    let mut project = None;
    let mut title = None;
    import_io::for_each_json_value_limit(path, MAX_METADATA_LINES, |value| {
        if id.is_none() {
            id = string_at(&value, "sessionId").map(str::to_string);
        }
        if project.is_none() {
            project = string_at(&value, "cwd").map(str::to_string);
        }
        if title.is_none()
            && value["type"] == "user"
            && !value["isSidechain"].as_bool().unwrap_or(false)
        {
            title = claude_message_text(&value["message"]["content"]);
        }
        !(id.is_some() && project.is_some() && title.is_some())
    });
    let id = id.or_else(|| {
        path.file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
    })?;
    Some(SessionEntry {
        id,
        title: title.unwrap_or_else(|| "Untitled session".to_string()),
        project: project.unwrap_or_else(|| claude_project_from_path(path)),
        updated_at: modified(path),
    })
}

fn claude_message_text(content: &Value) -> Option<String> {
    if let Some(text) = content.as_str() {
        return clean_title(text);
    }
    content
        .as_array()?
        .iter()
        .filter(|part| part["type"] == "text")
        .filter_map(|part| string_at(part, "text"))
        .find_map(clean_title)
}

fn claude_project_from_path(path: &Path) -> String {
    path.parent()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown".to_string())
}

fn parse_grok(path: &Path) -> Option<SessionEntry> {
    if path.metadata().ok()?.len() > import_io::MAX_JSONL_LINE_BYTES as u64 {
        return None;
    }
    let value: Value = serde_json::from_reader(File::open(path).ok()?).ok()?;
    let id = string_at(&value["info"], "id")
        .map(str::to_string)
        .or_else(|| {
            path.parent()?
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })?;
    let project = string_at(&value["info"], "cwd")
        .map(str::to_string)
        .or_else(|| {
            let name = path.parent()?.parent()?.file_name()?.to_string_lossy();
            Some(percent_decode(&name))
        })
        .unwrap_or_else(|| "unknown".to_string());
    let title = string_at(&value, "session_summary")
        .and_then(clean_title)
        .unwrap_or_else(|| "Untitled session".to_string());
    Some(SessionEntry {
        id,
        title,
        project,
        updated_at: modified(path),
    })
}

fn string_at<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key)?.as_str()
}

fn clean_title(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty()
        || trimmed.starts_with("<environment_context>")
        || trimmed.starts_with("<user_instructions>")
        || trimmed.starts_with("<system-reminder>")
        || trimmed.starts_with("# AGENTS.md")
    {
        return None;
    }
    let collapsed = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(collapsed.chars().take(120).collect())
}

fn modified(path: &Path) -> SystemTime {
    path.metadata()
        .and_then(|metadata| metadata.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) = (hex(bytes[index + 1]), hex(bytes[index + 2]))
        {
            output.push(high * 16 + low);
            index += 3;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8_lossy(&output).into_owned()
}

fn hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_root(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "asterline-session-picker-{name}-{}",
            std::process::id()
        ));
        fs::remove_dir_all(&path).ok();
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn discovers_codex_metadata_and_filters_table_rows() {
        let root = fixture_root("codex");
        let path = root.join("rollout-test.jsonl");
        fs::write(
            &path,
            concat!(
                r#"{"type":"session_meta","payload":{"id":"codex-1","cwd":"/tmp/project"}}"#,
                "\n",
                r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Fix the parser"}]}}"#
            ),
        )
        .unwrap();
        let entries = discover_in(BackendKind::Codex, &root);
        assert_eq!(entries[0].id, "codex-1");
        assert_eq!(entries[0].title, "Fix the parser");

        let mut picker = SessionPicker::from_entries(BackendKind::Codex, entries);
        picker.push_query('p');
        picker.push_query('a');
        assert_eq!(picker.visible_len(), 1);
        picker.clear_query();
        assert_eq!(picker.selected_entry().unwrap().project, "/tmp/project");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn custom_codex_home_does_not_require_a_user_home() {
        assert_eq!(
            provider_root_from(BackendKind::Codex, None, Some(Path::new("/custom/codex")),),
            Some(PathBuf::from("/custom/codex/sessions"))
        );
        assert_eq!(
            provider_root_from(BackendKind::Claude, Some(Path::new("/profiles/ada")), None,),
            Some(PathBuf::from("/profiles/ada/.claude/projects"))
        );
    }

    #[test]
    fn discovers_claude_and_grok_metadata() {
        let claude_root = fixture_root("claude");
        let claude_project = claude_root.join("-tmp-project");
        fs::create_dir_all(&claude_project).unwrap();
        fs::write(
            claude_project.join("claude-1.jsonl"),
            r#"{"type":"user","sessionId":"claude-1","cwd":"/tmp/project","message":{"role":"user","content":"Review the tests"}}"#,
        )
        .unwrap();
        let claude = discover_in(BackendKind::Claude, &claude_root);
        assert_eq!(claude[0].title, "Review the tests");

        let grok_root = fixture_root("grok");
        let grok_session = grok_root.join("%2Ftmp%2Fproject").join("grok-1");
        fs::create_dir_all(&grok_session).unwrap();
        fs::write(
            grok_session.join("summary.json"),
            r#"{"info":{"id":"grok-1","cwd":"/tmp/project"},"session_summary":"Implement the TUI"}"#,
        )
        .unwrap();
        let grok = discover_in(BackendKind::Grok, &grok_root);
        assert_eq!(grok[0].id, "grok-1");
        assert_eq!(grok[0].title, "Implement the TUI");

        fs::remove_dir_all(claude_root).ok();
        fs::remove_dir_all(grok_root).ok();
    }

    #[test]
    fn picker_navigation_is_bounded() {
        let entries = (0..10)
            .map(|index| SessionEntry {
                id: format!("session-{index}"),
                title: format!("Title {index}"),
                project: "/tmp/project".to_string(),
                updated_at: SystemTime::UNIX_EPOCH,
            })
            .collect();
        let mut picker = SessionPicker::from_entries(BackendKind::Codex, entries);
        picker.page_down(8);
        assert_eq!(picker.selected(), 8);
        picker.down();
        picker.down();
        assert_eq!(picker.selected(), 9);
        picker.page_up(8);
        assert_eq!(picker.selected(), 1);
    }

    #[test]
    fn session_list_is_limited_to_the_current_project() {
        let root = fixture_root("project-filter");
        let current = root.join("current");
        let other = root.join("other");
        fs::create_dir_all(&current).unwrap();
        fs::create_dir_all(&other).unwrap();
        let mut entries = vec![
            SessionEntry::fixture("current", "Current", &format!("{}/", current.display())),
            SessionEntry::fixture("other", "Other", &other.display().to_string()),
        ];

        retain_current_project(&mut entries, &current);

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "current");
        fs::remove_dir_all(root).ok();
    }

    #[cfg(windows)]
    #[test]
    fn session_filter_accepts_windows_case_and_separator_variants() {
        let mut entries = vec![SessionEntry::fixture(
            "current",
            "Current",
            "c:/work/asterline",
        )];

        retain_current_project(&mut entries, Path::new(r"C:\Work\Asterline"));

        assert_eq!(entries.len(), 1);
    }
}
