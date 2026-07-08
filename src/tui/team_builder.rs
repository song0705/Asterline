//! Interactive startup team builder.
//!
//! When no `--team` config is given, Asterline detects which backend CLIs are
//! available and lets you build a roster. The builder supports multiple members
//! on the same backend, per-member model, and per-member reasoning effort. On a
//! non-interactive stdout it falls back to the established default roster.

use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

use crate::domain::config::{DetectedBackends, default_member, default_team};
use crate::domain::team::{
    BackendKind, DefaultTarget, Effort, MemberId, PermissionMode, SandboxPolicy, SessionPolicy,
    TeamConfig, TeamMember, normalize_member_id as normalize_domain_member_id,
};
use crate::tui::theme;

/// Pick a team interactively from the detected backends. Returns `None` if the
/// user cancels or nothing is available.
pub fn run(detected: DetectedBackends, workspace: &Path) -> io::Result<Option<TeamConfig>> {
    let available: Vec<BackendKind> = [BackendKind::Codex, BackendKind::Claude, BackendKind::Agy]
        .into_iter()
        .filter(|b| is_detected(*b, detected))
        .collect();

    if available.is_empty() {
        return Ok(None);
    }
    if !io::stdout().is_terminal() {
        return Ok(default_team(workspace.to_path_buf(), detected));
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let outcome = select_loop(&mut terminal, workspace, &available);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    outcome
}

fn is_detected(backend: BackendKind, detected: DetectedBackends) -> bool {
    match backend {
        BackendKind::Codex => detected.codex,
        BackendKind::Claude => detected.claude,
        BackendKind::Agy => detected.agy,
    }
}

fn select_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    workspace: &Path,
    available: &[BackendKind],
) -> io::Result<Option<TeamConfig>> {
    let mut state = BuilderState::new(workspace.to_path_buf(), available);

    loop {
        terminal.draw(|frame| render(frame, &state))?;

        if let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            if state.handle_key(key.code, key.modifiers) {
                return Ok(state.finish());
            }
            if state.cancelled {
                return Ok(None);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Field {
    Name,
    Backend,
    Role,
    Model,
    Effort,
    Sandbox,
    Permission,
    Session,
    Cwd,
}

impl Field {
    pub(crate) const ALL: [Field; 9] = [
        Field::Name,
        Field::Backend,
        Field::Role,
        Field::Model,
        Field::Effort,
        Field::Sandbox,
        Field::Permission,
        Field::Session,
        Field::Cwd,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Backend => "backend",
            Self::Role => "role",
            Self::Model => "model",
            Self::Effort => "effort",
            Self::Sandbox => "sandbox",
            Self::Permission => "permission",
            Self::Session => "session",
            Self::Cwd => "cwd",
        }
    }

    pub(crate) fn is_text(self) -> bool {
        matches!(self, Self::Name | Self::Role | Self::Model | Self::Cwd)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EditState {
    pub(crate) field: Field,
    pub(crate) buffer: String,
}

struct BuilderState {
    workspace: PathBuf,
    available: Vec<BackendKind>,
    members: Vec<TeamMember>,
    selected: usize,
    field: usize,
    editing: Option<EditState>,
    cancelled: bool,
}

impl BuilderState {
    fn new(workspace: PathBuf, available: &[BackendKind]) -> Self {
        let mut members = Vec::new();
        for &backend in available {
            let mut member = default_member(backend);
            member.id = MemberId::new(unique_member_id(member.id.as_str(), &members, None));
            members.push(member);
        }
        Self {
            workspace,
            available: available.to_vec(),
            members,
            selected: 0,
            field: 0,
            editing: None,
            cancelled: false,
        }
    }

    fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        if self.editing.is_some() {
            self.handle_edit_key(code, modifiers);
            return false;
        }

        let ctrl = modifiers.contains(KeyModifiers::CONTROL);
        match code {
            KeyCode::Char('c') if ctrl => self.cancelled = true,
            KeyCode::Esc | KeyCode::Char('q') => self.cancelled = true,
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.selected + 1 < self.members.len() {
                    self.selected += 1;
                }
            }
            KeyCode::Left => self.prev_field(),
            KeyCode::Right | KeyCode::Tab => self.next_field(),
            KeyCode::BackTab => self.prev_field(),
            KeyCode::Char('a') => self.add_member(),
            KeyCode::Char('d') => self.delete_member(),
            KeyCode::Char('s') => return true,
            KeyCode::Enter => self.activate_field(),
            _ => {}
        }
        false
    }

    fn handle_edit_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        let Some(mut edit) = self.editing.take() else {
            return;
        };
        match code {
            KeyCode::Esc => {}
            KeyCode::Enter => self.commit_edit(edit),
            KeyCode::Backspace => {
                edit.buffer.pop();
                self.editing = Some(edit);
            }
            KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => {
                edit.buffer.clear();
                self.editing = Some(edit);
            }
            KeyCode::Char(ch) => {
                edit.buffer.push(ch);
                self.editing = Some(edit);
            }
            _ => self.editing = Some(edit),
        }
    }

    fn next_field(&mut self) {
        self.field = (self.field + 1) % Field::ALL.len();
    }

    fn prev_field(&mut self) {
        self.field = if self.field == 0 {
            Field::ALL.len() - 1
        } else {
            self.field - 1
        };
    }

    fn selected_field(&self) -> Field {
        Field::ALL[self.field]
    }

    fn selected_member(&self) -> &TeamMember {
        &self.members[self.selected]
    }

    fn selected_member_mut(&mut self) -> &mut TeamMember {
        &mut self.members[self.selected]
    }

    fn add_member(&mut self) {
        let backend = self
            .members
            .get(self.selected)
            .map(|member| member.backend)
            .or_else(|| self.available.first().copied())
            .unwrap_or(BackendKind::Codex);
        let mut member = default_member(backend);
        member.id = MemberId::new(unique_member_id(member.id.as_str(), &self.members, None));
        member.display_name = unique_display_name(&member.display_name, &self.members);
        self.members.push(member);
        self.selected = self.members.len() - 1;
    }

    fn delete_member(&mut self) {
        if self.members.len() <= 1 {
            return;
        }
        self.members.remove(self.selected);
        if self.selected >= self.members.len() {
            self.selected = self.members.len() - 1;
        }
    }

    fn activate_field(&mut self) {
        let field = self.selected_field();
        if field.is_text() {
            self.editing = Some(EditState {
                field,
                buffer: field_value(self.selected_member(), field),
            });
        } else {
            self.cycle_field(field);
        }
    }

    fn cycle_field(&mut self, field: Field) {
        match field {
            Field::Backend => {
                let current = self.selected_member().backend;
                let next = cycle_backend(current, &self.available);
                self.selected_member_mut().backend = next;
            }
            Field::Effort => {
                let next = cycle_effort(self.selected_member().effort);
                self.selected_member_mut().effort = next;
            }
            Field::Sandbox => {
                let next = cycle_sandbox(self.selected_member().sandbox);
                self.selected_member_mut().sandbox = next;
            }
            Field::Permission => {
                let next = cycle_permission(self.selected_member().permission_mode);
                self.selected_member_mut().permission_mode = next;
            }
            Field::Session => {
                let next = match self.selected_member().session_policy {
                    SessionPolicy::Resume => SessionPolicy::Fresh,
                    SessionPolicy::Fresh => SessionPolicy::Resume,
                };
                self.selected_member_mut().session_policy = next;
            }
            _ => {}
        }
    }

    fn commit_edit(&mut self, edit: EditState) {
        let value = edit.buffer.trim();
        match edit.field {
            Field::Name => {
                if !value.is_empty() {
                    let fallback = self.selected_member().backend.as_str();
                    let display_name =
                        unique_display_name_except(value, &self.members, Some(self.selected));
                    let id = unique_member_id(&display_name, &self.members, Some(self.selected));
                    let member = self.selected_member_mut();
                    member.display_name = display_name;
                    member.id = MemberId::new(normalize_member_id(&id, fallback));
                }
            }
            Field::Role => {
                if !value.is_empty() {
                    self.selected_member_mut().role = value.to_string();
                }
            }
            Field::Model => {
                self.selected_member_mut().model = if value.is_empty() {
                    None
                } else {
                    Some(value.to_string())
                };
            }
            Field::Cwd => {
                self.selected_member_mut().cwd = if value.is_empty() {
                    None
                } else {
                    Some(PathBuf::from(value))
                };
            }
            _ => {}
        }
    }

    fn finish(&self) -> Option<TeamConfig> {
        if self.members.is_empty() {
            return None;
        }
        let mut config = TeamConfig::new("custom", self.workspace.clone());
        for member in self.members.clone() {
            config = config.with_member(member);
        }
        if let Some(first) = config.members.first().map(|m| m.id.clone()) {
            config.default_target = Some(DefaultTarget::Member(first));
        }
        config.validate().ok()?;
        Some(config)
    }
}

fn render(frame: &mut ratatui::Frame<'_>, state: &BuilderState) {
    let height = (state.members.len() as u16 + 22).min(frame.area().height);
    let area = centered(frame.area(), 92, height);
    let block = Block::default()
        .title(" Asterline · build your team ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme::accent());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let avail = inner.width as usize;

    let mut lines = vec![
        Line::from(Span::styled(
            "Customize members, backend CLIs, model, and reasoning effort:",
            theme::muted(),
        )),
        Line::raw(""),
        Line::from(Span::styled(" Members", theme::accent_bold())),
    ];

    // Distribute available width across columns dynamically.
    // Layout: " › name @handle backend role=… model=… effort=…"
    let name_w = avail.clamp(8, 18);
    let handle_w = avail.clamp(6, 14);
    let backend_w = 7;
    let rest = avail.saturating_sub(name_w + handle_w + backend_w + 6);
    let role_w = rest.clamp(6, 16);
    let model_w = rest.saturating_sub(role_w).clamp(6, 16);

    for (i, member) in state.members.iter().enumerate() {
        let pointer = if i == state.selected { "›" } else { " " };
        let style = if i == state.selected {
            theme::selection()
        } else {
            theme::emphasis()
        };
        let muted_style = if i == state.selected {
            theme::selection()
        } else {
            theme::muted()
        };
        let backend_color = theme::backend_color(member.backend);
        let backend_style = if i == state.selected {
            theme::selection()
        } else {
            Style::default().fg(backend_color)
        };
        lines.push(Line::from(vec![
            Span::styled(format!(" {pointer} "), style),
            Span::styled(
                theme::pad_width(&truncate(&member.display_name, name_w), name_w),
                style,
            ),
            Span::styled(" ", style),
            Span::styled(
                theme::pad_width(&format!("@{}", member.id), handle_w),
                muted_style,
            ),
            Span::styled(" ", style),
            Span::styled(
                theme::pad_width(member.backend.as_str(), backend_w),
                backend_style,
            ),
            Span::styled(" ", style),
            Span::styled(
                format!("role={} ", theme::clip_width(&member.role, role_w)),
                muted_style,
            ),
            Span::styled(
                format!(
                    "model={} ",
                    theme::clip_width(member.model.as_deref().unwrap_or("default"), model_w)
                ),
                muted_style,
            ),
            Span::styled(
                format!(
                    "effort={}",
                    member
                        .effort
                        .map(|effort| effort.as_str())
                        .unwrap_or("default")
                ),
                muted_style,
            ),
        ]));
    }

    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        " Selected member fields",
        theme::accent_bold(),
    )));

    let selected = state.selected_member();
    lines.push(Line::from(vec![
        Span::styled("     handle: ", theme::muted()),
        Span::styled(format!("@{}", selected.id), theme::accent()),
        Span::styled(" (auto)", theme::muted()),
    ]));
    for (idx, field) in Field::ALL.iter().enumerate() {
        let selected_field = idx == state.field;
        let style = if selected_field {
            theme::selection_cell()
        } else {
            theme::text()
        };
        lines.push(Line::from(Span::styled(
            format!(" {:>10}: {}", field.label(), field_value(selected, *field)),
            style,
        )));
    }

    lines.push(Line::raw(""));
    if let Some(edit) = &state.editing {
        lines.push(Line::from(vec![
            Span::styled(
                format!(" editing {}: ", edit.field.label()),
                theme::warning_bold(),
            ),
            Span::styled(edit.buffer.clone(), theme::emphasis()),
        ]));
        lines.push(Line::from(Span::styled(
            "Enter commit · Esc cancel · Ctrl+U clear",
            theme::muted_italic(),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            "↑/↓ member · ←/→ field · Enter edit/cycle · a add · d delete · s start · Esc quit",
            theme::muted_italic(),
        )));
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    let x = area.x + (area.width - width) / 2;
    let y = area.y + (area.height - height) / 2;
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(y - area.y),
            Constraint::Length(height),
            Constraint::Min(0),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(x - area.x),
            Constraint::Length(width),
            Constraint::Min(0),
        ])
        .split(vertical[1])[1]
}

