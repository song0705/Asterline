//! Live team roster editor used by the `/team` drawer.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use crossterm::event::{KeyCode, KeyModifiers};

use crate::domain::config::{DetectedBackends, default_member, detect_backends};
use crate::domain::event::UiCommand;
use crate::domain::team::{BackendKind, DefaultTarget, MemberId, TeamConfig, TeamMember};
use crate::tui::session_picker::SessionPicker;
use crate::tui::team_builder::{
    BackendPicker, EditState, Field, ModelCatalog, ModelChoices, ModelPicker, cycle_effort,
    cycle_permission, cycle_sandbox, field_value, normalize_member_id, unique_display_name,
    unique_display_name_except, unique_member_id,
};

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum TeamEditorOutcome {
    Ignored,
    Consumed(Option<UiCommand>),
    /// Apply a narrow picker-originated update, then return to the chat.
    ApplyAndClose(UiCommand),
    Close,
}

#[derive(Debug)]
pub(crate) struct TeamEditor {
    team: String,
    workspace: PathBuf,
    default_target: Option<DefaultTarget>,
    members: Vec<TeamMember>,
    detected: DetectedBackends,
    available: Vec<BackendKind>,
    selected: usize,
    field: usize,
    field_mode: bool,
    editing: Option<EditState>,
    model_catalog: ModelCatalog,
    backend_picker: Option<BackendPicker>,
    model_picker: Option<ModelPicker>,
    model_picker_pending: bool,
    /// `/model` uses the same picker as `/team`, but applies its selected
    /// model and effort immediately instead of leaving a draft to save.
    model_picker_apply_immediately: bool,
    session_picker: Option<SessionPicker>,
    backend_detection: Option<Receiver<DetectedBackends>>,
    dirty: bool,
    notice: Option<String>,
}

impl TeamEditor {
    #[cfg(test)]
    pub(crate) fn new(
        team: impl Into<String>,
        workspace: impl Into<PathBuf>,
        default_target: Option<DefaultTarget>,
        members: Vec<TeamMember>,
    ) -> Self {
        Self::with_model_catalog(
            team,
            workspace,
            default_target,
            members,
            ModelCatalog::default(),
        )
    }

    /// Construct an editor with a catalog retained by the surrounding TUI.
    /// The catalog is safe to move between drawers: it owns its worker
    /// receivers and is already keyed by backend plus member working directory.
    pub(crate) fn with_model_catalog(
        team: impl Into<String>,
        workspace: impl Into<PathBuf>,
        default_target: Option<DefaultTarget>,
        members: Vec<TeamMember>,
        model_catalog: ModelCatalog,
    ) -> Self {
        Self {
            team: team.into(),
            workspace: workspace.into(),
            default_target,
            members,
            detected: DetectedBackends {
                codex: false,
                claude: false,
                grok: false,
                agy: false,
            },
            available: Vec::new(),
            selected: 0,
            field: 0,
            field_mode: false,
            editing: None,
            model_catalog,
            backend_picker: None,
            model_picker: None,
            model_picker_pending: false,
            model_picker_apply_immediately: false,
            session_picker: None,
            backend_detection: None,
            dirty: false,
            notice: None,
        }
    }

    /// Return the reusable catalog when this short-lived editor closes.
    pub(crate) fn into_model_catalog(self) -> ModelCatalog {
        self.model_catalog
    }

    pub(crate) fn members(&self) -> &[TeamMember] {
        &self.members
    }

    pub(crate) fn selected(&self) -> usize {
        self.selected
    }

    pub(crate) fn field_index(&self) -> usize {
        self.field
    }

    pub(crate) fn field_mode(&self) -> bool {
        self.field_mode
    }

    pub(crate) fn editing(&self) -> Option<&EditState> {
        self.editing.as_ref()
    }

    pub(crate) fn insert_edit_text(&mut self, text: &str) -> bool {
        let Some(edit) = self.editing.as_mut() else {
            return false;
        };
        edit.insert_text(text);
        true
    }

    pub(crate) fn dirty(&self) -> bool {
        self.dirty
    }

    pub(crate) fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    pub(crate) fn model_picker(&self) -> Option<&ModelPicker> {
        self.model_picker.as_ref()
    }

    pub(crate) fn model_picker_applies_immediately(&self) -> bool {
        self.model_picker_apply_immediately
    }

    /// Focus one member's discovered model catalog. This is shared by the
    /// `/model` command and the normal Team editor field, so it retains the
    /// same async discovery and model-specific effort choices.
    pub(crate) fn open_model_picker_for(&mut self, member: &MemberId) -> Result<(), String> {
        let Some(selected) = self
            .members
            .iter()
            .position(|candidate| &candidate.id == member)
        else {
            return Err(format!("unknown member: {member}"));
        };
        self.selected = selected;
        self.field = Field::ALL
            .iter()
            .position(|field| *field == Field::Model)
            .expect("Model is a Team editor field");
        self.field_mode = true;
        self.editing = None;
        self.backend_picker = None;
        self.session_picker = None;
        self.model_picker = None;
        self.model_picker_pending = false;
        self.model_picker_apply_immediately = true;
        self.cycle_model(true);
        Ok(())
    }

