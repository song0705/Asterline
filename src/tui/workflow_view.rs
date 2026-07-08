//! Workflow presentation: the `/runs` drawer content, the footer hint for the
//! active run, and the pure helpers that summarize runs, steps, owners, and
//! timelines. Pure `WorkflowRunSummary -> Line` logic; no layout code.

use std::collections::BTreeMap;

use ratatui::style::Color;
use ratatui::text::{Line, Span};

use crate::domain::event::{
    WorkflowRunEventSummary, WorkflowRunStatus, WorkflowRunSummary, WorkflowStepStatus,
    WorkflowStepSummary,
};
use crate::tui::app_state::AppState;
use crate::tui::theme;
use crate::tui::theme::{pad_width, truncate_width, workflow_status_color};

/// One-line hint about the latest run, shown in the footer when idle.
pub(crate) fn workflow_footer_hint(state: &AppState) -> Option<(String, Color)> {
    let run = state.latest_workflow_run()?;
    let progress = workflow_step_progress_suffix(run);
    match run.status {
        WorkflowRunStatus::Running => Some((
            format!(
                "● {} running{progress} · /runs details · /abort cancel",
                run.id
            ),
            theme::WARNING,
        )),
        WorkflowRunStatus::Verifying => Some((
            format!(
                "⏳ {} verifying{progress} · /runs details · /abort cancel",
                run.id
            ),
            theme::WARNING,
        )),
        WorkflowRunStatus::Done if run.verification.is_none() => Some((
            format!(
                "● {} done{progress} · /verify to check · /runs details",
                run.id
            ),
            theme::SUCCESS,
        )),
        WorkflowRunStatus::Failed => Some((
            format!(
                "● {} failed{progress} · /runs details · /continue to fix",
                run.id
            ),
            theme::ERROR,
        )),
        WorkflowRunStatus::Blocked => Some((
            format!("● {} blocked{progress} · /runs details", run.id),
            theme::ERROR,
        )),
        _ => None,
    }
}

/// The `/runs` drawer body. Compact mode shows what you act on (selected run,
/// goal, progress, action, steps, history table); `x` expands the rest
/// (owner, times, owners workload, outcome, stages, timeline).
pub(crate) fn drawer_runs(state: &AppState, width: usize) -> Vec<Line<'static>> {
    let runs = state.workflow_runs();
    let detail = state.workflow_runs_detail();
    if runs.is_empty() {
        return vec![Line::styled(
            "no workflow runs yet — start one with /plan <goal>",
            theme::muted(),
        )];
    }

    let mut lines = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(" History: ", theme::muted()),
        Span::styled(workflow_history_summary(runs), theme::text()),
        Span::styled(" · View: ", theme::muted()),
        Span::styled(
            if detail { "details" } else { "compact" },
            if detail {
                theme::accent_bold()
            } else {
                theme::bold(theme::TEXT)
            },
        ),
    ]));
    if let Some(selected) = state.selected_workflow_run() {
        let surfaced = workflow_should_surface_outcome(selected);
        lines.push(Line::raw(""));
        let latest = runs.last();
        lines.push(Line::from(vec![
            Span::styled(format!(" Selected: {} ", selected.id), theme::accent_bold()),
            Span::styled(
                selected.status.as_str(),
                ratatui::style::Style::default().fg(workflow_status_color(selected.status)),
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
                Span::styled(workflow_time(&selected.created_at), theme::text()),
                Span::styled(" · updated ", theme::muted()),
                Span::styled(workflow_time(&selected.updated_at), theme::text()),
            ]));
        } else if surfaced {
            lines.push(Line::from(vec![
                Span::styled(" Attempt: ", theme::muted()),
                Span::styled(format!("#{}", selected.attempt), theme::warning_bold()),
            ]));
        }
        if let Some((progress, color)) = workflow_step_progress(selected) {
            lines.push(Line::from(vec![
                Span::styled(" Progress: ", theme::muted()),
                Span::styled(progress, theme::bold(color)),
            ]));
        }
        if detail && let Some((owners, color)) = workflow_owner_summary(selected) {
            lines.push(Line::from(vec![
                Span::styled(" Owners: ", theme::muted()),
                Span::styled(owners, theme::bold(color)),
            ]));
        }
        if detail || surfaced {
            let (outcome, outcome_color) = workflow_outcome(selected);
            lines.push(Line::from(vec![
                Span::styled(" Outcome: ", theme::muted()),
                Span::styled(outcome, theme::bold(outcome_color)),
            ]));
            lines.push(Line::from(vec![
                Span::styled(" Next: ", theme::muted()),
                Span::styled(workflow_next_action(selected), theme::text()),
            ]));
        }
        lines.push(Line::from(vec![
            Span::styled(" Action: ", theme::muted()),
            Span::styled(
                state.selected_workflow_stage_command().unwrap_or_default(),
                theme::accent_bold(),
            ),
        ]));
        if let Some(dispatch) = state.selected_workflow_dispatch_command() {
            lines.push(Line::from(vec![
                Span::styled(" Dispatch: ", theme::muted()),
                Span::styled(
                    truncate_width(&dispatch, width.saturating_sub(12).max(20)),
                    theme::accent_bold(),
                ),
            ]));
        }
        if detail {
            lines.push(workflow_stage_line(selected));
        }
        lines.extend(workflow_step_lines(
            selected,
            state.selected_workflow_step(),
            width,
        ));
        if detail || surfaced {
            lines.extend(workflow_timeline_lines(selected));
        }
        lines.push(Line::raw(""));
    }

    lines.push(runs_table_header());
    lines.push(runs_table_rule());

    let selected_id = state.selected_workflow_run().map(|run| run.id);
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