pub(crate) fn field_value(member: &TeamMember, field: Field) -> String {
    match field {
        Field::Name => member.display_name.clone(),
        Field::Backend => member.backend.as_str().to_string(),
        Field::Role => member.role.clone(),
        Field::Model => member
            .model
            .clone()
            .unwrap_or_else(|| "default".to_string()),
        Field::Effort => member
            .effort
            .map(|effort| effort.as_str().to_string())
            .unwrap_or_else(|| "default".to_string()),
        Field::Sandbox => match member.sandbox {
            SandboxPolicy::ReadOnly => "read-only".to_string(),
            SandboxPolicy::WorkspaceWrite => "workspace-write".to_string(),
            SandboxPolicy::DangerFullAccess => "danger-full-access".to_string(),
        },
        Field::Permission => member
            .permission_mode
            .map(|mode| mode.claude_arg().to_string())
            .unwrap_or_else(|| "default".to_string()),
        Field::Session => match member.session_policy {
            SessionPolicy::Resume => "resume".to_string(),
            SessionPolicy::Fresh => "fresh".to_string(),
        },
        Field::Cwd => member
            .cwd
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "workspace".to_string()),
    }
}

pub(crate) fn cycle_backend(current: BackendKind, available: &[BackendKind]) -> BackendKind {
    if available.is_empty() {
        return current;
    }
    let index = available
        .iter()
        .position(|backend| *backend == current)
        .unwrap_or(0);
    available[(index + 1) % available.len()]
}