    pub(crate) fn backend_picker(&self) -> Option<&BackendPicker> {
        self.backend_picker.as_ref()
    }

    pub(crate) fn model_catalog(&self) -> &ModelCatalog {
        &self.model_catalog
    }

    pub(crate) fn selected_cwd(&self) -> PathBuf {
        self.selected_member()
            .map(|member| member.resolved_cwd(&self.workspace))
            .unwrap_or_else(|| self.workspace.clone())
    }

    pub(crate) fn agent_availability_label(&self) -> String {
        if self.backend_detection.is_some() {
            return "checking installed Agent CLIs…".to_string();
        }
        [
            BackendKind::Codex,
            BackendKind::Claude,
            BackendKind::Grok,
            BackendKind::Agy,
        ]
        .into_iter()
        .map(|backend| {
            format!(
                "{} {}",
                backend.as_str(),
                if self.detected.contains(backend) {
                    "✓"
                } else {
                    "✕"
                }
            )
        })
        .collect::<Vec<_>>()
        .join(" · ")
    }

    pub(crate) fn load_agent_catalog(&mut self) {
        if self.backend_detection.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(detect_backends());
        });
        self.backend_detection = Some(rx);
        self.notice = Some("checking installed Agent CLIs…".to_string());
    }

    pub(crate) fn poll_agent_catalog(&mut self) {
        let result = self
            .backend_detection
            .as_ref()
            .map(|receiver| receiver.try_recv());
        match result {
            Some(Ok(detected)) => {
                self.backend_detection = None;
                self.detected = detected;
                self.available = [
                    BackendKind::Codex,
                    BackendKind::Claude,
                    BackendKind::Grok,
                    BackendKind::Agy,
                ]
                .into_iter()
                .filter(|backend| self.detected.contains(*backend))
                .collect();
                self.notice = Some(if self.available.is_empty() {
                    "no supported Agent CLI found on PATH".to_string()
                } else {
                    "Agent CLIs ready · open a Model field to load its catalog".to_string()
                });
            }
            Some(Err(TryRecvError::Disconnected)) => {
                self.backend_detection = None;
                self.notice = Some("Agent CLI check stopped unexpectedly".to_string());
            }
            Some(Err(TryRecvError::Empty)) | None => {}
        }
        self.model_catalog.poll();
        if self.model_picker_pending
            && self.field_mode
            && self.selected_field() == Field::Model
            && self.model_picker.is_none()
            && self.editing.is_none()
        {
            self.cycle_model(false);
        }
    }

    pub(crate) fn session_picker(&self) -> Option<&SessionPicker> {
        self.session_picker.as_ref()
    }

    pub(crate) fn default_label(&self) -> String {
        match self.normalized_default_target() {
            Some(DefaultTarget::Member(id)) => id.to_string(),
            Some(DefaultTarget::All) => "all".to_string(),
            None => "first member".to_string(),
        }
    }

    pub(crate) fn default_marker(&self, member: &TeamMember) -> &'static str {
        match self.normalized_default_target() {
            Some(DefaultTarget::All) => "all",
            Some(DefaultTarget::Member(id)) if id == member.id => "default",
            _ => "",
        }
    }

    pub(crate) fn selected_field(&self) -> Field {
        Field::ALL[self.field]
    }

    pub(crate) fn selected_member(&self) -> Option<&TeamMember> {
        self.members.get(self.selected)
    }

    pub(crate) fn field_value(&self, member: &TeamMember, field: Field) -> String {
        match field {
            Field::Model => self.model_catalog.model_label(member, &self.workspace),
            Field::Effort => self.model_catalog.effort_label(member, &self.workspace),
            _ => field_value(member, field),
        }
    }

    pub(crate) fn handle_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> TeamEditorOutcome {
        if self.backend_picker.is_some() {
            self.handle_backend_picker_key(code);
            return TeamEditorOutcome::Consumed(None);
        }
        if self.session_picker.is_some() {
            self.handle_session_picker_key(code, modifiers);
            return TeamEditorOutcome::Consumed(None);
        }
        if self.model_picker.is_some() {
            return match self.handle_model_picker_key(code, modifiers) {
                Some(command) => TeamEditorOutcome::ApplyAndClose(command),
                None => TeamEditorOutcome::Consumed(None),
            };
        }
        if self.editing.is_some() {
            self.handle_edit_key(code, modifiers);
            return TeamEditorOutcome::Consumed(None);
        }

        let ctrl = modifiers.contains(KeyModifiers::CONTROL);
        match code {
            KeyCode::Char('c') if ctrl => TeamEditorOutcome::Close,
            KeyCode::Esc if self.field_mode => {
                self.field_mode = false;
                self.model_picker_pending = false;
                self.model_picker_apply_immediately = false;
                TeamEditorOutcome::Consumed(None)
            }
            KeyCode::Esc | KeyCode::Char('q') => TeamEditorOutcome::Close,
            KeyCode::Up | KeyCode::Char('k') => {
                if self.field_mode {
                    self.prev_field();
                    self.model_picker_pending = false;
                    self.model_picker_apply_immediately = false;
                } else {
                    self.selected = self.selected.saturating_sub(1);
                }
                TeamEditorOutcome::Consumed(None)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.field_mode {
                    self.next_field();
                    self.model_picker_pending = false;
                    self.model_picker_apply_immediately = false;
                } else if self.selected + 1 < self.members.len() {
                    self.selected += 1;
                }
                TeamEditorOutcome::Consumed(None)
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab => {
                TeamEditorOutcome::Consumed(None)
            }
            KeyCode::Char('a') if !self.field_mode => {
                self.add_member();
                TeamEditorOutcome::Consumed(None)
            }
            KeyCode::Char('d') if !self.field_mode => {
                self.delete_member();
                TeamEditorOutcome::Consumed(None)
            }
            KeyCode::Char('t') if !self.field_mode => {
                self.set_default_to_selected();
                TeamEditorOutcome::Consumed(None)
            }
            KeyCode::Char('*') if !self.field_mode => {
                self.default_target = Some(DefaultTarget::All);
                self.dirty = true;
                self.notice =
                    Some("default target set to all members; press s to apply".to_string());
                TeamEditorOutcome::Consumed(None)
            }
            KeyCode::Char('s') => TeamEditorOutcome::Consumed(self.apply_command()),
            KeyCode::Char('r') => {
                self.notice = Some("discard changes by closing and reopening /team".to_string());
                TeamEditorOutcome::Consumed(None)
            }
            KeyCode::Char('e')
                if self.field_mode
                    && matches!(self.selected_field(), Field::Model | Field::SessionId) =>
            {
                self.edit_selected_field();
                TeamEditorOutcome::Consumed(None)
            }
            KeyCode::Enter if self.field_mode => {
                self.activate_field();
                TeamEditorOutcome::Consumed(None)
            }
            KeyCode::Enter => {
                self.field_mode = true;
                TeamEditorOutcome::Consumed(None)
            }
            KeyCode::Backspace | KeyCode::Char(_) => TeamEditorOutcome::Consumed(None),
            _ => TeamEditorOutcome::Ignored,
        }
    }

    fn handle_backend_picker_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Up => self.backend_picker.as_mut().unwrap().up(),
            KeyCode::Down => self.backend_picker.as_mut().unwrap().down(),
            KeyCode::Enter => {
                let Some(choice) = self
                    .backend_picker
                    .as_ref()
                    .and_then(BackendPicker::selected_choice)
                else {
                    return;
                };
                if !choice.installed {
                    self.notice = Some(format!(
                        "{} is not installed on PATH",
                        choice.backend.as_str()
                    ));
                    return;
                }
                let changed = self
                    .selected_member()
                    .is_some_and(|member| member.backend != choice.backend);
                if changed && let Some(member) = self.selected_member_mut() {
                    member.backend = choice.backend;
                    member.session_id = None;
                    member.model = None;
                    member.effort = None;
                }
                self.backend_picker = None;
                if changed {
                    self.dirty = true;
                    self.notice = Some("Agent CLI selected · press s to apply".to_string());
                }
            }
            KeyCode::Esc => self.backend_picker = None,
            _ => {}
        }
    }

    fn handle_model_picker_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Option<UiCommand> {
        match code {
            KeyCode::Up => {
                if let Some(picker) = &mut self.model_picker {
                    picker.up();
                }
            }
            KeyCode::Down => {
                if let Some(picker) = &mut self.model_picker {
                    picker.down();
                }
            }
            KeyCode::Left => self.model_picker.as_mut().unwrap().previous_effort(),
            KeyCode::Right => self.model_picker.as_mut().unwrap().next_effort(),
            KeyCode::Enter => {
                if self
                    .model_picker
                    .as_ref()
                    .is_none_or(|picker| picker.visible_len() == 0)
                {
                    return None;
                }
                let value = self.model_picker.as_ref().and_then(ModelPicker::value);
                let effort = self.model_picker.as_ref().and_then(ModelPicker::effort);
                let member_id = self.selected_member().map(|member| member.id.clone());
                if let Some(member) = self.selected_member_mut() {
                    member.model = value.clone();
                    member.effort = effort;
                }
                self.model_picker = None;
                self.dirty = true;
                if self.model_picker_apply_immediately {
                    self.model_picker_apply_immediately = false;
                    self.notice = Some("applying model and effort…".to_string());
                    return member_id.map(|member| UiCommand::SetMemberModelAndEffort {
                        member,
                        model: value,
                        effort,
                    });
                }
                self.notice = Some("model and effort selected · press s to apply".to_string());
            }
            KeyCode::Backspace => self.model_picker.as_mut().unwrap().pop_query(),
            KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.model_picker.as_mut().unwrap().clear_query();
            }
            KeyCode::Char(ch) if !modifiers.contains(KeyModifiers::CONTROL) => {
                self.model_picker.as_mut().unwrap().push_query(ch);
            }
            KeyCode::Esc => {
                self.model_picker = None;
                self.model_picker_apply_immediately = false;
            }
            _ => {}
        }
        None
    }

    fn handle_session_picker_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        const PAGE: usize = 8;
        match code {
            KeyCode::Up => self.session_picker.as_mut().unwrap().up(),
            KeyCode::Down => self.session_picker.as_mut().unwrap().down(),
            KeyCode::PageUp => self.session_picker.as_mut().unwrap().page_up(PAGE),
            KeyCode::PageDown => self.session_picker.as_mut().unwrap().page_down(PAGE),
            KeyCode::Backspace => self.session_picker.as_mut().unwrap().pop_query(),
            KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.session_picker.as_mut().unwrap().clear_query();
            }
            KeyCode::Char(ch) if !modifiers.contains(KeyModifiers::CONTROL) => {
                self.session_picker.as_mut().unwrap().push_query(ch);
            }
            KeyCode::Enter => {
                let selected = self
                    .session_picker
                    .as_ref()
                    .and_then(SessionPicker::selected_entry)
                    .map(|entry| entry.id.clone());
                self.session_picker = None;
                if let Some(session_id) = selected {
                    let member_id = self.selected_member().map(|member| member.id.clone());
                    if let Some(member_id) = member_id {
                        self.set_session_id(&member_id, session_id);
                    }
                } else {
                    self.notice = Some("no matching session selected".to_string());
                }
            }
            KeyCode::Esc => self.session_picker = None,
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

    fn selected_member_mut(&mut self) -> Option<&mut TeamMember> {
        self.members.get_mut(self.selected)
    }

    fn add_member(&mut self) {
        let backend = self
            .members
            .get(self.selected)
            .map(|member| member.backend)
            .unwrap_or(BackendKind::Codex);
        let mut member = default_member(backend);
        member.id = MemberId::new(unique_member_id(member.id.as_str(), &self.members, None));
        member.display_name = unique_display_name(&member.display_name, &self.members);
        self.members.push(member);
        self.selected = self.members.len() - 1;
        self.dirty = true;
        self.notice = Some("member added; press s to apply".to_string());
    }

    fn delete_member(&mut self) {
        if self.members.len() <= 1 {
            self.notice = Some("team needs at least one member".to_string());
            return;
        }
        self.members.remove(self.selected);
        if self.selected >= self.members.len() {
            self.selected = self.members.len() - 1;
        }
        self.ensure_default_target();
        self.dirty = true;
        self.notice = Some("member removed; press s to apply".to_string());
    }

    fn set_default_to_selected(&mut self) {
        let Some(member) = self.selected_member() else {
            return;
        };
        let id = member.id.clone();
        self.default_target = Some(DefaultTarget::Member(id.clone()));
        self.dirty = true;
        self.notice = Some(format!("default target set to {id}; press s to apply"));
    }

    fn activate_field(&mut self) {
        let field = self.selected_field();
        if field == Field::Backend {
            if self.backend_detection.is_some() {
                self.notice = Some("still checking installed Agent CLIs…".to_string());
                return;
            }
            let Some(member) = self.selected_member() else {
                return;
            };
            self.backend_picker = Some(BackendPicker::new(member.backend, self.detected));
            self.notice = Some("↑/↓ choose an installed Agent CLI · Enter select".to_string());
        } else if field == Field::Model {
            self.cycle_model(true);
        } else if field == Field::SessionId {
            let Some(member) = self.selected_member() else {
                return;
            };
            let backend = member.backend;
            let cwd = member.resolved_cwd(&self.workspace);
            let picker = SessionPicker::discover(backend, &cwd);
            self.notice = picker.error().map(str::to_string).or_else(|| {
                Some(format!(
                    "{} session(s) found · type to filter",
                    picker.visible_len()
                ))
            });
            self.session_picker = Some(picker);
        } else if field.is_text() {
            self.edit_selected_field();
        } else {
            self.cycle_field(field);
        }
    }

    pub(crate) fn set_session_id(&mut self, member_id: &MemberId, session_id: String) {
        let Some(member) = self
            .members
            .iter_mut()
            .find(|member| &member.id == member_id)
        else {
            self.notice = Some(format!("session selected for unknown member: {member_id}"));
            return;
        };
        member.session_id = Some(session_id.clone());
        member.session_policy = crate::domain::team::SessionPolicy::Resume;
        self.dirty = true;
        self.notice = Some(format!("session {session_id} selected · press s to apply"));
    }

    fn edit_selected_field(&mut self) {
        let field = self.selected_field();
        if field == Field::Model {
            self.model_picker_pending = false;
        }
        let Some(member) = self.selected_member() else {
            return;
        };
        let value = if field == Field::Model {
            member.model.clone().unwrap_or_default()
        } else {
            field_value(member, field)
        };
        self.editing = Some(EditState::new(field, value));
    }

    fn cycle_model(&mut self, retry_failed: bool) {
        let Some(member) = self.selected_member() else {
            return;
        };
        let backend = member.backend;
        // A configured member is enough to begin its model lookup. Do not make
        // `/model` wait behind an unrelated CLI probe (notably `agy --version`,
        // which can take several seconds). Once detection has completed, keep
        // its useful missing-CLI diagnostic.
        if self.backend_detection.is_none() && !self.detected.contains(backend) {
            self.notice = Some(format!("{} is not installed on PATH", backend.as_str()));
            return;
        }
        let current = member.model.clone();
        let current_effort = member.effort;
        let cwd = member.resolved_cwd(&self.workspace);
        if retry_failed {
            self.model_catalog.retry(backend, &cwd);
        }
        match self.model_catalog.models(backend, &cwd) {
            ModelChoices::Loading => {
                self.model_picker_pending = true;
                self.notice = Some(format!(
                    "loading {} model catalog… keep editing while it loads",
                    backend.as_str()
                ));
            }
            ModelChoices::Ready(models) => {
                self.model_picker_pending = false;
                self.model_picker = Some(ModelPicker::new(
                    backend,
                    current.as_deref(),
                    current_effort,
                    models,
                ));
                self.notice =
                    Some("↑/↓ choose model · ←/→ choose effort · Enter select".to_string());
            }
            ModelChoices::Failed(err) => {
                self.model_picker_pending = false;
                self.notice = Some(format!("{err} · press Enter to retry"));
            }
        }
    }

    fn cycle_field(&mut self, field: Field) {
        match field {
            Field::Effort => {
                let choices = self
                    .selected_member()
                    .map(|member| self.model_catalog.efforts(member, &self.workspace))
                    .unwrap_or_default();
                if let Some(member) = self.selected_member_mut() {
                    member.effort = cycle_effort(member.effort, &choices);
                }
                if choices.is_empty() {
                    self.notice = self.selected_member().map(|member| {
                        format!(
                            "{} does not support reasoning effort",
                            member.backend.as_str()
                        )
                    });
                }
            }
            Field::Sandbox => {
                if let Some(member) = self.selected_member_mut() {
                    member.sandbox = cycle_sandbox(member.sandbox);
                }
            }
            Field::Permission => {
                if let Some(member) = self.selected_member_mut() {
                    member.permission_mode = cycle_permission(member.permission_mode);
                }
            }
            Field::Session => {
                if let Some(member) = self.selected_member_mut() {
                    member.session_policy = match member.session_policy {
                        crate::domain::team::SessionPolicy::Resume => {
                            crate::domain::team::SessionPolicy::Fresh
                        }
                        crate::domain::team::SessionPolicy::Fresh => {
                            crate::domain::team::SessionPolicy::Resume
                        }
                    };
                    if member.session_policy == crate::domain::team::SessionPolicy::Fresh {
                        member.session_id = None;
                    }
                }
            }
            _ => {}
        }
        self.dirty = true;
        self.notice = Some("field changed; press s to apply".to_string());
    }

    fn commit_edit(&mut self, edit: EditState) {
        let value = edit.buffer.trim();
        match edit.field {
            Field::Name => {
                if !value.is_empty() {
                    let old_id = self.selected_member().map(|member| member.id.clone());
                    let fallback = self
                        .selected_member()
                        .map(|member| member.backend.as_str())
                        .unwrap_or("member");
                    let display_name =
                        unique_display_name_except(value, &self.members, Some(self.selected));
                    let id = unique_member_id(&display_name, &self.members, Some(self.selected));
                    if let Some(member) = self.selected_member_mut() {
                        member.display_name = display_name;
                        member.id = MemberId::new(normalize_member_id(&id, fallback));
                    }
                    if let (Some(old_id), Some(member)) = (old_id, self.selected_member())
                        && matches!(
                            self.default_target.as_ref(),
                            Some(DefaultTarget::Member(id)) if id == &old_id
                        )
                    {
                        self.default_target = Some(DefaultTarget::Member(member.id.clone()));
                    }
                }
            }
            Field::Role => {
                if !value.is_empty()
                    && let Some(member) = self.selected_member_mut()
                {
                    member.role = value.to_string();
                }
            }
            Field::Model => {
                if let Some(member) = self.selected_member_mut() {
                    member.model = if value.is_empty() || value.eq_ignore_ascii_case("default") {
                        None
                    } else {
                        Some(value.to_string())
                    };
                }
            }
            Field::SessionId => {
                let session_id = if value.is_empty() || value.eq_ignore_ascii_case("default") {
                    None
                } else {
                    Some(value.to_string())
                };
                if let Some(member) = self.selected_member_mut() {
                    member.session_id = session_id;
                    if member.session_id.is_some() {
                        member.session_policy = crate::domain::team::SessionPolicy::Resume;
                    }
                }
            }
            Field::Cwd => {
                let cwd = cwd_value(value, &self.workspace);
                if let Some(member) = self.selected_member_mut() {
                    member.cwd = cwd;
                }
            }
            _ => {}
        }
        self.dirty = true;
        self.notice = Some("field changed; press s to apply".to_string());
    }

    fn apply_command(&mut self) -> Option<UiCommand> {
        let default_target = self.normalized_default_target();
        let mut config = TeamConfig::new(self.team.clone(), self.workspace.clone());
        config.default_target = default_target.clone();
        for member in self.members.clone() {
            config = config.with_member(member);
        }
        match config.validate() {
            Ok(()) => {
                self.notice = Some("applying team changes".to_string());
                Some(UiCommand::ReplaceTeam {
                    members: config.members,
                    default_target,
                })
            }
            Err(err) => {
                self.notice = Some(format!("team update rejected: {err}"));
                None
            }
        }
    }

    fn normalized_default_target(&self) -> Option<DefaultTarget> {
        match &self.default_target {
            Some(DefaultTarget::All) => Some(DefaultTarget::All),
            Some(DefaultTarget::Member(id)) if self.members.iter().any(|m| &m.id == id) => {
                Some(DefaultTarget::Member(id.clone()))
            }
            _ => self
                .members
                .first()
                .map(|member| DefaultTarget::Member(member.id.clone())),
        }
    }

    fn ensure_default_target(&mut self) {
        self.default_target = self.normalized_default_target();
    }
}

