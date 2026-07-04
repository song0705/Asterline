//! Live team roster editor used by the `/team` drawer.

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyModifiers};

use crate::domain::config::default_member;
use crate::domain::event::UiCommand;
use crate::domain::team::{BackendKind, DefaultTarget, MemberId, TeamConfig, TeamMember};
use crate::tui::team_builder::{
    EditState, Field, cycle_backend, cycle_effort, cycle_permission, cycle_sandbox, field_value,
    normalize_member_id, unique_display_name, unique_display_name_except, unique_member_id,
};

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum TeamEditorOutcome {
    Ignored,
    Consumed(Option<UiCommand>),
    Close,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TeamEditor {
    team: String,
    workspace: PathBuf,
    default_target: Option<DefaultTarget>,
    members: Vec<TeamMember>,
    available: Vec<BackendKind>,
    selected: usize,
    field: usize,
    editing: Option<EditState>,
    dirty: bool,
    notice: Option<String>,
}

impl TeamEditor {
    pub(crate) fn new(
        team: impl Into<String>,
        workspace: impl Into<PathBuf>,
        default_target: Option<DefaultTarget>,
        members: Vec<TeamMember>,
    ) -> Self {
        Self {
            team: team.into(),
            workspace: workspace.into(),
            default_target,
            members,
            available: vec![BackendKind::Codex, BackendKind::Claude, BackendKind::Agy],
            selected: 0,
            field: 0,
            editing: None,
            dirty: false,
            notice: None,
        }
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

    pub(crate) fn editing(&self) -> Option<&EditState> {
        self.editing.as_ref()
    }

    pub(crate) fn dirty(&self) -> bool {
        self.dirty
    }

    pub(crate) fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
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

    pub(crate) fn handle_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> TeamEditorOutcome {
        if self.editing.is_some() {
            self.handle_edit_key(code, modifiers);
            return TeamEditorOutcome::Consumed(None);
        }

        let ctrl = modifiers.contains(KeyModifiers::CONTROL);
        match code {
            KeyCode::Char('c') if ctrl => TeamEditorOutcome::Close,
            KeyCode::Esc | KeyCode::Char('q') => TeamEditorOutcome::Close,
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
                TeamEditorOutcome::Consumed(None)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.selected + 1 < self.members.len() {
                    self.selected += 1;
                }
                TeamEditorOutcome::Consumed(None)
            }
            KeyCode::Left => {
                self.prev_field();
                TeamEditorOutcome::Consumed(None)
            }
            KeyCode::Right | KeyCode::Tab => {
                self.next_field();
                TeamEditorOutcome::Consumed(None)
            }
            KeyCode::BackTab => {
                self.prev_field();
                TeamEditorOutcome::Consumed(None)
            }
            KeyCode::Char('a') => {
                self.add_member();
                TeamEditorOutcome::Consumed(None)
            }
            KeyCode::Char('d') => {
                self.delete_member();
                TeamEditorOutcome::Consumed(None)
            }
            KeyCode::Char('t') => {
                self.set_default_to_selected();
                TeamEditorOutcome::Consumed(None)
            }
            KeyCode::Char('*') => {
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
            KeyCode::Enter => {
                self.activate_field();
                TeamEditorOutcome::Consumed(None)
            }
            KeyCode::Backspace | KeyCode::Char(_) => TeamEditorOutcome::Consumed(None),
            _ => TeamEditorOutcome::Ignored,
        }
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
        if field.is_text() {
            let Some(member) = self.selected_member() else {
                return;
            };
            self.editing = Some(EditState {
                field,
                buffer: field_value(member, field),
            });
        } else {
            self.cycle_field(field);
        }
    }

    fn cycle_field(&mut self, field: Field) {
        match field {
            Field::Backend => {
                let current = self.selected_member().map(|m| m.backend);
                if let Some(current) = current {
                    let next = cycle_backend(current, &self.available);
                    if let Some(member) = self.selected_member_mut() {
                        member.backend = next;
                    }
                }
            }
            Field::Effort => {
                if let Some(member) = self.selected_member_mut() {
                    member.effort = cycle_effort(member.effort);
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
                    member.model = if value.is_empty() || value == "default" {
                        None
                    } else {
                        Some(value.to_string())
                    };
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
        editor.commit_edit(EditState {
            field: Field::Name,
            buffer: "Lead Engineer".to_string(),
        });

        assert_eq!(
            editor.normalized_default_target(),
            Some(DefaultTarget::Member(MemberId::new("lead-engineer")))
        );
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
    fn left_and_right_arrows_select_member_fields() {
        let mut editor = editor();
        assert_eq!(editor.selected_field(), Field::Name);

        let right = editor.handle_key(KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(right, TeamEditorOutcome::Consumed(None));
        assert_eq!(editor.selected_field(), Field::Backend);

        let left = editor.handle_key(KeyCode::Left, KeyModifiers::NONE);
        assert_eq!(left, TeamEditorOutcome::Consumed(None));
        assert_eq!(editor.selected_field(), Field::Name);

        let wrap = editor.handle_key(KeyCode::Left, KeyModifiers::NONE);
        assert_eq!(wrap, TeamEditorOutcome::Consumed(None));
        assert_eq!(editor.selected_field(), Field::Cwd);
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
}
