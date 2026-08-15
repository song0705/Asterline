//! The TUI model. Every field is driven by `RuntimeEvent`s applied through
//! [`AppState::apply`]; the renderer reads it and the key handler mutates the
//! composer / drawer / scroll. No state is inferred from matching strings.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyModifiers};
use sha2::{Digest, Sha256};

use crate::adapter::parser::{append_bounded_text, bounded_text};
use crate::domain::config::{DetectedBackends, detect_backends};
use crate::domain::event::{
    AgentSessionId, ApprovalId, ChatItem, ConversationSummary, LogEntry, MemberStatus, MessageId,
    MessageTarget, RunId, RunStatus, RunStepStatus, RunSummary, RuntimeEvent, UiCommand,
};
use crate::domain::mode::TerminalMode;
use crate::domain::team::{
    BackendKind, DefaultTarget, Effort, MemberId, PermissionMode, SandboxPolicy, SessionPolicy,
    TeamMember,
};
use crate::run_support::suggested_verify_command;
use crate::tui::attach::AttachRequest;
use crate::tui::completion::{self, AgentSkill, Completion};
use crate::tui::composer::{Composer, MAX_COMPOSER_BYTES};
use crate::tui::drawers::Drawer;
use crate::tui::skills::SkillInfo;
use crate::tui::team_builder::ModelCatalog;
use crate::tui::team_editor::{TeamEditor, TeamEditorOutcome};
use uuid::Uuid;

const MAX_LOGS: usize = 4_000;
const MAX_LOG_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const MAX_CHAT_ITEMS: usize = 1_000;
pub(crate) const MAX_CHAT_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_CHAT_ITEM_BYTES: usize = 256 * 1024;
const MAX_PROMPT_HISTORY_ITEMS: usize = 1_000;
const MAX_PROMPT_HISTORY_BYTES: usize = 1024 * 1024;
const MIN_BOUNDED_CHAT_TEXT_BYTES: usize = 64;
const MAX_ACTIVE_TOOL_ID_BYTES: usize = 4 * 1024;
const MAX_ACTIVE_TOOL_NAME_BYTES: usize = 4 * 1024;
const MAX_ACTIVE_TOOL_SUMMARY_BYTES: usize = 16 * 1024;
const MAX_ACTIVE_REASONING_BYTES: usize = 8 * 1024;
const EARLIER_HISTORY_OMITTED: &str = "Earlier history omitted by TUI memory limit.";
const ACTIVE_MESSAGE_OUTPUT_OMITTED: &str =
    "[asterline: live response preview omitted by TUI memory limit]";
const ACTIVE_TOOL_OUTPUT_OMITTED: &str =
    "[asterline: live tool output omitted by TUI memory limit]";
const ACTIVE_MESSAGE_OUTPUT_INTERRUPTED: &str =
    "[asterline: response ended before a completion event]";
const ACTIVE_TOOL_OUTPUT_INTERRUPTED: &str = "[asterline: tool ended before a completion event]";
const COMPOSER_INPUT_TRUNCATED: &str =
    "Input truncated at the 256 KiB composer limit; split it into smaller messages.";

pub(crate) fn member_status_is_active(status: MemberStatus) -> bool {
    matches!(
        status,
        MemberStatus::Running
            | MemberStatus::Queued
            | MemberStatus::Waiting
            | MemberStatus::NeedsApproval
    )
}

#[derive(Clone, Debug)]
struct ActiveMessageCell {
    index: Option<usize>,
    member: MemberId,
    omitted: bool,
}

#[derive(Clone, Debug)]
struct ActiveToolCell {
    index: Option<usize>,
    name: String,
    summary: String,
    omitted: bool,
}

/// Header view of one member.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemberView {
    pub id: MemberId,
    pub display_name: String,
    pub backend: BackendKind,
    pub role: String,
    pub status: MemberStatus,
    pub session: Option<String>,
    pub cwd: String,
    pub model: Option<String>,
    pub effort: Option<Effort>,
    pub sandbox: SandboxPolicy,
    pub permission_mode: Option<PermissionMode>,
    pub session_policy: SessionPolicy,
}

/// A pending approval awaiting a decision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingApproval {
    pub id: ApprovalId,
    pub action: String,
    pub body: String,
}

/// Reverse incremental history search (Ctrl+R): a query and the index of the
/// currently-matched prompt-history entry.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct HistorySearch {
    query: String,
    match_idx: Option<usize>,
}

/// Inclusive start/end anchors for a mouse drag selection in the chat column.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChatSelection {
    pub start: (usize, usize),
    pub end: (usize, usize),
}

impl ChatSelection {
    pub fn normalized(self) -> ((usize, usize), (usize, usize)) {
        if self.start <= self.end {
            (self.start, self.end)
        } else {
            (self.end, self.start)
        }
    }

    pub fn is_empty(self) -> bool {
        self.start == self.end
    }
}

/// Transcript search (`/find`): query, matching chat indices, and current match.
#[derive(Clone, Debug, Eq, PartialEq)]
struct FindState {
    query: String,
    /// Indices into `chat` captured when the query was set.
    matches: Vec<usize>,
    /// Index into `matches` for the current jump target.
    current: usize,
}

pub struct AppState {
    team: String,
    workspace: String,
    default_target: Option<DefaultTarget>,
    active_mode: TerminalMode,
    members: Vec<MemberView>,
    chat: Vec<ChatItem>,
    message_index: HashMap<MessageId, ActiveMessageCell>,
    tool_index: HashMap<(MemberId, String), ActiveToolCell>,
    logs: Vec<LogEntry>,
    log_bytes: usize,
    runs: Vec<RunSummary>,
    selected_run: Option<RunId>,
    selected_run_step: Option<u32>,
    runs_detail: bool,
    pending_approvals: Vec<PendingApproval>,
    paused_routes: usize,
    composer: Composer,
    drawer: Option<Drawer>,
    scroll: usize,
    popup_selected: usize,
    popup_dismissed: bool,
    should_quit: bool,
    quit_armed: bool,
    runtime_available: bool,
    tools_expanded: bool,
    active_reasoning: HashMap<MemberId, String>,
    last_message_target: Option<MessageTarget>,
    header_selected: Option<usize>,
    attach_pending: Option<MemberId>,
    attach_request: Option<AttachRequest>,
    attach_release_pending: Option<MemberId>,
    /// Shell-style prompt history (oldest→newest): prior submissions recalled
    /// with ↑/↓. Seeded from replayed user messages, appended as you submit.
    prompt_history: Vec<String>,
    /// Position in `prompt_history` while browsing, or `None` when editing the
    /// live draft.
    history_cursor: Option<usize>,
    /// The live draft saved when history browsing begins, restored on the way
    /// back past the newest entry.
    history_draft: String,
    /// When each currently-running member started, for the elapsed-time
    /// "working" indicator. Set on entering Running, cleared otherwise.
    running_since: HashMap<MemberId, Instant>,
    /// Active reverse history search (Ctrl+R), if any.
    history_search: Option<HistorySearch>,
    /// Active transcript search (`/find`), if any.
    find: Option<FindState>,
    /// Vertical scroll offset for the open drawer (logs / team / diff).
    drawer_scroll: usize,
    /// Captured working-tree diff text for the diff drawer (`/diff`).
    diff_text: Option<String>,
    /// Editable draft shown by the `/team` drawer.
    team_editor: Option<TeamEditor>,
    /// Cross-drawer cache for asynchronously discovered backend models. The
    /// transient editor borrows it by ownership while open, then returns it on
    /// close so reopening `/team` does not rerun CLI discovery every time.
    model_catalog: ModelCatalog,
    /// Backend availability is checked asynchronously at startup so every
    /// installed CLI's workspace catalog can warm without blocking the TUI.
    model_catalog_detection: Option<Receiver<DetectedBackends>>,
    /// The active `ast` process warms all installed backends once at startup.
    /// Keeping that fixed avoids model-list subprocess churn while the Team
    /// changes.
    model_catalog_warmed: bool,
    skills: Vec<SkillInfo>,
    resume_choices: Vec<ConversationSummary>,
    selected_resume: usize,
    /// Drag-select range in flattened chat-line coordinates, if any.
    chat_selection: Option<ChatSelection>,
}

impl AppState {
    /// Create with replayed chat history (empty for a fresh session).
    pub fn new(mut chat: Vec<ChatItem>) -> Self {
        trim_initial_chat(&mut chat);
        // Seed prompt history from prior user messages (cross-session recall),
        // collapsing consecutive duplicates the way a shell history does.
        let mut prompt_history: Vec<String> = Vec::new();
        for item in &chat {
            if let ChatItem::User { body, .. } = item
                && prompt_history.last() != Some(body)
            {
                prompt_history.push(body.clone());
            }
        }
        trim_prompt_history(&mut prompt_history);
        Self {
            team: "Asterline".to_string(),
            workspace: String::new(),
            default_target: None,
            active_mode: TerminalMode::Normal,
            members: Vec::new(),
            chat,
            message_index: HashMap::new(),
            tool_index: HashMap::new(),
            logs: Vec::new(),
            log_bytes: 0,
            runs: Vec::new(),
            selected_run: None,
            selected_run_step: None,
            runs_detail: false,
            pending_approvals: Vec::new(),
            paused_routes: 0,
            composer: Composer::new(),
            drawer: None,
            scroll: 0,
            popup_selected: 0,
            popup_dismissed: false,
            should_quit: false,
            quit_armed: false,
            runtime_available: true,
            tools_expanded: false,
            active_reasoning: HashMap::new(),
            last_message_target: None,
            header_selected: None,
            attach_pending: None,
            attach_request: None,
            attach_release_pending: None,
            prompt_history,
            history_cursor: None,
            history_draft: String::new(),
            running_since: HashMap::new(),
            history_search: None,
            find: None,
            drawer_scroll: 0,
            diff_text: None,
            team_editor: None,
            model_catalog: ModelCatalog::default(),
            model_catalog_detection: None,
            model_catalog_warmed: false,
            skills: Vec::new(),
            resume_choices: Vec::new(),
            selected_resume: 0,
            chat_selection: None,
        }
    }

    // --- applying runtime events ----------------------------------------

    /// Seed the logs drawer with persisted entries replayed on startup, so logs
    /// survive a restart the way the chat transcript does.
    pub fn seed_logs(&mut self, logs: Vec<LogEntry>) {
        self.logs.clear();
        self.log_bytes = 0;
        for entry in logs {
            self.push_log(entry);
        }
    }

