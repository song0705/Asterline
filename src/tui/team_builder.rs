//! Interactive startup team builder.
//!
//! When no `--team` config is given, Asterline detects which backend CLIs are
//! available and lets you build a roster. The builder supports multiple members
//! on the same backend, per-member model, and per-member reasoning effort. On a
//! non-interactive stdout it falls back to the established default roster.

use std::collections::HashMap;
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use crossterm::event::{self, EnableBracketedPaste, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, enable_raw_mode};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use unicode_width::UnicodeWidthChar;

use crate::domain::config::{DetectedBackends, default_member, default_team};
use crate::domain::team::{
    BackendKind, DefaultTarget, Effort, MemberId, PermissionMode, SandboxPolicy, SessionPolicy,
    TeamConfig, TeamMember, normalize_member_id as normalize_domain_member_id,
};
use crate::tui::theme;

#[derive(Debug)]
enum ModelLoad {
    Loading(Receiver<Result<Vec<String>, String>>),
    Ready(Result<Vec<String>, String>),
}

#[derive(Debug, Default)]
pub(crate) struct ModelCatalog {
    loads: HashMap<(BackendKind, PathBuf), ModelLoad>,
}

pub(crate) enum ModelChoices {
    Loading,
    Ready(Vec<String>),
    Failed(String),
}

