//! The TUI model. Every field is driven by `RuntimeEvent`s applied through
//! [`AppState::apply`]; the renderer reads it and the key handler mutates the
//! composer / drawer / scroll. No state is inferred from matching strings.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crossterm::event::{KeyCode, KeyModifiers};

use crate::domain::event::{
    ApprovalId, ChatItem, LogEntry, MemberStatus, MessageId, MessageTarget, RuntimeEvent,
    WorkflowRunId, WorkflowRunStatus, WorkflowRunSummary, WorkflowStepStatus,
};
use crate::domain::team::{
    BackendKind, DefaultTarget, Effort, MemberId, PermissionMode, SandboxPolicy, SessionPolicy,
    TeamMember,
};
use crate::tui::attach::AttachRequest;
use crate::tui::completion::{self, Completion};
use crate::tui::composer::Composer;
use crate::tui::drawers::Drawer;
use crate::tui::team_editor::{TeamEditor, TeamEditorOutcome};
use crate::workflow::suggested_verify_command;

const MAX_LOGS: usize = 4000;

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

pub struct AppState {
    team: String,
    workspace: String,
    default_target: Option<DefaultTarget>,
    members: Vec<MemberView>,
    chat: Vec<ChatItem>,
    message_index: HashMap<MessageId, usize>,
    tool_index: HashMap<String, usize>,
    logs: Vec<LogEntry>,
    workflow_runs: Vec<WorkflowRunSummary>,
    selected_workflow_run: Option<WorkflowRunId>,
    selected_workflow_step: Option<u32>,
    workflow_runs_detail: bool,
    pending_approvals: Vec<PendingApproval>,
    paused_routes: usize,
    composer: Composer,
    drawer: Option<Drawer>,
    scroll: usize,
    popup_selected: usize,
    popup_dismissed: bool,
    should_quit: bool,
    quit_armed: bool,
    tools_expanded: bool,
    active_reasoning: HashMap<MemberId, String>,
    pending_user_messages: Vec<String>,
    header_selected: Option<usize>,
    attach_request: Option<AttachRequest>,
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
    /// Vertical scroll offset for the open drawer (logs / team / diff).
    drawer_scroll: usize,
    /// Captured working-tree diff text for the diff drawer (`/diff`).
    diff_text: Option<String>,
    /// Editable draft shown by the `/team` drawer.
    team_editor: Option<TeamEditor>,
}

impl AppState {
    /// Create with replayed chat history (empty for a fresh session).
    pub fn new(chat: Vec<ChatItem>) -> Self {
        // Seed prompt history from prior user messages (cross-session recall),
        // collapsing consecutive duplicates the way a shell history does.
        let mut prompt_history: Vec<String> = Vec::new();
        for item in &chat {
            if let ChatItem::User { body } = item
                && prompt_history.last() != Some(body)
            {
                prompt_history.push(body.clone());
            }
        }
        Self {
            team: "Asterline".to_string(),
            workspace: String::new(),
            default_target: None,
            members: Vec::new(),
            chat,
            message_index: HashMap::new(),
            tool_index: HashMap::new(),
            logs: Vec::new(),
            workflow_runs: Vec::new(),
            selected_workflow_run: None,
            selected_workflow_step: None,
            workflow_runs_detail: false,
            pending_approvals: Vec::new(),
            paused_routes: 0,
            composer: Composer::new(),
            drawer: None,
            scroll: 0,
            popup_selected: 0,
            popup_dismissed: false,
            should_quit: false,
            quit_armed: false,
            tools_expanded: false,
            active_reasoning: HashMap::new(),
            pending_user_messages: Vec::new(),
            header_selected: None,
            attach_request: None,
            prompt_history,
            history_cursor: None,
            history_draft: String::new(),
            running_since: HashMap::new(),
            history_search: None,
            drawer_scroll: 0,
            diff_text: None,
            team_editor: None,
        }
    }

    // --- applying runtime events ----------------------------------------

    /// Seed the logs drawer with persisted entries replayed on startup, so logs
    /// survive a restart the way the chat transcript does.
    pub fn seed_logs(&mut self, mut logs: Vec<LogEntry>) {
        if logs.len() > MAX_LOGS {
            logs.drain(0..logs.len() - MAX_LOGS);
        }
        self.logs = logs;
    }

