//! Run presentation: the `/runs` drawer content, the footer hint for the
//! active run, and the pure helpers that summarize runs, steps, owners, and
//! timelines. Pure `RunSummary -> Line` logic; no layout code.

use std::collections::BTreeMap;

use ratatui::style::Color;
use ratatui::text::{Line, Span};

use crate::domain::event::{
    ModeRunStatus, RunEventSummary, RunStatus, RunStepStatus, RunStepSummary, RunSummary,
};
use crate::domain::mode::{CollabMode, TerminalMode};
use crate::tui::app_state::AppState;
use crate::tui::theme;
use crate::tui::theme::{pad_width, run_status_color, truncate_width};

/// One-line hint about the latest run, shown in the footer when idle.
pub(crate) fn run_footer_hint(state: &AppState) -> Option<(String, Color)> {
    if !state.runtime_available() {
        return None;
    }
    let run = state.latest_run()?;
    if let Some(mode) = &run.mode
        && state.active_mode() != terminal_mode(mode.mode)
    {
        return None;
    }
    if is_completed_brainstorm(run) {
        return Some((
            format!(
                "◇ brainstorm {} · ranked result ready · new topic or /mode normal · /runs details",
                run.id
            ),
            theme::success_color(),
        ));
    }

    let mode_head = mode_run_head(run);
    let progress = if run.mode.is_some() {
        mode_progress_suffix(run)
    } else {
        run_step_progress_suffix(run)
    };
    match run.status {
        RunStatus::Running => Some((
            if let Some(head) = mode_head {
                format!("{head}{progress} · /runs details · Esc cancel")
            } else {
                format!(
                    "● {} running{progress} · /runs details · Esc cancel",
                    run.id
                )
            },
            theme::warning_color(),
        )),
        RunStatus::Verifying => Some((
            if let Some(head) = mode_head {
                format!("{head}{progress} · /runs details · Esc cancel")
            } else {
                format!(
                    "⏳ {} verifying{progress} · /runs details · Esc cancel",
                    run.id
                )
            },
            theme::warning_color(),
        )),
        RunStatus::Done if run.verification.is_none() => Some((
            if let Some(head) = mode_head {
                format!("{head}{progress} · /runs details")
            } else {
                format!("● {} done{progress} · /runs details", run.id)
            },
            theme::success_color(),
        )),
        RunStatus::Failed => Some((
            if let Some(head) = mode_head {
                format!("{head}{progress} · /runs details · /continue to fix")
            } else {
                format!(
                    "● {} failed{progress} · /runs details · /continue to fix",
                    run.id
                )
            },
            theme::error_color(),
        )),
        RunStatus::Blocked => Some((
            if let Some(head) = mode_head {
                format!("{head}{progress} · /runs details · /continue when resolved")
            } else {
                format!(
                    "● {} blocked{progress} · /runs details · /continue when resolved",
                    run.id
                )
            },
            theme::error_color(),
        )),
        _ => None,
    }
}

fn terminal_mode(mode: CollabMode) -> TerminalMode {
    match mode {
        CollabMode::Review => TerminalMode::Review,
        CollabMode::Plan => TerminalMode::Plan,
        CollabMode::Brainstorm => TerminalMode::Brainstorm,
        CollabMode::Team => TerminalMode::Team,
    }
}

/// `◇ {mode} {run.id}` when the run is a collaboration mode.
fn mode_run_head(run: &RunSummary) -> Option<String> {
    if let Some(mode) = &run.mode {
        return Some(format!("◇ {} {}", mode.mode.as_str(), run.id));
    }
    if run.legacy_mode.is_some() {
        return Some(format!("◇ legacy {}", run.id));
    }
    None
}

fn is_completed_brainstorm(run: &RunSummary) -> bool {
    run.status == RunStatus::Done
        && run
            .mode
            .as_ref()
            .is_some_and(|mode| mode.mode == CollabMode::Brainstorm)
}

/// Phase / iteration progress for mode runs.
fn mode_progress_suffix(run: &RunSummary) -> String {
    let Some(mode) = &run.mode else {
        return String::new();
    };
    let phase = mode.state.phase.as_str();
    match mode.mode {
        CollabMode::Review | CollabMode::Plan => {
            format!(
                " · iter {}/{} · {phase}",
                mode.state.iteration, mode.state.max_iterations
            )
        }
        CollabMode::Brainstorm => {
            let phase = brainstorm_phase_label(mode);
            format!(
                " · round {}/{} · {phase} · {} idea cards",
                mode.state.round, mode.state.rounds, mode.state.idea_count
            )
        }
        CollabMode::Team => format!(" · {phase}"),
    }
}