    pub fn apply(&mut self, event: RuntimeEvent) {
        match event {
            RuntimeEvent::Ready {
                team,
                workspace,
                default_target,
                members,
                runs,
            } => {
                // A refreshed roster invalidates any unsaved Team draft, but
                // its completed/in-flight model lookups remain reusable.
                self.stash_team_editor_catalog();
                self.team = team;
                self.skills = crate::tui::skills::discover(Path::new(&workspace));
                self.workspace = workspace;
                self.default_target = default_target;
                self.runs = runs;
                self.ensure_selected_run();
                self.ensure_selected_run_step();
                self.members = members
                    .into_iter()
                    .map(|m| MemberView {
                        id: m.id,
                        display_name: m.display_name,
                        backend: m.backend,
                        role: m.role,
                        status: m.status,
                        session: m.session,
                        cwd: m.cwd,
                        model: m.model,
                        effort: m.effort,
                        sandbox: m.sandbox,
                        permission_mode: m.permission_mode,
                        session_policy: m.session_policy,
                    })
                    .collect();
                let member_ids: std::collections::HashSet<MemberId> =
                    self.members.iter().map(|m| m.id.clone()).collect();
                self.running_since
                    .retain(|member, _| member_ids.contains(member));
                for member in &self.members {
                    if member_status_is_active(member.status) {
                        self.running_since
                            .entry(member.id.clone())
                            .or_insert_with(Instant::now);
                    } else {
                        self.running_since.remove(&member.id);
                    }
                }
                if let Some(idx) = self.header_selected
                    && idx >= self.members.len()
                {
                    self.header_selected = self.members.len().checked_sub(1);
                }
                if self.drawer == Some(Drawer::Team) {
                    self.open_team_editor();
                }
            }
            RuntimeEvent::ModeChanged { mode } => self.active_mode = mode,
            RuntimeEvent::TurnStarted { .. } | RuntimeEvent::TurnFinished { .. } => {}
            RuntimeEvent::UserMessage { body, targets, .. } => {
                let interrupted = self
                    .members
                    .iter()
                    .filter(|member| {
                        member_status_is_active(member.status)
                            && !targets.iter().any(|target| target == &member.id)
                    })
                    .map(|member| member.id.clone())
                    .collect();
                self.push(ChatItem::User {
                    body,
                    targets,
                    interrupted,
                });
            }
            RuntimeEvent::MemberStatus { member, status } => {
                if !member_status_is_active(status) {
                    self.finish_incomplete_cells_for_member(&member);
                }
                self.set_status(&member, status);
            }
            RuntimeEvent::MessageStarted { msg, member, .. } => {
                if self.message_index.contains_key(&msg) {
                    self.finish_incomplete_message(msg);
                } else if self.message_index.len() >= MAX_CHAT_ITEMS
                    && let Some(oldest_available) = self.message_index.keys().next().copied()
                {
                    self.finish_incomplete_message(oldest_available);
                }
                let active_member = member.clone();
                let (display_name, backend) = self.member_meta(&member);
                let idx = self.push(ChatItem::Agent {
                    member,
                    display_name,
                    backend,
                    text: String::new(),
                });
                let index = matches!(
                    self.chat.get(idx),
                    Some(ChatItem::Agent { member, .. }) if member == &active_member
                )
                .then_some(idx);
                self.message_index.insert(
                    msg,
                    ActiveMessageCell {
                        index,
                        member: active_member,
                        omitted: index.is_none(),
                    },
                );
            }
            RuntimeEvent::MessageDelta { msg, text } => {
                if let Some(idx) = self
                    .message_index
                    .get(&msg)
                    .filter(|cell| !cell.omitted)
                    .and_then(|cell| cell.index)
                    && let Some(ChatItem::Agent { text: body, .. }) = self.chat.get_mut(idx)
                {
                    let _ = append_bounded_text(body, &text, MAX_CHAT_ITEM_BYTES);
                    self.normalize_chat_item_at(idx);
                }
                self.trim_chat_for(0, 0);
            }
            RuntimeEvent::MessageCompleted { msg, text } => {
                if let Some(cell) = self.message_index.remove(&msg) {
                    let completed_text =
                        active_completion_text(ACTIVE_MESSAGE_OUTPUT_OMITTED, &text, cell.omitted);
                    let mut updated_index = None;
                    if !cell.omitted
                        && let Some(idx) = cell.index
                        && let Some(ChatItem::Agent {
                            text: body, member, ..
                        }) = self.chat.get_mut(idx)
                        && member == &cell.member
                    {
                        *body = completed_text.clone();
                        updated_index = Some(idx);
                    }
                    if let Some(idx) = updated_index {
                        self.normalize_chat_item_at(idx);
                    } else {
                        let (display_name, backend) = self.member_meta(&cell.member);
                        self.push(ChatItem::Agent {
                            member: cell.member.clone(),
                            display_name,
                            backend,
                            text: completed_text,
                        });
                    }
                    self.active_reasoning.remove(&cell.member);
                }
                self.trim_chat_for(0, 0);
            }
            RuntimeEvent::Reasoning { member, text } => {
                self.append_reasoning(member, &text);
            }
            RuntimeEvent::ToolStarted {
                member,
                tool_id,
                name,
                summary,
            } => {
                let key = (member.clone(), active_tool_key(&tool_id));
                if self.tool_index.contains_key(&key) {
                    self.finish_incomplete_tool(&key);
                } else if self.tool_index.len() >= MAX_CHAT_ITEMS
                    && let Some(oldest_available) = self.tool_index.keys().next().cloned()
                {
                    // The chat hot cache and its live-cell metadata share the
                    // same item-count ceiling. Provider protocol bugs cannot
                    // create an unbounded set of never-completed tombstones.
                    self.finish_incomplete_tool(&oldest_available);
                }
                let fallback_name = bounded_text(&name, MAX_ACTIVE_TOOL_NAME_BYTES);
                let fallback_summary = bounded_text(&summary, MAX_ACTIVE_TOOL_SUMMARY_BYTES);
                let idx = self.push(ChatItem::Tool {
                    member,
                    name,
                    summary,
                    detail: String::new(),
                    ok: None,
                });
                let (index, active_name, active_summary) = match self.chat.get(idx) {
                    Some(ChatItem::Tool { name, summary, .. }) => (
                        Some(idx),
                        bounded_text(name, MAX_ACTIVE_TOOL_NAME_BYTES),
                        bounded_text(summary, MAX_ACTIVE_TOOL_SUMMARY_BYTES),
                    ),
                    _ => (None, fallback_name, fallback_summary),
                };
                self.tool_index.insert(
                    key,
                    ActiveToolCell {
                        index,
                        name: active_name,
                        summary: active_summary,
                        omitted: index.is_none(),
                    },
                );
            }
            RuntimeEvent::ToolProgress {
                member,
                tool_id,
                delta,
            } => {
                let tool_id = active_tool_key(&tool_id);
                if let Some(idx) = self
                    .tool_index
                    .get(&(member, tool_id))
                    .filter(|cell| !cell.omitted)
                    .and_then(|cell| cell.index)
                    && let Some(ChatItem::Tool { detail, .. }) = self.chat.get_mut(idx)
                {
                    let _ = append_bounded_text(detail, &delta, MAX_CHAT_ITEM_BYTES);
                    self.normalize_chat_item_at(idx);
                }
                self.trim_chat_for(0, 0);
            }
            RuntimeEvent::ToolCompleted {
                member,
                tool_id,
                ok,
                output,
            } => {
                let tool_id = active_tool_key(&tool_id);
                if let Some(cell) = self.tool_index.remove(&(member.clone(), tool_id)) {
                    let completed_detail = if cell.omitted {
                        Some(active_completion_text(
                            ACTIVE_TOOL_OUTPUT_OMITTED,
                            &output,
                            true,
                        ))
                    } else if output.is_empty() {
                        None
                    } else {
                        Some(bounded_text(&output, MAX_CHAT_ITEM_BYTES))
                    };
                    let mut updated_index = None;
                    if !cell.omitted
                        && let Some(idx) = cell.index
                        && let Some(ChatItem::Tool {
                            ok: cell_ok,
                            detail,
                            ..
                        }) = self.chat.get_mut(idx)
                    {
                        *cell_ok = Some(ok);
                        if let Some(completed_detail) = &completed_detail {
                            *detail = completed_detail.clone();
                        }
                        updated_index = Some(idx);
                    }
                    if let Some(idx) = updated_index {
                        self.normalize_chat_item_at(idx);
                    } else {
                        self.push(ChatItem::Tool {
                            member,
                            name: cell.name,
                            summary: cell.summary,
                            detail: completed_detail.unwrap_or_default(),
                            ok: Some(ok),
                        });
                    }
                } else {
                    self.push(ChatItem::Tool {
                        member,
                        name: "tool".to_string(),
                        summary: String::new(),
                        detail: bounded_text(&output, MAX_CHAT_ITEM_BYTES),
                        ok: Some(ok),
                    });
                }
                self.trim_chat_for(0, 0);
            }
            RuntimeEvent::Route { from, to, body, .. } => {
                self.push(ChatItem::Route { from, to, body });
            }
            RuntimeEvent::FileChange { member, files, ok } => {
                self.push(ChatItem::Diff { member, files, ok });
            }
            RuntimeEvent::RouteError {
                from,
                target,
                reason,
                body,
                ..
            } => {
                self.push(ChatItem::Error {
                    member: Some(from),
                    message: format!("route to {target} failed: {reason} — {body}"),
                });
            }
            RuntimeEvent::RoutePaused {
                from,
                to,
                reason,
                queued,
                ..
            } => {
                self.paused_routes = queued;
                self.push(ChatItem::Notice {
                    text: format!(
                        "route paused {from} → {}: {reason} (queued {queued}; /retry to resume)",
                        to.join(", ")
                    ),
                });
            }
            RuntimeEvent::RouteQueueUpdated { queued } => {
                self.paused_routes = queued;
            }
            RuntimeEvent::SessionUpdated { member, session } => {
                if let Some(view) = self.members.iter_mut().find(|m| m.id == member) {
                    view.session = Some(session.0);
                }
            }
            RuntimeEvent::AttachGranted { member } => {
                let expected = self.attach_pending.take();
                if expected.as_ref() != Some(&member) {
                    self.push(ChatItem::Notice {
                        text: format!("could not attach: unexpected grant for member {member}"),
                    });
                    self.attach_release_pending = Some(member);
                    return;
                }
                let Some((display_name, backend, session, cwd)) = self
                    .members
                    .iter()
                    .find(|view| view.id == member)
                    .map(|view| {
                        (
                            view.display_name.clone(),
                            view.backend,
                            view.session.clone(),
                            view.cwd.clone(),
                        )
                    })
                else {
                    self.push(ChatItem::Notice {
                        text: format!("could not attach: runtime granted unknown member {member}"),
                    });
                    self.attach_release_pending = Some(member);
                    return;
                };
                // Claude can create a session with a caller-provided UUID.
                // Supplying it up front gives the transcript importer an exact
                // file to read after attach; it never needs to guess among
                // other Claude sessions created in the same workspace.
                let fresh_session = (backend == BackendKind::Claude && session.is_none())
                    .then(|| AgentSessionId(Uuid::new_v4().to_string()));
                self.attach_request = Some(AttachRequest {
                    member,
                    display_name,
                    backend,
                    session,
                    fresh_session,
                    cwd,
                });
            }
            RuntimeEvent::AttachDenied { member, reason } => {
                if self.attach_pending.as_ref() == Some(&member) {
                    self.attach_pending = None;
                }
                self.push(ChatItem::Notice {
                    text: format!("could not attach to {member}: {reason}"),
                });
            }
            RuntimeEvent::ApprovalRequested {
                id, action, body, ..
            } => {
                self.pending_approvals.push(PendingApproval {
                    id,
                    action: action.clone(),
                    body: body.clone(),
                });
                self.push(ChatItem::Notice {
                    text: format!("approval needed [{action}]: {body} — /approve or /reject"),
                });
            }
            RuntimeEvent::ApprovalResolved { id, decision } => {
                self.pending_approvals.retain(|a| a.id != id);
                self.push(ChatItem::Notice {
                    text: format!("approval {}", decision.as_str()),
                });
            }
            RuntimeEvent::MemberError { member, message } => {
                self.push(ChatItem::Error {
                    member: Some(member),
                    message,
                });
            }
            RuntimeEvent::RunUpdated { run } => {
                if let Some(existing) = self.runs.iter_mut().find(|r| r.id == run.id) {
                    *existing = run;
                } else {
                    self.runs.push(run);
                }
                self.ensure_selected_run();
                self.ensure_selected_run_step();
            }
            RuntimeEvent::Verdict {
                member,
                approve,
                summary,
                ..
            } => {
                self.push(ChatItem::Verdict {
                    member,
                    approve,
                    summary,
                });
            }
            RuntimeEvent::Log(entry) => {
                self.push_log(entry);
            }
            RuntimeEvent::Notice(text) => {
                self.push(ChatItem::Notice { text });
            }
            RuntimeEvent::SessionReset => {
                // Begin a fresh chat: clear the transcript and in-flight cells,
                // but keep members, logs, and prompt history. Runs belong to
                // the previous conversation and remain reachable via /resume.
                self.active_mode = TerminalMode::Normal;
                self.chat.clear();
                self.message_index.clear();
                self.tool_index.clear();
                self.active_reasoning.clear();
                self.pending_approvals.clear();
                self.paused_routes = 0;
                self.last_message_target = None;
                self.attach_pending = None;
                self.attach_request = None;
                self.running_since.clear();
                self.find = None;
                self.runs.clear();
                self.selected_run = None;
                self.selected_run_step = None;
                self.runs_detail = false;
                self.scroll = 0;
                self.drawer = None;
                self.drawer_scroll = 0;
                self.chat_selection = None;
                self.stash_team_editor_catalog();
            }
            RuntimeEvent::ResumeChoices { conversations } => {
                self.resume_choices = conversations;
                self.selected_resume = 0;
                self.drawer = Some(Drawer::Resume);
                self.drawer_scroll = 0;
                self.stash_team_editor_catalog();
            }
            RuntimeEvent::ConversationResumed { chat, .. } => {
                self.chat = chat;
                trim_initial_chat(&mut self.chat);
                self.message_index.clear();
                self.tool_index.clear();
                self.active_reasoning.clear();
                self.pending_approvals.clear();
                self.paused_routes = 0;
                self.last_message_target = None;
                self.attach_pending = None;
                self.attach_request = None;
                self.running_since.clear();
                self.find = None;
                self.scroll = 0;
                self.drawer = None;
                self.drawer_scroll = 0;
                self.chat_selection = None;
                self.stash_team_editor_catalog();
            }
        }
    }

