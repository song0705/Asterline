//! Two-layer `/mode` overlay: pick a mode, then edit its knobs.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::domain::event::UiCommand;
use crate::domain::mode::{
    BrainstormModeConfig, ModesConfig, PlanModeConfig, ReviewModeConfig, TeamModeConfig,
    TerminalMode, apply_mode_overrides, format_mode_binding, merge_modes, mode_binding_is_error,
    mode_field_source, prune_empty_mode_overrides, validate_mode_overrides, validate_terminal_mode,
};
use crate::domain::team::{DefaultTarget, MemberId, TeamConfig, TeamMember};
use crate::tui::team_builder::EditState;
use crate::tui::theme;
use crate::tui::theme::pad_width;

const MODE_NAME_WIDTH: usize = 11;
const MAX_ITERATIONS: u32 = 20;
const MAX_ROUNDS: u32 = 12;
const MAX_IDEAS: u32 = 12;
const BRAINSTORM_NOTE: &str = "Wave 1 is seed, last wave is stretch, middle waves build; synthesizer is the first roster member; voting is private";

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum ModeEditorOutcome {
    Ignored,
    Consumed(Vec<UiCommand>),
    Close,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ModeField {
    ReviewBuilder,
    ReviewReviewer,
    ReviewMaxIterations,
    ReviewReviewerHint,
    PlanLeader,
    PlanBuilder,
    PlanReviewer,
    PlanAutoExecute,
    PlanMaxIterations,
    BrainstormParticipants,
    BrainstormRounds,
    BrainstormIdeas,
    TeamCoordinator,
    TeamMaxIterations,
}

impl ModeField {
    fn for_mode(mode: TerminalMode) -> &'static [Self] {
        match mode {
            TerminalMode::Normal => &[],
            TerminalMode::Review => &[
                Self::ReviewBuilder,
                Self::ReviewReviewer,
                Self::ReviewMaxIterations,
                Self::ReviewReviewerHint,
            ],
            TerminalMode::Plan => &[
                Self::PlanLeader,
                Self::PlanBuilder,
                Self::PlanReviewer,
                Self::PlanAutoExecute,
                Self::PlanMaxIterations,
            ],
            TerminalMode::Brainstorm => &[
                Self::BrainstormParticipants,
                Self::BrainstormRounds,
                Self::BrainstormIdeas,
            ],
            TerminalMode::Team => &[Self::TeamCoordinator, Self::TeamMaxIterations],
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::ReviewBuilder | Self::PlanBuilder => "builder",
            Self::ReviewReviewer | Self::PlanReviewer => "reviewer",
            Self::PlanLeader => "leader",
            Self::TeamCoordinator => "coordinator",
            Self::BrainstormParticipants => "participants",
            Self::ReviewMaxIterations | Self::PlanMaxIterations | Self::TeamMaxIterations => {
                "max_iterations"
            }
            Self::PlanAutoExecute => "auto_execute",
            Self::ReviewReviewerHint => "reviewer_hint",
            Self::BrainstormRounds => "generation_rounds",
            Self::BrainstormIdeas => "ideas_per_round",
        }
    }

    fn label_width(mode: TerminalMode) -> usize {
        Self::for_mode(mode)
            .iter()
            .map(|field| field.label().len())
            .max()
            .unwrap_or(0)
    }

    fn is_member(self) -> bool {
        matches!(
            self,
            Self::ReviewBuilder
                | Self::ReviewReviewer
                | Self::PlanLeader
                | Self::PlanBuilder
                | Self::PlanReviewer
                | Self::TeamCoordinator
        )
    }

    fn is_participants(self) -> bool {
        matches!(self, Self::BrainstormParticipants)
    }

    fn is_number(self) -> bool {
        matches!(
            self,
            Self::ReviewMaxIterations
                | Self::PlanMaxIterations
                | Self::TeamMaxIterations
                | Self::BrainstormRounds
                | Self::BrainstormIdeas
        )
    }

    fn is_toggle(self) -> bool {
        matches!(self, Self::PlanAutoExecute)
    }

    fn is_command(self) -> bool {
        matches!(self, Self::ReviewReviewerHint)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MemberPicker {
    options: Vec<(MemberId, String)>,
    selected: usize,
    multi: bool,
    checked: Vec<bool>,
}

impl MemberPicker {
    pub(crate) fn selected(&self) -> usize {
        self.selected
    }

    pub(crate) fn multi(&self) -> bool {
        self.multi
    }

    pub(crate) fn options(&self) -> &[(MemberId, String)] {
        &self.options
    }

    pub(crate) fn checked(&self, index: usize) -> bool {
        self.checked.get(index).copied().unwrap_or(false)
    }

    fn up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    fn down(&mut self) {
        if self.selected + 1 < self.options.len() {
            self.selected += 1;
        }
    }

    fn toggle(&mut self) {
        if let Some(flag) = self.checked.get_mut(self.selected) {
            *flag = !*flag;
        }
    }
}

pub(crate) struct ModeEditor {
    team: String,
    workspace: PathBuf,
    default_target: Option<DefaultTarget>,
    members: Vec<TeamMember>,
    defaults: ModesConfig,
    applied: ModesConfig,
    pending: ModesConfig,
    active_mode: TerminalMode,
    selected: usize,
    field: usize,
    field_mode: bool,
    editing: Option<EditState>,
    member_picker: Option<MemberPicker>,
    notice: Option<String>,
}

impl ModeEditor {
    #[allow(clippy::too_many_arguments)] // Editor state mirrors the mode drawer's inputs.
    pub(crate) fn new(
        team: impl Into<String>,
        workspace: impl Into<PathBuf>,
        default_target: Option<DefaultTarget>,
        members: Vec<TeamMember>,
        defaults: ModesConfig,
        overrides: ModesConfig,
        active_mode: TerminalMode,
    ) -> Self {
        let selected = TerminalMode::ALL
            .iter()
            .position(|mode| *mode == active_mode)
            .unwrap_or(0);
        Self {
            team: team.into(),
            workspace: workspace.into(),
            default_target,
            members,
            defaults,
            applied: overrides.clone(),
            pending: overrides,
            active_mode,
            selected,
            field: 0,
            field_mode: false,
            editing: None,
            member_picker: None,
            notice: None,
        }
    }

    pub(crate) fn sync_from_runtime(&mut self, defaults: ModesConfig, overrides: ModesConfig) {
        self.defaults = defaults;
        self.applied = overrides.clone();
        if !self.dirty() {
            self.pending = overrides;
        }
    }

    pub(crate) fn set_active_mode(&mut self, mode: TerminalMode) {
        self.active_mode = mode;
    }

    pub(crate) fn dirty(&self) -> bool {
        self.pending != self.applied
    }

    #[cfg(test)]
    pub(crate) fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    pub(crate) fn field_mode(&self) -> bool {
        self.field_mode
    }

    pub(crate) fn selected_mode(&self) -> TerminalMode {
        TerminalMode::ALL[self.selected.min(TerminalMode::ALL.len() - 1)]
    }

    #[cfg(test)]
    pub(crate) fn selected_index(&self) -> usize {
        self.selected
    }

    #[cfg(test)]
    pub(crate) fn field_index(&self) -> usize {
        self.field
    }

    pub(crate) fn editing(&self) -> Option<&EditState> {
        self.editing.as_ref()
    }

    pub(crate) fn member_picker(&self) -> Option<&MemberPicker> {
        self.member_picker.as_ref()
    }

    pub(crate) fn insert_edit_text(&mut self, text: &str) -> bool {
        let Some(edit) = self.editing.as_mut() else {
            return false;
        };
        edit.insert_text(text);
        true
    }

    pub(crate) fn preview_config(&self) -> TeamConfig {
        let mut config = TeamConfig::new(&self.team, &self.workspace);
        config.members = self.members.clone();
        config.default_target = self.default_target.clone();
        config.modes = merge_modes(&self.defaults, &self.pending);
        config
    }

    pub(crate) fn handle_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> ModeEditorOutcome {
        if self.member_picker.is_some() {
            self.handle_picker_key(code);
            return ModeEditorOutcome::Consumed(Vec::new());
        }
        if self.editing.is_some() {
            self.handle_edit_key(code, modifiers);
            return ModeEditorOutcome::Consumed(Vec::new());
        }

        let ctrl = modifiers.contains(KeyModifiers::CONTROL);
        match code {
            KeyCode::Char('c') if ctrl => ModeEditorOutcome::Close,
            KeyCode::Esc if self.field_mode => {
                self.field_mode = false;
                ModeEditorOutcome::Consumed(Vec::new())
            }
            KeyCode::Esc | KeyCode::Char('q') => ModeEditorOutcome::Close,
            KeyCode::Up | KeyCode::Char('k') => {
                if self.field_mode {
                    self.prev_field();
                } else {
                    self.selected = self.selected.saturating_sub(1);
                    self.normalize_field();
                }
                ModeEditorOutcome::Consumed(Vec::new())
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.field_mode {
                    self.next_field();
                } else if self.selected + 1 < TerminalMode::ALL.len() {
                    self.selected += 1;
                    self.normalize_field();
                }
                ModeEditorOutcome::Consumed(Vec::new())
            }
            KeyCode::Enter if !self.field_mode => {
                self.enter_fields();
                ModeEditorOutcome::Consumed(Vec::new())
            }
            KeyCode::Left | KeyCode::Right if self.field_mode => {
                if self.selected_field().is_some_and(ModeField::is_number) {
                    self.step_number(code == KeyCode::Right);
                }
                ModeEditorOutcome::Consumed(Vec::new())
            }
            KeyCode::Char('r') if self.field_mode => {
                self.clear_selected_field();
                ModeEditorOutcome::Consumed(Vec::new())
            }
            KeyCode::Char('s') if self.field_mode => {
                ModeEditorOutcome::Consumed(self.apply_pending())
            }
            KeyCode::Char('w') if self.field_mode => {
                ModeEditorOutcome::Consumed(self.write_defaults())
            }
            KeyCode::Enter => {
                self.activate_field();
                ModeEditorOutcome::Consumed(Vec::new())
            }
            KeyCode::Char(' ') if self.field_mode => ModeEditorOutcome::Consumed(Vec::new()),
            KeyCode::Backspace | KeyCode::Char(_) => ModeEditorOutcome::Consumed(Vec::new()),
            _ => ModeEditorOutcome::Ignored,
        }
    }

    fn enter_fields(&mut self) {
        self.normalize_field();
        self.field_mode = true;
        if self.fields().is_empty() {
            self.notice =
                Some("normal has no knobs — plain text uses the last @target".to_string());
        }
    }

    fn apply_pending(&mut self) -> Vec<UiCommand> {
        let mode = self.selected_mode();
        if let Err(err) = self.ensure_pending_valid_for(mode) {
            self.notice = Some(err);
            return Vec::new();
        }
        let mut commands = Vec::new();
        if self.dirty() {
            commands.push(UiCommand::SetModeOverrides {
                overrides: self.pending.clone(),
            });
            self.applied = self.pending.clone();
        }
        commands.push(UiCommand::SetMode { mode });
        self.notice = Some(format!("selected {mode} for this chat"));
        commands
    }

    fn write_defaults(&mut self) -> Vec<UiCommand> {
        let mode = self.selected_mode();
        if matches!(mode, TerminalMode::Normal) {
            self.notice = Some("normal has no team.json defaults".to_string());
            return Vec::new();
        }
        if let Err(err) = self.ensure_pending_valid_for(mode) {
            self.notice = Some(err);
            return Vec::new();
        }
        let mut commands = Vec::new();
        if self.dirty() {
            commands.push(UiCommand::SetModeOverrides {
                overrides: self.pending.clone(),
            });
            self.applied = self.pending.clone();
        }
        commands.push(UiCommand::SaveModeDefaults { mode });
        self.notice = Some(format!("saving {mode} defaults to team.json"));
        commands
    }

    fn ensure_pending_valid_for(&self, mode: TerminalMode) -> Result<(), String> {
        validate_mode_overrides(&self.base_config(), &self.pending)?;
        validate_terminal_mode(&self.preview_config(), mode)
    }

    fn base_config(&self) -> TeamConfig {
        let mut config = TeamConfig::new(&self.team, &self.workspace);
        config.members = self.members.clone();
        config.default_target = self.default_target.clone();
        config.modes = self.defaults.clone();
        config
    }

    fn handle_picker_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Up => self.member_picker.as_mut().unwrap().up(),
            KeyCode::Down => self.member_picker.as_mut().unwrap().down(),
            KeyCode::Char(' ') if self.member_picker.as_ref().is_some_and(|p| p.multi) => {
                self.member_picker.as_mut().unwrap().toggle();
            }
            KeyCode::Enter => self.commit_picker(),
            KeyCode::Esc => self.member_picker = None,
            _ => {}
        }
    }

    fn commit_picker(&mut self) {
        let Some(picker) = self.member_picker.take() else {
            return;
        };
        let Some(field) = self.selected_field() else {
            return;
        };
        if picker.multi {
            let selected = picker
                .options
                .iter()
                .zip(picker.checked.iter())
                .filter(|(_, checked)| **checked)
                .map(|(option, _)| option.0.clone())
                .collect::<Vec<_>>();
            self.set_participants(Some(selected));
        } else if let Some((id, _)) = picker.options.get(picker.selected) {
            self.set_member_field(field, Some(id.clone()));
        }
        self.notice = Some("press s to select and apply to this chat".to_string());
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

    fn commit_edit(&mut self, edit: EditState) {
        let Some(field) = self.selected_field() else {
            return;
        };
        let value = edit.buffer.trim().to_string();
        if field.is_number() {
            match value.parse::<u32>() {
                Ok(number) => {
                    if let Err(err) = self.set_number_field(field, Some(number)) {
                        self.notice = Some(err);
                        return;
                    }
                }
                Err(_) => {
                    self.notice = Some(format!("{} needs a number", field.label()));
                    return;
                }
            }
        } else if field.is_command() {
            self.set_command_field(field, if value.is_empty() { None } else { Some(value) });
        }
        self.notice = Some("press s to select and apply to this chat".to_string());
    }

    fn activate_field(&mut self) {
        let Some(field) = self.selected_field() else {
            return;
        };
        if field.is_member() {
            self.open_member_picker(false);
        } else if field.is_participants() {
            self.open_member_picker(true);
        } else if field.is_toggle() {
            let next = !self.effective_auto_verify(field);
            self.set_toggle_field(field, Some(next));
            self.notice = Some("press s to select and apply to this chat".to_string());
        } else if field.is_number() {
            let current = self
                .effective_number(field)
                .unwrap_or(Self::number_default(field));
            self.editing = Some(EditState::named(field.label(), current.to_string()));
        } else if field.is_command() {
            let current = self.override_or_default_command(field).unwrap_or_default();
            self.editing = Some(EditState::named(field.label(), current));
        }
    }

    fn open_member_picker(&mut self, multi: bool) {
        if self.members.is_empty() {
            self.notice = Some("team has no members".to_string());
            return;
        }
        let options = self
            .members
            .iter()
            .map(|member| (member.id.clone(), member.display_name.clone()))
            .collect::<Vec<_>>();
        let current_ids = if multi {
            self.effective_participants()
        } else if let Some(field) = self.selected_field() {
            self.effective_member(field).into_iter().collect()
        } else {
            Vec::new()
        };
        let selected = options
            .iter()
            .position(|(id, _)| current_ids.first() == Some(id))
            .unwrap_or(0);
        let checked = options
            .iter()
            .map(|(id, _)| current_ids.iter().any(|current| current == id))
            .collect();
        self.member_picker = Some(MemberPicker {
            options,
            selected,
            multi,
            checked,
        });
    }

    fn step_number(&mut self, up: bool) {
        let Some(field) = self.selected_field() else {
            return;
        };
        let current = self
            .effective_number(field)
            .unwrap_or(Self::number_default(field));
        let (min, max) = self.number_bounds(field);
        let next = if up {
            current.saturating_add(1).min(max)
        } else {
            current.saturating_sub(1).max(min)
        };
        if let Err(err) = self.set_number_field(field, Some(next)) {
            self.notice = Some(err);
        }
    }

    fn number_default(field: ModeField) -> u32 {
        match field {
            ModeField::BrainstormIdeas => 4,
            _ => 3,
        }
    }

    fn number_bounds(&self, field: ModeField) -> (u32, u32) {
        match field {
            ModeField::BrainstormRounds => (2, MAX_ROUNDS),
            ModeField::BrainstormIdeas => (3, MAX_IDEAS),
            _ => (1, MAX_ITERATIONS),
        }
    }

    fn clear_selected_field(&mut self) {
        let Some(field) = self.selected_field() else {
            return;
        };
        match field {
            ModeField::ReviewBuilder
            | ModeField::ReviewReviewer
            | ModeField::PlanLeader
            | ModeField::PlanBuilder
            | ModeField::PlanReviewer
            | ModeField::TeamCoordinator => self.set_member_field(field, None),
            ModeField::BrainstormParticipants => self.set_participants(None),
            ModeField::ReviewMaxIterations
            | ModeField::PlanMaxIterations
            | ModeField::TeamMaxIterations
            | ModeField::BrainstormRounds
            | ModeField::BrainstormIdeas => {
                let _ = self.set_number_field(field, None);
            }
            ModeField::PlanAutoExecute => {
                self.set_toggle_field(field, None);
            }
            ModeField::ReviewReviewerHint => self.set_command_field(field, None),
        }
        prune_empty_mode_overrides(&mut self.pending);
        self.notice = Some("fell back to team.json / default".to_string());
    }

    fn fields(&self) -> &'static [ModeField] {
        ModeField::for_mode(self.selected_mode())
    }

    fn selected_field(&self) -> Option<ModeField> {
        let fields = self.fields();
        if fields.is_empty() {
            None
        } else {
            Some(fields[self.field.min(fields.len() - 1)])
        }
    }

    fn next_field(&mut self) {
        let len = self.fields().len();
        if len == 0 {
            return;
        }
        self.field = (self.field + 1) % len;
    }

    fn prev_field(&mut self) {
        let len = self.fields().len();
        if len == 0 {
            return;
        }
        self.field = if self.field == 0 {
            len - 1
        } else {
            self.field - 1
        };
    }

    fn normalize_field(&mut self) {
        let len = self.fields().len();
        self.field = if len == 0 { 0 } else { self.field.min(len - 1) };
    }

    fn review_mut(&mut self) -> &mut ReviewModeConfig {
        self.pending
            .review
            .get_or_insert_with(ReviewModeConfig::default)
    }

    fn plan_mut(&mut self) -> &mut PlanModeConfig {
        self.pending
            .plan
            .get_or_insert_with(PlanModeConfig::default)
    }

    fn brainstorm_mut(&mut self) -> &mut BrainstormModeConfig {
        self.pending
            .brainstorm
            .get_or_insert_with(BrainstormModeConfig::default)
    }

    fn team_mut(&mut self) -> &mut TeamModeConfig {
        self.pending
            .team
            .get_or_insert_with(TeamModeConfig::default)
    }

    fn set_member_field(&mut self, field: ModeField, value: Option<MemberId>) {
        match field {
            ModeField::ReviewBuilder => self.review_mut().builder = value,
            ModeField::ReviewReviewer => self.review_mut().reviewer = value,
            ModeField::PlanLeader => self.plan_mut().leader = value,
            ModeField::PlanBuilder => self.plan_mut().builder = value,
            ModeField::PlanReviewer => self.plan_mut().reviewer = value,
            ModeField::TeamCoordinator => self.team_mut().coordinator = value,
            _ => {}
        }
        prune_empty_mode_overrides(&mut self.pending);
    }

    fn set_participants(&mut self, value: Option<Vec<MemberId>>) {
        self.brainstorm_mut().participants = value;
        prune_empty_mode_overrides(&mut self.pending);
    }

    fn set_number_field(&mut self, field: ModeField, value: Option<u32>) -> Result<(), String> {
        if let Some(number) = value {
            let (min, max) = self.number_bounds(field);
            if number < min || number > max {
                return Err(format!("{} must be between {min} and {max}", field.label()));
            }
        }
        match field {
            ModeField::ReviewMaxIterations => self.review_mut().max_iterations = value,
            ModeField::PlanMaxIterations => self.plan_mut().max_iterations = value,
            ModeField::TeamMaxIterations => self.team_mut().max_iterations = value,
            ModeField::BrainstormRounds => self.brainstorm_mut().generation_rounds = value,
            ModeField::BrainstormIdeas => self.brainstorm_mut().ideas_per_round = value,
            _ => {}
        }
        prune_empty_mode_overrides(&mut self.pending);
        Ok(())
    }

    fn set_toggle_field(&mut self, field: ModeField, value: Option<bool>) {
        if field == ModeField::PlanAutoExecute {
            self.plan_mut().auto_execute = value;
        }
        prune_empty_mode_overrides(&mut self.pending);
    }

    fn set_command_field(&mut self, field: ModeField, value: Option<String>) {
        if field == ModeField::ReviewReviewerHint {
            self.review_mut().reviewer_hint = value;
        }
        prune_empty_mode_overrides(&mut self.pending);
    }

    fn field_overridden(&self, field: ModeField) -> bool {
        match field {
            ModeField::ReviewBuilder => self
                .pending
                .review
                .as_ref()
                .and_then(|cfg| cfg.builder.as_ref())
                .is_some(),
            ModeField::ReviewReviewer => self
                .pending
                .review
                .as_ref()
                .and_then(|cfg| cfg.reviewer.as_ref())
                .is_some(),
            ModeField::ReviewMaxIterations => self
                .pending
                .review
                .as_ref()
                .and_then(|cfg| cfg.max_iterations)
                .is_some(),
            ModeField::ReviewReviewerHint => self
                .pending
                .review
                .as_ref()
                .and_then(|cfg| cfg.reviewer_hint.as_ref())
                .is_some(),
            ModeField::PlanLeader => self
                .pending
                .plan
                .as_ref()
                .and_then(|cfg| cfg.leader.as_ref())
                .is_some(),
            ModeField::PlanBuilder => self
                .pending
                .plan
                .as_ref()
                .and_then(|cfg| cfg.builder.as_ref())
                .is_some(),
            ModeField::PlanReviewer => self
                .pending
                .plan
                .as_ref()
                .and_then(|cfg| cfg.reviewer.as_ref())
                .is_some(),
            ModeField::PlanAutoExecute => self
                .pending
                .plan
                .as_ref()
                .and_then(|cfg| cfg.auto_execute)
                .is_some(),
            ModeField::PlanMaxIterations => self
                .pending
                .plan
                .as_ref()
                .and_then(|cfg| cfg.max_iterations)
                .is_some(),
            ModeField::BrainstormParticipants => self
                .pending
                .brainstorm
                .as_ref()
                .and_then(|cfg| cfg.participants.as_ref())
                .is_some(),
            ModeField::BrainstormRounds => self
                .pending
                .brainstorm
                .as_ref()
                .and_then(|cfg| cfg.generation_rounds)
                .is_some(),
            ModeField::BrainstormIdeas => self
                .pending
                .brainstorm
                .as_ref()
                .and_then(|cfg| cfg.ideas_per_round)
                .is_some(),
            ModeField::TeamCoordinator => self
                .pending
                .team
                .as_ref()
                .and_then(|cfg| cfg.coordinator.as_ref())
                .is_some(),
            ModeField::TeamMaxIterations => self
                .pending
                .team
                .as_ref()
                .and_then(|cfg| cfg.max_iterations)
                .is_some(),
        }
    }

    fn field_in_team_json(&self, field: ModeField) -> bool {
        match field {
            ModeField::ReviewBuilder => self
                .defaults
                .review
                .as_ref()
                .and_then(|cfg| cfg.builder.as_ref())
                .is_some(),
            ModeField::ReviewReviewer => self
                .defaults
                .review
                .as_ref()
                .and_then(|cfg| cfg.reviewer.as_ref())
                .is_some(),
            ModeField::ReviewMaxIterations => self
                .defaults
                .review
                .as_ref()
                .and_then(|cfg| cfg.max_iterations)
                .is_some(),
            ModeField::ReviewReviewerHint => self
                .defaults
                .review
                .as_ref()
                .and_then(|cfg| cfg.reviewer_hint.as_ref())
                .is_some(),
            ModeField::PlanLeader => self
                .defaults
                .plan
                .as_ref()
                .and_then(|cfg| cfg.leader.as_ref())
                .is_some(),
            ModeField::PlanBuilder => self
                .defaults
                .plan
                .as_ref()
                .and_then(|cfg| cfg.builder.as_ref())
                .is_some(),
            ModeField::PlanReviewer => self
                .defaults
                .plan
                .as_ref()
                .and_then(|cfg| cfg.reviewer.as_ref())
                .is_some(),
            ModeField::PlanAutoExecute => self
                .defaults
                .plan
                .as_ref()
                .and_then(|cfg| cfg.auto_execute)
                .is_some(),
            ModeField::PlanMaxIterations => self
                .defaults
                .plan
                .as_ref()
                .and_then(|cfg| cfg.max_iterations)
                .is_some(),
            ModeField::BrainstormParticipants => self
                .defaults
                .brainstorm
                .as_ref()
                .and_then(|cfg| cfg.participants.as_ref())
                .is_some(),
            ModeField::BrainstormRounds => self
                .defaults
                .brainstorm
                .as_ref()
                .and_then(|cfg| cfg.generation_rounds)
                .is_some(),
            ModeField::BrainstormIdeas => self
                .defaults
                .brainstorm
                .as_ref()
                .and_then(|cfg| cfg.ideas_per_round)
                .is_some(),
            ModeField::TeamCoordinator => self
                .defaults
                .team
                .as_ref()
                .and_then(|cfg| cfg.coordinator.as_ref())
                .is_some(),
            ModeField::TeamMaxIterations => self
                .defaults
                .team
                .as_ref()
                .and_then(|cfg| cfg.max_iterations)
                .is_some(),
        }
    }

    fn field_source(&self, field: ModeField) -> crate::domain::mode::ModeValueSource {
        mode_field_source(self.field_overridden(field), self.field_in_team_json(field))
    }

    fn member_name(&self, id: &MemberId) -> String {
        self.members
            .iter()
            .find(|member| &member.id == id)
            .map(|member| member.display_name.clone())
            .unwrap_or_else(|| id.to_string())
    }

    fn effective_member(&self, field: ModeField) -> Option<MemberId> {
        let config = self.preview_config();
        match field {
            ModeField::ReviewBuilder => crate::domain::mode::resolve_mode_roles(
                &config,
                crate::domain::mode::CollabMode::Review,
            )
            .ok()
            .map(|(roles, _)| roles.builder),
            ModeField::ReviewReviewer => crate::domain::mode::resolve_mode_roles(
                &config,
                crate::domain::mode::CollabMode::Review,
            )
            .ok()
            .map(|(roles, _)| roles.reviewer),
            ModeField::PlanReviewer => crate::domain::mode::resolve_plan_reviewer(&config)
                .ok()
                .flatten(),
            ModeField::PlanLeader => crate::domain::mode::resolve_mode_roles(
                &config,
                crate::domain::mode::CollabMode::Plan,
            )
            .ok()
            .map(|(roles, _)| roles.leader),
            ModeField::PlanBuilder => crate::domain::mode::resolve_plan_builder(&config).ok(),
            ModeField::TeamCoordinator => {
                crate::domain::mode::resolve_team_coordinator(&config).ok()
            }
            _ => None,
        }
    }

    fn effective_participants(&self) -> Vec<MemberId> {
        crate::domain::mode::resolve_mode_roles(
            &self.preview_config(),
            crate::domain::mode::CollabMode::Brainstorm,
        )
        .map(|(roles, _)| roles.participants)
        .unwrap_or_else(|_| {
            self.members
                .iter()
                .map(|member| member.id.clone())
                .collect()
        })
    }

    fn effective_number(&self, field: ModeField) -> Option<u32> {
        let config = self.preview_config();
        match field {
            ModeField::ReviewMaxIterations => crate::domain::mode::resolve_mode_roles(
                &config,
                crate::domain::mode::CollabMode::Review,
            )
            .ok()
            .map(|(_, limits)| limits.max_iterations),
            ModeField::PlanMaxIterations => crate::domain::mode::resolve_mode_roles(
                &config,
                crate::domain::mode::CollabMode::Plan,
            )
            .ok()
            .map(|(_, limits)| limits.max_iterations),
            ModeField::TeamMaxIterations => crate::domain::mode::resolve_team_limits(&config)
                .ok()
                .map(|limits| limits.max_iterations),
            ModeField::BrainstormRounds => crate::domain::mode::resolve_mode_roles(
                &config,
                crate::domain::mode::CollabMode::Brainstorm,
            )
            .ok()
            .map(|(_, limits)| limits.rounds),
            ModeField::BrainstormIdeas => crate::domain::mode::resolve_mode_roles(
                &config,
                crate::domain::mode::CollabMode::Brainstorm,
            )
            .ok()
            .map(|(_, limits)| limits.ideas_per_round),
            _ => None,
        }
    }

    fn effective_auto_verify(&self, field: ModeField) -> bool {
        let config = self.preview_config();
        match field {
            ModeField::PlanAutoExecute => crate::domain::mode::resolve_plan_auto_execute(&config),
            _ => true,
        }
    }

    fn override_or_default_command(&self, field: ModeField) -> Option<String> {
        let merged = apply_mode_overrides(&self.base_config(), &self.pending).modes;
        match field {
            ModeField::ReviewReviewerHint => merged
                .review
                .as_ref()
                .and_then(|cfg| cfg.reviewer_hint.clone()),
            _ => None,
        }
    }

    fn field_value_text(&self, field: ModeField) -> (String, Option<String>) {
        if field == ModeField::PlanBuilder && self.effective_member(field).is_none() {
            return (
                String::new(),
                Some("required — choose a builder".to_string()),
            );
        }
        if field == ModeField::PlanReviewer && self.effective_member(field).is_none() {
            return (
                String::new(),
                Some("not set — skip plan review".to_string()),
            );
        }
        if field.is_member() {
            let name = self
                .effective_member(field)
                .map(|id| self.member_name(&id))
                .unwrap_or_else(|| "—".to_string());
            return (name, None);
        }
        if field.is_participants() {
            let names = self
                .effective_participants()
                .iter()
                .map(|id| self.member_name(id))
                .collect::<Vec<_>>()
                .join(", ");
            return (names, None);
        }
        if field.is_number() {
            let number = self
                .effective_number(field)
                .unwrap_or(Self::number_default(field));
            return (number.to_string(), None);
        }
        if field.is_toggle() {
            if field == ModeField::PlanAutoExecute {
                return (
                    if self.effective_auto_verify(field) {
                        "auto send".to_string()
                    } else {
                        "manual confirm".to_string()
                    },
                    None,
                );
            }
            return (
                if self.effective_auto_verify(field) {
                    "on".to_string()
                } else {
                    "off".to_string()
                },
                None,
            );
        }
        if field.is_command() {
            if let Some(command) = self.override_or_default_command(field) {
                return (command, None);
            }
            if field == ModeField::ReviewReviewerHint {
                return (
                    String::new(),
                    Some("optional — appended to the reviewer prompt".to_string()),
                );
            }
            return (
                String::new(),
                Some("optional — set to run after this mode finishes".to_string()),
            );
        }
        (String::new(), None)
    }

    pub(crate) fn lines(&self, run_banner: Option<&str>) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let dirty = if self.dirty() { "modified" } else { "saved" };
        lines.push(Line::from(vec![
            Span::styled(" Mode  ", theme::accent_bold()),
            Span::styled(format!("(this chat: {})", self.active_mode), theme::muted()),
            Span::raw("  "),
            Span::styled(
                format!("({dirty})"),
                if self.dirty() {
                    theme::warning()
                } else {
                    theme::muted()
                },
            ),
        ]));
        if let Some(banner) = run_banner {
            lines.push(Line::styled(format!(" {banner}"), theme::warning()));
        }
        lines.push(Line::styled(
            if self.member_picker.is_some() {
                if self.member_picker.as_ref().is_some_and(|p| p.multi) {
                    " ↑/↓ member · Space toggle · Enter confirm · Esc cancel"
                } else {
                    " ↑/↓ member · Enter choose · Esc cancel"
                }
            } else if self.field_mode {
                " ↑/↓ field · Enter edit · ←/→ step · r revert · s select + apply · w team.json · Esc list"
            } else {
                " ↑/↓ select · Enter fields · Esc close"
            },
            theme::muted(),
        ));
        if let Some(notice) = &self.notice {
            lines.push(Line::styled(format!(" {notice}"), theme::warning()));
        }
        lines.push(Line::raw(""));

        if let Some(picker) = &self.member_picker {
            lines.extend(member_picker_lines(picker));
            return lines;
        }

        if !self.field_mode {
            for (idx, mode) in TerminalMode::ALL.iter().enumerate() {
                let selected = idx == self.selected;
                let current = *mode == self.active_mode;
                let marker = if selected {
                    "▶ "
                } else if current {
                    "● "
                } else {
                    "  "
                };
                let config = self.preview_config();
                let binding = format_mode_binding(&config, *mode);
                let error = mode_binding_is_error(&config, *mode);
                let binding_style = if error {
                    theme::warning()
                } else if selected {
                    theme::bold(theme::emphasis_color())
                } else {
                    theme::muted()
                };
                lines.push(Line::from(vec![
                    Span::styled(
                        marker,
                        if selected {
                            theme::warning_bold()
                        } else if current {
                            theme::success_bold()
                        } else {
                            theme::muted()
                        },
                    ),
                    Span::styled(
                        pad_width(mode.as_str(), MODE_NAME_WIDTH),
                        if error {
                            theme::warning_bold()
                        } else {
                            theme::bold(theme::mode_color(*mode))
                        },
                    ),
                    Span::styled(binding, binding_style),
                ]));
            }
        } else {
            let mode = self.selected_mode();
            lines.push(Line::styled(
                format!(" {mode} fields"),
                theme::bold(theme::mode_color(mode)),
            ));
            if matches!(mode, TerminalMode::Normal) {
                lines.push(Line::styled(
                    " Plain text goes to the last @target. Collaboration runs stay off.",
                    theme::muted(),
                ));
            }
            let label_width = ModeField::label_width(mode);
            for (idx, field) in self.fields().iter().enumerate() {
                let selected = idx == self.field;
                let style = if selected {
                    theme::editor_field_focus()
                } else {
                    theme::text()
                };
                let (value, placeholder) = self.field_value_text(*field);
                let source = self.field_source(*field);
                let mut spans = vec![Span::styled(
                    format!(
                        " {} {:>label_width$}: ",
                        if selected { "›" } else { " " },
                        field.label()
                    ),
                    style,
                )];
                if value.is_empty() {
                    if let Some(placeholder) = placeholder {
                        spans.push(Span::styled(placeholder, theme::muted_italic()));
                    }
                } else {
                    spans.push(Span::styled(value, style));
                }
                spans.push(Span::styled(
                    format!(" ({})", source.label()),
                    theme::muted(),
                ));
                lines.push(Line::from(spans));
            }
            if matches!(mode, TerminalMode::Brainstorm) {
                lines.push(Line::styled(format!(" {BRAINSTORM_NOTE}"), theme::muted()));
            }
        }

        lines.push(Line::raw(""));
        let preview_mode = self.selected_mode();
        let preview = format_mode_binding(&self.preview_config(), preview_mode);
        let preview_error = mode_binding_is_error(&self.preview_config(), preview_mode);
        lines.push(Line::from(vec![
            Span::styled(" preview ", theme::muted()),
            Span::styled(
                preview,
                if preview_error {
                    theme::error()
                } else {
                    theme::text()
                },
            ),
        ]));
        lines
    }
}