fn cwd_value(value: &str, workspace: &Path) -> Option<PathBuf> {
    if value.is_empty() || value == "workspace" || value == workspace.display().to_string() {
        None
    } else {
        Some(PathBuf::from(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::team::Effort;

    fn editor() -> TeamEditor {
        TeamEditor::new(
            "t",
            "/tmp/ws",
            Some(DefaultTarget::Member(MemberId::new("builder"))),
            vec![TeamMember::new(
                "builder",
                "Builder",
                BackendKind::Codex,
                "impl",
            )],
        )
    }

    #[test]
    fn default_target_tracks_selected_member_and_all() {
        let mut editor = editor();
        editor.add_member();
        editor.set_default_to_selected();
        assert_eq!(
            editor.normalized_default_target(),
            Some(DefaultTarget::Member(editor.members[1].id.clone()))
        );

        let outcome = editor.handle_key(KeyCode::Char('*'), KeyModifiers::NONE);
        assert_eq!(outcome, TeamEditorOutcome::Consumed(None));
        assert_eq!(editor.normalized_default_target(), Some(DefaultTarget::All));
    }

    #[test]
    fn default_target_updates_when_name_changes_handle() {
        let mut editor = editor();
        editor.commit_edit(EditState::new(Field::Name, "Lead Engineer".to_string()));

        assert_eq!(
            editor.normalized_default_target(),
            Some(DefaultTarget::Member(MemberId::new("lead-engineer")))
        );
    }

    #[test]
    fn rename_avoids_reserved_all_target() {
        let mut editor = editor();

        editor.commit_edit(EditState::new(Field::Name, "All".to_string()));

        assert_eq!(editor.members[0].display_name, "All 2");
        assert_eq!(editor.members[0].id, MemberId::new("all-2"));
        let Some(UiCommand::ReplaceTeam { members, .. }) = editor.apply_command() else {
            panic!("expected replace command");
        };
        assert_eq!(members[0].display_name, "All 2");
    }

    #[test]
    fn member_model_catalog_stays_idle_until_model_field_is_opened() {
        let editor = TeamEditor::new(
            "t",
            "/tmp/ws",
            None,
            vec![TeamMember::new(
                "builder",
                "Builder",
                BackendKind::Claude,
                "impl",
            )],
        );

        assert!(
            !editor
                .model_catalog
                .contains(BackendKind::Claude, Path::new("/tmp/ws"))
        );
        assert_eq!(
            editor.field_value(&editor.members[0], Field::Model),
            "CLI default"
        );
    }

    #[test]
    fn agent_detection_is_polled_without_blocking_the_editor() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut editor = editor();
        editor.backend_detection = Some(rx);

        assert_eq!(
            editor.agent_availability_label(),
            "checking installed Agent CLIs…"
        );
        tx.send(DetectedBackends {
            codex: true,
            claude: false,
            grok: true,
            agy: false,
        })
        .unwrap();

        editor.poll_agent_catalog();

        assert_eq!(
            editor.agent_availability_label(),
            "codex ✓ · claude ✕ · grok ✓ · agy ✕"
        );
        assert_eq!(
            editor.available,
            vec![BackendKind::Codex, BackendKind::Grok]
        );
        assert!(
            editor
                .notice()
                .is_some_and(|notice| notice.contains("ready"))
        );
    }

    #[test]
    fn completed_model_load_opens_the_requested_picker() {
        let mut editor = editor();
        editor.detected.codex = true;
        editor.field_mode = true;
        editor.field = Field::ALL
            .iter()
            .position(|field| *field == Field::Model)
            .unwrap();
        editor.model_picker_pending = true;
        editor.model_catalog.seed(
            BackendKind::Codex,
            Path::new("/tmp/ws"),
            vec!["gpt-test".to_string()],
        );

        editor.poll_agent_catalog();

        assert!(editor.model_picker().is_some());
        assert!(!editor.model_picker_pending);
    }

    #[test]
    fn requested_model_picker_uses_a_ready_catalog_without_waiting_for_detection() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut editor = editor();
        editor.field_mode = true;
        editor.field = Field::ALL
            .iter()
            .position(|field| *field == Field::Model)
            .unwrap();
        editor.backend_detection = Some(rx);
        editor.model_catalog.seed(
            BackendKind::Codex,
            Path::new("/tmp/ws"),
            vec!["gpt-test".to_string()],
        );

        editor.activate_field();
        assert!(editor.model_picker().is_some());
        assert!(!editor.model_picker_pending);

        tx.send(DetectedBackends {
            codex: true,
            claude: false,
            grok: false,
            agy: false,
        })
        .unwrap();
        editor.poll_agent_catalog();
    }

    #[test]
    fn add_and_delete_members_in_draft() {
        let mut editor = editor();
        editor.add_member();
        assert_eq!(editor.members.len(), 2);
        assert_ne!(editor.members[0].id, editor.members[1].id);

        editor.delete_member();
        assert_eq!(editor.members.len(), 1);
    }

    #[test]
    fn enter_opens_fields_and_up_down_select_them() {
        let mut editor = editor();
        editor.add_member();
        editor.selected = 0;
        assert!(!editor.field_mode());
        assert_eq!(editor.selected_field(), Field::Name);

        let down_member = editor.handle_key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(down_member, TeamEditorOutcome::Consumed(None));
        assert_eq!(editor.selected(), 1);
        assert_eq!(editor.selected_field(), Field::Name);

        let right = editor.handle_key(KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(right, TeamEditorOutcome::Consumed(None));
        assert_eq!(editor.selected_field(), Field::Name);

        let enter = editor.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(enter, TeamEditorOutcome::Consumed(None));
        assert!(editor.field_mode());
        assert!(editor.editing().is_none());

        editor.handle_key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(editor.selected_field(), Field::Backend);
        assert_eq!(editor.selected(), 1);

        editor.handle_key(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(editor.selected_field(), Field::Name);

        let back_to_members = editor.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(back_to_members, TeamEditorOutcome::Consumed(None));
        assert!(!editor.field_mode());
        assert_eq!(
            editor.handle_key(KeyCode::Esc, KeyModifiers::NONE),
            TeamEditorOutcome::Close
        );
    }

    #[test]
    fn backend_field_opens_agent_list_and_rejects_missing_cli() {
        let mut editor = editor();
        editor.detected = DetectedBackends {
            codex: true,
            claude: false,
            grok: true,
            agy: false,
        };
        editor.field_mode = true;
        editor.field = Field::ALL
            .iter()
            .position(|field| *field == Field::Backend)
            .unwrap();

        editor.activate_field();
        let picker = editor.backend_picker().expect("backend picker");
        assert_eq!(picker.choices().len(), 4);
        assert!(picker.choices()[0].installed);
        assert!(!picker.choices()[1].installed);

        editor.handle_backend_picker_key(KeyCode::Down);
        editor.handle_backend_picker_key(KeyCode::Enter);
        assert_eq!(editor.members[0].backend, BackendKind::Codex);
        assert!(editor.backend_picker().is_some());
        assert!(
            editor
                .notice()
                .is_some_and(|notice| notice.contains("not installed"))
        );
    }

    #[test]
    fn backend_list_selection_replaces_agent_and_resets_capabilities() {
        let mut editor = editor();
        editor.detected = DetectedBackends {
            codex: true,
            claude: true,
            grok: false,
            agy: false,
        };
        editor.members[0].model = Some("gpt-old".to_string());
        editor.members[0].effort = Some(crate::domain::team::Effort::High);
        editor.members[0].session_id = Some("old-session".to_string());
        editor.field = Field::ALL
            .iter()
            .position(|field| *field == Field::Backend)
            .unwrap();

        editor.activate_field();
        editor.handle_backend_picker_key(KeyCode::Down);
        editor.handle_backend_picker_key(KeyCode::Enter);

        assert_eq!(editor.members[0].backend, BackendKind::Claude);
        assert_eq!(editor.members[0].model, None);
        assert_eq!(editor.members[0].effort, None);
        assert_eq!(editor.members[0].session_id, None);
        assert!(editor.backend_picker().is_none());
        assert!(editor.dirty());
    }

    #[test]
    fn apply_returns_replace_team_command() {
        let mut editor = editor();
        editor.add_member();
        let Some(UiCommand::ReplaceTeam {
            members,
            default_target,
        }) = editor.apply_command()
        else {
            panic!("expected replace command");
        };
        assert_eq!(members.len(), 2);
        assert_eq!(
            default_target,
            Some(DefaultTarget::Member(MemberId::new("builder")))
        );
    }

    #[test]
    fn session_id_field_binds_and_clears_native_history() {
        let mut editor = editor();
        editor.commit_edit(EditState::new(Field::SessionId, "thread-123".to_string()));
        assert_eq!(editor.members[0].session_id.as_deref(), Some("thread-123"));
        assert_eq!(
            editor.members[0].session_policy,
            crate::domain::team::SessionPolicy::Resume
        );

        editor.commit_edit(EditState::new(Field::SessionId, "default".to_string()));
        assert_eq!(editor.members[0].session_id, None);
    }

    #[test]
    fn escape_cancels_focused_field_edit() {
        let mut editor = editor();
        editor.field_mode = true;
        editor.edit_selected_field();
        editor.handle_key(KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(editor.editing().unwrap().buffer, "Builderx");

        editor.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(editor.editing().is_none());
        assert_eq!(editor.members[0].display_name, "Builder");
        assert!(!editor.dirty());
    }

    #[test]
    fn internal_session_picker_selection_updates_draft() {
        let mut editor = editor();
        editor.field_mode = true;
        editor.field = Field::ALL
            .iter()
            .position(|field| *field == Field::SessionId)
            .unwrap();
        editor.session_picker = Some(crate::tui::session_picker::SessionPicker::from_entries(
            BackendKind::Codex,
            vec![crate::tui::session_picker::SessionEntry::fixture(
                "thread-picked",
                "Fix the TUI",
                "/tmp/ws",
            )],
        ));

        assert_eq!(
            editor.handle_key(KeyCode::Enter, KeyModifiers::NONE),
            TeamEditorOutcome::Consumed(None)
        );
        assert_eq!(
            editor.members[0].session_id.as_deref(),
            Some("thread-picked")
        );
        assert!(editor.dirty());
    }

    #[test]
    fn grok_model_field_uses_visible_picker() {
        let mut editor = TeamEditor::new(
            "t",
            "/tmp/ws",
            None,
            vec![TeamMember::new(
                "grok",
                "Grok",
                BackendKind::Grok,
                "implementation",
            )],
        );
        editor.detected.grok = true;
        editor.field = Field::ALL
            .iter()
            .position(|field| *field == Field::Model)
            .unwrap();
        editor.model_catalog.seed(
            BackendKind::Grok,
            Path::new("/tmp/ws"),
            vec!["grok-build".to_string()],
        );

        editor.activate_field();
        assert!(editor.model_picker().is_some());
        editor.handle_model_picker_key(KeyCode::Down, KeyModifiers::NONE);
        editor.handle_model_picker_key(KeyCode::Right, KeyModifiers::NONE);
        editor.handle_model_picker_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(editor.members[0].model.as_deref(), Some("grok-build"));
        assert_eq!(editor.members[0].effort, Some(Effort::Low));
    }

    #[test]
    fn codex_model_field_uses_discovered_catalog() {
        let mut editor = editor();
        editor.detected.codex = true;
        editor.field = Field::ALL
            .iter()
            .position(|field| *field == Field::Model)
            .unwrap();

        editor.model_catalog.seed(
            BackendKind::Codex,
            Path::new("/tmp/ws"),
            vec!["gpt-5.6-sol".to_string()],
        );

        editor.activate_field();
        editor.handle_model_picker_key(KeyCode::Down, KeyModifiers::NONE);
        editor.handle_model_picker_key(KeyCode::Enter, KeyModifiers::NONE);

        assert_eq!(editor.members[0].model.as_deref(), Some("gpt-5.6-sol"));
    }

    #[test]
    fn slash_model_picker_applies_one_atomic_member_update() {
        let mut editor = editor();
        editor.detected.codex = true;
        editor.model_catalog.seed(
            BackendKind::Codex,
            Path::new("/tmp/ws"),
            vec!["gpt-5.6-sol".to_string()],
        );

        editor
            .open_model_picker_for(&MemberId::new("builder"))
            .unwrap();
        assert!(editor.model_picker().is_some());
        assert!(editor.model_picker_applies_immediately());
        // The first row intentionally restores the CLI default. Choose the
        // explicit discovered model for this assertion.
        editor.handle_model_picker_key(KeyCode::Down, KeyModifiers::NONE);

        let TeamEditorOutcome::ApplyAndClose(UiCommand::SetMemberModelAndEffort {
            member,
            model,
            effort,
        }) = editor.handle_key(KeyCode::Enter, KeyModifiers::NONE)
        else {
            panic!("expected an immediate model configuration command");
        };
        assert_eq!(member, MemberId::new("builder"));
        assert_eq!(model.as_deref(), Some("gpt-5.6-sol"));
        assert_eq!(effort, None);
        assert!(editor.model_picker().is_none());
    }

    #[test]
    fn slash_model_picker_can_restore_cli_default_and_clear_effort() {
        let mut editor = editor();
        editor.members[0].model = Some("gpt-5.6-sol".to_string());
        editor.members[0].effort = Some(crate::domain::team::Effort::High);
        editor.detected.codex = true;
        editor.model_catalog.seed(
            BackendKind::Codex,
            Path::new("/tmp/ws"),
            vec!["gpt-5.6-sol".to_string()],
        );

        editor
            .open_model_picker_for(&MemberId::new("builder"))
            .unwrap();
        let picker = editor.model_picker.as_mut().expect("model picker");
        // Opening on an explicitly configured model selects that model. Move
        // up to the always-present CLI-default row.
        picker.up();

        let TeamEditorOutcome::ApplyAndClose(UiCommand::SetMemberModelAndEffort {
            model,
            effort,
            ..
        }) = editor.handle_key(KeyCode::Enter, KeyModifiers::NONE)
        else {
            panic!("expected an immediate default reset");
        };
        assert_eq!(model, None);
        assert_eq!(effort, None);
    }
}