pub(crate) fn cycle_effort(current: Option<Effort>) -> Option<Effort> {
    match current {
        None => Some(Effort::Low),
        Some(Effort::Low) => Some(Effort::Medium),
        Some(Effort::Medium) => Some(Effort::High),
        Some(Effort::High) => Some(Effort::Xhigh),
        Some(Effort::Xhigh) => Some(Effort::Max),
        Some(Effort::Max) => None,
    }
}

pub(crate) fn cycle_sandbox(current: SandboxPolicy) -> SandboxPolicy {
    match current {
        SandboxPolicy::ReadOnly => SandboxPolicy::WorkspaceWrite,
        SandboxPolicy::WorkspaceWrite => SandboxPolicy::DangerFullAccess,
        SandboxPolicy::DangerFullAccess => SandboxPolicy::ReadOnly,
    }
}

pub(crate) fn cycle_permission(current: Option<PermissionMode>) -> Option<PermissionMode> {
    match current {
        None => Some(PermissionMode::AcceptEdits),
        Some(PermissionMode::Default) => Some(PermissionMode::AcceptEdits),
        Some(PermissionMode::AcceptEdits) => Some(PermissionMode::Plan),
        Some(PermissionMode::Plan) => Some(PermissionMode::Auto),
        Some(PermissionMode::Auto) => Some(PermissionMode::DontAsk),
        Some(PermissionMode::DontAsk) => Some(PermissionMode::BypassPermissions),
        Some(PermissionMode::BypassPermissions) => None,
    }
}