/// Detail line under goal/status for mode runs in the `/runs` drawer.
fn mode_detail_line(run: &RunSummary, mode: &ModeRunStatus) -> Line<'static> {
    let progress = match mode.mode {
        CollabMode::Review | CollabMode::Plan => format!(
            "iter {}/{}",
            mode.state.iteration, mode.state.max_iterations
        ),
        CollabMode::Brainstorm => format!(
            "round {}/{} · {} idea cards · {} ballots",
            mode.state.round, mode.state.rounds, mode.state.idea_count, mode.state.vote_count
        ),
        CollabMode::Team => "coordinator-driven".to_string(),
    };
    let phase = if is_completed_brainstorm(run) {
        "ranked result ready"
    } else if mode.mode == CollabMode::Brainstorm {
        brainstorm_phase_label(mode)
    } else {
        mode.state.phase.as_str()
    };
    Line::from(vec![
        Span::styled(" Mode: ", theme::muted()),
        Span::styled(
            format!("{} · phase: {} · {progress}", mode.mode.as_str(), phase),
            theme::accent_bold(),
        ),
    ])
}

fn brainstorm_phase_label(mode: &ModeRunStatus) -> &'static str {
    if mode.state.phase != "diverging" {
        return match mode.state.phase.as_str() {
            "voting" => "private voting",
            "synthesizing" => "ranked synthesis",
            "done" => "ranked result ready",
            _ => "generating",
        };
    }
    if mode.state.round <= 1 {
        "blind seed"
    } else if mode.state.round >= mode.state.rounds {
        "stretch"
    } else {
        "cross-pollinate"
    }
}

/// The `/runs` drawer body. Compact mode shows what you act on (selected run,
/// goal, progress, action, steps, history table); `x` expands the rest
/// (owner, times, owners workload, outcome, stages, timeline).
pub(crate) fn drawer_runs(state: &AppState, width: usize) -> Vec<Line<'static>> {
    let runs = state.runs();
    let detail = state.runs_detail();
    if runs.is_empty() {
        return vec![Line::styled(
            "no runs yet — select /mode plan, then send a goal",
            theme::muted(),
        )];
    }

    let mut lines = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(" History: ", theme::muted()),
        Span::styled(run_history_summary(runs), theme::text()),
        Span::styled(" · View: ", theme::muted()),
        Span::styled(
            if detail { "details" } else { "compact" },
            if detail {
                theme::accent_bold()
            } else {
                theme::bold(theme::text_color())
            },
        ),
    ]));
    if let Some(selected) = state.selected_run() {
        let surfaced = run_should_surface_outcome(selected);
        lines.push(Line::raw(""));
        let latest = runs.last();
        lines.push(Line::from(vec![
            Span::styled(format!(" Selected: {} ", selected.id), theme::accent_bold()),
            Span::styled(
                selected.status.as_str(),
                ratatui::style::Style::default().fg(run_status_color(selected.status)),
            ),
            latest
                .filter(|latest| latest.id != selected.id)
                .map(|latest| Span::styled(format!(" · latest {}", latest.id), theme::muted()))
                .unwrap_or_else(|| Span::raw("")),
        ]));
        lines.push(Line::from(vec![
            Span::styled(" Goal: ", theme::muted()),
            Span::styled(selected.goal.clone(), theme::emphasis()),
        ]));
        if let Some(mode) = &selected.mode {
            lines.push(mode_detail_line(selected, mode));
        }
        if detail {
            let owner = selected
                .coordinator
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "-".to_string());
            lines.push(Line::from(vec![
                Span::styled(" Owner: ", theme::muted()),
                Span::styled(owner, theme::text()),
                Span::styled(" · Attempt: ", theme::muted()),
                Span::styled(format!("#{}", selected.attempt), theme::warning_bold()),
            ]));
            lines.push(Line::from(vec![
                Span::styled(" Time: ", theme::muted()),
                Span::styled("created ", theme::muted()),
                Span::styled(run_time(&selected.created_at), theme::text()),
                Span::styled(" · updated ", theme::muted()),
                Span::styled(run_time(&selected.updated_at), theme::text()),
            ]));
        } else if surfaced {
            lines.push(Line::from(vec![
                Span::styled(" Attempt: ", theme::muted()),
                Span::styled(format!("#{}", selected.attempt), theme::warning_bold()),
            ]));
        }
        if let Some((progress, color)) = run_step_progress(selected) {
            lines.push(Line::from(vec![
                Span::styled(" Progress: ", theme::muted()),
                Span::styled(progress, theme::bold(color)),
            ]));
        }
        if detail && let Some((owners, color)) = run_owner_summary(selected) {
            lines.push(Line::from(vec![
                Span::styled(" Owners: ", theme::muted()),
                Span::styled(owners, theme::bold(color)),
            ]));
        }
        if detail || surfaced {
            let (outcome, outcome_color) = run_outcome(selected);
            lines.push(Line::from(vec![
                Span::styled(" Outcome: ", theme::muted()),
                Span::styled(outcome, theme::bold(outcome_color)),
            ]));
            lines.push(Line::from(vec![
                Span::styled(" Next: ", theme::muted()),
                Span::styled(run_next_action(selected), theme::text()),
            ]));
        }
        if !is_completed_brainstorm(selected)
            && let Some(action) = state.selected_run_stage_command()
        {
            lines.push(Line::from(vec![
                Span::styled(" Action: ", theme::muted()),
                Span::styled(action, theme::accent_bold()),
            ]));
        }
        if let Some(dispatch) = state.selected_run_dispatch_command() {
            lines.push(Line::from(vec![
                Span::styled(" Dispatch: ", theme::muted()),
                Span::styled(
                    truncate_width(&dispatch, width.saturating_sub(12).max(20)),
                    theme::accent_bold(),
                ),
            ]));
        }
        if detail && !is_completed_brainstorm(selected) {
            lines.push(run_stage_line(selected));
        }
        lines.extend(run_step_lines(selected, state.selected_run_step(), width));
        if detail || surfaced {
            lines.extend(run_timeline_lines(selected));
        }
        lines.push(Line::raw(""));
    }

    lines.push(runs_table_header());
    lines.push(runs_table_rule());

    let selected_id = state.selected_run().map(|run| run.id);
    for run in runs.iter().rev().take(50) {
        lines.extend(drawer_run(run, selected_id == Some(run.id), detail, width));
    }
    lines
}