fn member_picker_lines(picker: &MemberPicker) -> Vec<Line<'static>> {
    let mut lines = vec![Line::styled(
        if picker.multi {
            " Choose participants"
        } else {
            " Choose member"
        },
        theme::accent_bold(),
    )];
    for (index, (id, name)) in picker.options().iter().enumerate() {
        let selected = index == picker.selected();
        let style = if selected {
            theme::bold(theme::emphasis_color())
        } else {
            Style::default().fg(theme::emphasis_color())
        };
        let mark = if picker.multi {
            if picker.checked(index) {
                "[x] "
            } else {
                "[ ] "
            }
        } else if selected {
            "▶ "
        } else {
            "  "
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {mark}"),
                if selected {
                    theme::warning_bold()
                } else {
                    theme::muted()
                },
            ),
            Span::styled(format!("{name} "), style),
            Span::styled(format!("@{id}"), theme::muted()),
        ]));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::team::BackendKind;

    fn member(id: &str, role: &str) -> TeamMember {
        TeamMember::new(id, id, BackendKind::Codex, role)
    }

    fn editor() -> ModeEditor {
        ModeEditor::new(
            "mixed",
            "/tmp/ws",
            Some(DefaultTarget::Member(MemberId::new("builder"))),
            vec![
                member("builder", "implementation"),
                member("reviewer", "code review"),
                member("planner", "planning lead"),
            ],
            ModesConfig::default(),
            ModesConfig::default(),
            TerminalMode::Review,
        )
    }

    #[test]
    fn opens_on_the_active_mode() {
        let editor = editor();
        assert_eq!(editor.selected_mode(), TerminalMode::Review);
        assert!(!editor.field_mode());
    }

    #[test]
    fn enter_on_list_enters_fields_without_switching() {
        let mut editor = editor();
        let outcome = editor.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(outcome, ModeEditorOutcome::Consumed(Vec::new()));
        assert!(editor.field_mode());
    }

    #[test]
    fn tab_and_right_do_not_enter_fields() {
        let mut editor = editor();
        editor.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert!(!editor.field_mode());
        editor.handle_key(KeyCode::Right, KeyModifiers::NONE);
        assert!(!editor.field_mode());
    }

    #[test]
    fn enter_enters_fields_and_number_step_sets_pending() {
        let mut editor = editor();
        editor.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(editor.field_mode());
        editor.field = 2; // max_iterations
        editor.handle_key(KeyCode::Right, KeyModifiers::NONE);
        assert!(editor.dirty());
        assert_eq!(
            editor
                .pending
                .review
                .as_ref()
                .and_then(|cfg| cfg.max_iterations),
            Some(4)
        );
    }

    #[test]
    fn s_applies_overrides_and_selects_mode() {
        let mut editor = editor();
        editor.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        editor.field = 2;
        editor.handle_key(KeyCode::Right, KeyModifiers::NONE);
        let outcome = editor.handle_key(KeyCode::Char('s'), KeyModifiers::NONE);
        match outcome {
            ModeEditorOutcome::Consumed(commands) => {
                assert!(matches!(
                    commands.as_slice(),
                    [
                        UiCommand::SetModeOverrides { .. },
                        UiCommand::SetMode {
                            mode: TerminalMode::Review
                        }
                    ]
                ));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn w_emits_save_after_optional_apply() {
        let mut editor = editor();
        editor.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        editor.field = 2;
        editor.handle_key(KeyCode::Right, KeyModifiers::NONE);
        let outcome = editor.handle_key(KeyCode::Char('w'), KeyModifiers::NONE);
        match outcome {
            ModeEditorOutcome::Consumed(commands) => {
                assert!(matches!(
                    commands.as_slice(),
                    [
                        UiCommand::SetModeOverrides { .. },
                        UiCommand::SaveModeDefaults {
                            mode: TerminalMode::Review
                        }
                    ]
                ));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn invalid_builder_equals_reviewer_blocks_apply() {
        let mut editor = editor();
        editor.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        editor.set_member_field(ModeField::ReviewBuilder, Some(MemberId::new("reviewer")));
        editor.set_member_field(ModeField::ReviewReviewer, Some(MemberId::new("reviewer")));
        let outcome = editor.handle_key(KeyCode::Char('s'), KeyModifiers::NONE);
        assert_eq!(outcome, ModeEditorOutcome::Consumed(Vec::new()));
        assert!(
            editor
                .notice()
                .is_some_and(|text| text.contains("two distinct"))
        );
    }

    #[test]
    fn number_step_stops_at_bounds() {
        let mut editor = editor();
        editor.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        editor.field = 2;
        for _ in 0..30 {
            editor.handle_key(KeyCode::Left, KeyModifiers::NONE);
        }
        assert_eq!(
            editor
                .pending
                .review
                .as_ref()
                .and_then(|cfg| cfg.max_iterations),
            Some(1)
        );
        editor.selected = 3; // brainstorm
        editor.enter_fields();
        editor.field = 1; // rounds
        editor.handle_key(KeyCode::Left, KeyModifiers::NONE);
        assert_eq!(
            editor
                .pending
                .brainstorm
                .as_ref()
                .and_then(|cfg| cfg.generation_rounds),
            Some(2)
        );
    }

    #[test]
    fn participant_picker_toggles_with_space() {
        let mut editor = editor();
        editor.selected = 3;
        editor.enter_fields();
        editor.field = 0;
        editor.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(editor.member_picker().is_some_and(|picker| picker.multi()));
        editor.handle_key(KeyCode::Char(' '), KeyModifiers::NONE);
        editor.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(editor.member_picker().is_none());
        assert!(editor.dirty());
    }

    #[test]
    fn r_clears_conversation_override() {
        let mut editor = editor();
        editor.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        editor.field = 2;
        editor.handle_key(KeyCode::Right, KeyModifiers::NONE);
        assert!(editor.dirty());
        editor.handle_key(KeyCode::Char('r'), KeyModifiers::NONE);
        assert!(!editor.dirty());
    }

    #[test]
    fn plan_builder_is_required_and_reviewer_is_optional() {
        let mut editor = editor();
        editor.selected = 2; // plan
        editor.enter_fields();
        editor.field = 1; // builder

        assert_eq!(
            editor.field_value_text(ModeField::PlanBuilder),
            (
                String::new(),
                Some("required — choose a builder".to_string())
            )
        );

        editor.set_member_field(ModeField::PlanBuilder, Some(MemberId::new("builder")));
        assert_eq!(
            editor.field_value_text(ModeField::PlanBuilder),
            ("builder".to_string(), None)
        );
        assert_eq!(
            editor.field_value_text(ModeField::PlanReviewer),
            (
                String::new(),
                Some("not set — skip plan review".to_string())
            )
        );
    }
}
