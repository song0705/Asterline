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

/// Rough estimate of how many visual lines a [`ChatItem`] will occupy, used
/// only to keep the scroll position stable when new items arrive while the
/// user is browsing history. The exact count depends on the render width and
/// markdown layout, so this is a conservative lower bound.
fn estimate_item_lines(item: &ChatItem) -> usize {
    match item {
        ChatItem::User { body } => body.lines().count().max(1),
        ChatItem::Agent { text, .. } => {
            if text.is_empty() {
                1 // header only
            } else {
                text.lines().count().max(1) + 1 // +1 for header
            }
        }
        ChatItem::Tool { .. } => 1,
        ChatItem::Diff { files, .. } => 1 + files.len(),
        ChatItem::Route { body, .. } => 1 + body.lines().count().max(1),
        ChatItem::Notice { text } => text.lines().count().max(1),
        ChatItem::Error { message, .. } => message.lines().count().max(1),
    }
}

#[cfg(test)]
#[path = "app_state_tests.rs"]
mod tests;
