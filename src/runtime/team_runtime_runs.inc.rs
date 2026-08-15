impl TeamRuntime {
    fn handle_run_team(&mut self, goal: String, step: &mut RuntimeStep) {
        let goal = goal.trim().to_string();
        if goal.is_empty() {
            step.events
                .push(RuntimeEvent::Notice("team mode needs a goal".to_string()));
            return;
        }
        if self.mode_sessions.values().next().is_some() || !self.run_turns.is_empty() {
            step.events.push(RuntimeEvent::Notice(
                "a run is already active — press Esc to cancel it first".to_string(),
            ));
            return;
        }
        let id = match resolve_team_coordinator(&self.config) {
            Ok(id) => id,
            Err(err) => {
                step.events.push(RuntimeEvent::Notice(err));
                return;
            }
        };
        let limits = match resolve_team_limits(&self.config) {
            Ok(limits) => limits,
            Err(err) => {
                step.events.push(RuntimeEvent::Notice(err));
                return;
            }
        };

        let state = ModeStatusSummary {
            phase: "coordinating".to_string(),
            iteration: 1,
            max_iterations: limits.max_iterations,
            ..ModeStatusSummary::default()
        };
        let state_json = serde_json::to_string(&state).expect("mode status is serializable");
        let run = match self.store.create_mode_run(
            &goal,
            Some(&id),
            CollabMode::Team,
            &state_json,
        ) {
            Ok(run) => run,
            Err(err) => {
                step.events.push(RuntimeEvent::Notice(format!(
                    "could not create team run: {err}"
                )));
                return;
            }
        };
        let run_id = run.id;
        step.events.push(RuntimeEvent::RunUpdated { run });

        let teammates = self.team_teammate_list(&id);
        let prompt = team_start_prompt(&goal, &teammates);
        let turn = match self.store.create_turn() {
            Ok(turn) => turn,
            Err(err) => {
                step.events
                    .push(RuntimeEvent::Notice(format!("store error: {err}")));
                self.block_mode_run(run_id, "could not create the team turn", step);
                return;
            }
        };
        let display_body = format!("[team {run_id}] → {id}: {goal}");
        if let Err(err) =
            self.store
                .record_user(turn, std::slice::from_ref(&id), &display_body)
        {
            self.report_store_error("save a team dispatch", err, step);
            self.block_mode_run(run_id, "could not persist the team dispatch", step);
            return;
        }
        step.events.push(RuntimeEvent::TurnStarted { turn });
        step.events.push(RuntimeEvent::UserMessage {
            turn,
            targets: vec![id.clone()],
            body: display_body.clone(),
        });
        self.log(
            &id,
            LogEntry::info("user", format!("team {run_id} → {id}: {goal}")),
            step,
        );
        step.events.push(RuntimeEvent::Notice(format!(
            "team {run_id} started → {id}"
        )));
        self.run_turns.insert(turn, run_id);
        let gate = self.approvals_enabled && self.matcher.applies_to(ApprovalSurface::Mode);
        if gate && let Some(kind) = self.matcher.classify(&prompt) {
            match self.store.insert_approval(Some(turn), None, &kind, &prompt) {
                Ok(approval_id) => {
                    self.held_approvals.insert(
                        approval_id,
                        HeldApproval {
                            turn,
                            targets: vec![id],
                            prompt: prompt.clone(),
                            mode_run: Some(run_id),
                            member_request: None,
                        },
                    );
                    step.events.push(RuntimeEvent::ApprovalRequested {
                        id: approval_id,
                        member: None,
                        action: kind,
                        body: prompt,
                    });
                }
                Err(err) => {
                    self.report_store_error("save a team approval request", err, step);
                    self.block_mode_run(
                        run_id,
                        "could not persist the team approval request",
                        step,
                    );
                    self.check_turn_complete(turn, step);
                }
            }
        } else {
            self.enqueue_prompt(&id, turn, prompt, step);
        }
    }

    fn handle_continue_run(
        &mut self,
        run_id: Option<RunId>,
        note: Option<String>,
        step: &mut RuntimeStep,
    ) {
        let Some(run) = self.run_or_latest(run_id, "continue", step) else {
            return;
        };
        if let Some(legacy) = &run.legacy_mode {
            step.events.push(RuntimeEvent::Notice(format!(
                "{} is from an older Asterline (mode \"{legacy}\") — start a fresh run",
                run.id
            )));
            return;
        }
        if run.mode.as_ref().is_some_and(|mode| mode.mode != CollabMode::Team) {
            if self.mode_sessions.contains_key(&run.id) {
                step.events.push(RuntimeEvent::Notice(format!(
                    "{} is already active",
                    run.id
                )));
                return;
            }
            self.mode_resume(run, note, step);
            return;
        }
        if matches!(
            run.status,
            RunStatus::Running | RunStatus::Verifying
        ) {
            step.events.push(RuntimeEvent::Notice(format!(
                "{} is already active",
                run.id
            )));
            return;
        }

        let coordinator = run
            .coordinator
            .as_ref()
            .and_then(|id| self.config.find(id.as_str()).map(|m| m.id.clone()))
            .or_else(|| self.config.members.first().map(|m| m.id.clone()));
        let Some(id) = coordinator else {
            step.events
                .push(RuntimeEvent::Notice("team has no members".to_string()));
            return;
        };

        let turn = match self.store.create_turn() {
            Ok(turn) => turn,
            Err(err) => {
                step.events
                    .push(RuntimeEvent::Notice(format!("store error: {err}")));
                return;
            }
        };
        match self.store.continue_run(run.id, note.as_deref()) {
            Ok(updated) => step
                .events
                .push(RuntimeEvent::RunUpdated { run: updated }),
            Err(err) => {
                self.report_store_error("continue the team run", err, step);
                return;
            }
        }
        self.failed_runs.remove(&run.id);

        let display_body = match &note {
            Some(note) => format!("/continue {} {note}", run.id),
            None => format!("/continue {}", run.id),
        };
        if let Err(err) =
            self.store
                .record_user(turn, std::slice::from_ref(&id), &display_body)
        {
            self.report_store_error("save a continued team dispatch", err, step);
            self.block_mode_run(run.id, "could not persist the continued dispatch", step);
            return;
        }
        step.events.push(RuntimeEvent::TurnStarted { turn });
        step.events.push(RuntimeEvent::UserMessage {
            turn,
            targets: vec![id.clone()],
            body: display_body.clone(),
        });
        self.log(
            &id,
            LogEntry::info("user", format!("team {} continued → {id}", run.id)),
            step,
        );
        step.events.push(RuntimeEvent::Notice(format!(
            "team {} continued → {id}",
            run.id
        )));

        let verification = run.verification.as_ref().map(|v| {
            (
                v.command.as_str(),
                v.ok,
                v.summary.as_str(),
            )
        });
        let prompt = team_continue_prompt(
            run.id,
            &run.goal,
            run.status.as_str(),
            verification,
            note.as_deref(),
            false,
        );
        self.run_turns.insert(turn, run.id);
        self.enqueue_prompt(&id, turn, prompt, step);
    }

    /// Teammate list for coordinator prompts: `id (role)`, excluding the coordinator.
    fn team_teammate_list(&self, coordinator: &MemberId) -> String {
        self.config
            .members
            .iter()
            .filter(|m| &m.id != coordinator)
            .map(|m| format!("{} ({})", m.id, m.role))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Finish a non-mode-session run turn: team runs may auto-verify; others Done.
    fn finish_plain_or_team_run(&mut self, run_id: RunId, step: &mut RuntimeStep) -> bool {
        let run = match self.store.run(run_id) {
            Ok(run) => run,
            Err(err) => {
                self.report_store_error("load the finishing run", err, step);
                return false;
            }
        };
        let is_team = run
            .mode
            .as_ref()
            .is_some_and(|mode| mode.mode == CollabMode::Team);
        if is_team {
            let limits = resolve_team_limits(&self.config).unwrap_or_default();
            if limits.auto_verify
                && let Some(cmd) = resolve_verify_command(
                    limits.verify_command.as_deref(),
                    suggested_verify_command(&self.config.workspace),
                )
            {
                // Persist iteration/max_iterations (and verifying phase) before the UI event.
                let mut status = run
                    .mode
                    .as_ref()
                    .map(|m| m.state.clone())
                    .unwrap_or_default();
                if status.iteration == 0 {
                    status.iteration = 1;
                }
                if status.max_iterations == 0 {
                    status.max_iterations = limits.max_iterations;
                }
                status.phase = "verifying".to_string();
                let json = match serde_json::to_string(&status) {
                    Ok(json) => json,
                    Err(err) => {
                        step.events.push(RuntimeEvent::Notice(format!(
                            "could not encode team verification state: {err}"
                        )));
                        return false;
                    }
                };
                match self.store.update_run_mode_state(run_id, &json) {
                    Ok(updated) => step
                        .events
                        .push(RuntimeEvent::RunUpdated { run: updated }),
                    Err(err) => {
                        self.report_store_error("save team verification state", err, step);
                        return false;
                    }
                }
                match self
                    .store
                    .update_run_status(run_id, RunStatus::Verifying)
                {
                    Ok(updated) => step
                        .events
                        .push(RuntimeEvent::RunUpdated { run: updated }),
                    Err(err) => {
                        self.report_store_error("save team verification status", err, step);
                        return false;
                    }
                }
                step.events
                    .push(RuntimeEvent::Notice(format!("verifying {run_id}: {cmd}")));
                step.verify_actions.push(VerifyAction {
                    run_id,
                    command: cmd,
                    workspace: self.config.workspace.clone(),
                    cancel: Arc::new(AtomicBool::new(false)),
                });
                return true;
            }
        }
        match self.store.update_run_status(run_id, RunStatus::Done) {
            Ok(updated) => {
                step.events.push(RuntimeEvent::RunUpdated { run: updated });
                true
            }
            Err(err) => {
                self.report_store_error("finish the run", err, step);
                false
            }
        }
    }

    /// Auto-continue a team run after verification failure (no ModeSession).
    fn team_verify_failure_continue(
        &mut self,
        run: &RunSummary,
        command: &str,
        summary: &str,
        step: &mut RuntimeStep,
    ) {
        let mut status = run
            .mode
            .as_ref()
            .map(|m| m.state.clone())
            .unwrap_or_default();
        let mut iteration = if status.iteration == 0 {
            1
        } else {
            status.iteration
        };
        let max_iterations = if status.max_iterations == 0 {
            resolve_team_limits(&self.config)
                .map(|l| l.max_iterations)
                .unwrap_or(3)
        } else {
            status.max_iterations
        };

        if iteration >= max_iterations {
            step.events.push(RuntimeEvent::Notice(format!(
                "verification failed after {max_iterations} attempts — team run failed"
            )));
            return;
        }

        iteration = iteration.saturating_add(1);
        status.iteration = iteration;
        status.max_iterations = max_iterations;
        status.phase = "coordinating".to_string();
        if let Ok(json) = serde_json::to_string(&status)
            && let Err(err) = self.store.update_run_mode_state(run.id, &json)
        {
            self.report_store_error("save team verification state", err, step);
            return;
        }

        let coordinator = run
            .coordinator
            .as_ref()
            .and_then(|id| self.config.find(id.as_str()).map(|m| m.id.clone()))
            .or_else(|| self.config.members.first().map(|m| m.id.clone()));
        let Some(id) = coordinator else {
            step.events
                .push(RuntimeEvent::Notice("team has no members".to_string()));
            return;
        };

        let turn = match self.store.create_turn() {
            Ok(turn) => turn,
            Err(err) => {
                step.events
                    .push(RuntimeEvent::Notice(format!("store error: {err}")));
                return;
            }
        };

        // continue_run clears verification columns and bumps attempt; store first.
        match self.store.continue_run(run.id, None) {
            Ok(updated) => {
                self.failed_runs.remove(&run.id);
                step.events.push(RuntimeEvent::RunUpdated { run: updated });
            }
            Err(err) => {
                self.report_store_error("continue the failed team run", err, step);
                return;
            }
        }

        let display_body = format!("[team {}] auto-continue after verify failure", run.id);
        if let Err(err) =
            self.store
                .record_user(turn, std::slice::from_ref(&id), &display_body)
        {
            self.report_store_error("save a verification repair dispatch", err, step);
            self.block_mode_run(run.id, "could not persist the repair dispatch", step);
            return;
        }
        step.events.push(RuntimeEvent::TurnStarted { turn });
        step.events.push(RuntimeEvent::UserMessage {
            turn,
            targets: vec![id.clone()],
            body: display_body.clone(),
        });
        step.events.push(RuntimeEvent::Notice(format!(
            "team {} continued → {id} (verify repair {iteration}/{max_iterations})",
            run.id
        )));

        let prompt = team_continue_prompt(
            run.id,
            &run.goal,
            "failed",
            Some((command, false, summary)),
            None,
            true,
        );
        self.run_turns.insert(turn, run.id);
        self.enqueue_prompt(&id, turn, prompt, step);
    }

    fn handle_note_run(
        &mut self,
        run_id: Option<RunId>,
        note: String,
        step: &mut RuntimeStep,
    ) {
        let Some(run) = self.run_or_latest(run_id, "annotate", step) else {
            return;
        };
        match self.store.add_run_note(run.id, &note) {
            Ok(run) => {
                let id = run.id;
                step.events.push(RuntimeEvent::RunUpdated { run });
                step.events
                    .push(RuntimeEvent::Notice(format!("run {id} note recorded")));
            }
            Err(err) => step.events.push(RuntimeEvent::Notice(format!(
                "could not record note: {err}"
            ))),
        }
    }

    fn handle_block_run(
        &mut self,
        run_id: Option<RunId>,
        reason: String,
        step: &mut RuntimeStep,
    ) {
        let Some(run) = self.run_or_latest(run_id, "block", step) else {
            return;
        };
        if run.status == RunStatus::Verifying {
            step.events.push(RuntimeEvent::Notice(format!(
                "{} is verifying; press Esc before marking it blocked",
                run.id
            )));
            return;
        }
        match self.store.block_run(run.id, &reason) {
            Ok(run) => {
                let id = run.id;
                self.failed_runs.insert(id);
                step.events.push(RuntimeEvent::RunUpdated { run });
                step.events
                    .push(RuntimeEvent::Notice(format!("run {id} blocked")));
            }
            Err(err) => step.events.push(RuntimeEvent::Notice(format!(
                "could not mark run blocked: {err}"
            ))),
        }
    }

    fn handle_verify_run(
        &mut self,
        run_id: Option<RunId>,
        command: Option<String>,
        step: &mut RuntimeStep,
    ) {
        let Some(run) = self.run_or_latest(run_id, "verify", step) else {
            return;
        };
        // The mode engine owns verification for live sessions; a manual /verify
        // mid-phase would fight the FSM over the run status.
        if self.mode_sessions.contains_key(&run.id)
            || self.run_turns.values().any(|active| *active == run.id)
        {
            step.events.push(RuntimeEvent::Notice(format!(
                "{} is an active mode run — press Esc to cancel it before manual verification",
                run.id
            )));
            return;
        }
        if run.status == RunStatus::Verifying {
            step.events.push(RuntimeEvent::Notice(format!(
                "{} is already verifying",
                run.id
            )));
            return;
        }
        let command = command
            .or_else(|| suggested_verify_command(&self.config.workspace).map(ToString::to_string));
        let Some(command) = command else {
            step.events.push(RuntimeEvent::Notice(
                "no verification command found (pass /verify [run-id] <command>)".to_string(),
            ));
            return;
        };

        let run_id = run.id;
        let updated = match self
            .store
            .update_run_status(run_id, RunStatus::Verifying)
        {
            Ok(run) => run,
            Err(err) => {
                self.report_store_error("start verification", err, step);
                return;
            }
        };
        step.events.push(RuntimeEvent::RunUpdated { run: updated });
        step.events.push(RuntimeEvent::Notice(format!(
            "verifying {}: {command}",
            run_id
        )));
        step.verify_actions.push(VerifyAction {
            run_id,
            command,
            workspace: self.config.workspace.clone(),
            cancel: Arc::new(AtomicBool::new(false)),
        });
    }

    fn handle_add_run_step(
        &mut self,
        run_id: Option<RunId>,
        owner: Option<MemberId>,
        title: String,
        step: &mut RuntimeStep,
    ) {
        if let Some(owner) = &owner
            && self.config.member(owner).is_none()
        {
            step.events
                .push(RuntimeEvent::Notice(format!("unknown step owner: {owner}")));
            return;
        }
        let Some(run) = self.run_or_latest(run_id, "add a step to", step) else {
            return;
        };
        match self.store.add_run_step(run.id, owner.as_ref(), &title) {
            Ok(run) => {
                let id = run.id;
                step.events.push(RuntimeEvent::RunUpdated { run });
                let suffix = owner
                    .as_ref()
                    .map(|owner| format!(" for @{owner}"))
                    .unwrap_or_default();
                step.events.push(RuntimeEvent::Notice(format!(
                    "run {id} step added{suffix}"
                )));
            }
            Err(err) => step.events.push(RuntimeEvent::Notice(format!(
                "could not add run step: {err}"
            ))),
        }
    }

    fn handle_update_run_step(
        &mut self,
        run_id: Option<RunId>,
        step_number: u32,
        status: RunStepStatus,
        note: Option<String>,
        step: &mut RuntimeStep,
    ) {
        let Some(run) = self.run_or_latest(run_id, "update a step on", step) else {
            return;
        };
        match self
            .store
            .update_run_step(run.id, step_number, status, note.as_deref())
        {
            Ok(run) => {
                let id = run.id;
                step.events.push(RuntimeEvent::RunUpdated { run });
                step.events.push(RuntimeEvent::Notice(format!(
                    "run {id} step #{step_number} marked {}",
                    status.as_str()
                )));
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                step.events.push(RuntimeEvent::Notice(format!(
                    "{} step #{step_number} was not found",
                    run.id
                )));
            }
            Err(err) => step.events.push(RuntimeEvent::Notice(format!(
                "could not update run step: {err}"
            ))),
        }
    }

    fn handle_rename_run_step(
        &mut self,
        run_id: Option<RunId>,
        step_number: u32,
        title: String,
        step: &mut RuntimeStep,
    ) {
        let Some(run) = self.run_or_latest(run_id, "rename a step on", step) else {
            return;
        };
        match self.store.rename_run_step(run.id, step_number, &title) {
            Ok(run) => {
                let id = run.id;
                step.events.push(RuntimeEvent::RunUpdated { run });
                step.events.push(RuntimeEvent::Notice(format!(
                    "run {id} step #{step_number} renamed"
                )));
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                step.events.push(RuntimeEvent::Notice(format!(
                    "{} step #{step_number} was not found",
                    run.id
                )));
            }
            Err(err) => step.events.push(RuntimeEvent::Notice(format!(
                "could not rename run step: {err}"
            ))),
        }
    }

    fn handle_remove_run_step(
        &mut self,
        run_id: Option<RunId>,
        step_number: u32,
        step: &mut RuntimeStep,
    ) {
        let Some(run) = self.run_or_latest(run_id, "remove a step from", step) else {
            return;
        };
        match self.store.remove_run_step(run.id, step_number) {
            Ok(run) => {
                let id = run.id;
                step.events.push(RuntimeEvent::RunUpdated { run });
                step.events.push(RuntimeEvent::Notice(format!(
                    "run {id} step #{step_number} removed"
                )));
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                step.events.push(RuntimeEvent::Notice(format!(
                    "{} step #{step_number} was not found",
                    run.id
                )));
            }
            Err(err) => step.events.push(RuntimeEvent::Notice(format!(
                "could not remove run step: {err}"
            ))),
        }
    }

    fn handle_assign_run_step(
        &mut self,
        run_id: Option<RunId>,
        step_number: u32,
        owner: Option<MemberId>,
        step: &mut RuntimeStep,
    ) {
        if let Some(owner) = &owner
            && self.config.member(owner).is_none()
        {
            step.events
                .push(RuntimeEvent::Notice(format!("unknown step owner: {owner}")));
            return;
        }
        let Some(run) = self.run_or_latest(run_id, "assign a step on", step) else {
            return;
        };
        match self
            .store
            .assign_run_step(run.id, step_number, owner.as_ref())
        {
            Ok(run) => {
                let id = run.id;
                step.events.push(RuntimeEvent::RunUpdated { run });
                let label = owner
                    .as_ref()
                    .map(|owner| format!("@{owner}"))
                    .unwrap_or_else(|| "unassigned".to_string());
                step.events.push(RuntimeEvent::Notice(format!(
                    "run {id} step #{step_number} assigned to {label}"
                )));
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                step.events.push(RuntimeEvent::Notice(format!(
                    "{} step #{step_number} was not found",
                    run.id
                )));
            }
            Err(err) => step.events.push(RuntimeEvent::Notice(format!(
                "could not assign run step: {err}"
            ))),
        }
    }

    fn run_or_latest(
        &self,
        run_id: Option<RunId>,
        verb: &str,
        step: &mut RuntimeStep,
    ) -> Option<RunSummary> {
        match run_id {
            Some(id) => match self.store.active_run(id) {
                Ok(run) => Some(run),
                Err(_) => {
                    step.events
                        .push(RuntimeEvent::Notice(format!("{id} was not found")));
                    None
                }
            },
            None => {
                match self.store.latest_run() {
                    Ok(Some(run)) => Some(run),
                    Ok(None) => {
                        step.events
                            .push(RuntimeEvent::Notice(format!("no run to {verb}")));
                        None
                    }
                    Err(err) => {
                        step.events.push(RuntimeEvent::Notice(format!(
                            "could not load the latest run: {err}"
                        )));
                        None
                    }
                }
            }
        }
    }

    pub fn on_verify_output(&mut self, output: VerifyOutput) -> RuntimeStep {
        let mut step = RuntimeStep::default();
        let cancelled = output.cancelled;
        let ok = output.ok && !cancelled && output.start_error.is_none();
        let summary = if cancelled {
            "verification cancelled".to_string()
        } else if let Some(err) = &output.start_error {
            format!("could not start verification: {err}")
        } else {
            summarize_verify_output(&output.stdout, &output.stderr)
        };
        let saved = if cancelled {
            self.store
                .cancel_run_verification(output.run_id, &output.command, &summary)
        } else {
            self.store
                .set_run_verification(output.run_id, &output.command, ok, &summary)
        };
        match saved {
            Ok(run) => {
                if ok || cancelled {
                    self.failed_runs.remove(&output.run_id);
                } else {
                    self.failed_runs.insert(output.run_id);
                }
                step.events.push(RuntimeEvent::RunUpdated { run });
                step.events.push(RuntimeEvent::Notice(format!(
                    "verification {}: {}",
                    if cancelled {
                        "cancelled"
                    } else if ok {
                        "passed"
                    } else {
                        "failed"
                    },
                    summary
                )));
            }
            Err(err) => {
                step.events.push(RuntimeEvent::Notice(format!(
                    "could not save verification result: {err}"
                )));
                return step;
            }
        }

        // Review/plan ModeSession path (Verifying phase).
        let verifying = self
            .mode_sessions
            .get(&output.run_id)
            .is_some_and(|session| session.phase == ModePhase::Verifying);
        if verifying {
            if ok {
                // Pass: store already set Done; free the live session.
                self.mode_sessions.remove(&output.run_id);
                return step;
            }

            // User cancelled verification: no repair loop.
            if cancelled {
                self.mode_sessions.remove(&output.run_id);
                return step;
            }

            let Some(session) = self.mode_sessions.get(&output.run_id).cloned() else {
                return step;
            };
            // Only review/plan enter Verifying with a ModeSession.
            if !matches!(session.mode, CollabMode::Review | CollabMode::Plan) {
                self.mode_sessions.remove(&output.run_id);
                return step;
            }

            let next_iteration = session.iteration.saturating_add(1);
            if next_iteration > session.max_iterations {
                self.mode_sessions.remove(&output.run_id);
                // Keep Failed status + failed_runs entry from set_run_verification.
                step.events.push(RuntimeEvent::Notice(format!(
                    "verification failed after {} iterations — run failed",
                    session.max_iterations
                )));
                return step;
            }

            // Bounded repair loop: clear failure marker before re-dispatch.
            self.failed_runs.remove(&output.run_id);
            let feedback = format!("verification failed: {}\n{summary}", output.command);
            let mode = session.mode;
            let task = session.task.clone();
            let max_iterations = session.max_iterations;
            let builder = session.builder.clone();
            let leader = session.leader.clone();

            if let Some(s) = self.mode_sessions.get_mut(&output.run_id) {
                s.iteration = next_iteration;
                s.last_feedback = Some(feedback.clone());
                s.pending_verdict = None;
                s.reviewer_nudged = false;
                s.owner_nudged = false;
                s.builder_output.clear();
                match mode {
                    CollabMode::Plan => s.phase = ModePhase::Planning,
                    _ => s.phase = ModePhase::Building,
                }
            }

            // Store already wrote Failed; write Running before the UI event.
            let run = match self
                .store
                .update_run_status(output.run_id, RunStatus::Running)
            {
                Ok(run) => run,
                Err(err) => {
                    self.report_store_error("resume after failed verification", err, &mut step);
                    self.failed_runs.insert(output.run_id);
                    return step;
                }
            };
            step.events.push(RuntimeEvent::RunUpdated { run });
            if !self.persist_mode_state(output.run_id, &mut step) {
                self.failed_runs.insert(output.run_id);
                return step;
            }

            let (target, prompt, label) = match mode {
                CollabMode::Plan => (
                    leader.clone(),
                    plan_verify_failure_prompt(
                        &task,
                        &output.command,
                        &summary,
                        next_iteration,
                        max_iterations,
                    ),
                    "re-plan after verify failure",
                ),
                _ => (
                    builder.clone(),
                    verify_failure_prompt(
                        &task,
                        &output.command,
                        &summary,
                        next_iteration,
                        max_iterations,
                    ),
                    "repair after verify failure",
                ),
            };
            self.mode_dispatch(
                output.run_id,
                std::slice::from_ref(&target),
                prompt,
                format!(
                    "[{mode} {} · iter {next_iteration}/{max_iterations}] → {target}: {label}",
                    output.run_id
                ),
                &mut step,
            );
            return step;
        }

        // Team-mode path: no ModeSession. Only when this run is Team and not
        // owned by a live collab ModeSession (already guarded by `verifying`).
        // The repair loop applies only to auto-verify gate failures: the gate
        // stamps mode_state phase "verifying" before dispatching. A manual
        // /verify failure leaves the run Failed for the user to inspect.
        if self.mode_sessions.contains_key(&output.run_id) {
            return step;
        }
        if ok || cancelled {
            return step;
        }
        if let Ok(run) = self.store.run(output.run_id)
            && run
                .mode
                .as_ref()
                .is_some_and(|mode| mode.mode == CollabMode::Team && mode.state.phase == "verifying")
        {
            self.team_verify_failure_continue(&run, &output.command, &summary, &mut step);
        }
        step
    }
}

/// Checklist / coordination requirements shared by team start and continue prompts.
fn team_checklist_requirements() -> &'static str {
    "Checklist discipline (required):\n\
     1. Before delegating, emit `@@run_step {\"action\":\"add\",...}` lines for the major steps \
(owners are optional for team runs).\n\
     2. Keep step statuses current with `@@run_step` as work progresses.\n\
     3. Before ending your turn, ensure every step is done or blocked and post a final outcome summary."
}

