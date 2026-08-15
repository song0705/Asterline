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
use std::time::Duration;

use crossterm::event::{self, EnableBracketedPaste, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, enable_raw_mode};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::adapter::{DiscoveredCatalog, DiscoveredModel};
use crate::domain::config::{DetectedBackends, default_member, default_team};
use crate::domain::team::{
    BackendKind, DefaultTarget, Effort, MemberId, PermissionMode, SandboxPolicy, SessionPolicy,
    TeamConfig, TeamMember, normalize_member_id as normalize_domain_member_id,
};
use crate::tui::theme;

#[derive(Debug)]
enum ModelLoad {
    Loading(Receiver<Result<DiscoveredCatalog, String>>),
    Ready(Result<DiscoveredCatalog, String>),
}

#[derive(Debug, Default)]
pub(crate) struct ModelCatalog {
    loads: HashMap<(BackendKind, PathBuf), ModelLoad>,
    frozen: bool,
}

pub(crate) enum ModelChoices {
    Loading,
    Ready(Vec<DiscoveredModel>),
    Failed(String),
}

impl ModelCatalog {
    /// Begin an asynchronous lookup ahead of opening a model picker. Repeated
    /// calls for the same backend and working directory are coalesced.
    pub(crate) fn preload(&mut self, backend: BackendKind, cwd: &Path) {
        if !self.frozen {
            self.request(backend, cwd);
        }
    }

    /// Prevent follow-up UI actions from spawning more model-list commands.
    /// The normal Team UI freezes its initial roster catalog after startup;
    /// the interactive first-run builder intentionally leaves its own catalog
    /// open while the user is still composing a roster.
    pub(crate) fn freeze(&mut self) {
        self.frozen = true;
    }

    /// Retry exactly one catalog that failed during this `ast` session. A
    /// successful or still-loading catalog is retained, so every member with
    /// the same backend and working directory continues to share it.
    pub(crate) fn refresh_failed(
        &mut self,
        backend: BackendKind,
        cwd: &Path,
    ) -> Result<(), String> {
        let key = (backend, cwd.to_path_buf());
        match self.loads.get(&key) {
            Some(ModelLoad::Ready(Err(_))) => {
                self.loads.remove(&key);
                // This is the sole post-startup escape hatch: an explicit
                // retry of a failure selected by the user in the Team editor.
                self.request(backend, cwd);
                Ok(())
            }
            Some(ModelLoad::Loading(_)) => Err(format!(
                "{} model catalog is still loading",
                backend.as_str()
            )),
            Some(ModelLoad::Ready(Ok(_))) => Err(format!(
                "{} model catalog is already loaded for this ast session",
                backend.as_str()
            )),
            None => Err(format!(
                "{} was not in the startup roster; restart ast to load its models",
                backend.as_str()
            )),
        }
    }