    pub fn apply(&mut self, event: RuntimeEvent) {
        match event {
            RuntimeEvent::Ready {
                team,
                workspace,
                default_target,
                members,
                workflow_runs,
            } => {
                self.team = team;
                self.workspace = workspace;
                self.default_target = default_target;
                self.workflow_runs = workflow_runs;
                self.ensure_selected_workflow_run();
                self.ensure_selected_workflow_step();
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
                    if member.status == MemberStatus::Running {
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
            RuntimeEvent::TurnStarted { .. } | RuntimeEvent::TurnFinished { .. } => {
                self.active_reasoning.clear();
            }
            RuntimeEvent::UserMessage { body, .. } => {
                if let Some(pos) = self.pending_user_messages.iter().position(|m| m == &body) {
                    self.pending_user_messages.remove(pos);
                } else {
                    self.push(ChatItem::User { body });
                }
            }
            RuntimeEvent::MemberStatus { member, status } => self.set_status(&member, status),
            RuntimeEvent::MemberEffort { member, effort } => {
                if let Some(view) = self.members.iter_mut().find(|m| m.id == member) {
                    view.effort = Some(effort);
                }
            }
            RuntimeEvent::MessageStarted { msg, member, .. } => {
                let (display_name, backend) = self.member_meta(&member);
                let idx = self.push(ChatItem::Agent {
                    member,
                    display_name,
                    backend,
                    text: String::new(),
                });
                self.message_index.insert(msg, idx);
            }
            RuntimeEvent::MessageDelta { msg, text } => {
                if let Some(&idx) = self.message_index.get(&msg)
                    && let Some(ChatItem::Agent { text: body, .. }) = self.chat.get_mut(idx)
                {
                    body.push_str(&text);
                }
            }
            RuntimeEvent::MessageCompleted { msg, text } => {
                if let Some(&idx) = self.message_index.get(&msg)
                    && let Some(ChatItem::Agent {
                        text: body, member, ..
                    }) = self.chat.get_mut(idx)
                {
                    *body = text;
                    let m = member.clone();
                    self.active_reasoning.remove(&m);
                    self.set_status(&m, MemberStatus::Idle);
                }
                self.message_index.remove(&msg);
            }
            RuntimeEvent::Reasoning { member, text } => {
                self.active_reasoning.insert(member, text);
            }
            RuntimeEvent::ToolStarted {
                member,
                tool_id,
                name,
                summary,
            } => {
                let idx = self.push(ChatItem::Tool {
                    member,
                    name,
                    summary,
                    ok: None,
                });
                self.tool_index.insert(tool_id, idx);
            }
            RuntimeEvent::ToolCompleted {
                member,
                tool_id,
                ok,
                summary,
            } => {
                if let Some(idx) = self.tool_index.remove(&tool_id)
                    && let Some(ChatItem::Tool {
                        ok: cell_ok,
                        summary: cell_summary,
                        ..
                    }) = self.chat.get_mut(idx)
                {
                    *cell_ok = Some(ok);
                    if !summary.is_empty() {
                        *cell_summary = summary;
                    }
                } else {
                    self.push(ChatItem::Tool {
                        member,
                        name: "tool".to_string(),
                        summary,
                        ok: Some(ok),
                    });
                }
            }
            RuntimeEvent::Route { from, to, body, .. } => {
                self.push(ChatItem::Route { from, to, body });
            }
            RuntimeEvent::FileChange { member, files } => {
                self.push(ChatItem::Diff { member, files });
            }
            RuntimeEvent::RouteError {
                from,
                target,
                reason,
                ..
            } => {
                self.push(ChatItem::Error {
                    member: Some(from),
                    message: format!("route to {target} failed: {reason}"),
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
            RuntimeEvent::SessionUpdated { member, session } => {
                if let Some(view) = self.members.iter_mut().find(|m| m.id == member) {
                    view.session = Some(session.0);
                }
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
            RuntimeEvent::WorkflowRunUpdated { run } => {
                if let Some(existing) = self.workflow_runs.iter_mut().find(|r| r.id == run.id) {
                    *existing = run;
                } else {
                    self.workflow_runs.push(run);
                }
                self.ensure_selected_workflow_run();
                self.ensure_selected_workflow_step();
            }
            RuntimeEvent::Log(entry) => {
                self.logs.push(entry);
                if self.logs.len() > MAX_LOGS {
                    self.logs.drain(0..self.logs.len() - MAX_LOGS);
                }
            }
            RuntimeEvent::Notice(text) => {
                self.push(ChatItem::Notice { text });
            }
            RuntimeEvent::SessionReset => {
                // Begin a fresh chat: clear the transcript and in-flight cells,
                // but keep members, logs, and prompt history.
                self.chat.clear();
                self.message_index.clear();
                self.tool_index.clear();
                self.active_reasoning.clear();
                self.pending_user_messages.clear();
                self.running_since.clear();
                self.scroll = 0;
            }
        }
    }

    fn push(&mut self, item: ChatItem) -> usize {
        self.chat.push(item);
        self.chat.len() - 1
    }

    fn set_status(&mut self, member: &MemberId, status: MemberStatus) {
        if let Some(view) = self.members.iter_mut().find(|m| &m.id == member) {
            view.status = status;
        }
        // Track elapsed "working" time: start the clock when a member begins
        // running (without resetting it on repeated Running events), stop it
        // on any other status.
        if status == MemberStatus::Running {
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

    fn member_meta(&self, member: &MemberId) -> (String, BackendKind) {
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
        self.message_index.values().any(|&idx| {
            if let Some(ChatItem::Agent { member, .. }) = self.chat.get(idx) {
                member == member_id
            } else {
                false
            }
        })
    }

    pub fn handle_user_message_submitted(&mut self, target: &MessageTarget, body: String) {
        self.pending_user_messages.push(body.clone());
        self.push(ChatItem::User { body });
        let targets = self.resolve_local_targets(target);
        for member_id in targets {
            self.set_status(&member_id, MemberStatus::Running);
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

    // --- accessors for the renderer -------------------------------------

    pub fn team(&self) -> &str {
        &self.team
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
    pub fn chat(&self) -> &[ChatItem] {
        &self.chat
    }
    pub fn logs(&self) -> &[LogEntry] {
        &self.logs
    }
    pub fn workflow_runs(&self) -> &[WorkflowRunSummary] {
        &self.workflow_runs
    }
    pub fn latest_workflow_run(&self) -> Option<&WorkflowRunSummary> {
        self.workflow_runs.last()
    }
    pub fn latest_workflow_action_command(&self) -> Option<String> {
        self.latest_workflow_run()
            .map(|run| workflow_action_command(run, &self.workspace, false))
    }
    pub fn selected_workflow_run(&self) -> Option<&WorkflowRunSummary> {
        self.selected_workflow_run
            .and_then(|id| self.workflow_runs.iter().find(|run| run.id == id))
            .or_else(|| self.latest_workflow_run())
    }
    pub fn selected_workflow_step(&self) -> Option<u32> {
        let run = self.selected_workflow_run()?;
        self.selected_workflow_step
            .filter(|step| run.steps.iter().any(|candidate| candidate.number == *step))
    }
    pub fn selected_workflow_action_command(&self) -> Option<String> {
        self.selected_workflow_run()
            .map(|run| workflow_action_command(run, &self.workspace, true))
    }
    pub fn selected_workflow_stage_command(&self) -> Option<String> {
        let run = self.selected_workflow_run()?;
        self.selected_workflow_step()
            .and_then(|step| workflow_step_action_command(run, step))
            .or_else(|| Some(workflow_action_command(run, &self.workspace, true)))
    }
    pub fn selected_workflow_dispatch_command(&self) -> Option<String> {
        let run = self.selected_workflow_run()?;
        let step = self.selected_workflow_step()?;
        workflow_step_dispatch_command(run, step)
    }
    pub fn workflow_runs_detail(&self) -> bool {
        self.workflow_runs_detail
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
    pub fn composer(&self) -> &Composer {
        &self.composer
    }
    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    pub fn running_count(&self) -> usize {
        self.members
            .iter()
            .filter(|m| m.status == MemberStatus::Running)
            .count()
    }

    pub fn first_pending_approval(&self) -> Option<ApprovalId> {
        self.pending_approvals.first().map(|a| a.id)
    }

    /// Request attaching to the member at `idx`'s live backend session. Skipped
    /// (with a notice) while that member is running, to avoid two processes on
    /// one session.
    pub fn request_attach(&mut self, idx: usize) {
        self.disarm_quit();
        let Some(member) = self.members.get(idx) else {
            self.header_selected = None;
            return;
        };
        if member.status == MemberStatus::Running {
            let name = member.display_name.clone();
            self.push(ChatItem::Notice {
                text: format!("{name} is running — /abort before attaching to its session"),
            });
            self.header_selected = None;
            return;
        }
        self.attach_request = Some(AttachRequest {
            member: member.id.clone(),
            display_name: member.display_name.clone(),
            backend: member.backend,
            session: member.session.clone(),
            cwd: member.cwd.clone(),
        });
        self.header_selected = None;
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
        completion::compute(&self.composer.head(), &self.member_ids())
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
            self.prompt_history.push(text.to_string());
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
            self.team_editor = None;
            None
        } else {
            match drawer {
                Drawer::Team => self.open_team_editor(),
                Drawer::Runs => {
                    self.team_editor = None;
                    self.ensure_selected_workflow_run();
                }
                _ => {
                    self.team_editor = None;
                }
            }
            Some(drawer)
        };
        self.drawer_scroll = 0;
    }

    pub fn close_drawer(&mut self) {
        self.disarm_quit();
        self.drawer = None;
        self.drawer_scroll = 0;
        self.team_editor = None;
    }

    pub fn stage_selected_workflow_action(&mut self) -> bool {
        if self.drawer != Some(Drawer::Runs) || !self.composer.is_empty() {
            return false;
        }
        let Some(command) = self.selected_workflow_stage_command() else {
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

    pub fn stage_selected_workflow_dispatch(&mut self) -> bool {
        if self.drawer != Some(Drawer::Runs) || !self.composer.is_empty() {
            return false;
        }
        let Some(command) = self.selected_workflow_dispatch_command() else {
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

    pub fn toggle_workflow_runs_detail(&mut self) -> bool {
        if self.drawer != Some(Drawer::Runs) || !self.composer.is_empty() {
            return false;
        }
        self.disarm_quit();
        self.workflow_runs_detail = !self.workflow_runs_detail;
        self.drawer_scroll = 0;
        true
    }

    pub fn select_newer_workflow_run(&mut self) {
        if self.drawer != Some(Drawer::Runs) {
            return;
        }
        self.disarm_quit();
        self.selected_workflow_step = None;
        self.ensure_selected_workflow_run();
        let Some(id) = self.selected_workflow_run else {
            return;
        };
        let Some(index) = self.workflow_run_index(id) else {
            return;
        };
        if index + 1 < self.workflow_runs.len() {
            self.selected_workflow_run = Some(self.workflow_runs[index + 1].id);
        }
    }

    pub fn select_older_workflow_run(&mut self) {
        if self.drawer != Some(Drawer::Runs) {
            return;
        }
        self.disarm_quit();
        self.selected_workflow_step = None;
        self.ensure_selected_workflow_run();
        let Some(id) = self.selected_workflow_run else {
            return;
        };
        let Some(index) = self.workflow_run_index(id) else {
            return;
        };
        if index > 0 {
            self.selected_workflow_run = Some(self.workflow_runs[index - 1].id);
        }
    }

    pub fn select_previous_workflow_step(&mut self) -> bool {
        if self.drawer != Some(Drawer::Runs) {
            return false;
        }
        self.disarm_quit();
        let Some(run) = self.selected_workflow_run() else {
            self.selected_workflow_step = None;
            return false;
        };
        if run.steps.is_empty() {
            self.selected_workflow_step = None;
            return false;
        }
        let next = match self.selected_workflow_step() {
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
        self.selected_workflow_step = next;
        true
    }

    pub fn select_next_workflow_step(&mut self) -> bool {
        if self.drawer != Some(Drawer::Runs) {
            return false;
        }
        self.disarm_quit();
        let Some(run) = self.selected_workflow_run() else {
            self.selected_workflow_step = None;
            return false;
        };
        if run.steps.is_empty() {
            self.selected_workflow_step = None;
            return false;
        }
        let next = match self.selected_workflow_step() {
            None => run.steps.first().map(|step| step.number),
            Some(number) => run
                .steps
                .iter()
                .position(|step| step.number == number)
                .and_then(|idx| run.steps.get(idx + 1))
                .map(|step| step.number),
        }
        .or_else(|| run.steps.last().map(|step| step.number));
        self.selected_workflow_step = next;
        true
    }

    fn ensure_selected_workflow_run(&mut self) {
        let selected_is_valid = self
            .selected_workflow_run
            .is_some_and(|id| self.workflow_runs.iter().any(|run| run.id == id));
        if !selected_is_valid {
            self.selected_workflow_run = self.workflow_runs.last().map(|run| run.id);
            self.selected_workflow_step = None;
        }
    }

    fn ensure_selected_workflow_step(&mut self) {
        let Some(step) = self.selected_workflow_step else {
            return;
        };
        let step_is_valid = self
            .selected_workflow_run()
            .is_some_and(|run| run.steps.iter().any(|candidate| candidate.number == step));
        if !step_is_valid {
            self.selected_workflow_step = None;
        }
    }

    fn workflow_run_index(&self, id: WorkflowRunId) -> Option<usize> {
        self.workflow_runs.iter().position(|run| run.id == id)
    }

    /// The drawer's vertical scroll offset (top line to show).
    pub fn drawer_scroll(&self) -> usize {
        self.drawer_scroll
    }
    pub fn drawer_scroll_up(&mut self) {
        self.disarm_quit();
        self.drawer_scroll = self.drawer_scroll.saturating_sub(1);
    }
    pub fn drawer_scroll_down(&mut self) {
        self.disarm_quit();
        self.drawer_scroll = self.drawer_scroll.saturating_add(1);
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

    fn open_team_editor(&mut self) {
        let members = self
            .members
            .iter()
            .map(|view| self.view_to_member(view))
            .collect();
        self.team_editor = Some(TeamEditor::new(
            self.team.clone(),
            PathBuf::from(self.workspace.clone()),
            self.default_target.clone(),
            members,
        ));
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
        self.disarm_quit();
        self.scroll = self.scroll.saturating_add(1);
    }

    pub fn scroll_down(&mut self) {
        self.disarm_quit();
        self.scroll = self.scroll.saturating_sub(1);
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

pub(crate) fn workflow_action_command(
    run: &WorkflowRunSummary,
    workspace: &str,
    include_run_id: bool,
) -> String {
    match run.status {
        WorkflowRunStatus::Running | WorkflowRunStatus::Verifying => "/abort".to_string(),
        WorkflowRunStatus::Done if run.verification.is_none() => {
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
            command
        }
        WorkflowRunStatus::Done => "/plan ".to_string(),
        WorkflowRunStatus::Failed if run.verification.is_some() => {
            let mut command = continue_command_prefix(run, include_run_id);
            command.push_str(" fix failing verification");
            command
        }
        WorkflowRunStatus::Failed => continue_command_prefix(run, include_run_id),
        WorkflowRunStatus::Blocked => {
            let mut command = continue_command_prefix(run, include_run_id);
            command.push_str(" blocker resolved");
            command
        }
        WorkflowRunStatus::Planned => "/retry".to_string(),
    }
}

fn verify_command_prefix(run: &WorkflowRunSummary, include_run_id: bool) -> String {
    if include_run_id {
        format!("/verify {}", run.id)
    } else {
        "/verify".to_string()
    }
}

fn continue_command_prefix(run: &WorkflowRunSummary, include_run_id: bool) -> String {
    if include_run_id {
        format!("/continue {}", run.id)
    } else {
        "/continue".to_string()
    }
}

fn workflow_step_action_command(run: &WorkflowRunSummary, step: u32) -> Option<String> {
    let step = run
        .steps
        .iter()
        .find(|candidate| candidate.number == step)?;
    let (action, note) = match step.status {
        WorkflowStepStatus::Todo => ("doing", None),
        WorkflowStepStatus::Doing => ("done", None),
        WorkflowStepStatus::Blocked => ("doing", Some("blocker resolved")),
        WorkflowStepStatus::Done => ("todo", Some("reopen")),
    };
    let mut command = format!("/step {action} {} {}", run.id, step.number);
    if let Some(note) = note {
        command.push(' ');
        command.push_str(note);
    }
    Some(command)
}

fn workflow_step_dispatch_command(run: &WorkflowRunSummary, step: u32) -> Option<String> {
    let step = run
        .steps
        .iter()
        .find(|candidate| candidate.number == step)?;
    let Some(owner) = &step.owner else {
        return Some(format!("/step assign {} {} ", run.id, step.number));
    };

    let instruction = match step.status {
        WorkflowStepStatus::Todo => "Start",
        WorkflowStepStatus::Doing => "Continue",
        WorkflowStepStatus::Blocked => "Revisit blocked",
        WorkflowStepStatus::Done => "Review completed",
    };
    Some(format!(
        "@{owner} {instruction} {} step #{}: {}. Update the checklist with @@workflow_step as you progress.",
        run.id, step.number, step.title
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::event::{
        AgentSessionId, ApprovalDecision, MemberSummary, TurnId, WorkflowRunId, WorkflowRunStatus,
        WorkflowRunSummary, WorkflowStepSummary, WorkflowVerification,
    };

    fn ready() -> RuntimeEvent {
        RuntimeEvent::Ready {
            team: "mixed".to_string(),
            workspace: "/tmp/ws".to_string(),
            default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
            workflow_runs: Vec::new(),
            members: vec![MemberSummary {
                id: MemberId::new("builder"),
                display_name: "Builder".to_string(),
                backend: BackendKind::Codex,
                role: "impl".to_string(),
                status: MemberStatus::Idle,
                session: None,
                cwd: String::new(),
                model: None,
                effort: None,
                sandbox: SandboxPolicy::ReadOnly,
                permission_mode: Some(PermissionMode::Default),
                session_policy: SessionPolicy::Resume,
            }],
        }
    }

    #[test]
    fn ready_populates_header() {
        let mut state = AppState::new(Vec::new());
        state.apply(ready());
        assert_eq!(state.team(), "mixed");
        assert_eq!(state.members().len(), 1);
    }

    #[test]
    fn workflow_run_updates_insert_then_replace() {
        let mut state = AppState::new(Vec::new());
        let run = WorkflowRunSummary {
            id: WorkflowRunId(1),
            goal: "ship parser".to_string(),
            status: WorkflowRunStatus::Running,
            coordinator: Some(MemberId::new("builder")),
            verification: None,
            created_at: "2026-06-28 10:00:00".to_string(),
            updated_at: "2026-06-28 10:00:00".to_string(),
            attempt: 1,
            events: Vec::new(),
            steps: Vec::new(),
        };

        state.apply(RuntimeEvent::WorkflowRunUpdated { run: run.clone() });
        assert_eq!(state.workflow_runs(), std::slice::from_ref(&run));
        assert_eq!(state.latest_workflow_run(), Some(&run));

        let updated = WorkflowRunSummary {
            status: WorkflowRunStatus::Done,
            verification: Some(WorkflowVerification {
                command: "cargo test".to_string(),
                ok: true,
                summary: "ok".to_string(),
            }),
            ..run
        };
        state.apply(RuntimeEvent::WorkflowRunUpdated {
            run: updated.clone(),
        });

        assert_eq!(state.workflow_runs(), std::slice::from_ref(&updated));
        assert_eq!(state.latest_workflow_run(), Some(&updated));
    }

    #[test]
    fn runs_drawer_stages_selected_workflow_action_without_overwriting_draft() {
        let mut state = AppState::new(Vec::new());
        state.apply(ready());
        state.apply(RuntimeEvent::WorkflowRunUpdated {
            run: WorkflowRunSummary {
                id: WorkflowRunId(1),
                goal: "ship parser".to_string(),
                status: WorkflowRunStatus::Done,
                coordinator: Some(MemberId::new("builder")),
                verification: None,
                created_at: "2026-06-28 10:00:00".to_string(),
                updated_at: "2026-06-28 10:00:00".to_string(),
                attempt: 1,
                events: Vec::new(),
                steps: Vec::new(),
            },
        });

        state.toggle_drawer(Drawer::Runs);
        assert!(state.stage_selected_workflow_action());
        assert_eq!(state.drawer(), None);
        assert_eq!(state.composer().text(), "/verify run-1");

        state.clear_composer();
        state.insert_char('x');
        state.toggle_drawer(Drawer::Runs);
        assert!(!state.stage_selected_workflow_action());
        assert_eq!(state.drawer(), Some(Drawer::Runs));
        assert_eq!(state.composer().text(), "x");
    }

    #[test]
    fn runs_drawer_can_select_an_older_workflow_run() {
        let mut state = AppState::new(Vec::new());
        state.apply(RuntimeEvent::Ready {
            team: "mixed".to_string(),
            workspace: "/tmp/ws".to_string(),
            default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
            workflow_runs: vec![
                WorkflowRunSummary {
                    id: WorkflowRunId(1),
                    goal: "ship parser".to_string(),
                    status: WorkflowRunStatus::Done,
                    coordinator: Some(MemberId::new("builder")),
                    verification: None,
                    created_at: "2026-06-28 10:00:00".to_string(),
                    updated_at: "2026-06-28 10:05:00".to_string(),
                    attempt: 1,
                    events: Vec::new(),
                    steps: Vec::new(),
                },
                WorkflowRunSummary {
                    id: WorkflowRunId(2),
                    goal: "refactor ui".to_string(),
                    status: WorkflowRunStatus::Running,
                    coordinator: Some(MemberId::new("builder")),
                    verification: None,
                    created_at: "2026-06-28 10:10:00".to_string(),
                    updated_at: "2026-06-28 10:12:00".to_string(),
                    attempt: 1,
                    events: Vec::new(),
                    steps: Vec::new(),
                },
            ],
            members: Vec::new(),
        });
        state.toggle_drawer(Drawer::Runs);

        assert_eq!(
            state.selected_workflow_run().map(|run| run.id),
            Some(WorkflowRunId(2))
        );
        state.select_older_workflow_run();
        assert_eq!(
            state.selected_workflow_run().map(|run| run.id),
            Some(WorkflowRunId(1))
        );
        assert_eq!(
            state.selected_workflow_action_command().as_deref(),
            Some("/verify run-1")
        );
        assert!(state.stage_selected_workflow_action());
        assert_eq!(state.composer().text(), "/verify run-1");
    }

    #[test]
    fn runs_drawer_can_select_steps_and_stage_step_actions() {
        let mut state = AppState::new(Vec::new());
        state.apply(ready());
        state.apply(RuntimeEvent::WorkflowRunUpdated {
            run: WorkflowRunSummary {
                id: WorkflowRunId(1),
                goal: "ship checklist".to_string(),
                status: WorkflowRunStatus::Running,
                coordinator: Some(MemberId::new("builder")),
                verification: None,
                created_at: "2026-06-28 10:00:00".to_string(),
                updated_at: "2026-06-28 10:00:00".to_string(),
                attempt: 1,
                events: Vec::new(),
                steps: vec![
                    WorkflowStepSummary {
                        number: 1,
                        status: WorkflowStepStatus::Todo,
                        owner: Some(MemberId::new("builder")),
                        title: "Write parser tests".to_string(),
                        note: None,
                        updated_at: "2026-06-28 10:01:00".to_string(),
                    },
                    WorkflowStepSummary {
                        number: 2,
                        status: WorkflowStepStatus::Doing,
                        owner: None,
                        title: "Wire checklist UI".to_string(),
                        note: None,
                        updated_at: "2026-06-28 10:02:00".to_string(),
                    },
                    WorkflowStepSummary {
                        number: 3,
                        status: WorkflowStepStatus::Blocked,
                        owner: None,
                        title: "Wait for credentials".to_string(),
                        note: None,
                        updated_at: "2026-06-28 10:03:00".to_string(),
                    },
                    WorkflowStepSummary {
                        number: 4,
                        status: WorkflowStepStatus::Done,
                        owner: None,
                        title: "Document result".to_string(),
                        note: None,
                        updated_at: "2026-06-28 10:04:00".to_string(),
                    },
                ],
            },
        });
        state.toggle_drawer(Drawer::Runs);

        assert_eq!(state.selected_workflow_step(), None);
        assert_eq!(
            state.selected_workflow_stage_command().as_deref(),
            Some("/abort")
        );

        assert!(state.select_next_workflow_step());
        assert_eq!(state.selected_workflow_step(), Some(1));
        assert_eq!(
            state.selected_workflow_stage_command().as_deref(),
            Some("/step doing run-1 1")
        );
        assert_eq!(
            state.selected_workflow_dispatch_command().as_deref(),
            Some(
                "@builder Start run-1 step #1: Write parser tests. Update the checklist with @@workflow_step as you progress."
            )
        );

        state.select_newer_workflow_run();
        assert_eq!(state.selected_workflow_step(), None);
        assert_eq!(
            state.selected_workflow_stage_command().as_deref(),
            Some("/abort")
        );

        assert!(state.select_next_workflow_step());

        assert!(state.select_next_workflow_step());
        assert_eq!(
            state.selected_workflow_stage_command().as_deref(),
            Some("/step done run-1 2")
        );
        assert_eq!(
            state.selected_workflow_dispatch_command().as_deref(),
            Some("/step assign run-1 2 ")
        );

        assert!(state.select_next_workflow_step());
        assert_eq!(
            state.selected_workflow_stage_command().as_deref(),
            Some("/step doing run-1 3 blocker resolved")
        );

        assert!(state.select_next_workflow_step());
        assert_eq!(
            state.selected_workflow_stage_command().as_deref(),
            Some("/step todo run-1 4 reopen")
        );

        assert!(state.stage_selected_workflow_action());
        assert_eq!(state.composer().text(), "/step todo run-1 4 reopen");
    }

    #[test]
    fn workflow_action_previews_detected_verify_command() {
        let dir =
            std::env::temp_dir().join(format!("asterline-action-preview-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        let mut state = AppState::new(Vec::new());
        state.apply(RuntimeEvent::Ready {
            team: "mixed".to_string(),
            workspace: dir.display().to_string(),
            default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
            workflow_runs: Vec::new(),
            members: Vec::new(),
        });
        state.apply(RuntimeEvent::WorkflowRunUpdated {
            run: WorkflowRunSummary {
                id: WorkflowRunId(1),
                goal: "ship parser".to_string(),
                status: WorkflowRunStatus::Done,
                coordinator: Some(MemberId::new("builder")),
                verification: None,
                created_at: "2026-06-28 10:00:00".to_string(),
                updated_at: "2026-06-28 10:00:00".to_string(),
                attempt: 1,
                events: Vec::new(),
                steps: Vec::new(),
            },
        });

        assert_eq!(
            state.latest_workflow_action_command().as_deref(),
            Some("/verify cargo test")
        );
        state.toggle_drawer(Drawer::Runs);
        assert!(state.stage_selected_workflow_action());
        assert_eq!(state.composer().text(), "/verify run-1 cargo test");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn workflow_action_continues_failed_and_blocked_runs() {
        let mut state = AppState::new(Vec::new());
        state.apply(ready());
        state.apply(RuntimeEvent::WorkflowRunUpdated {
            run: WorkflowRunSummary {
                id: WorkflowRunId(1),
                goal: "ship parser".to_string(),
                status: WorkflowRunStatus::Failed,
                coordinator: Some(MemberId::new("builder")),
                verification: Some(WorkflowVerification {
                    command: "cargo test".to_string(),
                    ok: false,
                    summary: "failed".to_string(),
                }),
                created_at: "2026-06-28 10:00:00".to_string(),
                updated_at: "2026-06-28 10:00:00".to_string(),
                attempt: 2,
                events: Vec::new(),
                steps: Vec::new(),
            },
        });

        assert_eq!(
            state.latest_workflow_action_command().as_deref(),
            Some("/continue fix failing verification")
        );
        state.toggle_drawer(Drawer::Runs);
        assert!(state.stage_selected_workflow_action());
        assert_eq!(
            state.composer().text(),
            "/continue run-1 fix failing verification"
        );

        state.apply(RuntimeEvent::WorkflowRunUpdated {
            run: WorkflowRunSummary {
                id: WorkflowRunId(2),
                goal: "unblock release".to_string(),
                status: WorkflowRunStatus::Blocked,
                coordinator: Some(MemberId::new("builder")),
                verification: None,
                created_at: "2026-06-28 10:00:00".to_string(),
                updated_at: "2026-06-28 10:00:00".to_string(),
                attempt: 2,
                events: Vec::new(),
                steps: Vec::new(),
            },
        });

        assert_eq!(
            state.latest_workflow_action_command().as_deref(),
            Some("/continue blocker resolved")
        );
    }

    #[test]
    fn streaming_message_builds_agent_cell() {
        let mut state = AppState::new(Vec::new());
        state.apply(ready());
        let builder = MemberId::new("builder");
        state.apply(RuntimeEvent::MessageStarted {
            msg: MessageId(1),
            turn: TurnId(1),
            member: builder.clone(),
        });
        state.apply(RuntimeEvent::MessageDelta {
            msg: MessageId(1),
            text: "Hel".to_string(),
        });
        state.apply(RuntimeEvent::MessageDelta {
            msg: MessageId(1),
            text: "lo".to_string(),
        });
        state.apply(RuntimeEvent::MessageCompleted {
            msg: MessageId(1),
            text: "Hello".to_string(),
        });

        assert!(matches!(
            state.chat().last(),
            Some(ChatItem::Agent { text, .. }) if text == "Hello"
        ));
    }

    #[test]
    fn tool_completion_updates_existing_cell() {
        let mut state = AppState::new(Vec::new());
        state.apply(ready());
        let builder = MemberId::new("builder");
        state.apply(RuntimeEvent::ToolStarted {
            member: builder.clone(),
            tool_id: "t1".to_string(),
            name: "shell".to_string(),
            summary: "cargo test".to_string(),
        });
        state.apply(RuntimeEvent::ToolCompleted {
            member: builder,
            tool_id: "t1".to_string(),
            ok: true,
            summary: "cargo test".to_string(),
        });
        // One tool cell, now marked ok.
        let tools: Vec<_> = state
            .chat()
            .iter()
            .filter(|i| matches!(i, ChatItem::Tool { .. }))
            .collect();
        assert_eq!(tools.len(), 1);
        assert!(matches!(tools[0], ChatItem::Tool { ok: Some(true), .. }));
    }

    #[test]
    fn logs_do_not_enter_chat() {
        let mut state = AppState::new(Vec::new());
        state.apply(RuntimeEvent::Log(LogEntry::warn("builder", "stderr noise")));
        assert!(state.chat().is_empty());
        assert_eq!(state.logs().len(), 1);
    }

    #[test]
    fn seeded_logs_replay_into_the_drawer() {
        let mut state = AppState::new(Vec::new());
        state.seed_logs(vec![
            LogEntry::info("builder", "started"),
            LogEntry::warn("reviewer", "slow"),
        ]);
        assert_eq!(state.logs().len(), 2);
        // Live logs still append after seeding.
        state.apply(RuntimeEvent::Log(LogEntry::error("runtime", "boom")));
        assert_eq!(state.logs().len(), 3);
    }

    #[test]
    fn approvals_track_pending_and_resolve() {
        let mut state = AppState::new(Vec::new());
        state.apply(RuntimeEvent::ApprovalRequested {
            id: ApprovalId(1),
            member: None,
            action: "git".to_string(),
            body: "git push".to_string(),
        });
        assert_eq!(state.first_pending_approval(), Some(ApprovalId(1)));
        state.apply(RuntimeEvent::ApprovalResolved {
            id: ApprovalId(1),
            decision: ApprovalDecision::Approve,
        });
        assert!(state.first_pending_approval().is_none());
    }

    #[test]
    fn member_status_drives_running_count() {
        let mut state = AppState::new(Vec::new());
        state.apply(ready());
        let builder = MemberId::new("builder");
        state.apply(RuntimeEvent::MemberStatus {
            member: builder.clone(),
            status: MemberStatus::Running,
        });
        assert_eq!(state.running_count(), 1);
        state.apply(RuntimeEvent::MemberStatus {
            member: builder,
            status: MemberStatus::Idle,
        });
        assert_eq!(state.running_count(), 0);
    }

    #[test]
    fn default_target_all_marks_every_member_running_optimistically() {
        let mut state = AppState::new(Vec::new());
        state.apply(RuntimeEvent::Ready {
            team: "mixed".to_string(),
            workspace: "/tmp/ws".to_string(),
            default_target: Some(DefaultTarget::All),
            workflow_runs: Vec::new(),
            members: vec![
                MemberSummary {
                    id: MemberId::new("builder"),
                    display_name: "Builder".to_string(),
                    backend: BackendKind::Codex,
                    role: "impl".to_string(),
                    status: MemberStatus::Idle,
                    session: None,
                    cwd: String::new(),
                    model: None,
                    effort: None,
                    sandbox: SandboxPolicy::ReadOnly,
                    permission_mode: Some(PermissionMode::Default),
                    session_policy: SessionPolicy::Resume,
                },
                MemberSummary {
                    id: MemberId::new("reviewer"),
                    display_name: "Reviewer".to_string(),
                    backend: BackendKind::Claude,
                    role: "review".to_string(),
                    status: MemberStatus::Idle,
                    session: None,
                    cwd: String::new(),
                    model: None,
                    effort: None,
                    sandbox: SandboxPolicy::ReadOnly,
                    permission_mode: Some(PermissionMode::Default),
                    session_policy: SessionPolicy::Resume,
                },
            ],
        });

        state.handle_user_message_submitted(&MessageTarget::Default, "go".to_string());

        assert_eq!(state.running_count(), 2);
    }

    #[test]
    fn drawer_toggles() {
        let mut state = AppState::new(Vec::new());
        state.toggle_drawer(Drawer::Logs);
        assert_eq!(state.drawer(), Some(Drawer::Logs));
        state.toggle_drawer(Drawer::Logs);
        assert_eq!(state.drawer(), None);
        let _ = AgentSessionId("x".to_string());
    }

    #[test]
    fn drawer_scroll_down_increases_render_offset() {
        let mut state = AppState::new(Vec::new());
        state.toggle_drawer(Drawer::Logs);

        state.drawer_scroll_up();
        assert_eq!(state.drawer_scroll(), 0);

        state.drawer_scroll_down();
        assert_eq!(state.drawer_scroll(), 1);

        state.drawer_scroll_up();
        assert_eq!(state.drawer_scroll(), 0);
    }

    #[test]
    fn quit_requires_two_consecutive_requests() {
        let mut state = AppState::new(Vec::new());

        state.request_quit();
        assert!(!state.should_quit());
        assert!(state.chat().iter().any(|item| matches!(
            item,
            ChatItem::Notice { text } if text.contains("Ctrl+C again")
        )));

        state.request_quit();
        assert!(state.should_quit());
    }

    #[test]
    fn quit_confirmation_is_disarmed_by_input() {
        let mut state = AppState::new(Vec::new());

        state.request_quit();
        state.insert_char('x');
        state.clear_composer();
        state.request_quit();

        assert!(!state.should_quit());
    }

    #[test]
    fn team_drawer_editor_can_add_and_apply_member() {
        let mut state = AppState::new(Vec::new());
        state.apply(ready());
        state.toggle_drawer(Drawer::Team);

        let add = state.handle_team_editor_key(KeyCode::Char('a'), KeyModifiers::NONE);
        assert_eq!(add, TeamEditorOutcome::Consumed(None));

        let apply = state.handle_team_editor_key(KeyCode::Char('s'), KeyModifiers::NONE);
        let TeamEditorOutcome::Consumed(Some(crate::domain::event::UiCommand::ReplaceTeam {
            members,
            ..
        })) = apply
        else {
            panic!("expected replace team command");
        };
        assert_eq!(members.len(), 2);
    }

    #[test]
    fn slash_opens_command_popup_and_accept_inserts() {
        let mut state = AppState::new(Vec::new());
        state.apply(ready());
        for ch in "/as".chars() {
            state.insert_char(ch);
        }
        let completion = state.completion().expect("command popup");
        assert_eq!(completion.items[0].insert, "/ask ");
        assert!(state.accept_completion());
        assert_eq!(state.composer().text(), "/ask ");
    }

    #[test]
    fn at_opens_member_popup_and_accepts() {
        let mut state = AppState::new(Vec::new());
        state.apply(ready());
        for ch in "@bu".chars() {
            state.insert_char(ch);
        }
        let completion = state.completion().expect("member popup");
        assert_eq!(completion.items[0].insert, "@builder ");
        state.accept_completion();
        assert_eq!(state.composer().text(), "@builder ");
    }

    #[test]
    fn dismiss_hides_popup_until_text_changes() {
        let mut state = AppState::new(Vec::new());
        state.apply(ready());
        state.insert_char('/');
        assert!(state.completion().is_some());
        state.dismiss_popup();
        assert!(state.completion().is_none());
        state.insert_char('a');
        assert!(state.completion().is_some());
    }

    #[test]
    fn header_roster_selection() {
        let mut state = AppState::new(Vec::new());
        state.members = vec![
            MemberView {
                id: MemberId::new("builder"),
                display_name: "Builder".to_string(),
                backend: BackendKind::Codex,
                role: "impl".to_string(),
                status: MemberStatus::Idle,
                session: None,
                cwd: String::new(),
                model: None,
                effort: None,
                sandbox: SandboxPolicy::ReadOnly,
                permission_mode: Some(PermissionMode::Default),
                session_policy: SessionPolicy::Resume,
            },
            MemberView {
                id: MemberId::new("reviewer"),
                display_name: "Reviewer".to_string(),
                backend: BackendKind::Claude,
                role: "review".to_string(),
                status: MemberStatus::Idle,
                session: None,
                cwd: String::new(),
                model: None,
                effort: None,
                sandbox: SandboxPolicy::ReadOnly,
                permission_mode: Some(PermissionMode::Default),
                session_policy: SessionPolicy::Resume,
            },
        ];
        assert_eq!(state.header_selected(), None);

        state.select_next_member();
        assert_eq!(state.header_selected(), Some(0)); // builder

        state.select_next_member();
        assert_eq!(state.header_selected(), Some(1)); // reviewer

        state.select_prev_member();
        assert_eq!(state.header_selected(), Some(0)); // builder

        state.insert_char('x');
        assert_eq!(state.header_selected(), None); // cleared on typing
    }

    #[test]
    fn prompt_history_seeds_from_replayed_user_messages() {
        let chat = vec![
            ChatItem::User {
                body: "first".to_string(),
            },
            ChatItem::Agent {
                member: MemberId::new("builder"),
                display_name: "Builder".to_string(),
                backend: BackendKind::Codex,
                text: "ok".to_string(),
            },
            ChatItem::User {
                body: "second".to_string(),
            },
        ];
        let mut state = AppState::new(chat);

        // ↑ recalls newest-first across sessions.
        state.history_prev();
        assert_eq!(state.composer().text(), "second");
        state.history_prev();
        assert_eq!(state.composer().text(), "first");
        // Already at the oldest entry — further ↑ stays put.
        state.history_prev();
        assert_eq!(state.composer().text(), "first");
        // ↓ walks back toward newest.
        state.history_next();
        assert_eq!(state.composer().text(), "second");
    }

    #[test]
    fn history_preserves_and_restores_the_live_draft() {
        let mut state = AppState::new(vec![ChatItem::User {
            body: "prior".to_string(),
        }]);
        for ch in "draft".chars() {
            state.insert_char(ch);
        }
        state.history_prev();
        assert_eq!(state.composer().text(), "prior");
        assert!(state.browsing_history());
        // Stepping past the newest restores what was being typed.
        state.history_next();
        assert_eq!(state.composer().text(), "draft");
        assert!(!state.browsing_history());
    }

    #[test]
    fn submitting_records_history_and_skips_consecutive_dupes() {
        let mut state = AppState::new(Vec::new());
        state.record_submission("build it");
        state.record_submission("build it"); // dup ignored
        state.record_submission("   "); // blank ignored
        state.record_submission("review it");

        state.history_prev();
        assert_eq!(state.composer().text(), "review it");
        state.history_prev();
        assert_eq!(state.composer().text(), "build it");
    }

    #[test]
    fn reverse_history_search_finds_steps_and_accepts() {
        let mut state = AppState::new(Vec::new());
        state.record_submission("build the parser");
        state.record_submission("review the parser");
        state.record_submission("run tests");

        state.start_history_search();
        assert!(state.in_history_search());

        for ch in "parser".chars() {
            state.history_search_input(ch);
        }
        // Newest match first.
        assert_eq!(state.history_search().unwrap().1, Some("review the parser"));
        // Ctrl+R again → next older match.
        state.history_search_again();
        assert_eq!(state.history_search().unwrap().1, Some("build the parser"));

        state.accept_history_search();
        assert!(!state.in_history_search());
        assert_eq!(state.composer().text(), "build the parser");
    }

    #[test]
    fn reverse_history_search_cancel_keeps_composer() {
        let mut state = AppState::new(Vec::new());
        state.record_submission("hello world");
        for ch in "draft".chars() {
            state.insert_char(ch);
        }
        state.start_history_search();
        state.history_search_input('h');
        state.cancel_history_search();
        assert!(!state.in_history_search());
        assert_eq!(state.composer().text(), "draft");
    }
}