fn team_start_prompt(goal: &str, teammates: &str) -> String {
    format!(
        "Coordinate this goal with the Asterline team.\n\nGoal: {goal}\n\n\
         {}\n\
         {}\n\n\
         Plan the work, emit the checklist first, then delegate to teammates through the team protocol. \
         Add a teammate first if the roster lacks a needed specialty. \
         Teammates: {}.",
        team_skill_hint(),
        team_checklist_requirements(),
        teammates
    )
}

/// Shared continue / verify-repair prompt for team runs.
///
/// `verify_repair` adds a fix-and-redelegate instruction after a failed gate.
fn team_continue_prompt(
    run_id: RunId,
    goal: &str,
    status: &str,
    verification: Option<(&str, bool, &str)>,
    note: Option<&str>,
    verify_repair: bool,
) -> String {
    let verification = verification
        .map(|(command, ok, summary)| {
            format!(
                "\nPrevious verification: {command} ({})\nSummary:\n{summary}",
                if ok { "passed" } else { "failed" }
            )
        })
        .unwrap_or_default();
    let note = note
        .map(|note| format!("\nUser note: {note}"))
        .unwrap_or_default();
    let repair = if verify_repair {
        "\nVerification failed — fix the failures, update the checklist, and re-delegate as needed."
    } else {
        ""
    };
    format!(
        "Continue team run {run_id}.\n\nGoal: {goal}\nCurrent status: {status}{verification}{note}{repair}\n\n\
         {}\n\
         {}\n\n\
         Review the current state, continue the plan, delegate through the team protocol, \
         and report what changed. If the roster lacks a needed specialty, add a teammate first.",
        team_skill_hint(),
        team_checklist_requirements()
    )
}