/// Column widths of the runs history table (marker/run, status, try, steps,
/// updated, owner; the goal column takes the rest).
const RUNS_COLUMNS: [usize; 6] = [8, 9, 4, 10, 12, 9];

fn runs_table_header() -> Line<'static> {
    let cells = ["   Run", "Status", "Try", "Steps", "Updated", "Owner"];
    let mut text = String::new();
    for (cell, width) in cells.iter().zip(RUNS_COLUMNS) {
        text.push_str(&pad_width(cell, width));
        text.push_str("│ ");
    }
    text.push_str("Goal");
    Line::from(Span::styled(text, theme::accent_bold()))
}

fn runs_table_rule() -> Line<'static> {
    let mut text = String::new();
    for width in RUNS_COLUMNS {
        text.push_str(&"─".repeat(width));
        text.push_str("┼─");
    }
    text.push_str("─".repeat(6).as_str());
    Line::from(Span::styled(text, theme::muted()))
}

fn drawer_run(run: &RunSummary, selected: bool, detail: bool, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let owner = run
        .coordinator
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_else(|| "-".to_string());
    let updated = run_time(&run.updated_at);
    let marker = if selected { "›" } else { " " };
    let row_style = if selected {
        theme::selection()
    } else {
        ratatui::style::Style::default()
    };
    let status_style = if selected {
        row_style
    } else {
        ratatui::style::Style::default().fg(run_status_color(run.status))
    };
    let (steps, steps_color) = run_step_table_cell(run);
    let cell = |text: &str, width: usize, color: Color| {
        Span::styled(
            pad_width(text, width),
            row_style.fg(if selected { Color::Black } else { color }),
        )
    };
    let sep = Span::styled("│ ", row_style.fg(theme::muted_color()));
    lines.push(Line::from(vec![
        cell(
            &format!(" {marker} {}", run.id),
            RUNS_COLUMNS[0],
            theme::accent_color(),
        ),
        sep.clone(),
        Span::styled(
            pad_width(run.status.as_str(), RUNS_COLUMNS[1]),
            status_style,
        ),
        sep.clone(),
        cell(
            &format!("#{}", run.attempt),
            RUNS_COLUMNS[2],
            theme::warning_color(),
        ),
        sep.clone(),
        cell(&steps, RUNS_COLUMNS[3], steps_color),
        sep.clone(),
        cell(&updated, RUNS_COLUMNS[4], theme::text_color()),
        sep.clone(),
        cell(&owner, RUNS_COLUMNS[5], theme::text_color()),
        sep,
        Span::styled(
            truncate_width(&run.goal, width.saturating_sub(67).max(10)),
            row_style.fg(if selected {
                Color::Black
            } else {
                theme::emphasis_color()
            }),
        ),
    ]));
    if selected || !detail {
        return lines;
    }
    let (outcome, outcome_color) = run_outcome(run);
    lines.push(Line::styled(
        format!("   └─ outcome: {outcome}"),
        ratatui::style::Style::default().fg(outcome_color),
    ));
    if let Some(verification) = &run.verification {
        lines.push(Line::styled(
            format!("      check: {}", verification.command),
            theme::muted(),
        ));
        for line in verification.summary.lines().take(3) {
            lines.push(Line::styled(format!("      {line}"), theme::muted()));
        }
    }
    lines
}

fn run_step_lines(
    run: &RunSummary,
    selected_step: Option<u32>,
    width: usize,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if run.steps.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(" Steps: ", theme::muted()),
            Span::styled(
                format!("/step add {} [@owner] <next step>", run.id),
                theme::muted(),
            ),
        ]));
        return lines;
    }

    lines.push(Line::from(vec![Span::styled(" Steps:", theme::muted())]));
    for step in run.steps.iter().take(8) {
        lines.push(run_step_line(
            step,
            selected_step == Some(step.number),
            width,
        ));
        if let Some(note) = &step.note
            && !note.trim().is_empty()
        {
            lines.push(Line::styled(
                format!(
                    "     {}",
                    truncate_width(note.trim(), width.saturating_sub(5).max(20))
                ),
                theme::muted(),
            ));
        }
    }
    lines
}