pub(crate) fn normalize_member_id(value: &str, fallback: &str) -> String {
    normalize_domain_member_id(value, fallback)
}

pub(crate) fn unique_member_id(base: &str, members: &[TeamMember], skip: Option<usize>) -> String {
    let base = normalize_member_id(base, "member");
    let mut candidate = base.clone();
    let mut suffix = 2usize;
    while members
        .iter()
        .enumerate()
        .any(|(idx, member)| Some(idx) != skip && member.id.as_str() == candidate.as_str())
    {
        candidate = format!("{base}-{suffix}");
        suffix += 1;
    }
    candidate
}

pub(crate) fn unique_display_name(base: &str, members: &[TeamMember]) -> String {
    unique_display_name_except(base, members, None)
}

pub(crate) fn unique_display_name_except(
    base: &str,
    members: &[TeamMember],
    skip: Option<usize>,
) -> String {
    let mut candidate = base.to_string();
    let mut suffix = 2usize;
    while members.iter().enumerate().any(|(idx, member)| {
        Some(idx) != skip && member.display_name.eq_ignore_ascii_case(&candidate)
    }) {
        candidate = format!("{base} {suffix}");
        suffix += 1;
    }
    candidate
}

pub(crate) fn truncate(value: &str, max: usize) -> String {
    crate::tui::theme::clip_width(value, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_builder_allows_duplicate_backends_with_unique_ids() {
        let available = [BackendKind::Codex, BackendKind::Agy];
        let mut state = BuilderState::new(PathBuf::from("/tmp/ws"), &available);
        state.add_member();
        state.members[2].model = Some("model-x".to_string());
        state.members[2].effort = Some(Effort::High);

        let config = state.finish().expect("valid team");
        assert!(config.validate().is_ok());
        assert_eq!(config.members.len(), 3);
        assert_eq!(config.members[0].backend, BackendKind::Codex);
        assert_eq!(config.members[2].backend, BackendKind::Codex);
        assert_eq!(config.members[2].model.as_deref(), Some("model-x"));
        assert_eq!(config.members[2].effort, Some(Effort::High));
    }

    #[test]
    fn name_commit_derives_and_deduplicates_id() {
        let available = [BackendKind::Codex];
        let mut state = BuilderState::new(PathBuf::from("/tmp/ws"), &available);
        state.add_member();
        state.selected = 1;
        state.commit_edit(EditState {
            field: Field::Name,
            buffer: "Builder".to_string(),
        });

        assert_eq!(state.members[1].id, MemberId::new("builder-2"));
        assert_eq!(state.members[1].display_name, "Builder 2");
    }
}
