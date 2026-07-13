impl TeamRuntime {
    fn handle_run_workflow(&mut self, goal: String, step: &mut RuntimeStep) {
        let coordinator = self
            .config
            .members
            .iter()
            .find(|m| m.role.to_lowercase().contains("plan"))
            .or_else(|| self.config.members.first())
            .map(|m| m.id.clone());
        let Some(id) = coordinator else {
            step.events.push(RuntimeEvent::Notice(
                "no members for a workflow".to_string(),
            ));
            return;
        };

        let run = match self.store.create_workflow_run(&goal, Some(&id)) {
            Ok(run) => run,
            Err(err) => {
                step.events.push(RuntimeEvent::Notice(format!(
                    "could not create workflow run: {err}"
                )));
                return;
            }
        };
        let run_id = run.id;
        step.events.push(RuntimeEvent::WorkflowRunUpdated { run });

        let teammates: Vec<String> = self
            .config
            .members
            .iter()
            .filter(|m| m.id != id)
            .map(|m| format!("{} ({})", m.id, m.role))
            .collect();
        let prompt = format!(
            "Coordinate this goal as a team workflow.\n\nGoal: {goal}\n\n\
             {}\n\
             Plan the work, delegate to teammates through the team protocol, and add a \
             teammate first if the roster lacks a needed specialty. \
             Teammates: {}.",
            team_skill_hint(),
            teammates.join(", ")
        );
        let turn = match self.store.create_turn() {
            Ok(turn) => turn,
            Err(err) => {
                step.events
                    .push(RuntimeEvent::Notice(format!("store error: {err}")));
                return;
            }
        };
        let display_body = format!("/plan {goal}");
        let _ = self
            .store
            .record_user(turn, std::slice::from_ref(&id), &display_body);
        step.events.push(RuntimeEvent::TurnStarted { turn });
        step.events.push(RuntimeEvent::UserMessage {
            turn,
            targets: vec![id.clone()],
            body: display_body.clone(),
        });
        self.log(
            &id,
            LogEntry::info("user", format!("workflow {run_id} → {id}: {goal}")),
            step,
        );
        step.events.push(RuntimeEvent::Notice(format!(
            "workflow {run_id} started → {id}"
        )));
        self.workflow_turns.insert(turn, run_id);
        self.enqueue_prompt(&id, turn, prompt, step);
    }

    fn handle_continue_workflow(
        &mut self,
        run_id: Option<WorkflowRunId>,
        note: Option<String>,
        step: &mut RuntimeStep,
    ) {
        let Some(run) = self.workflow_run_or_latest(run_id, "continue", step) else {
            return;
        };
        if run.mode.is_some() {
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
            WorkflowRunStatus::Running | WorkflowRunStatus::Verifying
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
            step.events.push(RuntimeEvent::Notice(
                "no members for a workflow".to_string(),
            ));
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
        if let Ok(updated) = self.store.continue_workflow_run(run.id, note.as_deref()) {
            step.events
                .push(RuntimeEvent::WorkflowRunUpdated { run: updated });
        }
        self.failed_workflow_runs.remove(&run.id);

        let display_body = match &note {
            Some(note) => format!("/continue {} {note}", run.id),
            None => format!("/continue {}", run.id),
        };
        let _ = self
            .store
            .record_user(turn, std::slice::from_ref(&id), &display_body);
        step.events.push(RuntimeEvent::TurnStarted { turn });
        step.events.push(RuntimeEvent::UserMessage {
            turn,
            targets: vec![id.clone()],
            body: display_body.clone(),
        });
        self.log(
            &id,
            LogEntry::info("user", format!("workflow {} continued → {id}", run.id)),
            step,
        );
        step.events.push(RuntimeEvent::Notice(format!(
            "workflow {} continued → {id}",
            run.id
        )));

        let verification = run
            .verification
            .as_ref()
            .map(|verification| {
                format!(
                    "\nPrevious verification: {} ({})\nSummary:\n{}",
                    verification.command,
                    if verification.ok { "passed" } else { "failed" },
                    verification.summary
                )
            })
            .unwrap_or_default();
        let note = note
            .as_deref()
            .map(|note| format!("\nUser note: {note}"))
            .unwrap_or_default();
        let prompt = format!(
            "Continue workflow run {}.\n\nGoal: {}\nCurrent status: {}{}{}\n\n\
             {}\n\
             Review the current state, continue the plan, delegate through the team protocol, \
             and report what changed. If the roster lacks a needed specialty, add a teammate first.",
            run.id,
            run.goal,
            run.status.as_str(),
            verification,
            note,
            team_skill_hint()
        );
        self.workflow_turns.insert(turn, run.id);
        self.enqueue_prompt(&id, turn, prompt, step);
    }

    fn handle_note_workflow(
        &mut self,
        run_id: Option<WorkflowRunId>,
        note: String,
        step: &mut RuntimeStep,
    ) {
        let Some(run) = self.workflow_run_or_latest(run_id, "annotate", step) else {
            return;
        };
        match self.store.add_workflow_note(run.id, &note) {
            Ok(run) => {
                let id = run.id;
                step.events.push(RuntimeEvent::WorkflowRunUpdated { run });
                step.events
                    .push(RuntimeEvent::Notice(format!("workflow {id} note recorded")));
            }
            Err(err) => step.events.push(RuntimeEvent::Notice(format!(
                "could not record note: {err}"
            ))),
        }
    }

    fn handle_block_workflow(
        &mut self,
        run_id: Option<WorkflowRunId>,
        reason: String,
        step: &mut RuntimeStep,
    ) {
        let Some(run) = self.workflow_run_or_latest(run_id, "block", step) else {
            return;
        };
        if run.status == WorkflowRunStatus::Verifying {
            step.events.push(RuntimeEvent::Notice(format!(
                "{} is verifying; /abort before marking it blocked",
                run.id
            )));
            return;
        }
        self.failed_workflow_runs.insert(run.id);
        match self.store.block_workflow_run(run.id, &reason) {
            Ok(run) => {
                let id = run.id;
                step.events.push(RuntimeEvent::WorkflowRunUpdated { run });
                step.events
                    .push(RuntimeEvent::Notice(format!("workflow {id} blocked")));
            }
            Err(err) => step.events.push(RuntimeEvent::Notice(format!(
                "could not mark workflow blocked: {err}"
            ))),
        }
    }

    fn handle_verify_workflow(
        &mut self,
        run_id: Option<WorkflowRunId>,
        command: Option<String>,
        step: &mut RuntimeStep,
    ) {
        let Some(run) = self.workflow_run_or_latest(run_id, "verify", step) else {
            return;
        };
        // The mode engine owns verification for live sessions; a manual /verify
        // mid-phase would fight the FSM over the run status.
        if self.mode_sessions.contains_key(&run.id) {
            step.events.push(RuntimeEvent::Notice(format!(
                "{} is an active mode run — /abort it before manual verification",
                run.id
            )));
            return;
        }
        if run.status == WorkflowRunStatus::Verifying {
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

        if let Ok(run) = self
            .store
            .update_workflow_status(run.id, WorkflowRunStatus::Verifying)
        {
            step.events.push(RuntimeEvent::WorkflowRunUpdated { run });
        }
        step.events.push(RuntimeEvent::Notice(format!(
            "verifying {}: {command}",
            run.id
        )));
        step.verify_actions.push(VerifyAction {
            run_id: run.id,
            command,
            workspace: self.config.workspace.clone(),
            cancel: Arc::new(AtomicBool::new(false)),
        });
    }

    fn handle_add_workflow_step(
        &mut self,
        run_id: Option<WorkflowRunId>,
        owner: Option<MemberId>,
        title: String,
        step: &mut RuntimeStep,
    ) {
        let Some(run) = self.workflow_run_or_latest(run_id, "add a step to", step) else {
            return;
        };
        match self.store.add_workflow_step(run.id, owner.as_ref(), &title) {
            Ok(run) => {
                let id = run.id;
                step.events.push(RuntimeEvent::WorkflowRunUpdated { run });
                let suffix = owner
                    .as_ref()
                    .map(|owner| format!(" for @{owner}"))
                    .unwrap_or_default();
                step.events.push(RuntimeEvent::Notice(format!(
                    "workflow {id} step added{suffix}"
                )));
            }
            Err(err) => step.events.push(RuntimeEvent::Notice(format!(
                "could not add workflow step: {err}"
            ))),
        }
    }

    fn handle_update_workflow_step(
        &mut self,
        run_id: Option<WorkflowRunId>,
        step_number: u32,
        status: WorkflowStepStatus,
        note: Option<String>,
        step: &mut RuntimeStep,
    ) {
        let Some(run) = self.workflow_run_or_latest(run_id, "update a step on", step) else {
            return;
        };
        match self
            .store
            .update_workflow_step(run.id, step_number, status, note.as_deref())
        {
            Ok(run) => {
                let id = run.id;
                step.events.push(RuntimeEvent::WorkflowRunUpdated { run });
                step.events.push(RuntimeEvent::Notice(format!(
                    "workflow {id} step #{step_number} marked {}",
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
                "could not update workflow step: {err}"
            ))),
        }
    }

    fn handle_rename_workflow_step(
        &mut self,
        run_id: Option<WorkflowRunId>,
        step_number: u32,
        title: String,
        step: &mut RuntimeStep,
    ) {
        let Some(run) = self.workflow_run_or_latest(run_id, "rename a step on", step) else {
            return;
        };
        match self.store.rename_workflow_step(run.id, step_number, &title) {
            Ok(run) => {
                let id = run.id;
                step.events.push(RuntimeEvent::WorkflowRunUpdated { run });
                step.events.push(RuntimeEvent::Notice(format!(
                    "workflow {id} step #{step_number} renamed"
                )));
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                step.events.push(RuntimeEvent::Notice(format!(
                    "{} step #{step_number} was not found",
                    run.id
                )));
            }
            Err(err) => step.events.push(RuntimeEvent::Notice(format!(
                "could not rename workflow step: {err}"
            ))),
        }
    }

    fn handle_remove_workflow_step(
        &mut self,
        run_id: Option<WorkflowRunId>,
        step_number: u32,
        step: &mut RuntimeStep,
    ) {
        let Some(run) = self.workflow_run_or_latest(run_id, "remove a step from", step) else {
            return;
        };
        match self.store.remove_workflow_step(run.id, step_number) {
            Ok(run) => {
                let id = run.id;
                step.events.push(RuntimeEvent::WorkflowRunUpdated { run });
                step.events.push(RuntimeEvent::Notice(format!(
                    "workflow {id} step #{step_number} removed"
                )));
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                step.events.push(RuntimeEvent::Notice(format!(
                    "{} step #{step_number} was not found",
                    run.id
                )));
            }
            Err(err) => step.events.push(RuntimeEvent::Notice(format!(
                "could not remove workflow step: {err}"
            ))),
        }
    }

    fn handle_assign_workflow_step(
        &mut self,
        run_id: Option<WorkflowRunId>,
        step_number: u32,
        owner: Option<MemberId>,
        step: &mut RuntimeStep,
    ) {
        let Some(run) = self.workflow_run_or_latest(run_id, "assign a step on", step) else {
            return;
        };
        match self
            .store
            .assign_workflow_step(run.id, step_number, owner.as_ref())
        {
            Ok(run) => {
                let id = run.id;
                step.events.push(RuntimeEvent::WorkflowRunUpdated { run });
                let label = owner
                    .as_ref()
                    .map(|owner| format!("@{owner}"))
                    .unwrap_or_else(|| "unassigned".to_string());
                step.events.push(RuntimeEvent::Notice(format!(
                    "workflow {id} step #{step_number} assigned to {label}"
                )));
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                step.events.push(RuntimeEvent::Notice(format!(
                    "{} step #{step_number} was not found",
                    run.id
                )));
            }
            Err(err) => step.events.push(RuntimeEvent::Notice(format!(
                "could not assign workflow step: {err}"
            ))),
        }
    }

    fn workflow_run_or_latest(
        &self,
        run_id: Option<WorkflowRunId>,
        verb: &str,
        step: &mut RuntimeStep,
    ) -> Option<WorkflowRunSummary> {
        match run_id {
            Some(id) => match self.store.workflow_run(id) {
                Ok(run) => Some(run),
                Err(_) => {
                    step.events
                        .push(RuntimeEvent::Notice(format!("{id} was not found")));
                    None
                }
            },
            None => {
                let run = self.store.latest_workflow_run().unwrap_or_default();
                if run.is_none() {
                    step.events
                        .push(RuntimeEvent::Notice(format!("no workflow run to {verb}")));
                }
                run
            }
        }
    }

    pub fn on_verify_output(&mut self, output: VerifyOutput) -> RuntimeStep {
        let mut step = RuntimeStep::default();
        let ok = output.ok && !output.cancelled && output.start_error.is_none();
        let summary = if output.cancelled {
            "verification cancelled".to_string()
        } else if let Some(err) = output.start_error {
            format!("could not start verification: {err}")
        } else {
            summarize_verify_output(&output.stdout, &output.stderr)
        };
        if ok {
            self.failed_workflow_runs.remove(&output.run_id);
        } else {
            self.failed_workflow_runs.insert(output.run_id);
        }
        match self
            .store
            .set_workflow_verification(output.run_id, &output.command, ok, &summary)
        {
            Ok(run) => {
                step.events.push(RuntimeEvent::WorkflowRunUpdated { run });
                step.events.push(RuntimeEvent::Notice(format!(
                    "verification {}: {}",
                    if ok { "passed" } else { "failed" },
                    summary
                )));
            }
            Err(err) => step.events.push(RuntimeEvent::Notice(format!(
                "could not save verification result: {err}"
            ))),
        }
        if self
            .mode_sessions
            .get(&output.run_id)
            .is_some_and(|session| session.phase == ModePhase::Verifying)
        {
            self.mode_sessions.remove(&output.run_id);
            if !ok {
                self.failed_workflow_runs.insert(output.run_id);
            }
        }
        step
    }
}