fn run_step_line(step: &RunStepSummary, selected: bool, width: usize) -> Line<'static> {
    let (marker, color) = run_step_marker(step.status);
    let row_style = if selected {
        theme::selection()
    } else {
        ratatui::style::Style::default().fg(color)
    };
    let prefix_style = if selected { row_style } else { theme::muted() };
    let marker_style = if selected {
        row_style
    } else {
        theme::bold(color)
    };
    let mut spans = vec![
        Span::styled(
            format!(
                "   {}{:>2}. ",
                if selected { "›" } else { " " },
                step.number
            ),
            prefix_style,
        ),
        Span::styled(format!("{marker} "), marker_style),
    ];
    if let Some(owner) = &step.owner {
        spans.push(Span::styled(format!("@{owner} "), row_style));
    }
    spans.push(Span::styled(
        truncate_width(&step.title, width.saturating_sub(10).max(20)),
        row_style,
    ));
    Line::from(spans)
}

fn run_step_marker(status: RunStepStatus) -> (&'static str, Color) {
    match status {
        RunStepStatus::Todo => ("○", theme::muted_color()),
        RunStepStatus::Doing => ("●", theme::warning_color()),
        RunStepStatus::Done => ("✓", theme::success_color()),
        RunStepStatus::Blocked => ("■", theme::error_color()),
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct RunStepStats {
    total: usize,
    done: usize,
    doing: usize,
    blocked: usize,
    todo: usize,
}

fn run_step_stats(run: &RunSummary) -> Option<RunStepStats> {
    if run.steps.is_empty() {
        return None;
    }
    let mut stats = RunStepStats {
        total: run.steps.len(),
        ..RunStepStats::default()
    };
    for step in &run.steps {
        match step.status {
            RunStepStatus::Todo => stats.todo += 1,
            RunStepStatus::Doing => stats.doing += 1,
            RunStepStatus::Done => stats.done += 1,
            RunStepStatus::Blocked => stats.blocked += 1,
        }
    }
    Some(stats)
}

pub(crate) fn run_step_progress(run: &RunSummary) -> Option<(String, Color)> {
    let stats = run_step_stats(run)?;
    let mut parts = vec![format!("{}/{} done", stats.done, stats.total)];
    if stats.doing > 0 {
        parts.push(format!("{} doing", stats.doing));
    }
    if stats.blocked > 0 {
        parts.push(format!("{} blocked", stats.blocked));
    }
    let color = if stats.blocked > 0 {
        theme::error_color()
    } else if stats.doing > 0 {
        theme::warning_color()
    } else if stats.done == stats.total {
        theme::success_color()
    } else {
        theme::text_color()
    };
    Some((parts.join(" · "), color))
}

fn run_step_progress_suffix(run: &RunSummary) -> String {
    run_step_progress(run)
        .map(|(progress, _)| format!(" · {progress}"))
        .unwrap_or_default()
}

pub(crate) fn run_step_table_cell(run: &RunSummary) -> (String, Color) {
    let Some(stats) = run_step_stats(run) else {
        return ("-".to_string(), theme::muted_color());
    };
    if stats.blocked > 0 {
        (
            format!("{}/{} block", stats.done, stats.total),
            theme::error_color(),
        )
    } else if stats.doing > 0 {
        (
            format!("{}/{} doing", stats.done, stats.total),
            theme::warning_color(),
        )
    } else if stats.done == stats.total {
        (
            format!("{}/{} done", stats.done, stats.total),
            theme::success_color(),
        )
    } else {
        (
            format!("{}/{} todo", stats.done, stats.total),
            theme::text_color(),
        )
    }
}

fn run_step_focus(run: &RunSummary) -> Option<String> {
    let focus = |status: RunStepStatus, label: &'static str| {
        run.steps
            .iter()
            .find(|step| step.status == status)
            .map(|step| {
                format!(
                    "{label} step #{}{}: {}",
                    step.number,
                    run_step_owner_suffix(step),
                    truncate_width(&step.title, 44)
                )
            })
    };
    focus(RunStepStatus::Blocked, "blocked")
        .or_else(|| focus(RunStepStatus::Doing, "current"))
        .or_else(|| focus(RunStepStatus::Todo, "next"))
        .or_else(|| (!run.steps.is_empty()).then(|| "all checklist steps are done".to_string()))
}

fn run_step_owner_suffix(step: &RunStepSummary) -> String {
    step.owner
        .as_ref()
        .map(|owner| format!(" @{owner}"))
        .unwrap_or_default()
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct RunOwnerStats {
    total: usize,
    active: usize,
    blocked: usize,
}

pub(crate) fn run_owner_summary(run: &RunSummary) -> Option<(String, Color)> {
    if run.steps.len() < 2 {
        return None;
    }

    let mut owners: BTreeMap<String, RunOwnerStats> = BTreeMap::new();
    for step in &run.steps {
        let key = step
            .owner
            .as_ref()
            .map(|owner| format!("@{owner}"))
            .unwrap_or_else(|| "unassigned".to_string());
        let stats = owners.entry(key).or_default();
        stats.total += 1;
        if matches!(
            step.status,
            RunStepStatus::Todo | RunStepStatus::Doing | RunStepStatus::Blocked
        ) {
            stats.active += 1;
        }
        if step.status == RunStepStatus::Blocked {
            stats.blocked += 1;
        }
    }

    let mut parts = Vec::new();
    for (owner, stats) in owners {
        let mut label = format!("{owner} {}/{}", stats.active, stats.total);
        if stats.blocked > 0 {
            label.push_str(&format!(" {} blocked", stats.blocked));
        } else if stats.active > 0 {
            label.push_str(" active");
        } else {
            label.push_str(" done");
        }
        parts.push(label);
    }

    let color = if parts.iter().any(|part| part.contains("blocked")) {
        theme::error_color()
    } else if parts.iter().any(|part| part.contains("active")) {
        theme::warning_color()
    } else {
        theme::success_color()
    };
    Some((parts.join(" · "), color))
}

fn run_timeline_lines(run: &RunSummary) -> Vec<Line<'static>> {
    if run.events.is_empty() {
        return Vec::new();
    }

    let mut lines = Vec::new();
    lines.push(Line::from(vec![Span::styled(" Timeline:", theme::muted())]));
    for event in run.events.iter().rev().take(6).rev() {
        lines.push(run_event_line(event));
        if let Some(detail) = &event.detail {
            for line in detail
                .lines()
                .filter(|line| !line.trim().is_empty())
                .skip(1)
                .take(1)
            {
                lines.push(Line::styled(
                    format!("     {}", truncate_width(line.trim(), 64)),
                    theme::muted(),
                ));
            }
        }
    }
    lines
}

fn run_event_line(event: &RunEventSummary) -> Line<'static> {
    let color = run_event_color(event.kind.as_str(), event.title.as_str());
    let title = event
        .detail
        .as_ref()
        .and_then(|detail| detail.lines().find(|line| !line.trim().is_empty()))
        .map(|detail| format!("{} · {}", event.title, truncate_width(detail.trim(), 42)))
        .unwrap_or_else(|| event.title.clone());
    Line::from(vec![
        Span::styled(
            format!("   {} ", run_time(&event.created_at)),
            theme::muted(),
        ),
        Span::styled(format!("#{} ", event.attempt), theme::warning_bold()),
        Span::styled(title, ratatui::style::Style::default().fg(color)),
    ])
}