fn drawer_run(
    run: &WorkflowRunSummary,
    selected: bool,
    detail: bool,
    width: usize,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let owner = run
        .coordinator
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_else(|| "-".to_string());
    let updated = workflow_time(&run.updated_at);
    let marker = if selected { "›" } else { " " };
    let row_style = if selected {
        theme::selection()
    } else {
        ratatui::style::Style::default()
    };
    let status_style = if selected {
        row_style
    } else {
        ratatui::style::Style::default().fg(workflow_status_color(run.status))
    };
    let (steps, steps_color) = workflow_step_table_cell(run);
    let cell = |text: &str, width: usize, color: Color| {
        Span::styled(
            pad_width(text, width),
            row_style.fg(if selected { Color::Black } else { color }),
        )
    };
    let sep = Span::styled("│ ", row_style.fg(theme::MUTED));
    lines.push(Line::from(vec![
        cell(
            &format!(" {marker} {}", run.id),
            RUNS_COLUMNS[0],
            theme::ACCENT,
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
            theme::WARNING,
        ),
        sep.clone(),
        cell(&steps, RUNS_COLUMNS[3], steps_color),
        sep.clone(),
        cell(&updated, RUNS_COLUMNS[4], theme::TEXT),
        sep.clone(),
        cell(&owner, RUNS_COLUMNS[5], theme::TEXT),
        sep,
        Span::styled(
            truncate_width(&run.goal, width.saturating_sub(67).max(10)),
            row_style.fg(if selected {
                Color::Black
            } else {
                theme::EMPHASIS
            }),
        ),
    ]));
    if selected || !detail {
        return lines;
    }
    let (outcome, outcome_color) = workflow_outcome(run);
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

fn workflow_step_lines(
    run: &WorkflowRunSummary,
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
        lines.push(workflow_step_line(
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

fn workflow_step_line(step: &WorkflowStepSummary, selected: bool, width: usize) -> Line<'static> {
    let (marker, color) = workflow_step_marker(step.status);
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

fn workflow_step_marker(status: WorkflowStepStatus) -> (&'static str, Color) {
    match status {
        WorkflowStepStatus::Todo => ("○", theme::MUTED),
        WorkflowStepStatus::Doing => ("●", theme::WARNING),
        WorkflowStepStatus::Done => ("✓", theme::SUCCESS),
        WorkflowStepStatus::Blocked => ("■", theme::ERROR),
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct WorkflowStepStats {
    total: usize,
    done: usize,
    doing: usize,
    blocked: usize,
    todo: usize,
}

fn workflow_step_stats(run: &WorkflowRunSummary) -> Option<WorkflowStepStats> {
    if run.steps.is_empty() {
        return None;
    }
    let mut stats = WorkflowStepStats {
        total: run.steps.len(),
        ..WorkflowStepStats::default()
    };
    for step in &run.steps {
        match step.status {
            WorkflowStepStatus::Todo => stats.todo += 1,
            WorkflowStepStatus::Doing => stats.doing += 1,
            WorkflowStepStatus::Done => stats.done += 1,
            WorkflowStepStatus::Blocked => stats.blocked += 1,
        }
    }
    Some(stats)
}

pub(crate) fn workflow_step_progress(run: &WorkflowRunSummary) -> Option<(String, Color)> {
    let stats = workflow_step_stats(run)?;
    let mut parts = vec![format!("{}/{} done", stats.done, stats.total)];
    if stats.doing > 0 {
        parts.push(format!("{} doing", stats.doing));
    }
    if stats.blocked > 0 {
        parts.push(format!("{} blocked", stats.blocked));
    }
    let color = if stats.blocked > 0 {
        theme::ERROR
    } else if stats.doing > 0 {
        theme::WARNING
    } else if stats.done == stats.total {
        theme::SUCCESS
    } else {
        theme::TEXT
    };
    Some((parts.join(" · "), color))
}

fn workflow_step_progress_suffix(run: &WorkflowRunSummary) -> String {
    workflow_step_progress(run)
        .map(|(progress, _)| format!(" · {progress}"))
        .unwrap_or_default()
}

pub(crate) fn workflow_step_table_cell(run: &WorkflowRunSummary) -> (String, Color) {
    let Some(stats) = workflow_step_stats(run) else {
        return ("-".to_string(), theme::MUTED);
    };
    if stats.blocked > 0 {
        (
            format!("{}/{} block", stats.done, stats.total),
            theme::ERROR,
        )
    } else if stats.doing > 0 {
        (
            format!("{}/{} doing", stats.done, stats.total),
            theme::WARNING,
        )
    } else if stats.done == stats.total {
        (
            format!("{}/{} done", stats.done, stats.total),
            theme::SUCCESS,
        )
    } else {
        (format!("{}/{} todo", stats.done, stats.total), theme::TEXT)
    }
}

fn workflow_step_focus(run: &WorkflowRunSummary) -> Option<String> {
    let focus = |status: WorkflowStepStatus, label: &'static str| {
        run.steps
            .iter()
            .find(|step| step.status == status)
            .map(|step| {
                format!(
                    "{label} step #{}{}: {}",
                    step.number,
                    workflow_step_owner_suffix(step),
                    truncate_width(&step.title, 44)
                )
            })
    };
    focus(WorkflowStepStatus::Blocked, "blocked")
        .or_else(|| focus(WorkflowStepStatus::Doing, "current"))
        .or_else(|| focus(WorkflowStepStatus::Todo, "next"))
        .or_else(|| (!run.steps.is_empty()).then(|| "all checklist steps are done".to_string()))
}

fn workflow_step_owner_suffix(step: &WorkflowStepSummary) -> String {
    step.owner
        .as_ref()
        .map(|owner| format!(" @{owner}"))
        .unwrap_or_default()
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct WorkflowOwnerStats {
    total: usize,
    active: usize,
    blocked: usize,
}

pub(crate) fn workflow_owner_summary(run: &WorkflowRunSummary) -> Option<(String, Color)> {
    if run.steps.len() < 2 {
        return None;
    }

    let mut owners: BTreeMap<String, WorkflowOwnerStats> = BTreeMap::new();
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
            WorkflowStepStatus::Todo | WorkflowStepStatus::Doing | WorkflowStepStatus::Blocked
        ) {
            stats.active += 1;
        }
        if step.status == WorkflowStepStatus::Blocked {
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
        theme::ERROR
    } else if parts.iter().any(|part| part.contains("active")) {
        theme::WARNING
    } else {
        theme::SUCCESS
    };
    Some((parts.join(" · "), color))
}

fn workflow_timeline_lines(run: &WorkflowRunSummary) -> Vec<Line<'static>> {
    if run.events.is_empty() {
        return Vec::new();
    }

    let mut lines = Vec::new();
    lines.push(Line::from(vec![Span::styled(" Timeline:", theme::muted())]));
    for event in run.events.iter().rev().take(6).rev() {
        lines.push(workflow_event_line(event));
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

fn workflow_event_line(event: &WorkflowRunEventSummary) -> Line<'static> {
    let color = workflow_event_color(event.kind.as_str());
    let title = event
        .detail
        .as_ref()
        .and_then(|detail| detail.lines().find(|line| !line.trim().is_empty()))
        .map(|detail| format!("{} · {}", event.title, truncate_width(detail.trim(), 42)))
        .unwrap_or_else(|| event.title.clone());
    Line::from(vec![
        Span::styled(
            format!("   {} ", workflow_time(&event.created_at)),
            theme::muted(),
        ),
        Span::styled(format!("#{} ", event.attempt), theme::warning_bold()),
        Span::styled(title, ratatui::style::Style::default().fg(color)),
    ])
}

fn workflow_event_color(kind: &str) -> Color {
    match kind {
        "started" | "continued" | "running" => theme::ACCENT,
        "note" => theme::EMPHASIS,
        "step_added" | "step_updated" | "step_renamed" | "step_removed" | "step_assigned" => {
            theme::ACCENT
        }
        "verifying" => theme::WARNING,
        "done" | "verification_passed" => theme::SUCCESS,
        "failed" | "verification_failed" => theme::ERROR,
        "blocked" => theme::ERROR,
        _ => theme::TEXT,
    }
}

fn workflow_should_surface_outcome(run: &WorkflowRunSummary) -> bool {
    matches!(
        run.status,
        WorkflowRunStatus::Failed | WorkflowRunStatus::Blocked
    ) || run
        .verification
        .as_ref()
        .is_some_and(|verification| !verification.ok)
}

pub(crate) fn workflow_history_summary(runs: &[WorkflowRunSummary]) -> String {
    let total = runs.len();
    let attempts: u32 = runs.iter().map(|run| run.attempt.max(1)).sum();
    let active = runs
        .iter()
        .filter(|run| {
            matches!(
                run.status,
                WorkflowRunStatus::Planned
                    | WorkflowRunStatus::Running
                    | WorkflowRunStatus::Verifying
            )
        })
        .count();
    let needs_check = runs
        .iter()
        .filter(|run| run.status == WorkflowRunStatus::Done && run.verification.is_none())
        .count();
    let verified = runs
        .iter()
        .filter(|run| {
            run.status == WorkflowRunStatus::Done
                && run
                    .verification
                    .as_ref()
                    .is_some_and(|verification| verification.ok)
        })
        .count();
    let failed = runs
        .iter()
        .filter(|run| {
            matches!(
                run.status,
                WorkflowRunStatus::Failed | WorkflowRunStatus::Blocked
            )
        })
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

pub(crate) fn workflow_outcome(run: &WorkflowRunSummary) -> (String, Color) {
    match (run.status, &run.verification) {
        (WorkflowRunStatus::Planned, _) => ("planning has not started".to_string(), theme::MUTED),
        (WorkflowRunStatus::Running, _) => ("team is working".to_string(), theme::WARNING),
        (WorkflowRunStatus::Verifying, _) => {
            ("verification is running".to_string(), theme::WARNING)
        }
        (WorkflowRunStatus::Done, Some(verification)) if verification.ok => (
            format!("verified by {}", verification.command),
            theme::SUCCESS,
        ),
        (WorkflowRunStatus::Done, Some(verification)) => (
            format!("verification failed: {}", verification.command),
            theme::ERROR,
        ),
        (WorkflowRunStatus::Done, None) => (
            "work done; verification pending".to_string(),
            theme::WARNING,
        ),
        (WorkflowRunStatus::Failed, Some(verification)) if verification.ok => (
            format!("work failed after check: {}", verification.command),
            theme::ERROR,
        ),
        (WorkflowRunStatus::Failed, Some(verification)) => (
            format!("verification failed: {}", verification.command),
            theme::ERROR,
        ),
        (WorkflowRunStatus::Failed, None) => {
            ("run failed before verification".to_string(), theme::ERROR)
        }
        (WorkflowRunStatus::Blocked, _) => (
            "blocked; needs user or teammate follow-up".to_string(),
            theme::ERROR,
        ),
    }
}

pub(crate) fn workflow_next_action(run: &WorkflowRunSummary) -> String {
    match run.status {
        WorkflowRunStatus::Running => workflow_step_focus(run)
            .unwrap_or_else(|| "watch the chat, or /abort to cancel".to_string()),
        WorkflowRunStatus::Verifying => {
            "verification is running in the background; /abort cancels it".to_string()
        }
        WorkflowRunStatus::Done if run.verification.is_none() => {
            "run the Action command to record a check".to_string()
        }
        WorkflowRunStatus::Done => "verified; start another run with /plan <goal>".to_string(),
        WorkflowRunStatus::Failed => "run the Action command to continue fixes".to_string(),
        WorkflowRunStatus::Blocked => "resolve blockers, then run the Action command".to_string(),
        WorkflowRunStatus::Planned => "wait for the coordinator or retry /plan".to_string(),
    }
}

fn workflow_stage_line(run: &WorkflowRunSummary) -> Line<'static> {
    let (plan, work, verify) = workflow_stages(run);
    Line::from(vec![
        Span::styled(" Stages: ", theme::muted()),
        workflow_stage_span("plan", plan),
        Span::styled("  →  ", theme::muted()),
        workflow_stage_span("work", work),
        Span::styled("  →  ", theme::muted()),
        workflow_stage_span("verify", verify),
    ])
}

#[derive(Clone, Copy)]
enum WorkflowStageState {
    Pending,
    Active,
    Passed,
    Failed,
    Blocked,
}

fn workflow_stages(
    run: &WorkflowRunSummary,
) -> (WorkflowStageState, WorkflowStageState, WorkflowStageState) {
    use WorkflowStageState::*;
    match run.status {
        WorkflowRunStatus::Planned => (Active, Pending, Pending),
        WorkflowRunStatus::Running => (Passed, Active, Pending),
        WorkflowRunStatus::Verifying => (Passed, Passed, Active),
        WorkflowRunStatus::Done if run.verification.is_some() => (Passed, Passed, Passed),
        WorkflowRunStatus::Done => (Passed, Passed, Pending),
        WorkflowRunStatus::Failed if run.verification.is_some() => (Passed, Passed, Failed),
        WorkflowRunStatus::Failed => (Passed, Failed, Pending),
        WorkflowRunStatus::Blocked => (Passed, Blocked, Pending),
    }
}

fn workflow_stage_span(name: &str, state: WorkflowStageState) -> Span<'static> {
    let (marker, label, color) = match state {
        WorkflowStageState::Pending => ("○", "pending", theme::MUTED),
        WorkflowStageState::Active => ("●", "active", theme::WARNING),
        WorkflowStageState::Passed => ("✓", "done", theme::SUCCESS),
        WorkflowStageState::Failed => ("✕", "failed", theme::ERROR),
        WorkflowStageState::Blocked => ("■", "blocked", theme::ERROR),
    };
    Span::styled(
        format!("{marker} {name} {label}"),
        ratatui::style::Style::default().fg(color),
    )
}

/// Compact `MM-DD HH:MM` form of a stored timestamp.
pub(crate) fn workflow_time(value: &str) -> String {
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
    use crate::domain::event::{WorkflowRunId, WorkflowVerification};
    use crate::domain::team::MemberId;

    fn run(
        id: u64,
        status: WorkflowRunStatus,
        verification: Option<WorkflowVerification>,
    ) -> WorkflowRunSummary {
        WorkflowRunSummary {
            id: WorkflowRunId(id),
            goal: format!("goal {id}"),
            status,
            coordinator: Some(MemberId::new("builder")),
            verification,
            created_at: "2026-06-28 10:00:00".to_string(),
            updated_at: "2026-06-28 10:00:00".to_string(),
            attempt: 1,
            events: Vec::new(),
            steps: Vec::new(),
        }
    }

    #[test]
    fn workflow_run_summaries_count_actionable_states() {
        let verified = Some(WorkflowVerification {
            command: "cargo test".to_string(),
            ok: true,
            summary: "ok".to_string(),
        });
        let failed_check = Some(WorkflowVerification {
            command: "cargo test".to_string(),
            ok: false,
            summary: "failed".to_string(),
        });
        let runs = vec![
            run(1, WorkflowRunStatus::Running, None),
            run(2, WorkflowRunStatus::Verifying, None),
            run(3, WorkflowRunStatus::Done, None),
            run(4, WorkflowRunStatus::Done, verified),
            {
                let mut run = run(5, WorkflowRunStatus::Failed, failed_check.clone());
                run.attempt = 2;
                run
            },
        ];

        assert_eq!(
            workflow_history_summary(&runs),
            "5 runs · 6 attempts · 2 active · 1 need check · 1 verified · 1 failed/blocked"
        );
        let failed = run(6, WorkflowRunStatus::Failed, failed_check);
        assert_eq!(
            workflow_outcome(&failed).0,
            "verification failed: cargo test"
        );

        let mut stepped = run(7, WorkflowRunStatus::Running, None);
        stepped.steps = vec![
            WorkflowStepSummary {
                number: 1,
                status: WorkflowStepStatus::Done,
                owner: Some(MemberId::new("builder")),
                title: "Map parser states".to_string(),
                note: None,
                updated_at: "2026-06-28 10:05:00".to_string(),
            },
            WorkflowStepSummary {
                number: 2,
                status: WorkflowStepStatus::Doing,
                owner: Some(MemberId::new("builder")),
                title: "Wire checklist UI".to_string(),
                note: None,
                updated_at: "2026-06-28 10:10:00".to_string(),
            },
            WorkflowStepSummary {
                number: 3,
                status: WorkflowStepStatus::Blocked,
                owner: None,
                title: "Wait for API credentials".to_string(),
                note: None,
                updated_at: "2026-06-28 10:12:00".to_string(),
            },
        ];
        assert_eq!(
            workflow_step_progress(&stepped).unwrap().0,
            "1/3 done · 1 doing · 1 blocked"
        );
        assert_eq!(workflow_step_table_cell(&stepped).0, "1/3 block");
        assert_eq!(
            workflow_next_action(&stepped),
            "blocked step #3: Wait for API credentials"
        );
        assert_eq!(
            workflow_owner_summary(&stepped).unwrap().0,
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