    fn push(&mut self, item: ChatItem) -> usize {
        let item = bound_chat_item(item);
        let incoming_bytes = chat_item_bytes(&item);
        self.trim_chat_for(1, incoming_bytes);
        // When the user has scrolled up to browse history, keep the view
        // pinned by growing the scroll offset to compensate for the new item.
        // The estimate is rough (we don't know the render width here) but
        // prevents the jarring jump-to-bottom on every new message.
        if self.scroll > 0 {
            let est_lines = estimate_item_lines(&item);
            self.scroll = self.scroll.saturating_add(est_lines);
        }
        self.chat.push(item);
        self.chat.len() - 1
    }

    fn push_log(&mut self, entry: LogEntry) {
        let entry = entry.bounded();
        self.log_bytes = self.log_bytes.saturating_add(entry.byte_len());
        self.logs.push(entry);

        let mut remove = 0;
        while remove < self.logs.len()
            && (self.logs.len() - remove > MAX_LOGS || self.log_bytes > MAX_LOG_BYTES)
        {
            self.log_bytes = self.log_bytes.saturating_sub(self.logs[remove].byte_len());
            remove += 1;
        }
        if remove > 0 {
            self.logs.drain(0..remove);
        }
    }

    fn normalize_chat_item_at(&mut self, index: usize) {
        let Some(slot) = self.chat.get_mut(index) else {
            return;
        };
        let item = std::mem::replace(slot, chat_truncation_notice());
        *slot = bound_chat_item(item);
    }

    /// A cancelled or failed backend can exit without emitting the matching
    /// message/tool completion events. Retire those live-cell indexes when the
    /// member becomes inactive so tombstones cannot leak indefinitely, while
    /// keeping an attributed, visibly incomplete final cell in the transcript.
    fn finish_incomplete_cells_for_member(&mut self, member: &MemberId) {
        let message_ids = self
            .message_index
            .iter()
            .filter_map(|(id, cell)| (&cell.member == member).then_some(*id))
            .collect::<Vec<_>>();
        for id in message_ids {
            self.finish_incomplete_message(id);
        }

        let tool_keys = self
            .tool_index
            .keys()
            .filter(|(tool_member, _)| tool_member == member)
            .cloned()
            .collect::<Vec<_>>();
        for key in tool_keys {
            self.finish_incomplete_tool(&key);
        }
        self.trim_chat_for(0, 0);
    }

    fn finish_incomplete_message(&mut self, id: MessageId) {
        let Some(cell) = self.message_index.remove(&id) else {
            return;
        };
        let updated_index = cell.index.filter(|&index| {
            if let Some(ChatItem::Agent {
                member, text: body, ..
            }) = self.chat.get_mut(index)
                && member == &cell.member
            {
                *body = interrupted_completion_text(
                    ACTIVE_MESSAGE_OUTPUT_OMITTED,
                    ACTIVE_MESSAGE_OUTPUT_INTERRUPTED,
                    body,
                    cell.omitted,
                );
                true
            } else {
                false
            }
        });
        if let Some(index) = updated_index {
            self.normalize_chat_item_at(index);
        } else {
            let (display_name, backend) = self.member_meta(&cell.member);
            self.push(ChatItem::Agent {
                member: cell.member,
                display_name,
                backend,
                text: interrupted_completion_text(
                    ACTIVE_MESSAGE_OUTPUT_OMITTED,
                    ACTIVE_MESSAGE_OUTPUT_INTERRUPTED,
                    "",
                    cell.omitted,
                ),
            });
        }
    }

    fn finish_incomplete_tool(&mut self, key: &(MemberId, String)) {
        let Some(cell) = self.tool_index.remove(key) else {
            return;
        };
        let updated_index = cell.index.filter(|&index| {
            if let Some(ChatItem::Tool {
                member: item_member,
                detail,
                ok,
                ..
            }) = self.chat.get_mut(index)
                && item_member == &key.0
            {
                *detail = interrupted_completion_text(
                    ACTIVE_TOOL_OUTPUT_OMITTED,
                    ACTIVE_TOOL_OUTPUT_INTERRUPTED,
                    detail,
                    cell.omitted,
                );
                *ok = Some(false);
                true
            } else {
                false
            }
        });
        if let Some(index) = updated_index {
            self.normalize_chat_item_at(index);
        } else {
            self.push(ChatItem::Tool {
                member: key.0.clone(),
                name: cell.name,
                summary: cell.summary,
                detail: interrupted_completion_text(
                    ACTIVE_TOOL_OUTPUT_OMITTED,
                    ACTIVE_TOOL_OUTPUT_INTERRUPTED,
                    "",
                    cell.omitted,
                ),
                ok: Some(false),
            });
        }
    }

    fn trim_chat_for(&mut self, incoming_items: usize, incoming_bytes: usize) {
        let mut bytes = self.chat.iter().map(chat_item_bytes).sum::<usize>();
        let protected = self
            .message_index
            .values()
            .filter_map(|cell| cell.index)
            .chain(self.tool_index.values().filter_map(|cell| cell.index))
            .collect::<HashSet<_>>();
        let mut remove = vec![false; self.chat.len()];
        let mut retained = self.chat.len();
        for (index, item) in self.chat.iter().enumerate() {
            if chat_budget_fits(retained, bytes, incoming_items, incoming_bytes) {
                break;
            }
            if protected.contains(&index) {
                continue;
            }
            remove[index] = true;
            retained -= 1;
            bytes = bytes.saturating_sub(chat_item_bytes(item));
        }
        // Keep an explicit, attributed placeholder for active output before
        // evicting its cell. Completion can then replace it, while the live
        // transcript still explains why the preview disappeared.
        if !chat_budget_fits(retained, bytes, incoming_items, incoming_bytes) {
            for (index, removed) in remove.iter().enumerate() {
                if chat_budget_fits(retained, bytes, incoming_items, incoming_bytes) {
                    break;
                }
                if *removed || !protected.contains(&index) {
                    continue;
                }
                let before = chat_item_bytes(&self.chat[index]);
                if compact_active_chat_item(&mut self.chat[index]) {
                    let after = chat_item_bytes(&self.chat[index]);
                    bytes = bytes.saturating_sub(before).saturating_add(after);
                    for cell in self.message_index.values_mut() {
                        if cell.index == Some(index) {
                            cell.omitted = true;
                        }
                    }
                    for cell in self.tool_index.values_mut() {
                        if cell.index == Some(index) {
                            cell.omitted = true;
                        }
                    }
                }
            }
        }
        // Active message/tool cells are preferred, not exempt: otherwise many
        // simultaneous streams can bypass the hard TUI memory ceiling.
        if !chat_budget_fits(retained, bytes, incoming_items, incoming_bytes) {
            for (index, item) in self.chat.iter().enumerate() {
                if chat_budget_fits(retained, bytes, incoming_items, incoming_bytes) {
                    break;
                }
                if remove[index] {
                    continue;
                }
                remove[index] = true;
                retained -= 1;
                bytes = bytes.saturating_sub(chat_item_bytes(item));
            }
        }
        if !remove.iter().any(|removed| *removed) {
            return;
        }
        let removed_before = removed_prefix_counts(&remove);
        let mut index = 0;
        self.chat.retain(|_| {
            let keep = !remove[index];
            index += 1;
            keep
        });
        for cell in self.message_index.values_mut() {
            cell.index = cell
                .index
                .and_then(|index| remap_index(index, &remove, &removed_before));
            if cell.index.is_none() {
                cell.omitted = true;
            }
        }
        for cell in self.tool_index.values_mut() {
            cell.index = cell
                .index
                .and_then(|index| remap_index(index, &remove, &removed_before));
            if cell.index.is_none() {
                cell.omitted = true;
            }
        }
        if let Some(find) = self.find.as_mut() {
            find.matches = find
                .matches
                .iter()
                .filter_map(|&index| remap_index(index, &remove, &removed_before))
                .collect();
            find.current = find.current.min(find.matches.len().saturating_sub(1));
        }
        self.chat_selection = None;
    }