fn run_event_color(kind: &str, title: &str) -> Color {
    match kind {
        "started" | "continued" | "running" => theme::accent_color(),
        "note" => theme::emphasis_color(),
        "step_added" | "step_updated" | "step_renamed" | "step_removed" | "step_assigned" => {
            theme::accent_color()
        }
        "verifying" => theme::warning_color(),
        "done" | "verification_passed" => theme::success_color(),
        "failed" | "verification_failed" => theme::error_color(),
        "blocked" => theme::error_color(),
        "verdict" if title.contains("Review approved") => theme::success_color(),
        "verdict" if title.contains("Changes requested") => theme::warning_color(),
        "verdict" => theme::text_color(),
        _ => theme::text_color(),
    }
}

fn run_should_surface_outcome(run: &RunSummary) -> bool {
    is_completed_brainstorm(run)
        || matches!(run.status, RunStatus::Failed | RunStatus::Blocked)
        || run
            .verification
            .as_ref()
            .is_some_and(|verification| !verification.ok)
}

pub(crate) fn run_history_summary(runs: &[RunSummary]) -> String {
    let total = runs.len();
    let attempts: u32 = runs.iter().map(|run| run.attempt.max(1)).sum();
    let active = runs
        .iter()
        .filter(|run| {
            matches!(
                run.status,
                RunStatus::Planned | RunStatus::Running | RunStatus::Verifying
            )
        })
        .count();
    let needs_check = runs
        .iter()
        .filter(|run| {
            run.status == RunStatus::Done
                && run.verification.is_none()
                && !is_completed_brainstorm(run)
        })
        .count();
    let verified = runs
        .iter()
        .filter(|run| {
            run.status == RunStatus::Done
                && run
                    .verification
                    .as_ref()
                    .is_some_and(|verification| verification.ok)
        })
        .count();
    let failed = runs
        .iter()
        .filter(|run| matches!(run.status, RunStatus::Failed | RunStatus::Blocked))
        .count();
    let mut parts = vec![format!(
        "{total} {}",
        if total == 1 { "run" } else { "runs" }
    )];
    if attempts > total as u32 {
        parts.push(format!("{attempts} attempts"));
    }
    if active > 0 {
        parts.push(format!("{active} active"));
    }
    if needs_check > 0 {
        parts.push(format!("{needs_check} need check"));
    }
    if verified > 0 {
        parts.push(format!("{verified} verified"));
    }
    if failed > 0 {
        parts.push(format!("{failed} failed/blocked"));
    }
    parts.join(" · ")
}