    /// Start discovery without cloning a ready model list. This is used by
    /// background warm-up paths that can be called on every TUI frame.
    fn request(&mut self, backend: BackendKind, cwd: &Path) {
        let key = (backend, cwd.to_path_buf());
        if self.loads.contains_key(&key) {
            return;
        }
        let worker_cwd = cwd.to_path_buf();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(crate::adapter::models::discover_catalog(
                backend,
                &worker_cwd,
            ));
        });
        self.loads.insert(key, ModelLoad::Loading(rx));
    }

    pub(crate) fn poll(&mut self) {
        let keys = self.loads.keys().cloned().collect::<Vec<_>>();
        for key in keys {
            let result = match self.loads.get(&key) {
                Some(ModelLoad::Loading(rx)) => match rx.try_recv() {
                    Ok(result) => Some(result),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => Some(Err(
                        "model discovery worker stopped unexpectedly".to_string(),
                    )),
                },
                _ => None,
            };
            if let Some(result) = result {
                self.loads.insert(key, ModelLoad::Ready(result));
            }
        }
    }

    pub(crate) fn models(&mut self, backend: BackendKind, cwd: &Path) -> ModelChoices {
        let key = (backend, cwd.to_path_buf());
        if self.frozen && !self.loads.contains_key(&key) {
            return ModelChoices::Failed(
                "not loaded for this ast session; restart to refresh models".to_string(),
            );
        }
        self.request(backend, cwd);
        let Some(load) = self.loads.get_mut(&key) else {
            return ModelChoices::Loading;
        };
        let result = match load {
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
        *load = ModelLoad::Ready(result);
        choices
    }

    #[cfg(test)]
    pub(crate) fn seed(&mut self, backend: BackendKind, cwd: &Path, models: Vec<String>) {
        self.loads.insert(
            (backend, cwd.to_path_buf()),
            ModelLoad::Ready(Ok(DiscoveredCatalog {
                models: models
                    .into_iter()
                    .enumerate()
                    .map(|(index, id)| {
                        let mut model = DiscoveredModel::simple(id);
                        model.is_default = index == 0;
                        model
                    })
                    .collect(),
                native_permission: None,
            })),
        );
    }

    #[cfg(test)]
    pub(crate) fn seed_with_native_permission(
        &mut self,
        backend: BackendKind,
        cwd: &Path,
        permission: &str,
    ) {
        self.loads.insert(
            (backend, cwd.to_path_buf()),
            ModelLoad::Ready(Ok(DiscoveredCatalog {
                models: Vec::new(),
                native_permission: Some(permission.to_string()),
            })),
        );
    }

    #[cfg(test)]
    pub(crate) fn contains(&self, backend: BackendKind, cwd: &Path) -> bool {
        self.loads.contains_key(&(backend, cwd.to_path_buf()))
    }

    fn is_loading_for(&self, backend: BackendKind, cwd: &Path) -> bool {
        matches!(
            self.loads.get(&(backend, cwd.to_path_buf())),
            Some(ModelLoad::Loading(_))
        )
    }

    fn is_failed_for(&self, backend: BackendKind, cwd: &Path) -> bool {
        matches!(
            self.loads.get(&(backend, cwd.to_path_buf())),
            Some(ModelLoad::Ready(Err(_)))
        )
    }

    fn has_ready_catalog(&self, backend: BackendKind, cwd: &Path) -> bool {
        matches!(
            self.loads.get(&(backend, cwd.to_path_buf())),
            Some(ModelLoad::Ready(Ok(_)))
        )
    }

    fn discovered_model(
        &self,
        backend: BackendKind,
        cwd: &Path,
        selected: Option<&str>,
    ) -> Option<&DiscoveredModel> {
        let ModelLoad::Ready(Ok(catalog)) = self.loads.get(&(backend, cwd.to_path_buf()))? else {
            return None;
        };
        match selected {
            Some(id) => catalog.models.iter().find(|model| model.id == id),
            None => catalog.models.iter().find(|model| model.is_default),
        }
    }

    /// Show a backend's configured permission default when Asterline leaves it
    /// unmodified. Codex is excluded: Asterline deliberately sends its product
    /// default (`never`) rather than inheriting a local CLI policy.
    pub(crate) fn native_permission_label(
        &self,
        member: &TeamMember,
        workspace: &Path,
    ) -> Option<String> {
        if member.backend == BackendKind::Codex {
            return None;
        }
        if !matches!(member.permission_mode, None | Some(PermissionMode::Default)) {
            return None;
        }
        let cwd = member.resolved_cwd(workspace);
        match self.loads.get(&(member.backend, cwd)) {
            Some(ModelLoad::Loading(_)) => Some("loading…".to_string()),
            Some(ModelLoad::Ready(Ok(catalog))) => catalog.native_permission.clone(),
            Some(ModelLoad::Ready(Err(_))) => Some("unavailable".to_string()),
            None => None,
        }
    }

    pub(crate) fn model_label(&self, member: &TeamMember, workspace: &Path) -> String {
        let cwd = member.resolved_cwd(workspace);
        let discovered = self.discovered_model(member.backend, &cwd, member.model.as_deref());
        let label = match (&member.model, discovered) {
            (None, Some(model)) => model.name.clone(),
            (None, None) if self.is_loading_for(member.backend, &cwd) => "loading…".to_string(),
            (None, None) if self.is_failed_for(member.backend, &cwd) => "unavailable".to_string(),
            // A successful catalog need not nominate a default model. It
            // still gives the picker real choices, so calling this state "not
            // loaded" made the field contradict the picker.
            (None, None) if self.has_ready_catalog(member.backend, &cwd) => {
                "CLI default".to_string()
            }
            (None, None) if self.frozen => "not preloaded at startup".to_string(),
            (None, None) => "CLI default".to_string(),
            (Some(id), Some(model)) if model.name != *id => format!("{} · {id}", model.name),
            (Some(id), _) => id.clone(),
        };

        // Agy commonly encodes the reasoning tier in the selected model name
        // (for example `gemini-…-high`), so showing it again is noise. Codex
        // and Grok report it separately in their catalogs; Claude only has a
        // value here when the member explicitly configured one.
        let effort = if member.backend == BackendKind::Agy {
            None
        } else {
            member
                .effort
                .or_else(|| discovered.and_then(|model| model.default_effort))
        };
        match effort {
            Some(effort) => format!("{label} · {}", effort.as_str()),
            None => label,
        }
    }

    pub(crate) fn backend_summary(&self, backend: BackendKind, cwd: &Path) -> String {
        match self.loads.get(&(backend, cwd.to_path_buf())) {
            None if self.frozen => "installed · models not loaded this session".to_string(),
            None => "installed · models load when selected".to_string(),
            Some(ModelLoad::Loading(_)) => "installed · loading models…".to_string(),
            Some(ModelLoad::Ready(Err(err))) => {
                format!("installed · model discovery failed: {}", one_line(err, 42))
            }
            Some(ModelLoad::Ready(Ok(catalog))) => {
                let model_names = catalog
                    .models
                    .iter()
                    .take(3)
                    .map(|model| model.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                let suffix = if catalog.models.len() > 3 {
                    ", …"
                } else {
                    ""
                };
                let mut efforts = catalog
                    .models
                    .iter()
                    .flat_map(|model| model.supported_efforts.iter().copied())
                    .collect::<Vec<_>>();
                if !efforts.is_empty() {
                    efforts.sort_by_key(|effort| effort_rank(*effort));
                    efforts.dedup();
                }
                let efforts = efforts
                    .iter()
                    .map(|effort| effort.as_str())
                    .collect::<Vec<_>>()
                    .join("/");
                let effort_suffix = if efforts.is_empty() {
                    String::new()
                } else {
                    format!(" · effort {efforts}")
                };
                format!(
                    "installed · {} model(s): {model_names}{suffix}{effort_suffix}",
                    catalog.models.len()
                )
            }
        }
    }
}

fn effort_rank(effort: Effort) -> usize {
    match effort {
        Effort::Low => 0,
        Effort::Medium => 1,
        Effort::High => 2,
        Effort::Xhigh => 3,
        Effort::Max => 4,
        Effort::Ultra => 5,
    }
}

fn one_line(value: &str, max: usize) -> String {
    let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if value.chars().count() <= max {
        value
    } else {
        format!(
            "{}…",
            value
                .chars()
                .take(max.saturating_sub(1))
                .collect::<String>()
        )
    }
}

fn model_choices(result: &Result<DiscoveredCatalog, String>) -> ModelChoices {
    match result {
        Ok(catalog) => ModelChoices::Ready(catalog.models.clone()),
        Err(err) => ModelChoices::Failed(err.clone()),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ModelChoice {
    value: Option<String>,
    model: Option<DiscoveredModel>,
}

impl ModelChoice {
    pub(crate) fn name(&self) -> &str {
        if self.value.is_none() {
            "default"
        } else {
            self.model
                .as_ref()
                .map_or("Custom model", |model| &model.name)
        }
    }

    pub(crate) fn id(&self, backend: BackendKind) -> String {
        match (&self.value, &self.model) {
            (None, Some(model)) => format!("{} (CLI default)", model.id),
            (None, None) => format!("{} CLI default", backend.as_str()),
            (Some(value), _) => value.clone(),
        }
    }

    fn supported_efforts(&self) -> &[Effort] {
        self.model
            .as_ref()
            .map_or(&[], |model| &model.supported_efforts)
    }

    fn default_effort(&self) -> Option<Effort> {
        self.model.as_ref().and_then(|model| model.default_effort)
    }

    fn default_effort_label(&self) -> String {
        self.default_effort().map_or_else(
            || "default".to_string(),
            |effort| effort.as_str().to_string(),
        )
    }

    fn effort_choices_label(&self) -> String {
        std::iter::once("default")
            .chain(
                self.ordered_supported_efforts()
                    .iter()
                    .map(|effort| effort.as_str()),
            )
            .collect::<Vec<_>>()
            .join(" · ")
    }

    fn ordered_supported_efforts(&self) -> Vec<Effort> {
        let mut efforts = self.supported_efforts().to_vec();
        efforts.sort_by_key(|effort| effort_rank(*effort));
        efforts.dedup();
        efforts
    }

    pub(crate) fn description(&self) -> Option<&str> {
        self.model
            .as_ref()
            .and_then(|model| model.description.as_deref())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ModelPicker {
    backend: BackendKind,
    options: Vec<ModelChoice>,
    selected: usize,
    effort: Option<Effort>,
    query: String,
}

impl ModelPicker {
    pub(crate) fn new(
        backend: BackendKind,
        current: Option<&str>,
        current_effort: Option<Effort>,
        models: Vec<DiscoveredModel>,
    ) -> Self {
        // Once the CLI gives us concrete models, make its actual default the
        // selected entry instead of obscuring it behind a generic `default`.
        // Keep `default` only as the empty-catalog fallback.
        let mut options = Vec::new();
        if models.is_empty() {
            options.push(ModelChoice {
                value: None,
                model: None,
            });
        }
        if let Some(current) = current
            && !models.iter().any(|model| model.id == current)
        {
            options.push(ModelChoice {
                value: Some(current.to_string()),
                model: Some(DiscoveredModel::simple(current)),
            });
        }
        options.extend(models.into_iter().map(|model| ModelChoice {
            value: Some(model.id.clone()),
            model: Some(model),
        }));
        let selected = current
            .and_then(|current| {
                options
                    .iter()
                    .position(|choice| choice.value.as_deref() == Some(current))
            })
            .or_else(|| {
                options
                    .iter()
                    .position(|choice| choice.model.as_ref().is_some_and(|model| model.is_default))
            })
            .unwrap_or(0);
        let mut picker = Self {
            backend,
            options,
            selected,
            effort: current_effort,
            query: String::new(),
        };
        picker.normalize_effort();
        picker
    }

    pub(crate) fn backend(&self) -> BackendKind {
        self.backend
    }

    #[cfg(test)]
    pub(crate) fn selected(&self) -> usize {
        self.selected
    }

    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    fn visible_indices(&self) -> Vec<usize> {
        let query = self.query.trim().to_ascii_lowercase();
        self.options
            .iter()
            .enumerate()
            .filter(|(_, choice)| {
                query.is_empty()
                    || choice.name().to_ascii_lowercase().contains(&query)
                    || choice
                        .id(self.backend)
                        .to_ascii_lowercase()
                        .contains(&query)
                    || choice
                        .description()
                        .is_some_and(|text| text.to_ascii_lowercase().contains(&query))
            })
            .map(|(index, _)| index)
            .collect()
    }

    pub(crate) fn window(&self, max: usize) -> (usize, Vec<&ModelChoice>) {
        let visible = self.visible_indices();
        if visible.is_empty() {
            return (0, Vec::new());
        }
        let selected = visible
            .iter()
            .position(|index| *index == self.selected)
            .unwrap_or(0);
        let max = max.max(1).min(visible.len());
        let start = selected
            .saturating_sub(max / 2)
            .min(visible.len().saturating_sub(max));
        (
            start,
            visible[start..start + max]
                .iter()
                .map(|index| &self.options[*index])
                .collect(),
        )
    }

    pub(crate) fn visible_len(&self) -> usize {
        self.visible_indices().len()
    }

    pub(crate) fn selected_choice(&self) -> Option<&ModelChoice> {
        self.options.get(self.selected)
    }

    pub(crate) fn up(&mut self) {
        let visible = self.visible_indices();
        let position = visible
            .iter()
            .position(|index| *index == self.selected)
            .unwrap_or(0);
        if let Some(index) = visible.get(position.saturating_sub(1)) {
            self.selected = *index;
            self.normalize_effort();
        }
    }

    pub(crate) fn down(&mut self) {
        let visible = self.visible_indices();
        let position = visible
            .iter()
            .position(|index| *index == self.selected)
            .unwrap_or(0);
        if let Some(index) = visible.get(position + 1) {
            self.selected = *index;
            self.normalize_effort();
        }
    }

    pub(crate) fn previous_effort(&mut self) {
        let choices = self.ordered_selected_efforts();
        if choices.is_empty() {
            return;
        }
        self.effort = match self
            .effort
            .and_then(|effort| choices.iter().position(|choice| *choice == effort))
        {
            None => self
                .selected_default_effort()
                .and_then(|effort| choices.iter().position(|choice| *choice == effort))
                .and_then(|index| index.checked_sub(1))
                .and_then(|index| choices.get(index).copied()),
            Some(0) => None,
            Some(index) => Some(choices[index - 1]),
        };
    }

    pub(crate) fn next_effort(&mut self) {
        let choices = self.ordered_selected_efforts();
        if choices.is_empty() {
            return;
        }
        self.effort = match self
            .effort
            .and_then(|effort| choices.iter().position(|choice| *choice == effort))
        {
            None => match self.selected_default_effort() {
                Some(effort) => choices
                    .iter()
                    .position(|choice| *choice == effort)
                    .and_then(|index| choices.get(index + 1).copied()),
                None => choices.first().copied(),
            },
            Some(index) => choices
                .get(index + 1)
                .copied()
                .or_else(|| choices.get(index).copied()),
        };
    }

    pub(crate) fn push_query(&mut self, ch: char) {
        self.query.push(ch);
        self.select_first_visible();
    }

    pub(crate) fn pop_query(&mut self) {
        self.query.pop();
        self.select_first_visible();
    }

    pub(crate) fn clear_query(&mut self) {
        self.query.clear();
        self.select_first_visible();
    }

    fn select_first_visible(&mut self) {
        if let Some(index) = self.visible_indices().first() {
            self.selected = *index;
            self.normalize_effort();
        }
    }

    pub(crate) fn value(&self) -> Option<String> {
        self.options
            .get(self.selected)
            .and_then(|choice| choice.value.clone())
    }

    fn selected_efforts(&self) -> &[Effort] {
        self.selected_choice()
            .map_or(&[], ModelChoice::supported_efforts)
    }

    fn ordered_selected_efforts(&self) -> Vec<Effort> {
        let mut efforts = self.selected_efforts().to_vec();
        efforts.sort_by_key(|effort| effort_rank(*effort));
        efforts.dedup();
        efforts
    }

    fn selected_default_effort(&self) -> Option<Effort> {
        self.selected_choice().and_then(|choice| {
            choice
                .default_effort()
                .filter(|effort| choice.supported_efforts().contains(effort))
        })
    }

    fn normalize_effort(&mut self) {
        if self
            .selected_choice()
            .is_some_and(|choice| choice.value.is_none())
        {
            // Selecting the CLI-default row resets both overrides. Otherwise
            // a stale model-specific effort would remain hidden behind a
            // seemingly-default selection.
            self.effort = None;
        }
        // Do not alter an existing override merely because the cursor visits a
        // different model. In particular, `↑`/`↓` must not turn a configured
        // `high` into that other model's `low`/`medium` default. Only ←/→ or
        // selecting the CLI-default row changes the stored override.
    }

    pub(crate) fn effort(&self) -> Option<Effort> {
        self.effort
    }

    /// An advertised capability list is authoritative when it is present.
    /// An empty list means the backend did not provide a machine-readable
    /// effort menu, so an existing explicit value must be preserved rather
    /// than guessed away.
    pub(crate) fn has_unsupported_effort_override(&self) -> bool {
        let choices = self.selected_efforts();
        !choices.is_empty() && self.effort.is_some_and(|effort| !choices.contains(&effort))
    }

    pub(crate) fn unsupported_effort_notice(&self) -> Option<String> {
        let effort = self.effort?;
        if !self.has_unsupported_effort_override() {
            return None;
        }
        let model = self
            .selected_choice()
            .map(|choice| choice.id(self.backend))
            .unwrap_or_else(|| "selected model".to_string());
        Some(format!(
            "{model} does not advertise {} · use ←/→ to choose an advertised effort",
            effort.as_str()
        ))
    }

    pub(crate) fn effort_label(&self) -> String {
        match self.effort {
            Some(effort) if self.has_unsupported_effort_override() => {
                format!("{} (unsupported)", effort.as_str())
            }
            Some(effort) => format!("{} (override)", effort.as_str()),
            None => self.selected_choice().map_or_else(
                || "default".to_string(),
                |choice| format!("default {}", choice.default_effort_label()),
            ),
        }
    }

    pub(crate) fn effort_choices_label(&self) -> String {
        self.selected_choice()
            .map_or_else(|| "default".to_string(), ModelChoice::effort_choices_label)
    }

    pub(crate) fn has_effort_controls(&self) -> bool {
        self.options
            .iter()
            .any(|choice| !choice.supported_efforts().is_empty())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct BackendChoice {
    pub(crate) backend: BackendKind,
    pub(crate) installed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BackendPicker {
    options: Vec<BackendChoice>,
    selected: usize,
}

impl BackendPicker {
    pub(crate) fn new(current: BackendKind, detected: DetectedBackends) -> Self {
        let options = [
            BackendKind::Codex,
            BackendKind::Claude,
            BackendKind::Grok,
            BackendKind::Agy,
        ]
        .into_iter()
        .map(|backend| BackendChoice {
            backend,
            installed: detected.contains(backend),
        })
        .collect::<Vec<_>>();
        let selected = options
            .iter()
            .position(|choice| choice.backend == current)
            .unwrap_or(0);
        Self { options, selected }
    }

    pub(crate) fn choices(&self) -> &[BackendChoice] {
        &self.options
    }

    pub(crate) fn selected(&self) -> usize {
        self.selected
    }

    pub(crate) fn up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub(crate) fn down(&mut self) {
        if self.selected + 1 < self.options.len() {
            self.selected += 1;
        }
    }

    pub(crate) fn selected_choice(&self) -> Option<BackendChoice> {
        self.options.get(self.selected).copied()
    }
}

pub(crate) fn backend_picker_lines(
    picker: &BackendPicker,
    catalog: &ModelCatalog,
    cwd: &Path,
    width: usize,
) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::styled(" Agent CLI catalog", theme::accent_bold()),
        Line::styled(
            " Availability is checked first; model catalogs load when selected.",
            theme::muted(),
        ),
    ];
    for (index, choice) in picker.choices().iter().enumerate() {
        let selected = index == picker.selected();
        let status = if choice.installed {
            catalog.backend_summary(choice.backend, cwd)
        } else {
            "not installed on PATH".to_string()
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
                theme::pad_width(choice.backend.as_str(), 8),
                if selected {
                    theme::emphasis()
                } else {
                    Style::default().fg(theme::backend_color(choice.backend))
                },
            ),
            Span::styled(
                theme::clip_width(&status, width.saturating_sub(13)),
                if choice.installed {
                    theme::text()
                } else {
                    theme::muted()
                },
            ),
        ]));
    }
    lines.push(Line::styled(
        " ↑/↓ choose · Enter apply · Esc cancel",
        theme::muted_italic(),
    ));
    lines
}

pub(crate) fn model_picker_lines(
    picker: &ModelPicker,
    width: usize,
    max_rows: usize,
) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::styled(" Model catalog", theme::accent_bold()),
        Line::from(vec![
            Span::styled(" Search: ", theme::muted()),
            Span::styled(
                if picker.query().is_empty() {
                    "type a model name or ID…".to_string()
                } else {
                    picker.query().to_string()
                },
                if picker.query().is_empty() {
                    theme::muted_italic()
                } else {
                    theme::emphasis()
                },
            ),
            Span::styled(
                format!("  {} match(es)", picker.visible_len()),
                theme::muted(),
            ),
        ]),
    ];
    if picker.visible_len() == 0 {
        lines.push(Line::styled(" No matching models.", theme::muted()));
        return lines;
    }

    let show_effort = picker.has_effort_controls();
    let available = width
        .saturating_sub(if show_effort { 7 } else { 4 })
        .max(30);
    let name_width = (available * 24 / 100).max(10);
    let id_width = if show_effort {
        (available * 43 / 100).max(12)
    } else {
        available.saturating_sub(name_width).max(12)
    };
    let effort_width = available.saturating_sub(name_width + id_width).max(8);
    let mut header = vec![
        Span::styled("   Model", theme::muted()),
        Span::styled(
            theme::pad_width("", name_width.saturating_sub(5)),
            theme::muted(),
        ),
        Span::styled("│ ID", theme::muted()),
        Span::styled(
            theme::pad_width("", id_width.saturating_sub(3)),
            theme::muted(),
        ),
    ];
    if show_effort {
        header.push(Span::styled("│ Effort (←/→)", theme::muted()));
    }
    lines.push(Line::from(header));

    let (start, choices) = picker.window(max_rows);
    if start > 0 {
        lines.push(Line::styled("   …", theme::muted()));
    }
    for choice in choices {
        let selected = picker
            .selected_choice()
            .is_some_and(|current| current == choice);
        let row_style = if selected {
            theme::emphasis()
        } else {
            theme::text()
        };
        let mut row = vec![
            Span::styled(
                if selected { " ▶ " } else { "   " },
                if selected {
                    theme::warning_bold()
                } else {
                    theme::muted()
                },
            ),
            Span::styled(
                theme::pad_width(&theme::clip_width(choice.name(), name_width), name_width),
                row_style,
            ),
            Span::styled("│ ", theme::muted()),
            Span::styled(
                theme::pad_width(
                    &theme::clip_width(&choice.id(picker.backend()), id_width.saturating_sub(2)),
                    id_width.saturating_sub(2),
                ),
                row_style,
            ),
        ];
        if show_effort {
            row.push(Span::styled("│ ", theme::muted()));
            row.push(Span::styled(
                theme::clip_width(
                    &if selected {
                        picker.effort_label()
                    } else {
                        choice.default_effort_label()
                    },
                    effort_width.saturating_sub(2),
                ),
                if selected { row_style } else { theme::muted() },
            ));
        }
        lines.push(Line::from(row));
    }
    if start + max_rows.min(picker.visible_len()) < picker.visible_len() {
        lines.push(Line::styled("   …", theme::muted()));
    }
    if let Some(description) = picker.selected_choice().and_then(ModelChoice::description) {
        lines.push(Line::from(vec![
            Span::styled(" About: ", theme::muted()),
            Span::styled(
                theme::clip_width(description, width.saturating_sub(8)),
                theme::text(),
            ),
        ]));
    }
    if show_effort {
        lines.push(Line::from(vec![
            Span::styled(" Effort choices: ", theme::muted()),
            Span::styled(
                theme::clip_width(&picker.effort_choices_label(), width.saturating_sub(17)),
                theme::text(),
            ),
        ]));
    }
    lines.push(Line::styled(
        if show_effort {
            " Type to filter · ↑/↓ model · ←/→ effort · Enter apply both · Esc cancel"
        } else {
            " Type to filter · ↑/↓ model · Enter apply · Esc cancel"
        },
        theme::muted_italic(),
    ));
    lines
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
    state.preload_agent_catalog();

    loop {
        state.poll_agent_catalog();
        terminal.draw(|frame| render(frame, &state))?;

        if !event::poll(Duration::from_millis(50))? {
            continue;
        }
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
    Sandbox,
    Permission,
    Session,
    SessionId,
}

impl Field {
    pub(crate) const ALL: &'static [Field] = &[
        Field::Name,
        Field::Backend,
        Field::Role,
        Field::Model,
        Field::Sandbox,
        Field::Permission,
        Field::Session,
        Field::SessionId,
    ];

    const CLAUDE_FIELDS: &'static [Field] = &[
        Field::Name,
        Field::Backend,
        Field::Role,
        Field::Model,
        Field::Permission,
        Field::Session,
        Field::SessionId,
    ];

    const NO_SEPARATE_EFFORT_FIELDS: &'static [Field] = &[
        Field::Name,
        Field::Backend,
        Field::Role,
        Field::Model,
        Field::Sandbox,
        Field::Permission,
        Field::Session,
        Field::SessionId,
    ];

    pub(crate) fn for_backend(backend: BackendKind) -> &'static [Field] {
        match backend {
            BackendKind::Codex => Self::ALL,
            // Claude has no sandbox CLI parameter. Showing Codex's three
            // sandbox names here made a saved Team setting look effective
            // when the adapter deliberately did not pass it.
            BackendKind::Claude => Self::CLAUDE_FIELDS,
            // Model selection owns effort for every backend. Keep the member
            // form focused on independent settings only.
            BackendKind::Grok | BackendKind::Agy => Self::NO_SEPARATE_EFFORT_FIELDS,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Backend => "backend",
            Self::Role => "role",
            Self::Model => "model",
            Self::Sandbox => "sandbox",
            Self::Permission => "permission",
            Self::Session => "session",
            Self::SessionId => "session id",
        }
    }

    pub(crate) fn label_for_backend(self, backend: BackendKind) -> &'static str {
        match (self, backend) {
            (Self::Permission, BackendKind::Codex) => "approval policy",
            (Self::Permission, BackendKind::Claude | BackendKind::Grok) => "permission mode",
            (Self::Permission, BackendKind::Agy) => "execution mode",
            (Self::Sandbox, BackendKind::Agy) => "terminal sandbox",
            _ => self.label(),
        }
    }

    /// Detail rows are rendered as `label: value`. Keep their colons aligned
    /// even when a backend exposes longer, backend-specific field labels.
    pub(crate) fn label_width_for_backend(backend: BackendKind) -> usize {
        Self::for_backend(backend)
            .iter()
            .map(|field| field.label_for_backend(backend).len())
            .max()
            .unwrap_or_default()
    }

    pub(crate) fn is_text(self) -> bool {
        matches!(
            self,
            Self::Name | Self::Role | Self::Model | Self::SessionId
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EditState {
    pub(crate) field: Field,
    pub(crate) buffer: String,
    /// Unicode scalar index kept on a user-visible grapheme boundary.
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
        self.cursor = self.grapheme_boundary_at_or_after(self.cursor);
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
        let graphemes = self.buffer.graphemes(true).collect::<Vec<_>>();
        let cursor = self.grapheme_index_at_or_after(self.cursor);
        let mut start = cursor;
        let mut cursor_width = 0;
        while start > 0 {
            let grapheme_width = UnicodeWidthStr::width(graphemes[start - 1]);
            if grapheme_width > 0 && cursor_width + grapheme_width > width.saturating_sub(1) {
                break;
            }
            start -= 1;
            cursor_width += grapheme_width;
        }
        let mut visible = String::new();
        let mut visible_width = 0;
        for grapheme in &graphemes[start..] {
            let grapheme_width = UnicodeWidthStr::width(*grapheme);
            if grapheme_width > 0 && visible_width + grapheme_width > width {
                break;
            }
            visible.push_str(grapheme);
            visible_width += grapheme_width;
        }
        (visible, cursor_width.min(width) as u16)
    }

    fn char_len(&self) -> usize {
        self.buffer.chars().count()
    }

    fn move_left(&mut self) {
        self.cursor = self.previous_grapheme_boundary(self.cursor);
    }

    fn move_right(&mut self) {
        self.cursor = self.next_grapheme_boundary(self.cursor);
    }

    fn move_word_left(&mut self) {
        let graphemes = self.buffer.graphemes(true).collect::<Vec<_>>();
        let mut index = self.grapheme_index_at_or_after(self.cursor);
        while index > 0 && graphemes[index - 1].chars().all(char::is_whitespace) {
            index -= 1;
        }
        while index > 0 && !graphemes[index - 1].chars().all(char::is_whitespace) {
            index -= 1;
        }
        self.cursor = graphemes[..index]
            .iter()
            .map(|grapheme| grapheme.chars().count())
            .sum();
    }

    fn move_word_right(&mut self) {
        let graphemes = self.buffer.graphemes(true).collect::<Vec<_>>();
        let mut index = self.grapheme_index_at_or_after(self.cursor);
        while index < graphemes.len() && !graphemes[index].chars().all(char::is_whitespace) {
            index += 1;
        }
        while index < graphemes.len() && graphemes[index].chars().all(char::is_whitespace) {
            index += 1;
        }
        self.cursor = graphemes[..index]
            .iter()
            .map(|grapheme| grapheme.chars().count())
            .sum();
    }

    fn delete_backward(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let mut chars = self.buffer.chars().collect::<Vec<_>>();
        let start = self.previous_grapheme_boundary(self.cursor);
        chars.drain(start..self.cursor);
        self.cursor = start;
        self.buffer = chars.into_iter().collect();
    }

    fn delete_forward(&mut self) {
        let mut chars = self.buffer.chars().collect::<Vec<_>>();
        if self.cursor < chars.len() {
            let end = self.next_grapheme_boundary(self.cursor);
            chars.drain(self.cursor..end);
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

    fn grapheme_boundaries(&self) -> Vec<usize> {
        let mut scalar = 0;
        let mut boundaries = vec![0];
        for grapheme in self.buffer.graphemes(true) {
            scalar += grapheme.chars().count();
            boundaries.push(scalar);
        }
        boundaries
    }

    fn previous_grapheme_boundary(&self, cursor: usize) -> usize {
        self.grapheme_boundaries()
            .into_iter()
            .take_while(|boundary| *boundary < cursor)
            .last()
            .unwrap_or(0)
    }

    fn next_grapheme_boundary(&self, cursor: usize) -> usize {
        self.grapheme_boundaries()
            .into_iter()
            .find(|boundary| *boundary > cursor)
            .unwrap_or(self.char_len())
    }

    fn grapheme_boundary_at_or_after(&self, cursor: usize) -> usize {
        self.grapheme_boundaries()
            .into_iter()
            .find(|boundary| *boundary >= cursor)
            .unwrap_or(self.char_len())
    }

    fn grapheme_index_at_or_after(&self, cursor: usize) -> usize {
        self.grapheme_boundaries()
            .into_iter()
            .position(|boundary| boundary >= cursor)
            .unwrap_or_else(|| self.buffer.graphemes(true).count())
    }
}

struct BuilderState {
    workspace: PathBuf,
    detected: DetectedBackends,
    available: Vec<BackendKind>,
    members: Vec<TeamMember>,
    selected: usize,
    field: usize,
    field_mode: bool,
    editing: Option<EditState>,
    model_catalog: ModelCatalog,
    backend_picker: Option<BackendPicker>,
    model_picker: Option<ModelPicker>,
    model_picker_pending: bool,
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
        let detected = DetectedBackends {
            codex: available.contains(&BackendKind::Codex),
            claude: available.contains(&BackendKind::Claude),
            grok: available.contains(&BackendKind::Grok),
            agy: available.contains(&BackendKind::Agy),
        };
        Self {
            workspace,
            detected,
            available: available.to_vec(),
            members,
            selected: 0,
            field: 0,
            field_mode: false,
            editing: None,
            model_catalog: ModelCatalog::default(),
            backend_picker: None,
            model_picker: None,
            model_picker_pending: false,
            notice: None,
            cancelled: false,
        }
    }

    fn preload_agent_catalog(&mut self) {
        self.notice = Some("Agent CLIs ready · open a Model field to load its catalog".to_string());
    }

    fn poll_agent_catalog(&mut self) {
        self.model_catalog.poll();
        if self.model_picker_pending
            && self.field_mode
            && self.selected_field() == Field::Model
            && self.model_picker.is_none()
            && self.editing.is_none()
        {
            self.cycle_model();
        }
    }

    fn field_value(&self, member: &TeamMember, field: Field) -> String {
        match field {
            Field::Model => self.model_catalog.model_label(member, &self.workspace),
            Field::Permission => self
                .model_catalog
                .native_permission_label(member, &self.workspace)
                .unwrap_or_else(|| field_value(member, field)),
            _ => field_value(member, field),
        }
    }

    fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        if self.backend_picker.is_some() {
            self.handle_backend_picker_key(code);
            return false;
        }
        if self.model_picker.is_some() {
            self.handle_model_picker_key(code, modifiers);
            return false;
        }
        if self.editing.is_some() {
            self.handle_edit_key(code, modifiers);
            return false;
        }

        let ctrl = modifiers.contains(KeyModifiers::CONTROL);
        match code {
            KeyCode::Char('c') if ctrl => self.cancelled = true,
            KeyCode::Esc if self.field_mode => {
                self.field_mode = false;
                self.model_picker_pending = false;
            }
            KeyCode::Esc | KeyCode::Char('q') => self.cancelled = true,
            KeyCode::Up | KeyCode::Char('k') => {
                if self.field_mode {
                    self.prev_field();
                    self.model_picker_pending = false;
                } else {
                    self.selected = self.selected.saturating_sub(1);
                    self.normalize_field();
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.field_mode {
                    self.next_field();
                    self.model_picker_pending = false;
                } else if self.selected + 1 < self.members.len() {
                    self.selected += 1;
                    self.normalize_field();
                }
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab => {}
            KeyCode::Char('a') if !self.field_mode => self.add_member(),
            KeyCode::Char('d') if !self.field_mode => self.delete_member(),
            KeyCode::Char('s') => return true,
            KeyCode::Char('t') if self.field_mode && self.selected_field() == Field::Model => {
                self.refresh_failed_model_catalog()
            }
            KeyCode::Char('e') if self.field_mode && self.selected_field() == Field::Model => {
                self.edit_selected_field()
            }
            KeyCode::Enter if self.field_mode => self.activate_field(),
            KeyCode::Enter => {
                self.normalize_field();
                self.field_mode = true;
            }
            _ => {}
        }
        false
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
                if self.selected_member().backend != choice.backend {
                    let member = self.selected_member_mut();
                    member.backend = choice.backend;
                    member.session_id = None;
                    member.model = None;
                    member.effort = None;
                    self.normalize_field();
                }
                self.backend_picker = None;
                self.notice = Some("Agent CLI selected · press s to start".to_string());
            }
            KeyCode::Esc => self.backend_picker = None,
            _ => {}
        }
    }

    fn handle_model_picker_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
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
                    return;
                }
                if let Some(notice) = self
                    .model_picker
                    .as_ref()
                    .and_then(ModelPicker::unsupported_effort_notice)
                {
                    self.notice = Some(notice);
                    return;
                }
                let value = self.model_picker.as_ref().and_then(ModelPicker::value);
                let effort = self.model_picker.as_ref().and_then(ModelPicker::effort);
                let member = self.selected_member_mut();
                member.model = value;
                member.effort = effort;
                self.model_picker = None;
                self.notice = Some("model setting selected · press s to start".to_string());
            }
            KeyCode::Backspace => self.model_picker.as_mut().unwrap().pop_query(),
            KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.model_picker.as_mut().unwrap().clear_query();
            }
            KeyCode::Char(ch) if !modifiers.contains(KeyModifiers::CONTROL) => {
                self.model_picker.as_mut().unwrap().push_query(ch);
            }
            KeyCode::Esc => self.model_picker = None,
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
        self.field = (self.field + 1) % self.fields().len();
    }

    fn prev_field(&mut self) {
        self.field = if self.field == 0 {
            self.fields().len() - 1
        } else {
            self.field - 1
        };
    }

    fn selected_field(&self) -> Field {
        self.fields()[self.field.min(self.fields().len() - 1)]
    }

    fn fields(&self) -> &'static [Field] {
        Field::for_backend(self.selected_member().backend)
    }

    fn normalize_field(&mut self) {
        self.field = self.field.min(self.fields().len() - 1);
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
        self.normalize_field();
    }

    fn delete_member(&mut self) {
        if self.members.len() <= 1 {
            return;
        }
        self.members.remove(self.selected);
        if self.selected >= self.members.len() {
            self.selected = self.members.len() - 1;
        }
        self.normalize_field();
    }

    fn activate_field(&mut self) {
        let field = self.selected_field();
        if field == Field::Backend {
            self.backend_picker = Some(BackendPicker::new(
                self.selected_member().backend,
                self.detected,
            ));
            self.notice = Some("↑/↓ choose an installed Agent CLI · Enter select".to_string());
        } else if field == Field::Model {
            self.cycle_model();
        } else if field.is_text() {
            self.edit_selected_field();
        } else {
            self.cycle_field(field);
        }
    }

    fn edit_selected_field(&mut self) {
        let field = self.selected_field();
        if field == Field::Model {
            self.model_picker_pending = false;
        }
        if field.is_text() {
            let value = match field {
                Field::Model => self.selected_member().model.clone().unwrap_or_default(),
                // Display labels such as "select a session" must never
                // become an editable session ID.
                Field::SessionId => self
                    .selected_member()
                    .session_id
                    .clone()
                    .unwrap_or_default(),
                _ => field_value(self.selected_member(), field),
            };
            self.editing = Some(EditState::new(field, value));
        }
    }

    fn cycle_model(&mut self) {
        let backend = self.selected_member().backend;
        let cwd = self.selected_member().resolved_cwd(&self.workspace);
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
                    self.selected_member().model.as_deref(),
                    self.selected_member().effort,
                    models,
                ));
                self.notice =
                    Some("↑/↓ choose model · ←/→ choose effort · Enter select".to_string());
            }
            ModelChoices::Failed(err) => {
                self.model_picker_pending = false;
                self.notice = Some(format!("{err} · focus Model and press t to retry"));
            }
        }
    }

    fn refresh_failed_model_catalog(&mut self) {
        let backend = self.selected_member().backend;
        let cwd = self.selected_member().resolved_cwd(&self.workspace);
        match self.model_catalog.refresh_failed(backend, &cwd) {
            Ok(()) => {
                self.model_picker_pending = true;
                self.notice = Some(format!(
                    "reloading {} model catalog… keep editing while it loads",
                    backend.as_str()
                ));
            }
            Err(message) => self.notice = Some(message),
        }
    }

    fn cycle_field(&mut self, field: Field) {
        match field {
            Field::Sandbox => {
                let next = cycle_sandbox(self.selected_member().sandbox);
                self.selected_member_mut().sandbox = next;
            }
            Field::Permission => {
                let next = cycle_permission_for_backend(
                    self.selected_member().backend,
                    self.selected_member().sandbox,
                    self.selected_member().permission_mode,
                );
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
                self.selected_member_mut().model =
                    if value.is_empty() || value.eq_ignore_ascii_case("default") {
                        None
                    } else {
                        Some(value.to_string())
                    };
            }
            Field::SessionId => {
                let session_id = if value.is_empty() || value.eq_ignore_ascii_case("default") {
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
    let model_picker_height = state
        .model_picker
        .as_ref()
        .map(|picker| {
            picker.window(8).1.len() as u16 + if picker.has_effort_controls() { 7 } else { 5 }
        })
        .unwrap_or(0);
    let backend_picker_height = state.backend_picker.as_ref().map_or(0, |_| 8);
    let picker_height = model_picker_height + backend_picker_height;
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
            "Customize members, backend CLIs, and model settings:",
            theme::muted(),
        )),
        Line::raw(""),
        Line::from(Span::styled(" Members", theme::accent_bold())),
    ];

    // Distribute available width across columns dynamically.
    // Layout: " ▶ name @handle backend role=… model=…"
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
                    theme::clip_width(
                        &state.model_catalog.model_label(member, &state.workspace),
                        model_w
                    )
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
    let label_width = Field::label_width_for_backend(selected.backend);
    lines.push(Line::from(vec![
        Span::styled("     handle: ", theme::muted()),
        Span::styled(format!("@{}", selected.id), theme::accent()),
        Span::styled(" (generated)", theme::muted()),
    ]));
    for (idx, field) in state.fields().iter().enumerate() {
        let selected_field = state.field_mode && idx == state.field;
        let style = if selected_field {
            theme::editor_field_focus()
        } else {
            theme::text()
        };
        lines.push(Line::from(Span::styled(
            format!(
                " {} {:>label_width$}: {}",
                if selected_field { "›" } else { " " },
                field.label_for_backend(selected.backend),
                state.field_value(selected, *field)
            ),
            style,
        )));
    }

    if let Some(picker) = &state.backend_picker {
        lines.push(Line::raw(""));
        lines.extend(backend_picker_lines(
            picker,
            &state.model_catalog,
            &state.selected_member().resolved_cwd(&state.workspace),
            avail,
        ));
    }

    if let Some(picker) = &state.model_picker {
        lines.push(Line::raw(""));
        lines.extend(model_picker_lines(picker, avail, 8));
    }

    lines.push(Line::raw(""));
    if let Some(notice) = &state.notice {
        lines.push(Line::from(Span::styled(notice.clone(), theme::warning())));
    }
    if state.editing.is_none() {
        lines.push(Line::from(Span::styled(
            if state.field_mode {
                "↑/↓ field · Enter edit/choose · e manual model · s start · Esc members"
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
        Field::Sandbox => match member.backend {
            BackendKind::Codex => member.sandbox.codex_arg().to_string(),
            BackendKind::Claude => "not passed".to_string(),
            BackendKind::Grok => member.sandbox.grok_arg().to_string(),
            BackendKind::Agy => {
                if member.sandbox == SandboxPolicy::DangerFullAccess {
                    "off".to_string()
                } else {
                    "on".to_string()
                }
            }
        },
        Field::Permission => permission_value(member),
        Field::Session => match member.session_policy {
            SessionPolicy::Resume => "resume".to_string(),
            SessionPolicy::Fresh => "fresh".to_string(),
        },
        Field::SessionId => match (&member.session_policy, &member.session_id) {
            (SessionPolicy::Resume, Some(session_id)) => session_id.clone(),
            (SessionPolicy::Resume, None) => "select a session".to_string(),
            (SessionPolicy::Fresh, _) => "not set (fresh)".to_string(),
        },
    }
}

pub(crate) fn cycle_sandbox(current: SandboxPolicy) -> SandboxPolicy {
    match current {
        SandboxPolicy::ReadOnly => SandboxPolicy::WorkspaceWrite,
        SandboxPolicy::WorkspaceWrite => SandboxPolicy::DangerFullAccess,
        SandboxPolicy::DangerFullAccess => SandboxPolicy::ReadOnly,
    }
}

fn permission_value(member: &TeamMember) -> String {
    match member.backend {
        // The stored values are an adapter compatibility layer. Render the
        // actual App Server policy instead of leaking Claude's names into the
        // Codex editor. `never` is Asterline's product default, rather than a
        // deferred local Codex setting.
        BackendKind::Codex => match member.permission_mode {
            None | Some(PermissionMode::Default) => "never".to_string(),
            Some(PermissionMode::AcceptEdits | PermissionMode::Plan) => "untrusted".to_string(),
            Some(PermissionMode::Auto) => "on-request".to_string(),
            Some(PermissionMode::DontAsk | PermissionMode::BypassPermissions) => {
                "never".to_string()
            }
        },
        BackendKind::Claude | BackendKind::Grok => member
            .permission_mode
            .map(|mode| mode.claude_arg().to_string())
            .unwrap_or_else(|| "default".to_string()),
        BackendKind::Agy => match member.permission_mode {
            None
            | Some(PermissionMode::Default | PermissionMode::Auto | PermissionMode::DontAsk) => {
                "CLI default".to_string()
            }
            Some(PermissionMode::AcceptEdits) => "accept-edits".to_string(),
            Some(PermissionMode::Plan) => "plan".to_string(),
            Some(PermissionMode::BypassPermissions)
                if member.sandbox == SandboxPolicy::DangerFullAccess =>
            {
                "dangerously-skip-permissions".to_string()
            }
            Some(PermissionMode::BypassPermissions) => "requires terminal sandbox off".to_string(),
        },
    }
}

pub(crate) fn cycle_permission_for_backend(
    backend: BackendKind,
    sandbox: SandboxPolicy,
    current: Option<PermissionMode>,
) -> Option<PermissionMode> {
    match backend {
        BackendKind::Codex => match current {
            None | Some(PermissionMode::Default) => Some(PermissionMode::AcceptEdits),
            Some(PermissionMode::AcceptEdits | PermissionMode::Plan) => Some(PermissionMode::Auto),
            Some(PermissionMode::Auto) => None,
            Some(PermissionMode::DontAsk | PermissionMode::BypassPermissions) => {
                Some(PermissionMode::AcceptEdits)
            }
        },
        BackendKind::Claude | BackendKind::Grok => match current {
            None | Some(PermissionMode::Default) => Some(PermissionMode::AcceptEdits),
            Some(PermissionMode::AcceptEdits) => Some(PermissionMode::Plan),
            Some(PermissionMode::Plan) => Some(PermissionMode::Auto),
            Some(PermissionMode::Auto) => Some(PermissionMode::DontAsk),
            Some(PermissionMode::DontAsk) => Some(PermissionMode::BypassPermissions),
            Some(PermissionMode::BypassPermissions) => None,
        },
        BackendKind::Agy => match current {
            None
            | Some(PermissionMode::Default | PermissionMode::Auto | PermissionMode::DontAsk) => {
                Some(PermissionMode::AcceptEdits)
            }
            Some(PermissionMode::AcceptEdits) => Some(PermissionMode::Plan),
            Some(PermissionMode::Plan) if sandbox == SandboxPolicy::DangerFullAccess => {
                Some(PermissionMode::BypassPermissions)
            }
            Some(PermissionMode::Plan) => None,
            Some(PermissionMode::BypassPermissions) => None,
        },
    }
}

pub(crate) fn normalize_member_id(value: &str, fallback: &str) -> String {
    normalize_domain_member_id(value, fallback)
}

pub(crate) fn unique_member_id(base: &str, members: &[TeamMember], skip: Option<usize>) -> String {
    let base = normalize_member_id(base, "member");
    let mut candidate = base.clone();
    let mut suffix = 2usize;
    while candidate.eq_ignore_ascii_case("all")
        || members
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
    while candidate.eq_ignore_ascii_case("all")
        || members.iter().enumerate().any(|(idx, member)| {
            Some(idx) != skip && member.display_name.eq_ignore_ascii_case(&candidate)
        })
    {
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
    fn permission_fields_render_and_cycle_by_backend_capability() {
        let mut codex = TeamMember::new("codex", "Codex", BackendKind::Codex, "impl");
        assert_eq!(
            Field::Permission.label_for_backend(codex.backend),
            "approval policy"
        );
        assert_eq!(field_value(&codex, Field::Permission), "never");
        codex.permission_mode = cycle_permission_for_backend(codex.backend, codex.sandbox, None);
        assert_eq!(field_value(&codex, Field::Permission), "untrusted");
        codex.permission_mode =
            cycle_permission_for_backend(codex.backend, codex.sandbox, codex.permission_mode);
        assert_eq!(field_value(&codex, Field::Permission), "on-request");

        let mut claude = TeamMember::new("claude", "Claude", BackendKind::Claude, "review");
        assert_eq!(
            Field::Permission.label_for_backend(claude.backend),
            "permission mode"
        );
        assert!(!Field::for_backend(BackendKind::Claude).contains(&Field::Sandbox));
        claude.permission_mode = cycle_permission_for_backend(claude.backend, claude.sandbox, None);
        assert_eq!(field_value(&claude, Field::Permission), "acceptEdits");

        let mut agy = TeamMember::new("agy", "Agy", BackendKind::Agy, "research");
        assert_eq!(
            Field::Permission.label_for_backend(agy.backend),
            "execution mode"
        );
        assert_eq!(field_value(&agy, Field::Sandbox), "on");
        assert_eq!(field_value(&agy, Field::Permission), "CLI default");
        agy.permission_mode = cycle_permission_for_backend(agy.backend, agy.sandbox, None);
        assert_eq!(field_value(&agy, Field::Permission), "accept-edits");
        agy.permission_mode =
            cycle_permission_for_backend(agy.backend, agy.sandbox, agy.permission_mode);
        assert_eq!(field_value(&agy, Field::Permission), "plan");
        assert_eq!(
            cycle_permission_for_backend(agy.backend, agy.sandbox, agy.permission_mode),
            None
        );
        agy.sandbox = SandboxPolicy::DangerFullAccess;
        assert_eq!(
            cycle_permission_for_backend(agy.backend, agy.sandbox, agy.permission_mode),
            Some(PermissionMode::BypassPermissions)
        );
    }

    #[test]
    fn catalog_shows_the_native_permission_when_member_has_no_override() {
        let cwd = Path::new("/tmp/ws");
        let mut catalog = ModelCatalog::default();
        catalog.seed_with_native_permission(BackendKind::Claude, cwd, "bypassPermissions");
        let member = TeamMember::new("claude", "Claude", BackendKind::Claude, "review");

        assert_eq!(
            catalog.native_permission_label(&member, cwd),
            Some("bypassPermissions".to_string())
        );
    }

    #[test]
    fn effort_is_configured_only_in_the_model_picker() {
        for backend in [
            BackendKind::Codex,
            BackendKind::Claude,
            BackendKind::Grok,
            BackendKind::Agy,
        ] {
            assert!(
                !Field::for_backend(backend)
                    .iter()
                    .any(|field| field.label() == "effort")
            );
        }
    }

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
    fn name_commit_avoids_reserved_all_target() {
        let available = [BackendKind::Codex];
        let mut state = BuilderState::new(PathBuf::from("/tmp/ws"), &available);

        state.commit_edit(EditState::new(Field::Name, "ALL".to_string()));

        assert_eq!(state.members[0].display_name, "ALL 2");
        assert_eq!(state.members[0].id, MemberId::new("all-2"));
        assert!(state.finish().expect("valid team").validate().is_ok());
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
        state.field = state
            .fields()
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
        state.handle_model_picker_key(KeyCode::Down, KeyModifiers::NONE);
        state.handle_model_picker_key(KeyCode::Down, KeyModifiers::NONE);
        state.handle_model_picker_key(KeyCode::Right, KeyModifiers::NONE);
        state.handle_model_picker_key(KeyCode::Right, KeyModifiers::NONE);
        state.handle_model_picker_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(state.members[0].model.as_deref(), Some("grok-4.5"));
        assert_eq!(state.members[0].effort, None);
    }

    #[test]
    fn grok_picker_hides_unreported_effort_controls() {
        let picker = ModelPicker::new(
            BackendKind::Grok,
            None,
            None,
            vec![DiscoveredModel::simple("grok-4.5")],
        );
        let rendered = model_picker_lines(&picker, 90, 8)
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert!(!picker.has_effort_controls());
        assert!(!rendered.contains("Effort"));
        assert!(!rendered.contains("←/→"));
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

        assert_eq!(one[0].id, "project-one");
        assert_eq!(two[0].id, "project-two");
    }

    #[test]
    fn completed_model_load_opens_requested_picker() {
        let mut state = BuilderState::new(PathBuf::from("/tmp/ws"), &[BackendKind::Codex]);
        state.field_mode = true;
        state.field = Field::ALL
            .iter()
            .position(|field| *field == Field::Model)
            .unwrap();
        state.model_picker_pending = true;
        state.model_catalog.seed(
            BackendKind::Codex,
            Path::new("/tmp/ws"),
            vec!["gpt-test".to_string()],
        );

        state.poll_agent_catalog();

        assert!(state.model_picker.is_some());
        assert!(!state.model_picker_pending);
    }

    #[test]
    fn model_picker_keeps_an_unverified_custom_model_effort() {
        let picker = ModelPicker::new(
            BackendKind::Claude,
            Some("company-model"),
            Some(Effort::High),
            vec![DiscoveredModel::simple("sonnet")],
        );

        assert_eq!(picker.value().as_deref(), Some("company-model"));
        assert_eq!(picker.selected(), 0);
        assert_eq!(picker.effort(), Some(Effort::High));
        assert!(!picker.has_unsupported_effort_override());
    }

    #[test]
    fn agy_effort_qualified_model_needs_no_redundant_effort_override() {
        let mut model = DiscoveredModel::simple("gemini-3.6-flash-high");
        model.default_effort = Some(Effort::High);
        model.supported_efforts = vec![Effort::High];
        let picker = ModelPicker::new(
            BackendKind::Agy,
            Some("gemini-3.6-flash-high"),
            None,
            vec![model],
        );
        let mut state = BuilderState::new(PathBuf::from("/tmp/ws"), &[BackendKind::Agy]);
        state.model_picker = Some(picker);

        state.handle_model_picker_key(KeyCode::Enter, KeyModifiers::NONE);

        assert_eq!(
            state.members[0].model.as_deref(),
            Some("gemini-3.6-flash-high")
        );
        // The selected Agy model name already carries this setting. Persisting
        // another `--effort high` override would make a catalog default look
        // user-selected and can become stale after a model switch.
        assert_eq!(state.members[0].effort, None);
    }

    #[test]
    fn model_picker_arrows_follow_increasing_effort_order() {
        let mut model = DiscoveredModel::simple("grok-4.6");
        model.default_effort = Some(Effort::High);
        // This is Grok's cache/menu order. The UI must still make right mean
        // "more", rather than walking that raw descending sequence.
        model.supported_efforts = vec![Effort::Xhigh, Effort::High, Effort::Medium, Effort::Low];
        let mut picker = ModelPicker::new(
            BackendKind::Grok,
            Some("grok-4.6"),
            Some(Effort::High),
            vec![model],
        );

        assert_eq!(
            picker.effort_choices_label(),
            "default · low · medium · high · xhigh"
        );
        picker.next_effort();
        assert_eq!(picker.effort(), Some(Effort::Xhigh));
        picker.previous_effort();
        picker.previous_effort();
        assert_eq!(picker.effort(), Some(Effort::Medium));
    }

    #[test]
    fn browsing_models_keeps_native_default_effort_inherited() {
        let mut medium = DiscoveredModel::simple("gpt-medium");
        medium.default_effort = Some(Effort::Medium);
        medium.supported_efforts = vec![Effort::Low, Effort::Medium, Effort::High];
        let mut low = DiscoveredModel::simple("gpt-low");
        low.default_effort = Some(Effort::Low);
        low.supported_efforts = vec![Effort::Low, Effort::Medium];
        let mut picker = ModelPicker::new(
            BackendKind::Codex,
            Some("gpt-medium"),
            None,
            vec![medium, low],
        );

        assert_eq!(picker.effort(), None);
        assert_eq!(picker.effort_label(), "default medium");

        picker.down();
        assert_eq!(picker.effort(), None);
        assert_eq!(picker.effort_label(), "default low");

        picker.up();
        assert_eq!(picker.effort(), None);
        assert_eq!(picker.effort_label(), "default medium");
    }

    #[test]
    fn effort_arrows_are_relative_to_the_native_default() {
        let mut model = DiscoveredModel::simple("gpt-medium");
        model.default_effort = Some(Effort::Medium);
        model.supported_efforts = vec![Effort::Low, Effort::Medium, Effort::High];
        let mut picker =
            ModelPicker::new(BackendKind::Codex, Some("gpt-medium"), None, vec![model]);

        assert_eq!(picker.effort(), None);
        assert_eq!(picker.effort_label(), "default medium");

        picker.next_effort();
        assert_eq!(picker.effort(), Some(Effort::High));

        let mut picker = ModelPicker::new(
            BackendKind::Codex,
            Some("gpt-medium"),
            None,
            vec![DiscoveredModel {
                id: "gpt-medium".to_string(),
                name: "gpt-medium".to_string(),
                description: None,
                default_effort: Some(Effort::Medium),
                supported_efforts: vec![Effort::Low, Effort::Medium, Effort::High],
                is_default: false,
            }],
        );
        picker.previous_effort();
        assert_eq!(picker.effort(), Some(Effort::Low));
    }

    #[test]
    fn browsing_a_different_model_never_discards_the_original_effort_override() {
        let mut spark = DiscoveredModel::simple("gpt-5.3-codex-spark");
        spark.default_effort = Some(Effort::High);
        spark.supported_efforts = vec![Effort::Medium, Effort::High];
        let mut sol = DiscoveredModel::simple("gpt-5.6-sol");
        sol.default_effort = Some(Effort::Low);
        sol.supported_efforts = vec![Effort::Low];
        let mut picker = ModelPicker::new(
            BackendKind::Codex,
            Some("gpt-5.3-codex-spark"),
            Some(Effort::High),
            vec![spark, sol],
        );

        assert_eq!(picker.effort(), Some(Effort::High));
        assert!(!picker.has_unsupported_effort_override());
        assert_eq!(picker.effort_label(), "high (override)");

        picker.down();
        assert_eq!(picker.effort(), Some(Effort::High));
        assert!(picker.has_unsupported_effort_override());
        assert_eq!(picker.effort_label(), "high (unsupported)");
        assert!(
            picker
                .unsupported_effort_notice()
                .is_some_and(|notice| notice.contains("gpt-5.6-sol"))
        );

        picker.up();
        assert_eq!(picker.effort(), Some(Effort::High));
        assert!(!picker.has_unsupported_effort_override());
        assert_eq!(picker.effort_label(), "high (override)");
    }

    #[test]
    fn model_picker_requires_an_explicit_effort_choice_before_applying_an_incompatible_model() {
        let mut spark = DiscoveredModel::simple("gpt-5.3-codex-spark");
        spark.supported_efforts = vec![Effort::High];
        let mut sol = DiscoveredModel::simple("gpt-5.6-sol");
        sol.supported_efforts = vec![Effort::Low];
        let mut state = BuilderState::new(PathBuf::from("/tmp/ws"), &[BackendKind::Codex]);
        state.model_picker = Some(ModelPicker::new(
            BackendKind::Codex,
            Some("gpt-5.3-codex-spark"),
            Some(Effort::High),
            vec![spark, sol],
        ));
        state.model_picker.as_mut().unwrap().down();

        state.handle_model_picker_key(KeyCode::Enter, KeyModifiers::NONE);

        assert!(state.model_picker.is_some());
        assert_eq!(state.members[0].model, None);
        assert_eq!(state.members[0].effort, None);
        assert!(
            state
                .notice
                .as_deref()
                .is_some_and(|notice| notice.contains("does not advertise high"))
        );
    }

    #[test]
    fn backend_picker_shows_only_reported_model_capabilities() {
        let detected = DetectedBackends {
            codex: true,
            claude: false,
            grok: false,
            agy: false,
        };
        let picker = BackendPicker::new(BackendKind::Codex, detected);
        let mut catalog = ModelCatalog::default();
        catalog.seed(
            BackendKind::Codex,
            Path::new("/tmp/ws"),
            vec!["gpt-test".to_string()],
        );

        let text = backend_picker_lines(&picker, &catalog, Path::new("/tmp/ws"), 120)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("codex"));
        assert!(text.contains("installed · 1 model(s): gpt-test"));
        assert!(!text.contains("effort "));
        assert!(text.contains("claude"));
        assert!(text.contains("not installed on PATH"));
    }

    #[test]
    fn model_picker_filters_names_ids_and_descriptions() {
        let mut sol = DiscoveredModel::simple("gpt-5.6-sol");
        sol.name = "GPT-5.6-Sol".to_string();
        sol.description = Some("Frontier coding model".to_string());
        sol.is_default = true;
        let mut picker = ModelPicker::new(
            BackendKind::Codex,
            None,
            None,
            vec![sol, DiscoveredModel::simple("gpt-5.4-mini")],
        );

        for ch in "frontier".chars() {
            picker.push_query(ch);
        }
        assert_eq!(picker.visible_len(), 1);
        assert_eq!(picker.value().as_deref(), Some("gpt-5.6-sol"));
        picker.clear_query();
        for ch in "mini".chars() {
            picker.push_query(ch);
        }
        assert_eq!(picker.visible_len(), 1);
        assert_eq!(picker.value().as_deref(), Some("gpt-5.4-mini"));
    }

    #[test]
    fn catalog_shows_detected_model_without_default_prefix() {
        let mut model = DiscoveredModel::simple("gpt-5.6-sol");
        model.name = "GPT-5.6-Sol".to_string();
        model.default_effort = Some(Effort::Medium);
        model.supported_efforts = vec![Effort::Low, Effort::Medium, Effort::High];
        model.is_default = true;
        let mut catalog = ModelCatalog::default();
        catalog.loads.insert(
            (BackendKind::Codex, PathBuf::from("/tmp/ws")),
            ModelLoad::Ready(Ok(DiscoveredCatalog {
                models: vec![model],
                native_permission: None,
            })),
        );
        let member = TeamMember::new("builder", "Builder", BackendKind::Codex, "impl");

        assert_eq!(
            catalog.model_label(&member, Path::new("/tmp/ws")),
            "GPT-5.6-Sol · medium"
        );
    }

    #[test]
    fn model_label_shows_the_effective_effort_except_for_agy() {
        let cwd = Path::new("/tmp/ws");
        let mut catalog = ModelCatalog::default();
        catalog.seed(BackendKind::Grok, cwd, vec!["grok-4.6".to_string()]);
        let mut grok = TeamMember::new("grok", "Grok", BackendKind::Grok, "review");
        grok.model = Some("grok-4.6".to_string());
        grok.effort = Some(Effort::Xhigh);
        assert_eq!(catalog.model_label(&grok, cwd), "grok-4.6 · xhigh");

        catalog.seed(
            BackendKind::Agy,
            cwd,
            vec!["gemini-3.6-flash-high".to_string()],
        );
        let mut agy = TeamMember::new("agy", "Agy", BackendKind::Agy, "research");
        agy.model = Some("gemini-3.6-flash-high".to_string());
        assert_eq!(catalog.model_label(&agy, cwd), "gemini-3.6-flash-high");
    }

    #[test]
    fn catalog_and_picker_show_the_detected_default_model() {
        let mut catalog = ModelCatalog::default();
        catalog.seed(
            BackendKind::Claude,
            Path::new("/tmp/ws"),
            vec![
                "claude-sonnet-4-6".to_string(),
                "claude-opus-4-6".to_string(),
            ],
        );
        let member = TeamMember::new("builder", "Builder", BackendKind::Claude, "impl");
        let ModelChoices::Ready(models) = catalog.models(BackendKind::Claude, Path::new("/tmp/ws"))
        else {
            panic!("expected detected models");
        };
        let picker = ModelPicker::new(BackendKind::Claude, None, None, models);

        assert_eq!(
            catalog.model_label(&member, Path::new("/tmp/ws")),
            "claude-sonnet-4-6"
        );
        assert_eq!(picker.visible_len(), 2);
        assert_eq!(picker.value().as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(
            picker.selected_choice().map(ModelChoice::name),
            Some("claude-sonnet-4-6")
        );
    }

    #[test]
    fn loaded_catalog_without_a_reported_default_is_not_labeled_not_loaded() {
        let mut catalog = ModelCatalog::default();
        catalog.loads.insert(
            (BackendKind::Agy, PathBuf::from("/tmp/ws")),
            ModelLoad::Ready(Ok(DiscoveredCatalog {
                models: vec![DiscoveredModel::simple("gemini-3.6-pro")],
                native_permission: None,
            })),
        );
        let member = TeamMember::new("planner", "Planner", BackendKind::Agy, "plan");

        assert_eq!(
            catalog.model_label(&member, Path::new("/tmp/ws")),
            "CLI default"
        );
        let ModelChoices::Ready(models) = catalog.models(BackendKind::Agy, Path::new("/tmp/ws"))
        else {
            panic!("expected the successful catalog to stay available to the picker");
        };
        assert_eq!(models[0].id, "gemini-3.6-pro");
    }

    #[test]
    fn model_picker_keeps_cli_default_only_when_no_models_are_discovered() {
        let picker = ModelPicker::new(BackendKind::Claude, None, None, Vec::new());

        assert_eq!(picker.visible_len(), 1);
        assert_eq!(picker.value(), None);
        assert_eq!(
            picker.selected_choice().map(ModelChoice::name),
            Some("default")
        );
    }

    #[test]
    fn catalog_shows_loading_instead_of_default_while_discovery_runs() {
        let (_tx, rx) = mpsc::channel();
        let mut catalog = ModelCatalog::default();
        catalog.loads.insert(
            (BackendKind::Codex, PathBuf::from("/tmp/ws")),
            ModelLoad::Loading(rx),
        );
        let member = TeamMember::new("builder", "Builder", BackendKind::Codex, "impl");

        assert_eq!(
            catalog.model_label(&member, Path::new("/tmp/ws")),
            "loading…"
        );
    }

    #[test]
    fn catalog_marks_failed_default_model_as_unavailable() {
        let mut catalog = ModelCatalog::default();
        catalog.loads.insert(
            (BackendKind::Grok, PathBuf::from("/tmp/ws")),
            ModelLoad::Ready(Err("network unavailable".to_string())),
        );
        let member = TeamMember::new("builder", "Builder", BackendKind::Grok, "impl");

        assert_eq!(
            catalog.model_label(&member, Path::new("/tmp/ws")),
            "unavailable"
        );
        assert!(
            catalog
                .backend_summary(BackendKind::Grok, Path::new("/tmp/ws"))
                .contains("model discovery failed")
        );
    }

    #[test]
    fn frozen_catalog_never_starts_a_late_lookup() {
        let mut catalog = ModelCatalog::default();
        let cwd = Path::new("/tmp/new-member-workspace");
        catalog.freeze();

        let ModelChoices::Failed(message) = catalog.models(BackendKind::Agy, cwd) else {
            panic!("a frozen catalog must not start a new lookup");
        };
        assert!(message.contains("restart"));
        assert!(!catalog.contains(BackendKind::Agy, cwd));

        let member = TeamMember::new("planner", "Planner", BackendKind::Agy, "plan");
        assert_eq!(
            catalog.model_label(&member, cwd),
            "not preloaded at startup"
        );
    }

    #[test]
    fn session_id_field_shows_a_real_resume_id_or_an_honest_unbound_state() {
        let mut member = TeamMember::new("builder", "Builder", BackendKind::Codex, "impl");

        assert_eq!(field_value(&member, Field::Model), "default");
        assert_eq!(field_value(&member, Field::SessionId), "select a session");

        member.session_id = Some("thread-abc123".to_string());
        assert_eq!(field_value(&member, Field::SessionId), "thread-abc123");

        member.session_policy = SessionPolicy::Fresh;
        assert_eq!(field_value(&member, Field::SessionId), "not set (fresh)");
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
        let mut edit = EditState::new(Field::SessionId, "/very/long/session-id".to_string());
        edit.apply_key(KeyCode::Home, KeyModifiers::NONE);
        edit.apply_key(KeyCode::Right, KeyModifiers::NONE);
        let (visible, cursor) = edit.visible_window(8);
        assert!(theme::display_width(&visible) <= 8);
        assert_eq!(cursor, 1);
    }

    #[test]
    fn edit_state_moves_and_deletes_complete_graphemes() {
        let mut combining = EditState::new(Field::Role, "e\u{301}".to_string());
        combining.apply_key(KeyCode::Left, KeyModifiers::NONE);
        assert_eq!(combining.cursor, 0);
        combining.apply_key(KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(combining.cursor, 2);
        combining.apply_key(KeyCode::Backspace, KeyModifiers::NONE);
        assert!(combining.buffer.is_empty());

        let mut family = EditState::new(Field::Role, "👨‍👩‍👧‍👦".to_string());
        family.apply_key(KeyCode::Home, KeyModifiers::NONE);
        family.apply_key(KeyCode::Delete, KeyModifiers::NONE);
        assert!(family.buffer.is_empty());
    }
}