    fn set_status(&mut self, member: &MemberId, status: MemberStatus) {
        if let Some(view) = self.members.iter_mut().find(|m| &m.id == member) {
            view.status = status;
        }
        // Queued means a run is still active and another prompt is waiting;
        // keep the original elapsed timer until the runtime explicitly idles.
        if member_status_is_active(status) {
            self.running_since
                .entry(member.clone())
                .or_insert_with(Instant::now);
        } else {
            self.running_since.remove(member);
        }
        if status == MemberStatus::Idle
            || status == MemberStatus::Failed
            || status == MemberStatus::NeedsApproval
        {
            self.active_reasoning.remove(member);
        }
    }

    fn append_reasoning(&mut self, member: MemberId, delta: &str) {
        let delta = bounded_text(delta, MAX_ACTIVE_REASONING_BYTES);
        if delta.is_empty() {
            return;
        }
        let reasoning = self.active_reasoning.entry(member).or_default();
        if delta.starts_with(reasoning.as_str()) {
            *reasoning = delta;
        } else if !reasoning.ends_with(&delta) {
            let _ = append_bounded_text(reasoning, &delta, MAX_ACTIVE_REASONING_BYTES);
        }
    }

    pub(crate) fn member_meta(&self, member: &MemberId) -> (String, BackendKind) {
        self.members
            .iter()
            .find(|m| &m.id == member)
            .map(|m| (m.display_name.clone(), m.backend))
            .unwrap_or_else(|| (member.to_string(), BackendKind::Codex))
    }

    pub fn member_display(&self, member: &MemberId) -> String {
        self.members
            .iter()
            .find(|m| &m.id == member)
            .map(|m| m.display_name.clone())
            .unwrap_or_else(|| member.to_string())
    }

    pub fn has_active_message(&self, member_id: &MemberId) -> bool {
        self.message_index
            .values()
            .any(|cell| &cell.member == member_id)
    }

    pub fn omitted_active_output_count(&self) -> usize {
        self.message_index
            .values()
            .filter(|cell| cell.omitted && cell.index.is_none())
            .count()
            + self
                .tool_index
                .values()
                .filter(|cell| cell.omitted && cell.index.is_none())
                .count()
    }

    pub fn remember_user_message_target(&mut self, target: &MessageTarget) {
        self.remember_message_target(target);
    }

    pub fn inherited_user_message(&self, text: &str) -> Option<(MessageTarget, String)> {
        let target = self.last_message_target.clone()?;
        if self.resolve_local_targets(&target).is_empty() {
            return None;
        }
        let body = text.trim();
        if body.is_empty() {
            return None;
        }
        Some((target.clone(), format_inherited_user_body(&target, body)))
    }

    pub fn clear_last_message_target(&mut self) {
        self.last_message_target = None;
    }

    fn remember_message_target(&mut self, target: &MessageTarget) {
        match target {
            MessageTarget::All | MessageTarget::Member(_) => {
                self.last_message_target = Some(target.clone());
            }
            MessageTarget::Default | MessageTarget::Members(_) => {}
        }
    }

    pub fn resolve_local_targets(&self, target: &MessageTarget) -> Vec<MemberId> {
        match target {
            MessageTarget::Default => match &self.default_target {
                Some(DefaultTarget::All) => self.members.iter().map(|m| m.id.clone()).collect(),
                Some(DefaultTarget::Member(id)) => {
                    let resolved = self.resolve_local_named(std::slice::from_ref(id));
                    if resolved.is_empty() {
                        self.members
                            .first()
                            .map(|m| vec![m.id.clone()])
                            .unwrap_or_default()
                    } else {
                        resolved
                    }
                }
                None => self
                    .members
                    .first()
                    .map(|m| vec![m.id.clone()])
                    .unwrap_or_default(),
            },
            MessageTarget::All => self.members.iter().map(|m| m.id.clone()).collect(),
            MessageTarget::Member(id) => self.resolve_local_named(std::slice::from_ref(id)),
            MessageTarget::Members(ids) => self.resolve_local_named(ids),
        }
    }

    fn resolve_local_named(&self, ids: &[MemberId]) -> Vec<MemberId> {
        let mut resolved = Vec::new();
        for id in ids {
            if let Some(member) = self
                .members
                .iter()
                .find(|m| m.id == *id || m.display_name.eq_ignore_ascii_case(id.as_str()))
                && !resolved.contains(&member.id)
            {
                resolved.push(member.id.clone());
            }
        }
        resolved
    }

    fn resolve_member_id(&self, requested: &MemberId) -> Option<MemberId> {
        self.members
            .iter()
            .find(|member| {
                member.id == *requested
                    || member.display_name.eq_ignore_ascii_case(requested.as_str())
            })
            .map(|member| member.id.clone())
    }

    pub(crate) fn member_backend(&self, requested: &MemberId) -> Option<BackendKind> {
        let id = self.resolve_member_id(requested)?;
        self.members
            .iter()
            .find(|member| member.id == id)
            .map(|member| member.backend)
    }

    /// Return a target-safe prompt invocation only when the slash spelling is
    /// one Asterline actually discovered for that member's backend. This keeps
    /// unknown interactive controls out of noninteractive `codex exec`.
    pub(crate) fn targeted_skill_command(
        &self,
        requested: &MemberId,
        body: &str,
    ) -> Option<UiCommand> {
        let member = self.resolve_member_id(requested)?;
        let backend = self.member_backend(&member)?;
        let token = body.split_whitespace().next()?;
        let invocation = if backend == BackendKind::Codex && token.starts_with('/') {
            format!("${}", token.trim_start_matches('/'))
        } else {
            token.to_string()
        };
        self.skills
            .iter()
            .find(|skill| skill.backend == backend && skill.invocation == invocation)?;
        Some(
            self.normalize_known_skill_invocation(UiCommand::UserMessage {
                target: MessageTarget::Member(member.clone()),
                body: format!("@{member} {body}"),
            }),
        )
    }

    // --- accessors for the renderer -------------------------------------

    pub fn team(&self) -> &str {
        &self.team
    }
    pub fn active_mode(&self) -> TerminalMode {
        self.active_mode
    }
    pub fn workspace(&self) -> &str {
        &self.workspace
    }
    pub fn default_target(&self) -> Option<&DefaultTarget> {
        self.default_target.as_ref()
    }
    pub fn members(&self) -> &[MemberView] {
        &self.members
    }

    /// A live member's effective model label. Model discovery is deliberately
    /// display-only: it must not pin a CLI default into team.json, but the
    /// activity line should still show the model and effort the CLI reported
    /// instead of two misleading `default` placeholders.
    pub(crate) fn member_runtime_profile(&self, view: &MemberView) -> String {
        let member = self.view_to_member(view);
        let workspace = PathBuf::from(&self.workspace);
        let model = self
            .team_editor
            .as_ref()
            .map(|editor| editor.model_catalog().model_label(&member, &workspace))
            .unwrap_or_else(|| self.model_catalog.model_label(&member, &workspace));
        format!("model: {model}")
    }

    pub fn chat(&self) -> &[ChatItem] {
        &self.chat
    }

    pub fn logs(&self) -> &[LogEntry] {
        &self.logs
    }
    pub fn runs(&self) -> &[RunSummary] {
        &self.runs
    }
    pub fn latest_run(&self) -> Option<&RunSummary> {
        self.runs.last()
    }
    pub fn latest_run_action_command(&self) -> Option<String> {
        self.latest_run()
            .and_then(|run| run_action_command(run, &self.workspace, false))
    }
    pub fn selected_run(&self) -> Option<&RunSummary> {
        self.selected_run
            .and_then(|id| self.runs.iter().find(|run| run.id == id))
            .or_else(|| self.latest_run())
    }
    pub fn selected_run_step(&self) -> Option<u32> {
        let run = self.selected_run()?;
        self.selected_run_step
            .filter(|step| run.steps.iter().any(|candidate| candidate.number == *step))
    }
    pub fn selected_run_action_command(&self) -> Option<String> {
        self.selected_run()
            .and_then(|run| run_action_command(run, &self.workspace, true))
    }
    pub fn selected_run_stage_command(&self) -> Option<String> {
        let run = self.selected_run()?;
        self.selected_run_step()
            .and_then(|step| run_step_action_command(run, step))
            .or_else(|| run_action_command(run, &self.workspace, true))
    }
    pub fn selected_run_dispatch_command(&self) -> Option<String> {
        let run = self.selected_run()?;
        let step = self.selected_run_step()?;
        run_step_dispatch_command(run, step)
    }
    pub fn runs_detail(&self) -> bool {
        self.runs_detail
    }
    pub fn pending_approvals(&self) -> &[PendingApproval] {
        &self.pending_approvals
    }
    pub fn paused_routes(&self) -> usize {
        self.paused_routes
    }
    pub fn drawer(&self) -> Option<Drawer> {
        self.drawer.clone()
    }
    pub fn header_selected(&self) -> Option<usize> {
        self.header_selected
    }
    pub fn scroll(&self) -> usize {
        self.scroll
    }

    pub fn chat_selection(&self) -> Option<ChatSelection> {
        self.chat_selection
    }

    pub fn begin_chat_selection(&mut self, pos: (usize, usize)) {
        self.disarm_quit();
        self.chat_selection = Some(ChatSelection {
            start: pos,
            end: pos,
        });
    }

    pub fn update_chat_selection(&mut self, pos: (usize, usize)) {
        if let Some(selection) = self.chat_selection.as_mut() {
            selection.end = pos;
        }
    }

    pub fn clear_chat_selection(&mut self) {
        self.chat_selection = None;
    }
    pub fn composer(&self) -> &Composer {
        &self.composer
    }
    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    pub fn runtime_available(&self) -> bool {
        self.runtime_available
    }

    pub fn mark_runtime_unavailable(&mut self) {
        if !self.runtime_available {
            return;
        }
        let active_members = self
            .message_index
            .values()
            .map(|cell| cell.member.clone())
            .chain(self.tool_index.keys().map(|(member, _)| member.clone()))
            .collect::<HashSet<_>>();
        for member in active_members {
            self.finish_incomplete_cells_for_member(&member);
        }
        self.runtime_available = false;
        self.attach_pending = None;
        self.attach_request = None;
        self.paused_routes = 0;
        self.pending_approvals.clear();
        self.active_reasoning.clear();
        self.running_since.clear();
        for member in &mut self.members {
            if member_status_is_active(member.status) {
                member.status = MemberStatus::Failed;
            }
        }
        self.push(ChatItem::Error {
            member: None,
            message: "runtime stopped — input is disabled; press Ctrl+C to quit".to_string(),
        });
    }

    pub fn running_count(&self) -> usize {
        self.members
            .iter()
            .filter(|m| member_status_is_active(m.status))
            .count()
    }

    pub fn verification_active(&self) -> bool {
        self.runs
            .iter()
            .any(|run| run.status == crate::domain::event::RunStatus::Verifying)
    }

    pub fn has_cancelable_work(&self) -> bool {
        self.runtime_available
            && (self.running_count() > 0
                || self.verification_active()
                || self.paused_routes > 0
                || !self.pending_approvals.is_empty()
                || self.attach_pending.is_some()
                || self.attach_request.is_some())
    }

    pub fn first_pending_approval(&self) -> Option<ApprovalId> {
        self.pending_approvals.first().map(|a| a.id)
    }