pub(crate) fn run_outcome(run: &RunSummary) -> (String, Color) {
    match (run.status, &run.verification) {
        (RunStatus::Planned, _) => ("planning has not started".to_string(), theme::muted_color()),
        (RunStatus::Running, _) => ("team is working".to_string(), theme::warning_color()),
        (RunStatus::Verifying, _) => (
            "verification is running".to_string(),
            theme::warning_color(),
        ),
        (RunStatus::Done, _) if is_completed_brainstorm(run) => {
            let (count, ballots) = run
                .mode
                .as_ref()
                .map(|mode| (mode.state.idea_count, mode.state.vote_count))
                .unwrap_or_default();
            (
                format!("brainstorm ranked result ready · {count} idea cards · {ballots} ballots"),
                theme::success_color(),
            )
        }
        (RunStatus::Done, Some(verification)) if verification.ok => (
            format!("verified by {}", verification.command),
            theme::success_color(),
        ),
        (RunStatus::Done, Some(verification)) => (
            format!("verification failed: {}", verification.command),
            theme::error_color(),
        ),
        (RunStatus::Done, None) => (
            "work done; verification pending".to_string(),
            theme::warning_color(),
        ),
        (RunStatus::Failed, Some(verification)) if verification.ok => (
            format!("work failed after check: {}", verification.command),
            theme::error_color(),
        ),
        (RunStatus::Failed, Some(verification)) => (
            format!("verification failed: {}", verification.command),
            theme::error_color(),
        ),
        (RunStatus::Failed, None) => (
            "run failed before verification".to_string(),
            theme::error_color(),
        ),
        (RunStatus::Blocked, _) => (
            "blocked; needs user or teammate follow-up".to_string(),
            theme::error_color(),
        ),
    }
}

pub(crate) fn run_next_action(run: &RunSummary) -> String {
    if is_completed_brainstorm(run) {
        return "inspect the idea set, type a new topic, or /mode normal".to_string();
    }

    match run.status {
        RunStatus::Running => run_step_focus(run).unwrap_or_else(|| {
            "watch the chat; close this drawer, then press Esc to cancel".to_string()
        }),
        RunStatus::Verifying => {
            "verification is running; close this drawer, then press Esc to cancel".to_string()
        }
        RunStatus::Done if run.verification.is_none() => {
            "run the Action command to record a check".to_string()
        }
        RunStatus::Done => "verified; select a mode and send the next goal".to_string(),
        RunStatus::Failed => "run the Action command to continue fixes".to_string(),
        RunStatus::Blocked => "resolve blockers, then run the Action command".to_string(),
        RunStatus::Planned => "wait for the coordinator or use /retry".to_string(),
    }
}

fn run_stage_line(run: &RunSummary) -> Line<'static> {
    let (plan, work, verify) = run_stages(run);
    Line::from(vec![
        Span::styled(" Stages: ", theme::muted()),
        run_stage_span("plan", plan),
        Span::styled("  →  ", theme::muted()),
        run_stage_span("work", work),
        Span::styled("  →  ", theme::muted()),
        run_stage_span("verify", verify),
    ])
}

#[derive(Clone, Copy)]
enum RunStageState {
    Pending,
    Active,
    Passed,
    Failed,
    Blocked,
}

fn run_stages(run: &RunSummary) -> (RunStageState, RunStageState, RunStageState) {
    use RunStageState::*;
    match run.status {
        RunStatus::Planned => (Active, Pending, Pending),
        RunStatus::Running => (Passed, Active, Pending),
        RunStatus::Verifying => (Passed, Passed, Active),
        RunStatus::Done if run.verification.is_some() => (Passed, Passed, Passed),
        RunStatus::Done => (Passed, Passed, Pending),
        RunStatus::Failed if run.verification.is_some() => (Passed, Passed, Failed),
        RunStatus::Failed => (Passed, Failed, Pending),
        RunStatus::Blocked => (Passed, Blocked, Pending),
    }
}

fn run_stage_span(name: &str, state: RunStageState) -> Span<'static> {
    let (marker, label, color) = match state {
        RunStageState::Pending => ("○", "pending", theme::muted_color()),
        RunStageState::Active => ("●", "active", theme::warning_color()),
        RunStageState::Passed => ("✓", "done", theme::success_color()),
        RunStageState::Failed => ("✕", "failed", theme::error_color()),
        RunStageState::Blocked => ("■", "blocked", theme::error_color()),
    };
    Span::styled(
        format!("{marker} {name} {label}"),
        ratatui::style::Style::default().fg(color),
    )
}