impl ModelCatalog {
    pub(crate) fn models(&mut self, backend: BackendKind, cwd: &Path) -> ModelChoices {
        let key = (backend, cwd.to_path_buf());
        match self.loads.entry(key) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                let worker_cwd = cwd.to_path_buf();
                let (tx, rx) = mpsc::channel();
                thread::spawn(move || {
                    let _ = tx.send(crate::adapter::discover_models(backend, &worker_cwd));
                });
                entry.insert(ModelLoad::Loading(rx));
                ModelChoices::Loading
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                let result = match entry.get_mut() {
                    ModelLoad::Loading(rx) => match rx.try_recv() {
                        Ok(result) => result,
                        Err(TryRecvError::Empty) => return ModelChoices::Loading,
                        Err(TryRecvError::Disconnected) => {
                            Err("model discovery worker stopped unexpectedly".to_string())
                        }
                    },
                    ModelLoad::Ready(result) => return model_choices(result),
                };
                let choices = model_choices(&result);
                entry.insert(ModelLoad::Ready(result));
                choices
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn seed(&mut self, backend: BackendKind, cwd: &Path, models: Vec<String>) {
        self.loads
            .insert((backend, cwd.to_path_buf()), ModelLoad::Ready(Ok(models)));
    }
}

fn model_choices(result: &Result<Vec<String>, String>) -> ModelChoices {
    match result {
        Ok(models) => ModelChoices::Ready(models.clone()),
        Err(err) => ModelChoices::Failed(err.clone()),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ModelPicker {
    options: Vec<Option<String>>,
    selected: usize,
}

impl ModelPicker {
    pub(crate) fn new(current: Option<&str>, models: Vec<String>) -> Self {
        let mut options = vec![None];
        if let Some(current) = current
            && !models.iter().any(|model| model == current)
        {
            options.push(Some(current.to_string()));
        }
        options.extend(models.into_iter().map(Some));
        let selected = current
            .and_then(|current| {
                options
                    .iter()
                    .position(|model| model.as_deref() == Some(current))
            })
            .unwrap_or(0);
        Self { options, selected }
    }

    pub(crate) fn options(&self) -> &[Option<String>] {
        &self.options
    }

    pub(crate) fn selected(&self) -> usize {
        self.selected
    }

    pub(crate) fn window(&self, max: usize) -> (usize, &[Option<String>]) {
        let max = max.max(1).min(self.options.len());
        let start = self
            .selected
            .saturating_sub(max / 2)
            .min(self.options.len().saturating_sub(max));
        (start, &self.options[start..start + max])
    }

    pub(crate) fn up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub(crate) fn down(&mut self) {
        if self.selected + 1 < self.options.len() {
            self.selected += 1;
        }
    }

    pub(crate) fn value(&self) -> Option<String> {
        self.options.get(self.selected).cloned().flatten()
    }
}

/// Pick a team interactively from the detected backends. Returns `None` if the
/// user cancels or nothing is available.
pub fn run(detected: DetectedBackends, workspace: &Path) -> io::Result<Option<TeamConfig>> {
    super::enable_tui_colors();
    let available: Vec<BackendKind> = [
        BackendKind::Codex,
        BackendKind::Claude,
        BackendKind::Grok,
        BackendKind::Agy,
    ]
    .into_iter()
    .filter(|b| is_detected(*b, detected))
    .collect();

    if available.is_empty() {
        return Ok(None);
    }
    if !io::stdout().is_terminal() {
        return Ok(default_team(workspace.to_path_buf(), detected));
    }

    let mut restore = super::TerminalRestore::default();
    enable_raw_mode()?;
    restore.raw_mode = true;
    let mut stdout = io::stdout();
    restore.alternate_screen = true;
    restore.bracketed_paste = true;
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let outcome = select_loop(&mut terminal, workspace, &available);

    let cleanup = restore.restore();
    match outcome {
        Err(err) => Err(err),
        Ok(value) => cleanup.map(|()| value),
    }
}

fn is_detected(backend: BackendKind, detected: DetectedBackends) -> bool {
    match backend {
        BackendKind::Codex => detected.codex,
        BackendKind::Claude => detected.claude,
        BackendKind::Grok => detected.grok,
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

        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if state.handle_key(key.code, key.modifiers) {
                    return Ok(state.finish());
                }
                if state.cancelled {
                    return Ok(None);
                }
            }
            Event::Paste(text) => {
                if let Some(edit) = state.editing.as_mut() {
                    edit.insert_text(&text);
                }
            }
            _ => {}
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
    SessionId,
    Cwd,
}

impl Field {
    pub(crate) const ALL: [Field; 10] = [
        Field::Name,
        Field::Backend,
        Field::Role,
        Field::Model,
        Field::Effort,
        Field::Sandbox,
        Field::Permission,
        Field::Session,
        Field::SessionId,
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
            Self::SessionId => "session id",
            Self::Cwd => "cwd",
        }
    }

    pub(crate) fn is_text(self) -> bool {
        matches!(
            self,
            Self::Name | Self::Role | Self::Model | Self::SessionId | Self::Cwd
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EditState {
    pub(crate) field: Field,
    pub(crate) buffer: String,
    /// Cursor as a Unicode scalar index, so movement never splits UTF-8.
    pub(crate) cursor: usize,
}

impl EditState {
    pub(crate) fn new(field: Field, buffer: String) -> Self {
        let cursor = buffer.chars().count();
        Self {
            field,
            buffer,
            cursor,
        }
    }

    pub(crate) fn insert_text(&mut self, text: &str) {
        let text = text.replace("\r\n", " ").replace(['\r', '\n'], " ");
        let mut chars = self.buffer.chars().collect::<Vec<_>>();
        let inserted = text.chars().collect::<Vec<_>>();
        let count = inserted.len();
        let insert_at = self.cursor.min(chars.len());
        chars.splice(insert_at..insert_at, inserted);
        self.cursor = insert_at.saturating_add(count);
        self.buffer = chars.into_iter().collect();
    }

    pub(crate) fn apply_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        let ctrl = modifiers.contains(KeyModifiers::CONTROL);
        let alt = modifiers.contains(KeyModifiers::ALT);
        match code {
            KeyCode::Left => self.move_left(),
            KeyCode::Right => self.move_right(),
            KeyCode::Char('b') if ctrl => self.move_left(),
            KeyCode::Char('f') if ctrl => self.move_right(),
            KeyCode::Char('b') if alt => self.move_word_left(),
            KeyCode::Char('f') if alt => self.move_word_right(),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.char_len(),
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::Char('e') if ctrl => self.cursor = self.char_len(),
            KeyCode::Backspace => self.delete_backward(),
            KeyCode::Delete => self.delete_forward(),
            KeyCode::Char('d') if ctrl => self.delete_forward(),
            KeyCode::Char('u') if ctrl => self.delete_to_start(),
            KeyCode::Char('k') if ctrl => self.delete_to_end(),
            KeyCode::Char('w') if ctrl => self.delete_word_backward(),
            KeyCode::Char(ch) if !ctrl && !alt && !ch.is_control() => {
                self.insert_text(&ch.to_string());
            }
            _ => {}
        }
    }

    pub(crate) fn visible_window(&self, width: usize) -> (String, u16) {
        if width == 0 {
            return (String::new(), 0);
        }
        let chars = self.buffer.chars().collect::<Vec<_>>();
        let cursor = self.cursor.min(chars.len());
        let mut start = cursor;
        let mut cursor_width = 0;
        while start > 0 {
            let char_width = UnicodeWidthChar::width(chars[start - 1]).unwrap_or(0);
            if char_width > 0 && cursor_width + char_width > width.saturating_sub(1) {
                break;
            }
            start -= 1;
            cursor_width += char_width;
        }
        let mut visible = String::new();
        let mut visible_width = 0;
        for ch in &chars[start..] {
            let char_width = UnicodeWidthChar::width(*ch).unwrap_or(0);
            if char_width > 0 && visible_width + char_width > width {
                break;
            }
            visible.push(*ch);
            visible_width += char_width;
        }
        (visible, cursor_width.min(width) as u16)
    }

    fn char_len(&self) -> usize {
        self.buffer.chars().count()
    }

    fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    fn move_right(&mut self) {
        self.cursor = self.cursor.saturating_add(1).min(self.char_len());
    }

    fn move_word_left(&mut self) {
        let chars = self.buffer.chars().collect::<Vec<_>>();
        while self.cursor > 0 && chars[self.cursor - 1].is_whitespace() {
            self.cursor -= 1;
        }
        while self.cursor > 0 && !chars[self.cursor - 1].is_whitespace() {
            self.cursor -= 1;
        }
    }

    fn move_word_right(&mut self) {
        let chars = self.buffer.chars().collect::<Vec<_>>();
        while self.cursor < chars.len() && !chars[self.cursor].is_whitespace() {
            self.cursor += 1;
        }
        while self.cursor < chars.len() && chars[self.cursor].is_whitespace() {
            self.cursor += 1;
        }
    }

    fn delete_backward(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let mut chars = self.buffer.chars().collect::<Vec<_>>();
        self.cursor -= 1;
        chars.remove(self.cursor);
        self.buffer = chars.into_iter().collect();
    }

    fn delete_forward(&mut self) {
        let mut chars = self.buffer.chars().collect::<Vec<_>>();
        if self.cursor < chars.len() {
            chars.remove(self.cursor);
            self.buffer = chars.into_iter().collect();
        }
    }

    fn delete_to_start(&mut self) {
        let mut chars = self.buffer.chars().collect::<Vec<_>>();
        chars.drain(..self.cursor.min(chars.len()));
        self.cursor = 0;
        self.buffer = chars.into_iter().collect();
    }

    fn delete_to_end(&mut self) {
        let mut chars = self.buffer.chars().collect::<Vec<_>>();
        chars.truncate(self.cursor.min(chars.len()));
        self.buffer = chars.into_iter().collect();
    }

    fn delete_word_backward(&mut self) {
        let end = self.cursor;
        self.move_word_left();
        let mut chars = self.buffer.chars().collect::<Vec<_>>();
        chars.drain(self.cursor..end.min(chars.len()));
        self.buffer = chars.into_iter().collect();
    }
}

struct BuilderState {
    workspace: PathBuf,
    available: Vec<BackendKind>,
    members: Vec<TeamMember>,
    selected: usize,
    field: usize,
    field_mode: bool,
    editing: Option<EditState>,
    model_catalog: ModelCatalog,
    model_picker: Option<ModelPicker>,
    notice: Option<String>,
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
            field_mode: false,
            editing: None,
            model_catalog: ModelCatalog::default(),
            model_picker: None,
            notice: None,
            cancelled: false,
        }
    }

    fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        if self.model_picker.is_some() {
            self.handle_model_picker_key(code);
            return false;
        }
        if self.editing.is_some() {
            self.handle_edit_key(code, modifiers);
            return false;
        }

        let ctrl = modifiers.contains(KeyModifiers::CONTROL);
        match code {
            KeyCode::Char('c') if ctrl => self.cancelled = true,
            KeyCode::Esc if self.field_mode => self.field_mode = false,
            KeyCode::Esc | KeyCode::Char('q') => self.cancelled = true,
            KeyCode::Up | KeyCode::Char('k') => {
                if self.field_mode {
                    self.prev_field();
                } else {
                    self.selected = self.selected.saturating_sub(1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.field_mode {
                    self.next_field();
                } else if self.selected + 1 < self.members.len() {
                    self.selected += 1;
                }
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab => {}
            KeyCode::Char('a') if !self.field_mode => self.add_member(),
            KeyCode::Char('d') if !self.field_mode => self.delete_member(),
            KeyCode::Char('s') => return true,
            KeyCode::Char('e') if self.field_mode && self.selected_field() == Field::Model => {
                self.edit_selected_field()
            }
            KeyCode::Enter if self.field_mode => self.activate_field(),
            KeyCode::Enter => self.field_mode = true,
            _ => {}
        }
        false
    }

    fn handle_model_picker_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(picker) = &mut self.model_picker {
                    picker.up();
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(picker) = &mut self.model_picker {
                    picker.down();
                }
            }
            KeyCode::Enter => {
                let value = self.model_picker.as_ref().and_then(ModelPicker::value);
                self.selected_member_mut().model = value;
                self.model_picker = None;
                self.notice = Some("model selected · press s to start".to_string());
            }
            KeyCode::Esc | KeyCode::Char('q') => self.model_picker = None,
            _ => {}
        }
    }

    fn handle_edit_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        let Some(mut edit) = self.editing.take() else {
            return;
        };
        match code {
            KeyCode::Esc => {}
            KeyCode::Enter => self.commit_edit(edit),
            _ => {
                edit.apply_key(code, modifiers);
                self.editing = Some(edit);
            }
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
        if field == Field::Model {
            self.cycle_model();
        } else if field.is_text() {
            self.edit_selected_field();
        } else {
            self.cycle_field(field);
        }
    }

    fn edit_selected_field(&mut self) {
        let field = self.selected_field();
        if field.is_text() {
            self.editing = Some(EditState::new(
                field,
                field_value(self.selected_member(), field),
            ));
        }
    }

    fn cycle_model(&mut self) {
        let backend = self.selected_member().backend;
        let cwd = self.selected_member().resolved_cwd(&self.workspace);
        match self.model_catalog.models(backend, &cwd) {
            ModelChoices::Loading => {
                self.notice = Some(format!(
                    "loading {} models in the background · press Enter again shortly",
                    backend.as_str()
                ));
            }
            ModelChoices::Ready(models) => {
                self.model_picker = Some(ModelPicker::new(
                    self.selected_member().model.as_deref(),
                    models,
                ));
                self.notice = Some("↑/↓ choose model · Enter select · Esc cancel".to_string());
            }
            ModelChoices::Failed(err) => self.notice = Some(err),
        }
    }

    fn cycle_field(&mut self, field: Field) {
        match field {
            Field::Backend => {
                let current = self.selected_member().backend;
                let next = cycle_backend(current, &self.available);
                let member = self.selected_member_mut();
                member.backend = next;
                member.session_id = None;
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
                let member = self.selected_member_mut();
                member.session_policy = next;
                if next == SessionPolicy::Fresh {
                    member.session_id = None;
                }
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
                self.selected_member_mut().model = if value.is_empty() || value == "default" {
                    None
                } else {
                    Some(value.to_string())
                };
            }
            Field::SessionId => {
                let session_id = if value.is_empty() || value.eq_ignore_ascii_case("auto") {
                    None
                } else {
                    Some(value.to_string())
                };
                let member = self.selected_member_mut();
                member.session_id = session_id;
                if member.session_id.is_some() {
                    member.session_policy = SessionPolicy::Resume;
                }
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
    let picker_height = state
        .model_picker
        .as_ref()
        .map(|picker| picker.window(8).1.len() as u16 + 3)
        .unwrap_or(0);
    let height = (state.members.len() as u16 + 22 + picker_height).min(frame.area().height);
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
    // Layout: " ▶ name @handle backend role=… model=… effort=…"
    let name_w = avail.clamp(8, 18);
    let handle_w = avail.clamp(6, 14);
    let backend_w = 7;
    let rest = avail.saturating_sub(name_w + handle_w + backend_w + 6);
    let role_w = rest.clamp(6, 16);
    let model_w = rest.saturating_sub(role_w).clamp(6, 16);

    for (i, member) in state.members.iter().enumerate() {
        let selected = i == state.selected;
        let style = if i == state.selected {
            theme::bold(theme::emphasis_color())
        } else {
            theme::emphasis()
        };
        let muted_style = if i == state.selected {
            theme::bold(theme::emphasis_color())
        } else {
            theme::muted()
        };
        let backend_color = theme::backend_color(member.backend);
        let backend_style = if i == state.selected {
            theme::bold(theme::emphasis_color())
        } else {
            Style::default().fg(backend_color)
        };
        lines.push(Line::from(vec![
            Span::styled(
                if selected { " ▶ " } else { "   " },
                if selected {
                    theme::warning_bold()
                } else {
                    theme::muted()
                },
            ),
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
        let selected_field = state.field_mode && idx == state.field;
        let style = if selected_field {
            theme::editor_field_focus()
        } else {
            theme::text()
        };
        lines.push(Line::from(Span::styled(
            format!(
                " {} {:>10}: {}",
                if selected_field { "›" } else { " " },
                field.label(),
                field_value(selected, *field)
            ),
            style,
        )));
    }

    if let Some(picker) = &state.model_picker {
        lines.push(Line::raw(""));
        lines.push(Line::styled(" Model choices", theme::accent_bold()));
        let (start, options) = picker.window(8);
        if start > 0 {
            lines.push(Line::styled("    …", theme::muted()));
        }
        for (offset, model) in options.iter().enumerate() {
            let selected = start + offset == picker.selected();
            lines.push(Line::from(vec![
                Span::styled(
                    if selected { "  › " } else { "    " },
                    if selected {
                        theme::editor_focus()
                    } else {
                        theme::muted()
                    },
                ),
                Span::styled(
                    model.as_deref().unwrap_or("default").to_string(),
                    if selected {
                        theme::emphasis()
                    } else {
                        theme::text()
                    },
                ),
            ]));
        }
        if start + options.len() < picker.options().len() {
            lines.push(Line::styled("    …", theme::muted()));
        }
    }

    lines.push(Line::raw(""));
    if let Some(notice) = &state.notice {
        lines.push(Line::from(Span::styled(notice.clone(), theme::warning())));
    }
    if state.editing.is_none() {
        lines.push(Line::from(Span::styled(
            if state.field_mode {
                "↑/↓ field · Enter edit/cycle · e manual model · s start · Esc members"
            } else {
                "↑/↓ member · Enter fields · a add · d delete · s start · Esc quit"
            },
            theme::muted_italic(),
        )));
    }

    frame.render_widget(Paragraph::new(lines), inner);
    if let Some(edit) = &state.editing {
        render_edit_box(frame, inner, edit);
    }
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

/// Focused single-line editor shared by startup and the live Team drawer.
pub(crate) fn render_edit_box(frame: &mut ratatui::Frame<'_>, area: Rect, edit: &EditState) {
    let popup = centered(
        area,
        area.width.saturating_sub(2).min(72),
        area.height.min(7),
    );
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .title(format!(" Edit {} ", edit.field.label()))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme::accent_bold());
    let body = block.inner(popup);
    frame.render_widget(block, popup);
    if body.height == 0 {
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(body);
    let input_block = Block::default()
        .title(" Value ")
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(theme::warning_bold());
    let input_inner = input_block.inner(chunks[0]);
    frame.render_widget(input_block, chunks[0]);
    let (visible, cursor_x) = edit.visible_window(input_inner.width as usize);
    frame.render_widget(Paragraph::new(Line::raw(visible)), input_inner);
    if input_inner.width > 0 && input_inner.height > 0 {
        frame.set_cursor_position((
            input_inner.x + cursor_x.min(input_inner.width.saturating_sub(1)),
            input_inner.y,
        ));
    }
    if chunks[1].height > 0 {
        frame.render_widget(
            Paragraph::new(Line::styled(
                " Enter save · Esc cancel · ←/→ move · Ctrl+U/W/K edit",
                theme::muted_italic(),
            )),
            chunks[1],
        );
    }
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
        Field::SessionId => member
            .session_id
            .clone()
            .unwrap_or_else(|| "auto".to_string()),
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
        state.commit_edit(EditState::new(Field::Name, "Builder".to_string()));

        assert_eq!(state.members[1].id, MemberId::new("builder-2"));
        assert_eq!(state.members[1].display_name, "Builder 2");
    }

    #[test]
    fn enter_opens_fields_and_up_down_select_them() {
        let available = [BackendKind::Codex, BackendKind::Claude];
        let mut state = BuilderState::new(PathBuf::from("/tmp/ws"), &available);

        state.handle_key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(state.selected, 1);
        assert_eq!(state.selected_field(), Field::Name);

        state.handle_key(KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(state.selected_field(), Field::Name);

        state.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(state.field_mode);
        assert!(state.editing.is_none());

        state.handle_key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(state.selected_field(), Field::Backend);
        assert_eq!(state.selected, 1);

        state.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(!state.field_mode);
        assert!(!state.cancelled);
    }

    #[test]
    fn grok_model_field_opens_picker_and_selects_choice() {
        let mut state = BuilderState::new(PathBuf::from("/tmp/ws"), &[BackendKind::Grok]);
        state.field = Field::ALL
            .iter()
            .position(|field| *field == Field::Model)
            .unwrap();
        state.model_catalog.seed(
            BackendKind::Grok,
            Path::new("/tmp/ws"),
            vec!["grok-build".to_string(), "grok-4.5".to_string()],
        );

        state.activate_field();
        assert!(state.model_picker.is_some());
        state.handle_model_picker_key(KeyCode::Down);
        state.handle_model_picker_key(KeyCode::Down);
        state.handle_model_picker_key(KeyCode::Enter);
        assert_eq!(state.members[0].model.as_deref(), Some("grok-4.5"));
    }

    #[test]
    fn model_catalog_is_scoped_to_member_working_directory() {
        let mut catalog = ModelCatalog::default();
        catalog.seed(
            BackendKind::Claude,
            Path::new("/tmp/one"),
            vec!["project-one".to_string()],
        );
        catalog.seed(
            BackendKind::Claude,
            Path::new("/tmp/two"),
            vec!["project-two".to_string()],
        );

        let ModelChoices::Ready(one) = catalog.models(BackendKind::Claude, Path::new("/tmp/one"))
        else {
            panic!("expected first project model");
        };
        let ModelChoices::Ready(two) = catalog.models(BackendKind::Claude, Path::new("/tmp/two"))
        else {
            panic!("expected second project model");
        };

        assert_eq!(one, vec!["project-one"]);
        assert_eq!(two, vec!["project-two"]);
    }

    #[test]
    fn model_picker_preserves_a_current_custom_model() {
        let picker = ModelPicker::new(Some("company-model"), vec!["sonnet".to_string()]);

        assert_eq!(picker.value().as_deref(), Some("company-model"));
        assert_eq!(picker.selected(), 1);
    }

    #[test]
    fn edit_state_moves_and_edits_unicode_at_the_cursor() {
        let mut edit = EditState::new(Field::Role, "你 model".to_string());
        edit.apply_key(KeyCode::Left, KeyModifiers::NONE);
        edit.apply_key(KeyCode::Left, KeyModifiers::NONE);
        edit.insert_text("好");
        assert_eq!(edit.buffer, "你 mod好el");

        edit.apply_key(KeyCode::Char('u'), KeyModifiers::CONTROL);
        assert_eq!(edit.buffer, "el");
        assert_eq!(edit.cursor, 0);
        edit.apply_key(KeyCode::Delete, KeyModifiers::NONE);
        assert_eq!(edit.buffer, "l");
    }

    #[test]
    fn edit_state_windows_long_values_around_cursor() {
        let mut edit = EditState::new(Field::Cwd, "/very/long/project/path".to_string());
        edit.apply_key(KeyCode::Home, KeyModifiers::NONE);
        edit.apply_key(KeyCode::Right, KeyModifiers::NONE);
        let (visible, cursor) = edit.visible_window(8);
        assert!(theme::display_width(&visible) <= 8);
        assert_eq!(cursor, 1);
    }
}