    /// Request attaching to the member at `idx`'s live backend session. The
    /// synchronous attach pauses event consumption, so it is disabled while
    /// any runtime work could still produce events.
    pub fn request_attach(&mut self, idx: usize) -> Option<MemberId> {
        let Some(member) = self.members.get(idx).map(|member| member.id.clone()) else {
            self.header_selected = None;
            return None;
        };
        self.request_attach_member(member)
    }

    /// Request a named member's existing native interactive session.
    pub(crate) fn request_attach_member_by_name(
        &mut self,
        requested: &MemberId,
    ) -> Option<MemberId> {
        let member = self.resolve_member_id(requested)?;
        self.request_attach_member(member)
    }

    fn request_attach_member(&mut self, member: MemberId) -> Option<MemberId> {
        self.disarm_quit();
        if !self.runtime_available {
            self.push(ChatItem::Notice {
                text: "Cannot attach because the runtime has stopped.".to_string(),
            });
            self.header_selected = None;
            return None;
        }
        if self.attach_pending.is_some() || self.attach_request.is_some() {
            self.push(ChatItem::Notice {
                text: "An attach request is already pending.".to_string(),
            });
            self.header_selected = None;
            return None;
        }
        if self.has_cancelable_work() {
            self.push(ChatItem::Notice {
                text: "Cannot attach while member work, verification, routing, or approval is active — press Esc to cancel it or resolve it first."
                    .to_string(),
            });
            self.header_selected = None;
            return None;
        }
        self.attach_pending = Some(member.clone());
        self.header_selected = None;
        Some(member)
    }

    pub fn attach_request_send_failed(&mut self) {
        self.attach_pending = None;
    }

    /// Cancel an attach that has not yet handed the terminal to the child CLI.
    /// The runtime reservation is released by the caller. If a grant races
    /// with this cancellation, the unexpected-grant path schedules a second,
    /// idempotent release when the event arrives.
    pub fn cancel_pending_attach(&mut self) -> Option<MemberId> {
        let member = self
            .attach_pending
            .take()
            .or_else(|| self.attach_request.take().map(|request| request.member));
        if let Some(member) = &member {
            self.push(ChatItem::Notice {
                text: format!("cancelled attach request for {member}"),
            });
        }
        member
    }

    pub fn take_attach_release_pending(&mut self) -> Option<MemberId> {
        self.attach_release_pending.take()
    }

    pub fn take_attach_request(&mut self) -> Option<AttachRequest> {
        self.attach_request.take()
    }

    // --- composer editing (each edit resets the completion popup) --------

    fn member_ids(&self) -> Vec<String> {
        self.members.iter().map(|m| m.id.to_string()).collect()
    }

    /// The active completion popup for the current composer text, if any.
    pub fn completion(&self) -> Option<Completion> {
        if self.popup_dismissed {
            return None;
        }
        let member_backends = self
            .members
            .iter()
            .flat_map(|member| {
                [
                    (member.id.to_string(), member.backend),
                    (member.display_name.clone(), member.backend),
                ]
            })
            .collect::<HashMap<_, _>>();
        let skills = self
            .skills
            .iter()
            .map(|skill| AgentSkill {
                name: skill.name.clone(),
                description: skill.description.clone(),
                backend: skill.backend,
                invocation: skill.invocation.clone(),
            })
            .collect::<Vec<_>>();
        completion::compute_with_agent_skills(
            &self.composer.head(),
            &self.member_ids(),
            &skills,
            &member_backends,
        )
    }

    pub fn popup_selected(&self) -> usize {
        self.popup_selected
    }

    fn reset_popup(&mut self) {
        self.popup_selected = 0;
        self.popup_dismissed = false;
    }

    pub(crate) fn disarm_quit(&mut self) {
        self.quit_armed = false;
    }

    pub fn insert_char(&mut self, ch: char) {
        self.disarm_quit();
        self.header_selected = None;
        self.composer.insert(ch);
        self.history_cursor = None;
        self.reset_popup();
    }
    pub fn insert_text(&mut self, text: &str) {
        self.disarm_quit();
        self.header_selected = None;
        // Bound before newline normalization so a giant paste does not create
        // another full-size temporary allocation on the UI thread.
        let mut end = text.len().min(MAX_COMPOSER_BYTES);
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        let pre_truncated = end < text.len();
        let text = text[..end].replace("\r\n", "\n").replace('\r', "\n");
        let fully_inserted = self.composer.insert_text(&text) && !pre_truncated;
        if !fully_inserted
            && !matches!(
                self.chat.last(),
                Some(ChatItem::Notice { text }) if text == COMPOSER_INPUT_TRUNCATED
            )
        {
            self.push(ChatItem::Notice {
                text: COMPOSER_INPUT_TRUNCATED.to_string(),
            });
        }
        self.history_cursor = None;
        self.reset_popup();
    }
    pub fn insert_newline(&mut self) {
        self.disarm_quit();
        self.header_selected = None;
        self.composer.insert_newline();
        self.history_cursor = None;
        self.reset_popup();
    }
    /// Move the cursor up within a multi-line composer; returns false if it is
    /// already on the first line (so the caller recalls history instead).
    pub fn composer_up(&mut self) -> bool {
        self.disarm_quit();
        self.composer.up()
    }
    /// Move the cursor down within a multi-line composer; returns false if it is
    /// already on the last line.
    pub fn composer_down(&mut self) -> bool {
        self.disarm_quit();
        self.composer.down()
    }
    pub fn backspace(&mut self) {
        self.disarm_quit();
        self.header_selected = None;
        self.composer.backspace();
        self.history_cursor = None;
        self.reset_popup();
    }
    pub fn delete_word(&mut self) {
        self.disarm_quit();
        self.header_selected = None;
        self.composer.delete_word();
        self.history_cursor = None;
        self.reset_popup();
    }
    pub fn clear_composer(&mut self) {
        self.disarm_quit();
        self.header_selected = None;
        self.composer.clear();
        self.history_cursor = None;
        self.reset_popup();
    }
    pub fn cursor_left(&mut self) {
        self.disarm_quit();
        self.composer.left();
        self.reset_popup();
    }
    pub fn cursor_right(&mut self) {
        self.disarm_quit();
        self.composer.right();
        self.reset_popup();
    }
    pub fn cursor_home(&mut self) {
        self.disarm_quit();
        self.composer.home();
        self.reset_popup();
    }
    pub fn cursor_end(&mut self) {
        self.disarm_quit();
        self.composer.end();
        self.reset_popup();
    }
    pub fn take_composer(&mut self) -> String {
        self.disarm_quit();
        let text = self.composer.take();
        self.history_cursor = None;
        self.reset_popup();
        text
    }

    pub fn popup_up(&mut self) {
        self.disarm_quit();
        self.popup_selected = self.popup_selected.saturating_sub(1);
    }
    pub fn popup_down(&mut self) {
        self.disarm_quit();
        if let Some(completion) = self.completion()
            && self.popup_selected + 1 < completion.items.len()
        {
            self.popup_selected += 1;
        }
    }
    pub fn dismiss_popup(&mut self) {
        self.disarm_quit();
        self.popup_dismissed = true;
    }

    /// Accept the highlighted completion. Returns true if the composer changed
    /// (false means the token already matched, so the caller should submit).
    pub fn accept_completion(&mut self) -> bool {
        let Some(completion) = self.completion() else {
            return false;
        };
        self.disarm_quit();
        let index = self.popup_selected.min(completion.items.len() - 1);
        let insert = completion.items[index].insert.clone();
        let before = self.composer.text();
        self.composer.replace_token(completion.token_start, &insert);
        self.reset_popup();
        self.composer.text() != before
    }

    // --- prompt history (shell-style ↑/↓ recall) ------------------------

    /// Record a submitted line into prompt history (skipping blanks and
    /// consecutive duplicates) and end any active browse.
    pub fn record_submission(&mut self, text: &str) {
        self.disarm_quit();
        let text = text.trim();
        if !text.is_empty() && self.prompt_history.last().map(String::as_str) != Some(text) {
            self.prompt_history
                .push(bounded_text(text, MAX_CHAT_ITEM_BYTES));
            trim_prompt_history(&mut self.prompt_history);
        }
        self.history_cursor = None;
        self.history_draft.clear();
    }