/// Compact `MM-DD HH:MM` form of a stored timestamp.
pub(crate) fn run_time(value: &str) -> String {
    let value = value.trim();
    let (date, time) = value
        .split_once(' ')
        .or_else(|| value.split_once('T'))
        .unwrap_or((value, ""));
    let mut date_parts = date.split('-');
    let (_, month, day) = (date_parts.next(), date_parts.next(), date_parts.next());
    let mut time_parts = time.trim_end_matches('Z').split(':');
    let (hour, minute) = (time_parts.next(), time_parts.next());

    match (month, day, hour, minute) {
        (Some(month), Some(day), Some(hour), Some(minute)) => {
            format!("{month}-{day} {hour}:{minute}")
        }
        _ => truncate_width(value, 16),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::event::{MemberStatus, MemberSummary, RuntimeEvent};
    use crate::domain::event::{ModeRunStatus, RunId, RunVerification};
    use crate::domain::mode::{CollabMode, ModeStatusSummary};
    use crate::domain::team::MemberId;
    use crate::domain::team::{
        BackendKind, DefaultTarget, PermissionMode, SandboxPolicy, SessionPolicy,
    };
    use crate::tui::app_state::AppState;
    use crate::tui::drawers::Drawer;

    fn run(id: u64, status: RunStatus, verification: Option<RunVerification>) -> RunSummary {
        RunSummary {
            id: RunId(id),
            goal: format!("goal {id}"),
            status,
            coordinator: Some(MemberId::new("builder")),
            verification,
            created_at: "2026-06-28 10:00:00".to_string(),
            updated_at: "2026-06-28 10:00:00".to_string(),
            attempt: 1,
            events: Vec::new(),
            steps: Vec::new(),
            mode: None,
            legacy_mode: None,
        }
    }

    fn mode_run() -> RunSummary {
        let mut run = run(3, RunStatus::Running, None);
        run.goal = "fix the parser".to_string();
        run.mode = Some(ModeRunStatus {
            mode: CollabMode::Review,
            state: ModeStatusSummary {
                phase: "build".to_string(),
                iteration: 1,
                max_iterations: 3,
                round: 0,
                rounds: 0,
                idea_count: 0,
                vote_count: 0,
            },
        });
        run
    }

    fn brainstorm_run(status: RunStatus, phase: &str) -> RunSummary {
        let mut run = run(8, status, None);
        run.goal = "choose a launch concept".to_string();
        run.mode = Some(ModeRunStatus {
            mode: CollabMode::Brainstorm,
            state: ModeStatusSummary {
                phase: phase.to_string(),
                iteration: 0,
                max_iterations: 0,
                round: 2,
                rounds: 2,
                idea_count: 6,
                vote_count: if status == RunStatus::Done { 3 } else { 0 },
            },
        });
        run
    }

    fn state_with(run: RunSummary) -> AppState {
        let active_mode = run.mode.as_ref().map(|mode| terminal_mode(mode.mode));
        let mut state = AppState::new(Vec::new());
        state.apply(RuntimeEvent::Ready {
            modes: Default::default(),
            mode_overrides: Default::default(),
            suggested_verify: None,
            team: "t".to_string(),
            workspace: String::new(),
            default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
            runs: vec![run],
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
                approvals_reviewer: crate::domain::team::CodexApprovalsReviewer::User,
                session_policy: SessionPolicy::Resume,
            }],
        });
        if let Some(mode) = active_mode {
            state.apply(RuntimeEvent::ModeChanged { mode });
        }
        state
    }

    #[test]
    fn mode_footer_hint_shows_badge_and_iteration() {
        let state = state_with(mode_run());
        let (text, _) = run_footer_hint(&state).expect("footer hint");
        assert!(text.contains("◇ review"), "missing mode badge: {text}");
        assert!(text.contains("iter 1/3"), "missing iteration: {text}");
        assert!(text.contains("build"), "missing phase: {text}");
        assert!(text.contains("run-3"), "missing run id: {text}");
    }

    #[test]
    fn mode_detail_line_in_runs_drawer() {
        let state = state_with(mode_run());
        let lines = drawer_runs(&state, 80);
        let text: String = lines
            .iter()
            .flat_map(|line| line.spans.iter().map(|s| s.content.as_ref()))
            .collect::<Vec<_>>()
            .join("");
        assert!(
            text.contains("review") && text.contains("phase:") && text.contains("iter 1/3"),
            "missing mode detail: {text}"
        );
    }

    #[test]
    fn completed_brainstorm_footer_uses_terminal_guidance() {
        let state = state_with(brainstorm_run(RunStatus::Done, "diverging"));
        let (text, color) = run_footer_hint(&state).expect("footer hint");

        assert_eq!(
            text,
            "◇ brainstorm run-8 · ranked result ready · new topic or /mode normal · /runs details"
        );
        assert_eq!(color, theme::success_color());
        assert!(!text.contains("diverging"));
        assert!(!text.contains("/verify"));
    }

    #[test]
    fn new_chat_hides_previous_brainstorm_footer_hint() {
        let mut state = state_with(brainstorm_run(RunStatus::Done, "diverging"));

        state.apply(RuntimeEvent::SessionReset);

        assert_eq!(state.active_mode(), TerminalMode::Normal);
        assert_eq!(run_footer_hint(&state), None);
        state.apply(RuntimeEvent::ModeChanged {
            mode: TerminalMode::Brainstorm,
        });
        assert_eq!(
            run_footer_hint(&state),
            None,
            "a mode change must not revive a run from the previous chat"
        );
        assert!(
            state.latest_run().is_none(),
            "the new conversation must start with an empty run list"
        );
    }

    #[test]
    fn new_chat_shows_footer_again_for_a_new_run() {
        let mut state = state_with(brainstorm_run(RunStatus::Done, "diverging"));
        state.apply(RuntimeEvent::SessionReset);

        let mut next = brainstorm_run(RunStatus::Running, "seed");
        next.id = RunId(9);
        state.apply(RuntimeEvent::ModeChanged {
            mode: TerminalMode::Brainstorm,
        });
        state.apply(RuntimeEvent::RunUpdated { run: next });

        let (text, _) = run_footer_hint(&state).expect("new run footer");
        assert!(text.contains("run-9"), "{text}");
    }

    #[test]
    fn completed_brainstorm_drawer_hides_stale_phase_and_verification_actions() {
        let run = brainstorm_run(RunStatus::Done, "diverging");
        let mut state = state_with(run.clone());
        state.toggle_drawer(Drawer::Runs);
        assert!(state.toggle_runs_detail());
        let lines = drawer_runs(&state, 100);
        let text = lines
            .iter()
            .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
            .collect::<Vec<_>>()
            .join("");

        assert!(text.contains("phase: ranked result ready"), "{text}");
        assert!(
            text.contains("brainstorm ranked result ready · 6 idea cards"),
            "{text}"
        );
        assert!(
            text.contains("inspect the idea set, type a new topic, or /mode normal"),
            "{text}"
        );
        assert!(!text.contains("diverging"), "{text}");
        assert!(!text.contains("/verify"), "{text}");
        assert!(!text.contains("verification pending"), "{text}");
        assert!(!text.contains("need check"), "{text}");
        assert_eq!(run_history_summary(&[run]), "1 run");
    }

    #[test]
    fn brainstorm_failures_and_blockers_keep_actionable_footer_hints() {
        let failed = state_with(brainstorm_run(RunStatus::Failed, "diverging"));
        let blocked = state_with(brainstorm_run(RunStatus::Blocked, "collecting"));

        let failed_text = run_footer_hint(&failed).expect("failed hint").0;
        let blocked_text = run_footer_hint(&blocked).expect("blocked hint").0;

        assert!(failed_text.contains("/continue to fix"), "{failed_text}");
        assert!(
            blocked_text.contains("/continue when resolved"),
            "{blocked_text}"
        );
        assert!(!failed_text.contains("complete · type"), "{failed_text}");
        assert!(!blocked_text.contains("complete · type"), "{blocked_text}");
    }

    #[test]
    fn run_summaries_count_actionable_states() {
        let verified = Some(RunVerification {
            command: "cargo test".to_string(),
            ok: true,
            summary: "ok".to_string(),
        });
        let failed_check = Some(RunVerification {
            command: "cargo test".to_string(),
            ok: false,
            summary: "failed".to_string(),
        });
        let runs = vec![
            run(1, RunStatus::Running, None),
            run(2, RunStatus::Verifying, None),
            run(3, RunStatus::Done, None),
            run(4, RunStatus::Done, verified),
            {
                let mut run = run(5, RunStatus::Failed, failed_check.clone());
                run.attempt = 2;
                run
            },
        ];

        assert_eq!(
            run_history_summary(&runs),
            "5 runs · 6 attempts · 2 active · 1 need check · 1 verified · 1 failed/blocked"
        );
        let failed = run(6, RunStatus::Failed, failed_check);
        assert_eq!(run_outcome(&failed).0, "verification failed: cargo test");

        let mut stepped = run(7, RunStatus::Running, None);
        stepped.steps = vec![
            RunStepSummary {
                number: 1,
                status: RunStepStatus::Done,
                owner: Some(MemberId::new("builder")),
                title: "Map parser states".to_string(),
                note: None,
                updated_at: "2026-06-28 10:05:00".to_string(),
            },
            RunStepSummary {
                number: 2,
                status: RunStepStatus::Doing,
                owner: Some(MemberId::new("builder")),
                title: "Wire checklist UI".to_string(),
                note: None,
                updated_at: "2026-06-28 10:10:00".to_string(),
            },
            RunStepSummary {
                number: 3,
                status: RunStepStatus::Blocked,
                owner: None,
                title: "Wait for API credentials".to_string(),
                note: None,
                updated_at: "2026-06-28 10:12:00".to_string(),
            },
        ];
        assert_eq!(
            run_step_progress(&stepped).unwrap().0,
            "1/3 done · 1 doing · 1 blocked"
        );
        assert_eq!(run_step_table_cell(&stepped).0, "1/3 block");
        assert_eq!(
            run_next_action(&stepped),
            "blocked step #3: Wait for API credentials"
        );
        assert_eq!(
            run_owner_summary(&stepped).unwrap().0,
            "@builder 1/2 active · unassigned 1/1 1 blocked"
        );
    }

    #[test]
    fn runs_table_rule_matches_header_columns() {
        let header = runs_table_header();
        let rule = runs_table_rule();
        let header_text: String = header.spans.iter().map(|s| s.content.as_ref()).collect();
        let rule_text: String = rule.spans.iter().map(|s| s.content.as_ref()).collect();
        // Every column separator in the header lines up with a cross in the rule.
        for (i, ch) in header_text.char_indices() {
            if ch == '│' {
                let offset = header_text[..i].chars().count();
                assert_eq!(
                    rule_text.chars().nth(offset),
                    Some('┼'),
                    "column at {offset}"
                );
            }
        }
    }
}