    /// Recall an older entry (↑). The first step saves the live draft and jumps
    /// to the newest entry; further steps walk backwards.
    pub fn history_prev(&mut self) {
        self.disarm_quit();
        if self.prompt_history.is_empty() {
            return;
        }
        let target = match self.history_cursor {
            None => {
                self.history_draft = self.composer.text();
                self.prompt_history.len() - 1
            }
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.history_cursor = Some(target);
        let text = self.prompt_history[target].clone();
        self.composer.set_text(&text);
        // Suppress the completion popup while browsing recalled commands.
        self.popup_dismissed = true;
        self.popup_selected = 0;
        self.header_selected = None;
    }

    /// Recall a newer entry (↓); stepping past the newest restores the draft.
    pub fn history_next(&mut self) {
        self.disarm_quit();
        let Some(i) = self.history_cursor else {
            return;
        };
        if i + 1 < self.prompt_history.len() {
            self.history_cursor = Some(i + 1);
            let text = self.prompt_history[i + 1].clone();
            self.composer.set_text(&text);
            self.popup_dismissed = true;
        } else {
            self.history_cursor = None;
            let draft = std::mem::take(&mut self.history_draft);
            self.composer.set_text(&draft);
        }
        self.popup_selected = 0;
        self.header_selected = None;
    }

    /// Whether the composer is currently showing a recalled history entry.
    pub fn browsing_history(&self) -> bool {
        self.history_cursor.is_some()
    }

    // --- reverse history search (Ctrl+R) --------------------------------

    pub fn in_history_search(&self) -> bool {
        self.history_search.is_some()
    }

    /// The active search as `(query, matched entry)` for rendering.
    pub fn history_search(&self) -> Option<(&str, Option<&str>)> {
        self.history_search.as_ref().map(|s| {
            (
                s.query.as_str(),
                s.match_idx.map(|i| self.prompt_history[i].as_str()),
            )
        })
    }

    pub fn start_history_search(&mut self) {
        self.disarm_quit();
        let match_idx = self.search_from("", None);
        self.history_search = Some(HistorySearch {
            query: String::new(),
            match_idx,
        });
        self.header_selected = None;
        self.popup_dismissed = true;
    }

    pub fn history_search_input(&mut self, ch: char) {
        self.disarm_quit();
        if let Some(mut search) = self.history_search.take() {
            search.query.push(ch);
            search.match_idx = self.search_from(&search.query, None);
            self.history_search = Some(search);
        }
    }

    pub fn history_search_backspace(&mut self) {
        self.disarm_quit();
        if let Some(mut search) = self.history_search.take() {
            search.query.pop();
            search.match_idx = self.search_from(&search.query, None);
            self.history_search = Some(search);
        }
    }

    /// Ctrl+R again: step to the next older match.
    pub fn history_search_again(&mut self) {
        self.disarm_quit();
        if let Some(mut search) = self.history_search.take() {
            let before = search.match_idx;
            if let Some(idx) = self.search_from(&search.query, before) {
                search.match_idx = Some(idx);
            }
            self.history_search = Some(search);
        }
    }

    /// Accept the current match into the composer and leave search.
    pub fn accept_history_search(&mut self) {
        self.disarm_quit();
        if let Some(search) = self.history_search.take()
            && let Some(idx) = search.match_idx
        {
            let text = self.prompt_history[idx].clone();
            self.composer.set_text(&text);
        }
        self.history_cursor = None;
    }

    pub fn cancel_history_search(&mut self) {
        self.disarm_quit();
        self.history_search = None;
    }

    // --- transcript search (`/find`) ------------------------------------

    /// Whether `/find` is active (including zero matches).
    pub fn find_active(&self) -> bool {
        self.find.is_some()
    }

    /// Active find as `(query, current 1-based, total)`. Out-of-range match
    /// indices (chat grew/shrank since `set_find`) are clamped in the counter.
    pub fn find(&self) -> Option<(&str, usize, usize)> {
        let find = self.find.as_ref()?;
        let valid: Vec<usize> = find
            .matches
            .iter()
            .copied()
            .filter(|&idx| idx < self.chat.len())
            .collect();
        let total = valid.len();
        let current = if total == 0 {
            0
        } else {
            // Prefer the stored position among still-valid matches.
            let preferred = find
                .matches
                .get(find.current)
                .copied()
                .filter(|&idx| idx < self.chat.len());
            match preferred {
                Some(idx) => valid.iter().position(|&i| i == idx).unwrap_or(0) + 1,
                None => (find.current.min(total - 1)) + 1,
            }
        };
        Some((find.query.as_str(), current, total))
    }

    /// Index into `chat` for the current find match, if any and still in range.
    pub fn find_current_chat_index(&self) -> Option<usize> {
        let find = self.find.as_ref()?;
        let idx = *find.matches.get(find.current)?;
        (idx < self.chat.len()).then_some(idx)
    }

    /// Set or clear transcript search. Empty/whitespace query clears to `None`.
    /// Otherwise case-insensitive substring match; `current` starts at the last
    /// (newest) match and the view jumps there.
    pub fn set_find(&mut self, query: &str) {
        self.disarm_quit();
        let query = query.trim();
        if query.is_empty() {
            self.find = None;
            return;
        }
        let needle = query.to_lowercase();
        let matches: Vec<usize> = self
            .chat
            .iter()
            .enumerate()
            .filter(|(_, item)| chat_item_search_text(item).to_lowercase().contains(&needle))
            .map(|(i, _)| i)
            .collect();
        let current = matches.len().saturating_sub(1);
        self.find = Some(FindState {
            query: query.to_string(),
            matches,
            current,
        });
        if let Some(idx) = self.find_current_chat_index() {
            self.scroll_to_chat_item(idx);
        }
    }

    pub fn find_next(&mut self) {
        self.disarm_quit();
        let idx = {
            let Some(find) = self.find.as_mut() else {
                return;
            };
            if find.matches.is_empty() {
                return;
            }
            find.current = (find.current + 1) % find.matches.len();
            find.matches[find.current]
        };
        if idx < self.chat.len() {
            self.scroll_to_chat_item(idx);
        }
    }

    pub fn find_prev(&mut self) {
        self.disarm_quit();
        let idx = {
            let Some(find) = self.find.as_mut() else {
                return;
            };
            if find.matches.is_empty() {
                return;
            }
            find.current = if find.current == 0 {
                find.matches.len() - 1
            } else {
                find.current - 1
            };
            find.matches[find.current]
        };
        if idx < self.chat.len() {
            self.scroll_to_chat_item(idx);
        }
    }

    pub fn clear_find(&mut self) {
        self.disarm_quit();
        self.find = None;
    }

    /// Approximate scroll so chat item `idx` sits near the bottom of a
    /// bottom-anchored view. Uses the same line estimate as scroll pinning when
    /// new items arrive (approximate: ignores wrap width and markdown layout).
    pub fn scroll_to_chat_item(&mut self, idx: usize) {
        if idx >= self.chat.len() {
            return;
        }
        self.scroll = self.chat[idx.saturating_add(1)..]
            .iter()
            .map(estimate_item_lines)
            .sum();
    }

    /// Newest history entry containing `query` (case-insensitive) strictly older
    /// than `before` (or the newest overall when `before` is `None`). An empty
    /// query matches the newest available entry.
    fn search_from(&self, query: &str, before: Option<usize>) -> Option<usize> {
        if self.prompt_history.is_empty() {
            return None;
        }
        let needle = query.to_lowercase();
        let start = match before {
            Some(0) => return None,
            Some(i) => i - 1,
            None => self.prompt_history.len() - 1,
        };
        (0..=start)
            .rev()
            .find(|&i| self.prompt_history[i].to_lowercase().contains(&needle))
    }

    // --- UI actions -----------------------------------------------------

    pub fn toggle_drawer(&mut self, drawer: Drawer) {
        self.disarm_quit();
        self.drawer = if self.drawer.as_ref() == Some(&drawer) {
            self.stash_team_editor_catalog();
            None
        } else {
            self.stash_team_editor_catalog();
            match drawer {
                Drawer::Team => self.open_team_editor(),
                Drawer::Runs => {
                    self.ensure_selected_run();
                }
                _ => {}
            }
            Some(drawer)
        };
        self.drawer_scroll = 0;
    }

    #[cfg(test)]
    pub fn set_skills(&mut self, skills: Vec<SkillInfo>) {
        self.skills = skills;
    }

    /// Keep the old convenient `@codex /skill` spelling only when it matches
    /// a discovered Codex skill. Unknown slash commands and native controls
    /// remain untouched; the runtime must not guess from a leading slash.
    pub(crate) fn normalize_known_skill_invocation(&self, command: UiCommand) -> UiCommand {
        let UiCommand::UserMessage {
            target: MessageTarget::Member(member),
            mut body,
        } = command
        else {
            return command;
        };
        let Some(view) = self.members.iter().find(|view| view.id == member) else {
            return UiCommand::UserMessage {
                target: MessageTarget::Member(member),
                body,
            };
        };
        if view.backend != BackendKind::Codex {
            return UiCommand::UserMessage {
                target: MessageTarget::Member(member),
                body,
            };
        }

        let prefix = format!("@{member}");
        let Some(after_member) = body.strip_prefix(&prefix) else {
            return UiCommand::UserMessage {
                target: MessageTarget::Member(member),
                body,
            };
        };
        let Some(after_slash) = after_member.trim_start().strip_prefix('/') else {
            return UiCommand::UserMessage {
                target: MessageTarget::Member(member),
                body,
            };
        };
        let end = after_slash
            .find(char::is_whitespace)
            .unwrap_or(after_slash.len());
        let name = &after_slash[..end];
        if name.is_empty() {
            return UiCommand::UserMessage {
                target: MessageTarget::Member(member),
                body,
            };
        }
        let invocation = format!("${name}");
        let Some(skill) = self
            .skills
            .iter()
            .find(|skill| skill.backend == BackendKind::Codex && skill.invocation == invocation)
        else {
            return UiCommand::UserMessage {
                target: MessageTarget::Member(member),
                body,
            };
        };
        body = format!("{prefix} {}{}", skill.invocation, &after_slash[end..]);
        UiCommand::UserMessage {
            target: MessageTarget::Member(member),
            body,
        }
    }

    pub fn resume_choices(&self) -> &[ConversationSummary] {
        &self.resume_choices
    }

    pub fn selected_resume(&self) -> usize {
        self.selected_resume
    }

    pub fn select_previous_resume(&mut self) {
        self.selected_resume = self.selected_resume.saturating_sub(1);
        self.drawer_scroll = self.selected_resume.saturating_mul(3);
    }

    pub fn select_next_resume(&mut self) {
        if self.selected_resume + 1 < self.resume_choices.len() {
            self.selected_resume += 1;
            self.drawer_scroll = self.selected_resume.saturating_mul(3);
        }
    }

    pub fn selected_resume_command(&self) -> Option<UiCommand> {
        (self.drawer == Some(Drawer::Resume))
            .then(|| self.resume_choices.get(self.selected_resume))
            .flatten()
            .map(|conversation| UiCommand::ResumeConversation {
                conversation: conversation.id,
            })
    }

    pub fn close_drawer(&mut self) {
        self.disarm_quit();
        self.drawer = None;
        self.drawer_scroll = 0;
        self.stash_team_editor_catalog();
    }

    pub fn stage_selected_run_action(&mut self) -> bool {
        if self.drawer != Some(Drawer::Runs) || !self.composer.is_empty() {
            return false;
        }
        let Some(command) = self.selected_run_stage_command() else {
            return false;
        };
        self.disarm_quit();
        self.header_selected = None;
        self.composer.set_text(&command);
        self.history_cursor = None;
        self.reset_popup();
        self.close_drawer();
        true
    }

    pub fn stage_selected_run_dispatch(&mut self) -> bool {
        if self.drawer != Some(Drawer::Runs) || !self.composer.is_empty() {
            return false;
        }
        let Some(command) = self.selected_run_dispatch_command() else {
            return false;
        };
        self.disarm_quit();
        self.header_selected = None;
        self.composer.set_text(&command);
        self.history_cursor = None;
        self.reset_popup();
        self.close_drawer();
        true
    }

    pub fn toggle_runs_detail(&mut self) -> bool {
        if self.drawer != Some(Drawer::Runs) || !self.composer.is_empty() {
            return false;
        }
        self.disarm_quit();
        self.runs_detail = !self.runs_detail;
        self.drawer_scroll = 0;
        true
    }

    pub fn select_newer_run(&mut self) {
        if self.drawer != Some(Drawer::Runs) {
            return;
        }
        self.disarm_quit();
        self.selected_run_step = None;
        self.ensure_selected_run();
        let Some(id) = self.selected_run else {
            return;
        };
        let Some(index) = self.run_index(id) else {
            return;
        };
        if index + 1 < self.runs.len() {
            self.selected_run = Some(self.runs[index + 1].id);
        }
    }

    pub fn select_older_run(&mut self) {
        if self.drawer != Some(Drawer::Runs) {
            return;
        }
        self.disarm_quit();
        self.selected_run_step = None;
        self.ensure_selected_run();
        let Some(id) = self.selected_run else {
            return;
        };
        let Some(index) = self.run_index(id) else {
            return;
        };
        if index > 0 {
            self.selected_run = Some(self.runs[index - 1].id);
        }
    }

    pub fn select_previous_run_step(&mut self) -> bool {
        if self.drawer != Some(Drawer::Runs) {
            return false;
        }
        self.disarm_quit();
        let Some(run) = self.selected_run() else {
            self.selected_run_step = None;
            return false;
        };
        if run.steps.is_empty() {
            self.selected_run_step = None;
            return false;
        }
        let next = match self.selected_run_step() {
            None => run.steps.last().map(|step| step.number),
            Some(number) => run
                .steps
                .iter()
                .position(|step| step.number == number)
                .and_then(|idx| idx.checked_sub(1))
                .and_then(|idx| run.steps.get(idx))
                .map(|step| step.number),
        }
        .or_else(|| run.steps.first().map(|step| step.number));
        self.selected_run_step = next;
        true
    }

    pub fn select_next_run_step(&mut self) -> bool {
        if self.drawer != Some(Drawer::Runs) {
            return false;
        }
        self.disarm_quit();
        let Some(run) = self.selected_run() else {
            self.selected_run_step = None;
            return false;
        };
        if run.steps.is_empty() {
            self.selected_run_step = None;
            return false;
        }
        let next = match self.selected_run_step() {
            None => run.steps.first().map(|step| step.number),
            Some(number) => run
                .steps
                .iter()
                .position(|step| step.number == number)
                .and_then(|idx| run.steps.get(idx + 1))
                .map(|step| step.number),
        }
        .or_else(|| run.steps.last().map(|step| step.number));
        self.selected_run_step = next;
        true
    }

    fn ensure_selected_run(&mut self) {
        let selected_is_valid = self
            .selected_run
            .is_some_and(|id| self.runs.iter().any(|run| run.id == id));
        if !selected_is_valid {
            self.selected_run = self.runs.last().map(|run| run.id);
            self.selected_run_step = None;
        }
    }

    fn ensure_selected_run_step(&mut self) {
        let Some(step) = self.selected_run_step else {
            return;
        };
        let step_is_valid = self
            .selected_run()
            .is_some_and(|run| run.steps.iter().any(|candidate| candidate.number == step));
        if !step_is_valid {
            self.selected_run_step = None;
        }
    }

    fn run_index(&self, id: RunId) -> Option<usize> {
        self.runs.iter().position(|run| run.id == id)
    }

    /// The drawer's vertical scroll offset (top line to show).
    pub fn drawer_scroll(&self) -> usize {
        self.drawer_scroll
    }
    pub fn drawer_scroll_up(&mut self) {
        self.drawer_scroll_by(-1);
    }
    pub fn drawer_scroll_down(&mut self) {
        self.drawer_scroll_by(1);
    }
    pub fn drawer_scroll_by(&mut self, delta: i32) {
        self.disarm_quit();
        if delta >= 0 {
            self.drawer_scroll = self.drawer_scroll.saturating_add(delta as usize);
        } else {
            self.drawer_scroll = self
                .drawer_scroll
                .saturating_sub(delta.unsigned_abs() as usize);
        }
    }

    /// The captured working-tree diff shown in the diff drawer.
    pub fn diff_text(&self) -> Option<&str> {
        self.diff_text.as_deref()
    }
    pub fn set_diff(&mut self, diff: String) {
        self.diff_text = Some(diff);
    }

    pub(crate) fn team_editor(&self) -> Option<&TeamEditor> {
        self.team_editor.as_ref()
    }

    pub(crate) fn handle_team_editor_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> TeamEditorOutcome {
        if self.drawer != Some(Drawer::Team) {
            return TeamEditorOutcome::Ignored;
        }
        let Some(editor) = self.team_editor.as_mut() else {
            return TeamEditorOutcome::Ignored;
        };
        editor.handle_key(code, modifiers)
    }

    pub(crate) fn insert_team_editor_text(&mut self, text: &str) -> bool {
        if self.drawer != Some(Drawer::Team) {
            return false;
        }
        self.team_editor
            .as_mut()
            .is_some_and(|editor| editor.insert_edit_text(text))
    }

    pub(crate) fn poll_team_editor_catalog(&mut self) {
        if let Some(editor) = self.team_editor.as_mut() {
            editor.poll_agent_catalog();
        } else {
            // A model worker may still be completing after its picker closed.
            // Poll it here so the next `/team` open can use the ready result.
            self.model_catalog.poll();
        }
    }

    /// Warm every installed backend for the startup workspace. Detection and
    /// model discovery both stay asynchronous: opening Asterline never waits
    /// for a slow CLI, while a later `/team` reuses the same keyed cache.
    pub(crate) fn warm_model_catalog_once(&mut self) {
        if self.model_catalog_warmed || self.workspace.trim().is_empty() {
            return;
        }
        if self.model_catalog_detection.is_none() {
            let (tx, rx) = mpsc::channel();
            thread::spawn(move || {
                let _ = tx.send(detect_backends());
            });
            self.model_catalog_detection = Some(rx);
            return;
        }
        let detected = match self
            .model_catalog_detection
            .as_ref()
            .map(Receiver::try_recv)
        {
            Some(Ok(detected)) => detected,
            Some(Err(TryRecvError::Empty)) | None => return,
            Some(Err(TryRecvError::Disconnected)) => DetectedBackends {
                codex: false,
                claude: false,
                grok: false,
                agy: false,
            },
        };
        self.model_catalog_detection = None;
        let workspace = PathBuf::from(&self.workspace);
        if let Some(editor) = self.team_editor.as_mut() {
            editor.preload_installed_model_catalogs(detected);
        } else {
            for backend in [
                BackendKind::Codex,
                BackendKind::Claude,
                BackendKind::Grok,
                BackendKind::Agy,
            ] {
                if detected.contains(backend) {
                    self.model_catalog.preload(backend, &workspace);
                }
            }
            self.model_catalog.freeze();
        }
        self.model_catalog_warmed = true;
    }

    fn open_team_editor(&mut self) {
        let members = self
            .members
            .iter()
            .map(|view| self.view_to_member(view))
            .collect();
        let model_catalog = std::mem::take(&mut self.model_catalog);
        let mut editor = TeamEditor::with_model_catalog(
            self.team.clone(),
            PathBuf::from(self.workspace.clone()),
            self.default_target.clone(),
            members,
            model_catalog,
        );
        editor.load_agent_catalog();
        self.team_editor = Some(editor);
    }

    fn stash_team_editor_catalog(&mut self) {
        if let Some(editor) = self.team_editor.take() {
            self.model_catalog = editor.into_model_catalog();
        }
    }

    fn view_to_member(&self, view: &MemberView) -> TeamMember {
        let mut member = TeamMember::new(
            view.id.clone(),
            view.display_name.clone(),
            view.backend,
            view.role.clone(),
        );
        member.cwd = if view.cwd.is_empty() || view.cwd == self.workspace {
            None
        } else {
            Some(PathBuf::from(&view.cwd))
        };
        member.model = view.model.clone();
        member.sandbox = view.sandbox;
        member.permission_mode = view.permission_mode;
        member.session_policy = view.session_policy;
        member.session_id = view.session.clone();
        member.effort = view.effort;
        member
    }

    pub fn select_next_member(&mut self) {
        self.disarm_quit();
        let len = self.members.len();
        if len == 0 {
            return;
        }
        let next_idx = match self.header_selected {
            None => 0,
            Some(idx) => (idx + 1) % len,
        };
        self.header_selected = Some(next_idx);
        self.reset_popup();
    }

    pub fn select_prev_member(&mut self) {
        self.disarm_quit();
        let len = self.members.len();
        if len == 0 {
            return;
        }
        let prev_idx = match self.header_selected {
            None => len - 1,
            Some(idx) => (idx + len - 1) % len,
        };
        self.header_selected = Some(prev_idx);
        self.reset_popup();
    }

    pub fn clear_header_selection(&mut self) {
        self.disarm_quit();
        self.header_selected = None;
    }

    pub fn scroll_up(&mut self) {
        self.scroll_by(1);
    }

    pub fn scroll_down(&mut self) {
        self.scroll_by(-1);
    }

    pub fn scroll_by(&mut self, delta: i32) {
        self.disarm_quit();
        if delta >= 0 {
            self.scroll = self.scroll.saturating_add(delta as usize);
        } else {
            self.scroll = self.scroll.saturating_sub(delta.unsigned_abs() as usize);
        }
    }

    pub fn reset_scroll(&mut self) {
        self.disarm_quit();
        self.scroll = 0;
    }

    pub fn quit(&mut self) {
        self.quit_armed = false;
        self.should_quit = true;
    }

    pub fn request_quit(&mut self) {
        if self.quit_armed {
            self.quit();
        } else {
            self.quit_armed = true;
            self.push(ChatItem::Notice {
                text: "press Ctrl+C again to quit".to_string(),
            });
        }
    }

    pub fn tools_expanded(&self) -> bool {
        self.tools_expanded
    }

    pub fn toggle_tools_expansion(&mut self) {
        self.disarm_quit();
        self.tools_expanded = !self.tools_expanded;
    }

    pub fn active_reasoning(&self) -> &HashMap<MemberId, String> {
        &self.active_reasoning
    }

    /// How long `member` has been running, for the "working" elapsed timer.
    pub fn member_elapsed_secs(&self, member: &MemberId) -> Option<u64> {
        self.running_since
            .get(member)
            .map(|t| t.elapsed().as_secs())
    }
}

pub(crate) fn run_action_command(
    run: &RunSummary,
    workspace: &str,
    include_run_id: bool,
) -> Option<String> {
    match run.status {
        RunStatus::Running | RunStatus::Verifying => None,
        RunStatus::Done if run.verification.is_none() => {
            let workspace = if workspace.is_empty() {
                Path::new(".")
            } else {
                Path::new(workspace)
            };
            let mut command = verify_command_prefix(run, include_run_id);
            if let Some(check) = suggested_verify_command(workspace) {
                command.push(' ');
                command.push_str(check);
            }
            Some(command)
        }
        RunStatus::Done => run
            .mode
            .as_ref()
            .map(|mode| format!("/mode {}", mode.mode.as_str()))
            .or_else(|| Some("/mode plan".to_string())),
        RunStatus::Failed if run.verification.is_some() => {
            let mut command = continue_command_prefix(run, include_run_id);
            command.push_str(" fix failing verification");
            Some(command)
        }
        RunStatus::Failed => Some(continue_command_prefix(run, include_run_id)),
        RunStatus::Blocked => {
            let mut command = continue_command_prefix(run, include_run_id);
            command.push_str(" blocker resolved");
            Some(command)
        }
        RunStatus::Planned => Some("/retry".to_string()),
    }
}

fn verify_command_prefix(run: &RunSummary, include_run_id: bool) -> String {
    if include_run_id {
        format!("/verify {}", run.id)
    } else {
        "/verify".to_string()
    }
}

fn continue_command_prefix(run: &RunSummary, include_run_id: bool) -> String {
    if include_run_id {
        format!("/continue {}", run.id)
    } else {
        "/continue".to_string()
    }
}

fn run_step_action_command(run: &RunSummary, step: u32) -> Option<String> {
    let step = run
        .steps
        .iter()
        .find(|candidate| candidate.number == step)?;
    let (action, note) = match step.status {
        RunStepStatus::Todo => ("doing", None),
        RunStepStatus::Doing => ("done", None),
        RunStepStatus::Blocked => ("doing", Some("blocker resolved")),
        RunStepStatus::Done => ("todo", Some("reopen")),
    };
    let mut command = format!("/step {action} {} {}", run.id, step.number);
    if let Some(note) = note {
        command.push(' ');
        command.push_str(note);
    }
    Some(command)
}

fn run_step_dispatch_command(run: &RunSummary, step: u32) -> Option<String> {
    let step = run
        .steps
        .iter()
        .find(|candidate| candidate.number == step)?;
    let Some(owner) = &step.owner else {
        return Some(format!("/step assign {} {} ", run.id, step.number));
    };

    let instruction = match step.status {
        RunStepStatus::Todo => "Start",
        RunStepStatus::Doing => "Continue",
        RunStepStatus::Blocked => "Revisit blocked",
        RunStepStatus::Done => "Review completed",
    };
    Some(format!(
        "@{owner} {}",
        crate::runtime::mode_prompts::manual_step_dispatch_text(
            run.id,
            instruction,
            step.number,
            &step.title,
        )
    ))
}

fn format_inherited_user_body(target: &MessageTarget, body: &str) -> String {
    match target {
        MessageTarget::All => format!("@all {body}"),
        MessageTarget::Member(member) => format!("@{member} {body}"),
        MessageTarget::Default | MessageTarget::Members(_) => body.to_string(),
    }
}

/// Searchable text for one chat item (used by `/find`).
fn chat_item_search_text(item: &ChatItem) -> String {
    match item {
        ChatItem::User { body, .. } => body.clone(),
        ChatItem::Agent { text, .. } => text.clone(),
        ChatItem::Tool {
            name,
            summary,
            detail,
            ..
        } => format!("{name} {summary} {detail}"),
        ChatItem::Diff { files, .. } => files
            .iter()
            .map(|(path, _)| path.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        ChatItem::Route { body, .. } => body.clone(),
        ChatItem::Notice { text } => text.clone(),
        ChatItem::Error { message, .. } => message.clone(),
        ChatItem::Verdict { summary, .. } => summary.clone(),
    }
}

/// Rough estimate of how many visual lines a [`ChatItem`] will occupy, used
/// only to keep the scroll position stable when new items arrive while the
/// user is browsing history, and for approximate `/find` jumps. The exact
/// count depends on the render width and markdown layout, so this is a
/// conservative lower bound.
fn estimate_item_lines(item: &ChatItem) -> usize {
    match item {
        ChatItem::User { body, .. } => body.lines().count().max(1),
        ChatItem::Agent { text, .. } => {
            if text.is_empty() {
                1 // header only
            } else {
                text.lines().count().max(1) + 1 // +1 for header
            }
        }
        ChatItem::Tool { detail, .. } => 1 + usize::from(!detail.is_empty()),
        ChatItem::Diff { files, .. } => 1 + files.len(),
        ChatItem::Route { body, .. } => 1 + body.lines().count().max(1),
        ChatItem::Notice { text } => text.lines().count().max(1),
        ChatItem::Error { message, .. } => message.lines().count().max(1),
        ChatItem::Verdict { summary, .. } => 1 + summary.lines().count(),
    }
}

fn chat_budget_fits(
    retained_items: usize,
    retained_bytes: usize,
    incoming_items: usize,
    incoming_bytes: usize,
) -> bool {
    retained_items.saturating_add(incoming_items) <= MAX_CHAT_ITEMS
        && retained_bytes.saturating_add(incoming_bytes) <= MAX_CHAT_BYTES
}

fn chat_truncation_notice() -> ChatItem {
    ChatItem::Notice {
        text: "[asterline: chat item truncated]".to_string(),
    }
}

fn bounded_chat_field(text: String, fixed_bytes: usize) -> Option<String> {
    let limit = MAX_CHAT_ITEM_BYTES.checked_sub(fixed_bytes)?;
    (limit >= MIN_BOUNDED_CHAT_TEXT_BYTES).then(|| bounded_text(&text, limit))
}

/// Bound one hot-cache item independently from the larger persisted replay
/// limits. Composite metadata that leaves no useful text budget degrades to a
/// visible notice, ensuring every variant obeys the same hard ceiling.
fn bound_chat_item(item: ChatItem) -> ChatItem {
    let bounded = match item {
        ChatItem::User {
            body,
            targets,
            interrupted,
        } => bounded_chat_field(body, 0).map(|body| ChatItem::User {
            body,
            targets,
            interrupted,
        }),
        ChatItem::Agent {
            member,
            display_name,
            backend,
            text,
        } => {
            let fixed = member.as_str().len().saturating_add(display_name.len());
            bounded_chat_field(text, fixed).map(|text| ChatItem::Agent {
                member,
                display_name,
                backend,
                text,
            })
        }
        ChatItem::Tool {
            member,
            name,
            summary,
            detail,
            ok,
        } => {
            let fixed = member
                .as_str()
                .len()
                .saturating_add(name.len())
                .saturating_add(summary.len());
            bounded_chat_field(detail, fixed).map(|detail| ChatItem::Tool {
                member,
                name,
                summary,
                detail,
                ok,
            })
        }
        ChatItem::Diff { member, files, ok } => (member.as_str().len().saturating_add(
            files
                .iter()
                .map(|(path, kind)| path.len().saturating_add(kind.len()))
                .sum::<usize>(),
        ) <= MAX_CHAT_ITEM_BYTES)
            .then_some(ChatItem::Diff { member, files, ok }),
        ChatItem::Route { from, to, body } => {
            let fixed = from
                .as_str()
                .len()
                .saturating_add(to.iter().map(String::len).sum::<usize>());
            bounded_chat_field(body, fixed).map(|body| ChatItem::Route { from, to, body })
        }
        ChatItem::Notice { text } => {
            bounded_chat_field(text, 0).map(|text| ChatItem::Notice { text })
        }
        ChatItem::Error { member, message } => {
            let fixed = member.as_ref().map_or(0, |member| member.as_str().len());
            bounded_chat_field(message, fixed).map(|message| ChatItem::Error { member, message })
        }
        ChatItem::Verdict {
            member,
            approve,
            summary,
        } => {
            let fixed = member.as_str().len();
            bounded_chat_field(summary, fixed).map(|summary| ChatItem::Verdict {
                member,
                approve,
                summary,
            })
        }
    };
    let bounded = bounded.unwrap_or_else(chat_truncation_notice);
    debug_assert!(chat_item_bytes(&bounded) <= MAX_CHAT_ITEM_BYTES);
    bounded
}

fn chat_item_bytes(item: &ChatItem) -> usize {
    match item {
        ChatItem::User { body, .. } => body.len(),
        ChatItem::Agent {
            member,
            display_name,
            text,
            ..
        } => member.as_str().len() + display_name.len() + text.len(),
        ChatItem::Tool {
            member,
            name,
            summary,
            detail,
            ..
        } => member.as_str().len() + name.len() + summary.len() + detail.len(),
        ChatItem::Diff { member, files, .. } => {
            member.as_str().len()
                + files
                    .iter()
                    .map(|(path, kind)| path.len() + kind.len())
                    .sum::<usize>()
        }
        ChatItem::Route { from, to, body } => {
            from.as_str().len() + body.len() + to.iter().map(String::len).sum::<usize>()
        }
        ChatItem::Notice { text } => text.len(),
        ChatItem::Error { member, message } => {
            member.as_ref().map_or(0, |member| member.as_str().len()) + message.len()
        }
        ChatItem::Verdict {
            member, summary, ..
        } => member.as_str().len() + summary.len(),
    }
}

fn trim_initial_chat(chat: &mut Vec<ChatItem>) {
    for item in chat.iter_mut() {
        let original = std::mem::replace(item, chat_truncation_notice());
        *item = bound_chat_item(original);
    }
    let mut bytes = chat.iter().map(chat_item_bytes).sum::<usize>();
    let mut remove = 0;
    while remove < chat.len()
        && (chat.len().saturating_sub(remove) > MAX_CHAT_ITEMS || bytes > MAX_CHAT_BYTES)
    {
        bytes = bytes.saturating_sub(chat_item_bytes(&chat[remove]));
        remove += 1;
    }
    if remove > 0 {
        chat.drain(..remove);
        let notice = chat_truncation_summary();
        let notice_bytes = chat_item_bytes(&notice);
        while !chat_budget_fits(chat.len(), bytes, 1, notice_bytes) && !chat.is_empty() {
            bytes = bytes.saturating_sub(chat_item_bytes(&chat[0]));
            chat.remove(0);
        }
        chat.insert(0, notice);
        chat.shrink_to_fit();
    }
}

fn chat_truncation_summary() -> ChatItem {
    ChatItem::Notice {
        text: EARLIER_HISTORY_OMITTED.to_string(),
    }
}

fn trim_prompt_history(history: &mut Vec<String>) {
    for entry in history.iter_mut() {
        if entry.len() > MAX_CHAT_ITEM_BYTES {
            *entry = bounded_text(entry, MAX_CHAT_ITEM_BYTES);
        }
    }
    let mut bytes = history.iter().map(String::len).sum::<usize>();
    let mut remove = 0;
    while remove < history.len()
        && (history.len().saturating_sub(remove) > MAX_PROMPT_HISTORY_ITEMS
            || bytes > MAX_PROMPT_HISTORY_BYTES)
    {
        bytes = bytes.saturating_sub(history[remove].len());
        remove += 1;
    }
    if remove > 0 {
        history.drain(..remove);
        history.shrink_to_fit();
    }
}

fn compact_active_chat_item(item: &mut ChatItem) -> bool {
    match item {
        ChatItem::Agent { text, .. } if text != ACTIVE_MESSAGE_OUTPUT_OMITTED => {
            *text = ACTIVE_MESSAGE_OUTPUT_OMITTED.to_string();
            true
        }
        ChatItem::Tool { detail, .. } if detail != ACTIVE_TOOL_OUTPUT_OMITTED => {
            *detail = ACTIVE_TOOL_OUTPUT_OMITTED.to_string();
            true
        }
        _ => false,
    }
}

fn active_completion_text(marker: &str, text: &str, omitted: bool) -> String {
    if !omitted {
        return bounded_text(text, MAX_CHAT_ITEM_BYTES);
    }
    if text.is_empty() {
        marker.to_string()
    } else {
        let mut completed = format!("{marker}\n\n");
        let _ = append_bounded_text(&mut completed, text, MAX_CHAT_ITEM_BYTES);
        completed
    }
}

fn active_tool_key(id: &str) -> String {
    if id.len() <= MAX_ACTIVE_TOOL_ID_BYTES {
        return id.to_string();
    }
    format!("sha256:{:x}", Sha256::digest(id.as_bytes()))
}

fn interrupted_completion_text(
    omission_marker: &str,
    interrupted_marker: &str,
    partial: &str,
    omitted: bool,
) -> String {
    let mut completed = String::new();
    if omitted {
        completed.push_str(omission_marker);
        completed.push_str("\n\n");
    }
    completed.push_str(interrupted_marker);
    if !partial.is_empty() && partial != omission_marker {
        completed.push_str("\n\n");
        let _ = append_bounded_text(&mut completed, partial, MAX_CHAT_ITEM_BYTES);
    }
    completed
}

fn removed_prefix_counts(remove: &[bool]) -> Vec<usize> {
    let mut counts = Vec::with_capacity(remove.len() + 1);
    counts.push(0);
    for &removed in remove {
        counts.push(counts.last().copied().unwrap_or(0) + usize::from(removed));
    }
    counts
}

fn remap_index(index: usize, remove: &[bool], removed_before: &[usize]) -> Option<usize> {
    (!remove.get(index).copied().unwrap_or(true)).then(|| index - removed_before[index])
}

#[cfg(test)]
#[path = "app_state_tests.rs"]
mod tests;
