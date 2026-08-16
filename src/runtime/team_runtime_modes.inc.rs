// Mode-run handlers (review, plan, brainstorm).
// Included at the bottom of team_runtime.rs so private fields stay accessible.

impl TeamRuntime {
    fn handle_run_mode(
        &mut self,
        mode: CollabMode,
        task: String,
        step: &mut RuntimeStep,
    ) {
        if mode == CollabMode::Team {
            self.handle_run_team(task, step);
            return;
        }
        let task = task.trim().to_string();
        if task.is_empty() {
            step.events
                .push(RuntimeEvent::Notice("mode needs a task".to_string()));
            return;
        }
        if let Some(existing) = self.mode_sessions.values().next() {
            step.events.push(RuntimeEvent::Notice(format!(
                "a {} run is already active — press Esc to cancel it first",
                existing.mode
            )));
            return;
        }

        let (roles, limits) = match resolve_mode_roles(&self.effective_config(), mode) {
            Ok(resolved) => resolved,
            Err(err) => {
                step.events.push(RuntimeEvent::Notice(err));
                return;
            }
        };
        let (plan_builder, plan_reviewer, auto_execute) = if mode == CollabMode::Plan {
            let config = self.effective_config();
            let builder = match resolve_plan_builder(&config) {
                Ok(builder) => builder,
                Err(err) => {
                    step.events.push(RuntimeEvent::Notice(err));
                    return;
                }
            };
            let reviewer = match resolve_plan_reviewer(&config) {
                Ok(reviewer) => reviewer,
                Err(err) => {
                    step.events.push(RuntimeEvent::Notice(err));
                    return;
                }
            };
            (Some(builder), reviewer, resolve_plan_auto_execute(&config))
        } else {
            (None, None, true)
        };

        let (phase, iteration, round) = match mode {
            CollabMode::Review => (ModePhase::Building, 1, 0),
            CollabMode::Plan => (ModePhase::Planning, 1, 0),
            CollabMode::Brainstorm => (ModePhase::Diverging, 0, 1),
            CollabMode::Team => unreachable!("team runs use handle_run_team"),
        };

        let session = ModeSession {
            mode,
            phase,
            task: task.clone(),
            builder: roles.builder.clone(),
            reviewer: plan_reviewer.clone().unwrap_or_else(|| roles.reviewer.clone()),
            leader: roles.leader.clone(),
            plan_builder,
            plan_reviewer,
            participants: roles.participants.clone(),
            iteration,
            max_iterations: limits.max_iterations,
            round,
            rounds: limits.rounds,
            ideas_per_round: limits.ideas_per_round,
            idea_count: 0,
            auto_verify: limits.auto_verify,
            auto_execute,
            verify_command: limits.verify_command.clone(),
            builder_output: String::new(),
            reviewer_nudged: false,
            owner_nudged: false,
            last_feedback: None,
            pending_verdict: None,
            reviewer_last_text: String::new(),
            cancelled: false,
            idea_batches: Vec::new(),
            votes: Vec::new(),
            vote_count: 0,
            brainstorm_summary: String::new(),
        };

        let state_json = match serde_json::to_string(&session) {
            Ok(json) => json,
            Err(err) => {
                step.events.push(RuntimeEvent::Notice(format!(
                    "could not serialize mode state: {err}"
                )));
                return;
            }
        };

        let coordinator = match mode {
            CollabMode::Review => Some(&session.builder),
            CollabMode::Plan => Some(&session.leader),
            CollabMode::Brainstorm => None,
            CollabMode::Team => unreachable!("team runs do not use ModeSession"),
        };

        let run = match self.store.create_mode_run(
            &task,
            coordinator,
            mode,
            &state_json,
        ) {
            Ok(run) => run,
            Err(err) => {
                step.events.push(RuntimeEvent::Notice(format!(
                    "could not create mode run: {err}"
                )));
                return;
            }
        };
        let run_id = run.id;
        step.events.push(RuntimeEvent::RunUpdated { run });
        let task_targets = match mode {
            CollabMode::Review => vec![session.builder.clone()],
            CollabMode::Plan => vec![session.leader.clone()],
            CollabMode::Brainstorm => session.participants.clone(),
            CollabMode::Team => unreachable!("team runs do not use ModeSession"),
        };
        if !self.record_mode_task_message(&task_targets, &task, step) {
            self.block_mode_run(run_id, "could not persist the mode task", step);
            return;
        }

        match mode {
            CollabMode::Review => {
                let verify = format_verify_label(
                    limits.auto_verify,
                    limits.verify_command.as_deref(),
                    suggested_verify_command(&self.config.workspace),
                );
                step.events.push(RuntimeEvent::Notice(format!(
                    "review {run_id} started → {} (reviewer: {}) · {verify}",
                    session.builder, session.reviewer
                )));
                let builder = session.builder.clone();
                self.mode_sessions.insert(run_id, session);
                let prompt = review_task_prompt(&task);
                self.mode_dispatch(
                    run_id,
                    std::slice::from_ref(&builder),
                    prompt,
                    format!(
                        "[{mode} {run_id} · iter 1/{}] → {builder}: {}",
                        limits.max_iterations,
                        short_mode_text(&task)
                    ),
                    step,
                );
            }
            CollabMode::Plan => {
                let builder = session
                    .plan_builder
                    .as_ref()
                    .expect("plan start validates its required builder");
                let reviewer = session
                    .plan_reviewer
                    .as_ref()
                    .map(|reviewer| reviewer.to_string())
                    .unwrap_or_else(|| "none".to_string());
                let execution = if session.auto_execute {
                    "auto execute"
                } else {
                    "manual execution confirmation"
                };
                let verify = format_verify_label(
                    limits.auto_verify,
                    limits.verify_command.as_deref(),
                    suggested_verify_command(&self.config.workspace),
                );
                step.events.push(RuntimeEvent::Notice(format!(
                    "plan {run_id} started → {} (builder: {builder}; reviewer: {reviewer}; {execution}) · {verify}",
                    session.leader
                )));
                let leader = session.leader.clone();
                self.mode_sessions.insert(run_id, session);
                let teammates = self.plan_teammate_list();
                let prompt = plan_plan_prompt(&task, &teammates);
                self.mode_dispatch(
                    run_id,
                    std::slice::from_ref(&leader),
                    prompt,
                    format!(
                        "[{mode} {run_id} · iter 1/{}] → {leader}: plan",
                        limits.max_iterations
                    ),
                    step,
                );
            }
            CollabMode::Brainstorm => {
                let n = session.participants.len();
                let rounds = session.rounds;
                let ideas_per_round = session.ideas_per_round;
                step.events.push(RuntimeEvent::Notice(format!(
                    "brainstorm {run_id} started · {n} participants · {rounds} generation waves · \
                     private voting and ranked synthesis follow"
                )));
                let participants = session.participants.clone();
                self.mode_sessions.insert(run_id, session);
                let prompt = self.with_brainstorm_skill(brainstorm_propose_prompt(
                    &task,
                    n,
                    ideas_per_round,
                ));
                self.mode_dispatch(
                    run_id,
                    &participants,
                    prompt,
                    format!("[{mode} {run_id} · generate 1/{rounds}] blind seed"),
                    step,
                );
            }
            CollabMode::Team => unreachable!("team runs use handle_run_team"),
        }
    }

    /// Persist and display the user's original mode task separately from
    /// internal phase-dispatch messages.
    fn record_mode_task_message(
        &mut self,
        targets: &[MemberId],
        task: &str,
        step: &mut RuntimeStep,
    ) -> bool {
        let turn = match self.store.create_turn() {
            Ok(turn) => turn,
            Err(err) => {
                self.report_store_error("create a mode task turn", err, step);
                return false;
            }
        };
        if let Err(err) = self.store.record_user(turn, targets, task) {
            self.report_store_error("save a mode task", err, step);
            return false;
        }
        step.events.push(RuntimeEvent::TurnStarted { turn });
        step.events.push(RuntimeEvent::UserMessage {
            turn,
            targets: targets.to_vec(),
            body: task.to_string(),
        });
        step.events.push(RuntimeEvent::TurnFinished { turn });
        true
    }

    fn plan_teammate_list(&self) -> Vec<(String, String)> {
        self.config
            .members
            .iter()
            .map(|m| (m.id.to_string(), m.role.clone()))
            .collect()
    }

    /// Dispatch one mode phase as a single turn (approval-gated enqueue).
    fn mode_dispatch(
        &mut self,
        run_id: RunId,
        targets: &[MemberId],
        prompt: String,
        display: String,
        step: &mut RuntimeStep,
    ) {
        let dispatches = targets
            .iter()
            .map(|member| (member.clone(), prompt.clone()))
            .collect();
        self.mode_dispatch_multi(run_id, dispatches, display, step);
    }

    /// One turn for multiple targets with per-member prompts. Approval gating is
    /// per member: risky prompts get their own held approval; clean ones enqueue.
    fn mode_dispatch_multi(
        &mut self,
        run_id: RunId,
        dispatches: Vec<(MemberId, String)>,
        display: String,
        step: &mut RuntimeStep,
    ) {
        if dispatches.is_empty() {
            return;
        }
        let targets: Vec<MemberId> = dispatches.iter().map(|(m, _)| m.clone()).collect();
        let turn = match self.store.create_turn() {
            Ok(turn) => turn,
            Err(err) => {
                self.report_store_error("create a mode dispatch turn", err, step);
                self.block_mode_run(run_id, "could not create a mode dispatch turn", step);
                return;
            }
        };
        if let Err(err) = self.store.record_user(turn, &targets, &display) {
            self.report_store_error("save a mode dispatch", err, step);
            self.block_mode_run(run_id, "could not persist a mode dispatch", step);
            return;
        }
        step.events.push(RuntimeEvent::TurnStarted { turn });
        step.events.push(RuntimeEvent::UserMessage {
            turn,
            targets: targets.clone(),
            body: display,
        });
        self.run_turns.insert(turn, run_id);

        let gate = self.approvals_enabled && self.matcher.applies_to(ApprovalSurface::Mode);
        for (member, prompt) in dispatches {
            if gate && let Some(kind) = self.matcher.classify(&prompt) {
                match self.store.insert_approval(Some(turn), None, &kind, &prompt) {
                    Ok(id) => {
                        self.held_approvals.insert(
                            id,
                            HeldApproval {
                                turn,
                                targets: vec![member],
                                prompt: prompt.clone(),
                                mode_run: Some(run_id),
                                member_request: None,
                            },
                        );
                        step.events.push(RuntimeEvent::ApprovalRequested {
                            id,
                            member: None,
                            action: kind,
                            body: prompt,
                        });
                    }
                    Err(err) => {
                        self.report_store_error("save a mode approval request", err, step);
                        self.block_mode_run(
                            run_id,
                            "could not persist a mode approval request",
                            step,
                        );
                        self.check_turn_complete(turn, step);
                        return;
                    }
                }
                continue;
            }
            self.enqueue_prompt(&member, turn, prompt, step);
        }
    }

    fn mode_on_turn_complete(&mut self, run_id: RunId, step: &mut RuntimeStep) {
        let Some(session) = self.mode_sessions.get(&run_id) else {
            return;
        };
        match session.mode {
            CollabMode::Review => self.mode_review_on_turn_complete(run_id, step),
            CollabMode::Plan => self.mode_plan_on_turn_complete(run_id, step),
            CollabMode::Brainstorm => self.mode_brainstorm_on_turn_complete(run_id, step),
            CollabMode::Team => unreachable!("team runs do not use ModeSession"),
        }
    }

    fn mode_review_on_turn_complete(&mut self, run_id: RunId, step: &mut RuntimeStep) {
        let failed = self.failed_runs.contains(&run_id);
        let Some(session) = self.mode_sessions.get(&run_id).cloned() else {
            return;
        };

        if session.cancelled || failed {
            let reason = if session.cancelled {
                "aborted by user"
            } else {
                "member run failed"
            };
            self.block_mode_run(run_id, reason, step);
            return;
        }

        match session.phase {
            ModePhase::Building => {
                let builder_display = self.member_display(&session.builder);
                let verify_cmd = session.verify_command.as_deref();
                let prompt = review_prompt(
                    &session.task,
                    &builder_display,
                    &session.builder_output,
                    verify_cmd,
                );
                if let Some(session) = self.mode_sessions.get_mut(&run_id) {
                    session.phase = ModePhase::Reviewing;
                    session.reviewer_nudged = false;
                    session.pending_verdict = None;
                    session.reviewer_last_text.clear();
                }
                if !self.persist_mode_state(run_id, step) {
                    return;
                }
                let (reviewer, max_iterations, iteration, mode) = {
                    let s = &self.mode_sessions[&run_id];
                    (s.reviewer.clone(), s.max_iterations, s.iteration, s.mode)
                };
                self.mode_dispatch(
                    run_id,
                    std::slice::from_ref(&reviewer),
                    prompt,
                    format!(
                        "[{mode} {run_id} · iter {iteration}/{max_iterations}] → {reviewer}: review"
                    ),
                    step,
                );
            }
            ModePhase::Reviewing | ModePhase::AwaitingVerdict => {
                self.mode_handle_verdict_phase(run_id, &session, step);
            }
            ModePhase::Verifying => {
                // Verification is external; agent turns should not complete in this phase.
            }
            _ => {}
        }
    }

    /// Shared approve / request_changes / nudge path for Review and Plan review phases.
    fn mode_handle_verdict_phase(
        &mut self,
        run_id: RunId,
        session: &ModeSession,
        step: &mut RuntimeStep,
    ) {
        let pending = self
            .mode_sessions
            .get_mut(&run_id)
            .and_then(|s| s.pending_verdict.take());
        match pending {
            Some(PersistedReviewVerdict {
                verdict: PersistedReviewVerdictKind::Approve,
                summary: _,
            }) => {
                if !self.persist_mode_state(run_id, step) {
                    return;
                }
                if session.mode == CollabMode::Plan {
                    let Some(builder) = session.plan_builder.clone() else {
                        self.block_mode_run(run_id, "plan mode needs a builder", step);
                        return;
                    };
                    self.mode_plan_dispatch_builder(run_id, &builder, step);
                    return;
                }
                self.mode_start_verification_or_finish(run_id, step);
            }
            Some(PersistedReviewVerdict {
                verdict: PersistedReviewVerdictKind::RequestChanges,
                summary,
            }) => {
                let feedback = summary
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| "(no summary provided)".to_string());
                self.mode_request_changes(run_id, feedback, step);
            }
            None if session.phase == ModePhase::Reviewing && !session.reviewer_nudged => {
                if let Some(s) = self.mode_sessions.get_mut(&run_id) {
                    s.reviewer_nudged = true;
                    s.phase = ModePhase::AwaitingVerdict;
                }
                if !self.persist_mode_state(run_id, step) {
                    return;
                }
                let (reviewer, max_iterations, iteration, mode) = {
                    let s = &self.mode_sessions[&run_id];
                    (s.reviewer.clone(), s.max_iterations, s.iteration, s.mode)
                };
                self.mode_dispatch(
                    run_id,
                    std::slice::from_ref(&reviewer),
                    verdict_nudge_prompt(),
                    format!(
                        "[{mode} {run_id} · iter {iteration}/{max_iterations}] → {reviewer}: verdict nudge"
                    ),
                    step,
                );
            }
            None => {
                // AwaitingVerdict with no structured verdict: treat free text as changes.
                let feedback = self
                    .mode_sessions
                    .get(&run_id)
                    .map(|s| {
                        let text = s.reviewer_last_text.trim();
                        if text.is_empty() {
                            "(reviewer gave no verdict)".to_string()
                        } else {
                            text.to_string()
                        }
                    })
                    .unwrap_or_else(|| "(reviewer gave no verdict)".to_string());
                step.events.push(RuntimeEvent::Notice(format!(
                    "{} {run_id}: {} gave no structured @@review verdict — treating the reply as request_changes",
                    session.mode, session.reviewer
                )));
                self.mode_request_changes(run_id, feedback, step);
            }
        }
    }

    fn mode_start_verification_or_finish(&mut self, run_id: RunId, step: &mut RuntimeStep) {
        let (auto_verify, configured) = self
            .mode_sessions
            .get(&run_id)
            .map(|s| (s.auto_verify, s.verify_command.clone()))
            .unwrap_or((false, None));
        if auto_verify
            && let Some(cmd) = crate::domain::mode::resolve_verify_command(
                configured.as_deref(),
                suggested_verify_command(&self.config.workspace),
            )
        {
            if let Some(session) = self.mode_sessions.get_mut(&run_id) {
                session.phase = ModePhase::Verifying;
            }
            if !self.persist_mode_state(run_id, step) {
                return;
            }
            match self.store.update_run_status(run_id, RunStatus::Verifying) {
                Ok(run) => step.events.push(RuntimeEvent::RunUpdated { run }),
                Err(err) => {
                    self.report_store_error("start mode verification", err, step);
                    return;
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
            return;
        }
        self.finish_mode_run_approved(run_id, step);
    }

    fn mode_plan_dispatch_builder(
        &mut self,
        run_id: RunId,
        builder: &MemberId,
        step: &mut RuntimeStep,
    ) {
        let steps = match self.store.run_steps_all(run_id) {
            Ok(steps) => steps,
            Err(err) => {
                self.report_store_error("load the approved plan checklist", err, step);
                self.block_mode_run(run_id, "approved plan checklist is unavailable", step);
                return;
            }
        };
        let executable = steps
            .iter()
            .filter(|item| item.status != RunStepStatus::Done)
            .map(|item| (item.number, item.title.clone()))
            .collect::<Vec<_>>();
        if executable.is_empty() {
            self.block_mode_run(run_id, "approved plan has no executable steps", step);
            return;
        }

        let mut last_run = None;
        for (number, _) in &executable {
            match self.store.assign_run_step(run_id, *number, Some(builder)) {
                Ok(_) => {}
                Err(err) => {
                    self.report_store_error("assign an approved plan step to the builder", err, step);
                    self.block_mode_run(run_id, "could not dispatch the approved plan", step);
                    return;
                }
            }
            match self
                .store
                .update_run_step(run_id, *number, RunStepStatus::Doing, None)
            {
                Ok(run) => last_run = Some(run),
                Err(err) => {
                    self.report_store_error("start an approved plan step", err, step);
                    self.block_mode_run(run_id, "could not dispatch the approved plan", step);
                    return;
                }
            }
        }
        if let Some(run) = last_run {
            step.events.push(RuntimeEvent::RunUpdated { run });
        }
        let (leader, mode, iteration, max_iterations, auto_execute) = {
            let session = &self.mode_sessions[&run_id];
            (
                session.leader.clone(),
                session.mode,
                session.iteration,
                session.max_iterations,
                session.auto_execute,
            )
        };
        if let Some(session) = self.mode_sessions.get_mut(&run_id) {
            session.phase = if auto_execute {
                ModePhase::Executing
            } else {
                ModePhase::AwaitingExecution
            };
            session.owner_nudged = false;
        }
        if !self.persist_mode_state(run_id, step) {
            return;
        }
        let prompt = step_dispatch_prompt(run_id, &leader, &executable);
        let display = format!(
            "[{mode} {run_id} · iter {iteration}/{max_iterations}] → {builder}: execute approved plan"
        );
        if auto_execute {
            self.mode_dispatch(run_id, std::slice::from_ref(builder), prompt, display, step);
        } else {
            self.mode_request_plan_execution_approval(run_id, builder, prompt, display, step);
        }
    }

    fn mode_request_plan_execution_approval(
        &mut self,
        run_id: RunId,
        builder: &MemberId,
        prompt: String,
        display: String,
        step: &mut RuntimeStep,
    ) {
        let turn = match self.store.create_turn() {
            Ok(turn) => turn,
            Err(err) => {
                self.report_store_error("create a plan execution confirmation", err, step);
                self.block_mode_run(run_id, "could not request plan execution confirmation", step);
                return;
            }
        };
        if let Err(err) = self.store.record_user(turn, std::slice::from_ref(builder), &display) {
            self.report_store_error("save a plan execution confirmation", err, step);
            self.block_mode_run(run_id, "could not request plan execution confirmation", step);
            return;
        }
        step.events.push(RuntimeEvent::TurnStarted { turn });
        step.events.push(RuntimeEvent::UserMessage {
            turn,
            targets: vec![builder.clone()],
            body: display.clone(),
        });
        self.run_turns.insert(turn, run_id);
        let body = format!("{display}\n\nApprove to send the approved plan to {builder}.");
        match self
            .store
            .insert_approval(Some(turn), None, "plan_execution", &body)
        {
            Ok(id) => {
                self.held_approvals.insert(
                    id,
                    HeldApproval {
                        turn,
                        targets: vec![builder.clone()],
                        prompt,
                        mode_run: Some(run_id),
                        member_request: None,
                    },
                );
                step.events.push(RuntimeEvent::ApprovalRequested {
                    id,
                    member: None,
                    action: "plan_execution".to_string(),
                    body,
                });
            }
            Err(err) => {
                self.report_store_error("save a plan execution confirmation", err, step);
                self.block_mode_run(run_id, "could not request plan execution confirmation", step);
                self.check_turn_complete(turn, step);
            }
        }
    }

    fn mode_plan_confirm_execution(&mut self, run_id: RunId, step: &mut RuntimeStep) -> bool {
        let Some(session) = self.mode_sessions.get_mut(&run_id) else {
            return false;
        };
        if session.mode != CollabMode::Plan || session.phase != ModePhase::AwaitingExecution {
            return true;
        }
        session.phase = ModePhase::Executing;
        session.owner_nudged = false;
        self.persist_mode_state(run_id, step)
    }

    fn mode_plan_on_turn_complete(&mut self, run_id: RunId, step: &mut RuntimeStep) {
        let failed = self.failed_runs.contains(&run_id);
        let Some(session) = self.mode_sessions.get(&run_id).cloned() else {
            return;
        };

        if session.cancelled {
            self.block_mode_run(run_id, "aborted by user", step);
            return;
        }

        if failed {
            // Executing: try a bounded re-plan instead of hard-blocking the whole run.
            if session.phase == ModePhase::Executing {
                self.mode_plan_on_member_failure(run_id, &session, step);
                return;
            }
            self.block_mode_run(run_id, "member run failed", step);
            return;
        }

        match session.phase {
            ModePhase::Planning => self.mode_plan_on_planning_complete(run_id, &session, step),
            ModePhase::Executing => self.mode_plan_on_executing_complete(run_id, &session, step),
            ModePhase::Reviewing | ModePhase::AwaitingVerdict | ModePhase::Verifying => {
                self.mode_review_on_turn_complete(run_id, step);
            }
            _ => {}
        }
    }

    /// Owner process failure during Executing: re-plan when iterations remain.
    fn mode_plan_on_member_failure(
        &mut self,
        run_id: RunId,
        session: &ModeSession,
        step: &mut RuntimeStep,
    ) {
        let next_iteration = session.iteration.saturating_add(1);
        if next_iteration > session.max_iterations {
            self.block_mode_run(
                run_id,
                &format!("max iterations reached ({})", session.max_iterations),
                step,
            );
            return;
        }

        let steps = match self.store.run_steps_all(run_id) {
            Ok(steps) => steps,
            Err(err) => {
                self.report_store_error("load the plan checklist", err, step);
                self.block_mode_run(run_id, "plan checklist is unavailable", step);
                return;
            }
        };
        let unfinished_lines = format_unfinished_step_lines(
            &steps
                .iter()
                .filter(|s| s.status != RunStepStatus::Done)
                .collect::<Vec<_>>(),
        );

        if let Some(s) = self.mode_sessions.get_mut(&run_id) {
            s.iteration = next_iteration;
            s.phase = ModePhase::Planning;
            s.reviewer_nudged = false;
            s.owner_nudged = false;
            s.pending_verdict = None;
        }
        // mark_run_turn already wrote Failed; restore Running before UI events.
        match self.store.update_run_status(run_id, RunStatus::Running) {
            Ok(run) => {
                self.failed_runs.remove(&run_id);
                step.events.push(RuntimeEvent::RunUpdated { run });
            }
            Err(err) => {
                self.report_store_error("restore the plan run status", err, step);
                self.block_mode_run(run_id, "could not persist plan recovery", step);
                return;
            }
        }
        if !self.persist_mode_state(run_id, step) {
            return;
        }

        let task = session.task.clone();
        let max_iterations = session.max_iterations;
        let mode = session.mode;
        let leader = session.leader.clone();
        let prompt = plan_progress_prompt(
            &task,
            &unfinished_lines,
            next_iteration,
            max_iterations,
            Some("a member run failed this round — reassign or adjust the plan"),
        );
        self.mode_dispatch(
            run_id,
            std::slice::from_ref(&leader),
            prompt,
            format!(
                "[{mode} {run_id} · iter {next_iteration}/{max_iterations}] → {leader}: progress"
            ),
            step,
        );
    }

    fn mode_plan_on_planning_complete(
        &mut self,
        run_id: RunId,
        session: &ModeSession,
        step: &mut RuntimeStep,
    ) {
        let steps = match self.store.run_steps_all(run_id) {
            Ok(steps) => steps,
            Err(err) => {
                self.report_store_error("load the plan checklist", err, step);
                self.block_mode_run(run_id, "plan checklist is unavailable", step);
                return;
            }
        };

        if steps.is_empty() {
            if !session.reviewer_nudged {
                if let Some(s) = self.mode_sessions.get_mut(&run_id) {
                    s.reviewer_nudged = true;
                }
                if !self.persist_mode_state(run_id, step) {
                    return;
                }
                let (leader, max_iterations, iteration, mode) = {
                    let s = &self.mode_sessions[&run_id];
                    (s.leader.clone(), s.max_iterations, s.iteration, s.mode)
                };
                self.mode_dispatch(
                    run_id,
                    std::slice::from_ref(&leader),
                    plan_nudge_prompt(),
                    format!(
                        "[{mode} {run_id} · iter {iteration}/{max_iterations}] → {leader}: plan nudge"
                    ),
                    step,
                );
            } else {
                self.block_mode_run(run_id, "no actionable plan produced", step);
            }
            return;
        }

        let Some(reviewer) = session.plan_reviewer.clone() else {
            let Some(builder) = session.plan_builder.clone() else {
                self.block_mode_run(run_id, "plan mode needs a builder", step);
                return;
            };
            self.mode_plan_dispatch_builder(run_id, &builder, step);
            return;
        };
        if let Some(s) = self.mode_sessions.get_mut(&run_id) {
            s.phase = ModePhase::Reviewing;
            s.reviewer_nudged = false;
            s.pending_verdict = None;
            s.reviewer_last_text.clear();
        }
        if !self.persist_mode_state(run_id, step) {
            return;
        }

        let (max_iterations, iteration, mode, verify_command) = {
            let s = &self.mode_sessions[&run_id];
            (
                s.max_iterations,
                s.iteration,
                s.mode,
                s.verify_command.clone(),
            )
        };
        let summary = format_lead_steps_summary(&steps);
        let prompt = plan_review_prompt(&session.task, &summary, verify_command.as_deref());
        self.mode_dispatch(
            run_id,
            std::slice::from_ref(&reviewer),
            prompt,
            format!(
                "[{mode} {run_id} · iter {iteration}/{max_iterations}] → {reviewer}: review plan"
            ),
            step,
        );
    }

    fn mode_plan_on_executing_complete(
        &mut self,
        run_id: RunId,
        session: &ModeSession,
        step: &mut RuntimeStep,
    ) {
        let steps = match self.store.run_steps_all(run_id) {
            Ok(steps) => steps,
            Err(err) => {
                self.report_store_error("load the plan checklist", err, step);
                self.block_mode_run(run_id, "plan checklist is unavailable", step);
                return;
            }
        };

        let unfinished: Vec<&RunStepSummary> = steps
            .iter()
            .filter(|s| s.status != RunStepStatus::Done)
            .collect();

        if unfinished.is_empty() {
            self.mode_start_verification_or_finish(run_id, step);
            return;
        }

        let owned_unfinished: HashMap<MemberId, Vec<(u32, String)>> = unfinished
            .iter()
            .filter(|step| step.status == RunStepStatus::Doing)
            .filter_map(|step| {
                step.owner
                    .as_ref()
                    .map(|owner| (owner.clone(), (step.number, step.title.clone())))
            })
            .fold(HashMap::new(), |mut grouped, (owner, item)| {
                grouped.entry(owner).or_default().push(item);
                grouped
            });
        if !session.owner_nudged && !owned_unfinished.is_empty() {
            if let Some(s) = self.mode_sessions.get_mut(&run_id) {
                s.owner_nudged = true;
            }
            if !self.persist_mode_state(run_id, step) {
                return;
            }
            let (max_iterations, iteration, mode) = {
                let s = &self.mode_sessions[&run_id];
                (s.max_iterations, s.iteration, s.mode)
            };
            let owners = owned_unfinished
                .keys()
                .map(ToString::to_string)
                .collect::<Vec<_>>();
            let dispatches = owned_unfinished
                .into_iter()
                .map(|(owner, steps)| {
                    let prompt = plan_step_nudge_prompt(run_id, &steps);
                    (owner, prompt)
                })
                .collect();
            self.mode_dispatch_multi(
                run_id,
                dispatches,
                format!(
                    "[{mode} {run_id} · iter {iteration}/{max_iterations}] → {}: checklist nudge",
                    owners.join(", ")
                ),
                step,
            );
            return;
        }

        let next_iteration = session.iteration.saturating_add(1);
        if next_iteration > session.max_iterations {
            self.block_mode_run(
                run_id,
                &format!("max iterations reached ({})", session.max_iterations),
                step,
            );
            return;
        }

        let unfinished_lines = format_unfinished_step_lines(&unfinished);

        if let Some(s) = self.mode_sessions.get_mut(&run_id) {
            s.iteration = next_iteration;
            s.phase = ModePhase::Planning;
            s.reviewer_nudged = false;
            s.owner_nudged = false;
        }
        if !self.persist_mode_state(run_id, step) {
            return;
        }

        let task = session.task.clone();
        let max_iterations = session.max_iterations;
        let mode = session.mode;
        let leader = session.leader.clone();
        let prompt = plan_progress_prompt(
            &task,
            &unfinished_lines,
            next_iteration,
            max_iterations,
            None,
        );
        self.mode_dispatch(
            run_id,
            std::slice::from_ref(&leader),
            prompt,
            format!(
                "[{mode} {run_id} · iter {next_iteration}/{max_iterations}] → {leader}: progress"
            ),
            step,
        );
    }

    fn mode_brainstorm_on_turn_complete(
        &mut self,
        run_id: RunId,
        step: &mut RuntimeStep,
    ) {
        let failed = self.failed_runs.contains(&run_id);
        let Some(session) = self.mode_sessions.get(&run_id).cloned() else {
            return;
        };

        if session.cancelled || failed {
            let reason = if session.cancelled {
                "aborted by user"
            } else {
                "member run failed"
            };
            self.block_mode_run(run_id, reason, step);
            return;
        }

        match session.phase {
            ModePhase::Diverging => {
                if session.round < session.rounds {
                    self.brainstorm_enter_generation_round(
                        run_id,
                        session.round.saturating_add(1),
                        step,
                    );
                } else {
                    self.brainstorm_enter_voting(run_id, step);
                }
            }
            ModePhase::Voting => self.brainstorm_enter_synthesis(run_id, step),
            ModePhase::Synthesizing => self.finish_mode_run_brainstorm(run_id, step),
            ModePhase::Done => {}
            _ => {}
        }
    }

    fn brainstorm_enter_generation_round(
        &mut self,
        run_id: RunId,
        round: u32,
        step: &mut RuntimeStep,
    ) {
        if let Some(session) = self.mode_sessions.get_mut(&run_id) {
            session.phase = ModePhase::Diverging;
            session.round = round;
        }
        if !self.persist_mode_state(run_id, step) {
            return;
        }
        let session = self.mode_sessions.get(&run_id).cloned().expect("session");
        let dispatches = self.brainstorm_generation_dispatches(&session);
        let stage = if round == session.rounds {
            "stretch"
        } else {
            "cross-pollinate"
        };
        self.mode_dispatch_multi(
            run_id,
            dispatches,
            format!(
                "[{} {run_id} · generate {round}/{}] {stage}",
                session.mode, session.rounds
            ),
            step,
        );
    }

    fn brainstorm_generation_dispatches(
        &self,
        session: &ModeSession,
    ) -> Vec<(MemberId, String)> {
        let round = session.round.max(1);
        let rounds = session.rounds.max(1);
        let n = session.participants.len();
        session
            .participants
            .iter()
            .map(|participant| {
                let prompt = if round == 1 {
                    brainstorm_propose_prompt(&session.task, n, session.ideas_per_round)
                } else {
                    let context = format_brainstorm_generation_context(session, participant);
                    if round == rounds {
                        brainstorm_stretch_prompt(
                            &session.task,
                            round,
                            rounds,
                            session.ideas_per_round,
                            &context,
                        )
                    } else {
                        brainstorm_build_prompt(
                            &session.task,
                            round,
                            rounds,
                            session.ideas_per_round,
                            &context,
                        )
                    }
                };
                (participant.clone(), self.with_brainstorm_skill(prompt))
            })
            .collect()
    }

    fn with_brainstorm_skill(&self, prompt: String) -> String {
        let skill = brainstorm_skill_text(&self.config.workspace);
        format!(
            "{prompt}\n\nDeployed ${ASTERLINE_BRAINSTORM_SKILL_NAME} protocol \
             (deployment-local and authoritative for card content and method):\n\n{skill}"
        )
    }

    fn brainstorm_enter_voting(&mut self, run_id: RunId, step: &mut RuntimeStep) {
        if let Some(session) = self.mode_sessions.get_mut(&run_id) {
            session.phase = ModePhase::Voting;
            session.votes.clear();
            session.vote_count = 0;
        }
        if !self.persist_mode_state(run_id, step) {
            return;
        }
        let Some(session) = self.mode_sessions.get(&run_id).cloned() else {
            return;
        };
        let idea_set = format_brainstorm_idea_set(&session);
        let prompt = self.with_brainstorm_skill(brainstorm_vote_prompt(
            &session.task,
            &idea_set,
            BRAINSTORM_VOTE_TOP_K,
        ));
        self.mode_dispatch(
            run_id,
            &session.participants,
            prompt,
            format!(
                "[{} {run_id} · vote] private top-{BRAINSTORM_VOTE_TOP_K} ranking",
                session.mode
            ),
            step,
        );
    }

    fn brainstorm_enter_synthesis(&mut self, run_id: RunId, step: &mut RuntimeStep) {
        let Some(snapshot) = self.mode_sessions.get(&run_id).cloned() else {
            return;
        };
        let Some(facilitator) = snapshot.participants.first().cloned() else {
            self.block_mode_run(run_id, "brainstorm has no synthesis facilitator", step);
            return;
        };
        let idea_set = format_brainstorm_idea_set(&snapshot);
        let tally = format_brainstorm_vote_tally(&snapshot);
        let ballots = format_brainstorm_ballots(&snapshot);
        if let Some(session) = self.mode_sessions.get_mut(&run_id) {
            session.phase = ModePhase::Synthesizing;
            session.brainstorm_summary.clear();
        }
        if !self.persist_mode_state(run_id, step) {
            return;
        }
        let prompt = self.with_brainstorm_skill(brainstorm_synthesis_prompt(
            &snapshot.task,
            &idea_set,
            &tally,
            &ballots,
        ));
        self.mode_dispatch(
            run_id,
            std::slice::from_ref(&facilitator),
            prompt,
            format!(
                "[{} {run_id} · synthesize] aggregate {} private ballots",
                snapshot.mode,
                snapshot.votes.len()
            ),
            step,
        );
    }

    fn mode_request_changes(
        &mut self,
        run_id: RunId,
        feedback: String,
        step: &mut RuntimeStep,
    ) {
        let (mode, target, task, max_iterations, next_iteration, reviewer) = {
            let Some(session) = self.mode_sessions.get(&run_id) else {
                return;
            };
            let next_iteration = session.iteration.saturating_add(1);
            let target = match session.mode {
                CollabMode::Plan => session.leader.clone(),
                _ => session.builder.clone(),
            };
            (
                session.mode,
                target,
                session.task.clone(),
                session.max_iterations,
                next_iteration,
                session.reviewer.clone(),
            )
        };
        if next_iteration > max_iterations {
            self.block_mode_run(
                run_id,
                &format!("max iterations reached ({max_iterations})"),
                step,
            );
            return;
        }
        if let Some(session) = self.mode_sessions.get_mut(&run_id) {
            session.iteration = next_iteration;
            session.last_feedback = Some(feedback.clone());
            session.pending_verdict = None;
            session.reviewer_nudged = false;
            session.owner_nudged = false;
            match mode {
                CollabMode::Plan => {
                    session.phase = ModePhase::Planning;
                    session.builder_output.clear();
                }
                _ => {
                    session.phase = ModePhase::Building;
                    session.builder_output.clear();
                }
            }
        }
        let reviewer_display = self.member_display(&reviewer);
        let prompt = match mode {
            CollabMode::Plan => plan_iteration_prompt(
                &task,
                &reviewer_display,
                &feedback,
                next_iteration,
                max_iterations,
            ),
            _ => review_iteration_prompt(
                &task,
                &reviewer_display,
                &feedback,
                next_iteration,
                max_iterations,
            ),
        };
        if !self.persist_mode_state(run_id, step) {
            return;
        }
        self.mode_dispatch(
            run_id,
            std::slice::from_ref(&target),
            prompt,
            format!(
                "[{mode} {run_id} · iter {next_iteration}/{max_iterations}] → {target}: {}",
                short_mode_text(&task)
            ),
            step,
        );
    }

    fn finish_mode_run_approved(&mut self, run_id: RunId, step: &mut RuntimeStep) {
        let run = match self
            .store
            .update_run_status(run_id, RunStatus::Done)
        {
            Ok(run) => run,
            Err(err) => {
                self.report_store_error("finish a mode run", err, step);
                return;
            }
        };
        self.mode_sessions.remove(&run_id);
        self.failed_runs.remove(&run_id);
        step.events.push(RuntimeEvent::RunUpdated { run });
        step.events.push(RuntimeEvent::Notice(format!(
            "{run_id} approved — done"
        )));
    }

    fn finish_mode_run_brainstorm(&mut self, run_id: RunId, step: &mut RuntimeStep) {
        let (card_count, batch_count, rounds, vote_count, participant_count) = self
            .mode_sessions
            .get(&run_id)
            .map(|session| {
                (
                    brainstorm_card_count(session),
                    session.idea_batches.len(),
                    session.rounds.max(1),
                    session.votes.len(),
                    session.participants.len(),
                )
            })
            .unwrap_or((0, 0, 0, 0, 0));
        if let Some(session) = self.mode_sessions.get_mut(&run_id) {
            session.phase = ModePhase::Done;
        }
        if !self.persist_mode_state_quiet(run_id, step) {
            return;
        }
        let run = match self
            .store
            .update_run_status(run_id, RunStatus::Done)
        {
            Ok(run) => run,
            Err(err) => {
                self.report_store_error("finish a brainstorm run", err, step);
                return;
            }
        };
        self.mode_sessions.remove(&run_id);
        self.failed_runs.remove(&run_id);
        step.events.push(RuntimeEvent::RunUpdated { run });
        let notice = format!(
            "brainstorm {run_id} ranked result ready · {card_count} idea cards from {batch_count} \
             contributions across {rounds} \
             generation waves · {vote_count}/{participant_count} private ballots aggregated · \
             type a new topic to brainstorm again · /mode normal for regular chat · /runs for \
             details"
        );
        if let Err(err) = self.store.record_notice(None, &notice) {
            self.report_store_error("save the brainstorm completion notice", err, step);
        }
        step.events.push(RuntimeEvent::Notice(notice));
    }

    /// Block a mode run, then free its live session only after persistence.
    fn block_mode_run(&mut self, run_id: RunId, reason: &str, step: &mut RuntimeStep) {
        match self.store.block_run(run_id, reason) {
            Ok(run) => {
                self.failed_runs.insert(run_id);
                self.mode_sessions.remove(&run_id);
                step.events.push(RuntimeEvent::RunUpdated { run });
                step.events
                    .push(RuntimeEvent::Notice(format!("{run_id} blocked: {reason}")));
            }
            Err(err) => step.events.push(RuntimeEvent::Notice(format!(
                "could not block mode run {run_id}: {err}"
            ))),
        }
    }

    fn block_all_mode_sessions(&mut self, reason: &str, step: &mut RuntimeStep) {
        let ids: Vec<RunId> = self.mode_sessions.keys().copied().collect();
        for run_id in ids {
            self.block_mode_run(run_id, reason, step);
        }
    }

    fn persist_mode_state(&mut self, run_id: RunId, step: &mut RuntimeStep) -> bool {
        let Some(session) = self.mode_sessions.get(&run_id) else {
            return false;
        };
        let json = match serde_json::to_string(session) {
            Ok(json) => json,
            Err(err) => {
                step.events.push(RuntimeEvent::Notice(format!(
                    "could not serialize mode state for {run_id}: {err}"
                )));
                return false;
            }
        };
        match self.store.update_run_mode_state(run_id, &json) {
            Ok(run) => {
                step.events.push(RuntimeEvent::RunUpdated { run });
                true
            }
            Err(err) => {
                self.report_store_error("save mode state", err, step);
                false
            }
        }
    }

    fn persist_mode_state_quiet(&self, run_id: RunId, step: &mut RuntimeStep) -> bool {
        let Some(session) = self.mode_sessions.get(&run_id) else {
            return false;
        };
        let json = match serde_json::to_string(session) {
            Ok(json) => json,
            Err(err) => {
                step.events.push(RuntimeEvent::Notice(format!(
                    "could not serialize mode state for {run_id}: {err}"
                )));
                return false;
            }
        };
        if let Err(err) = self.store.update_run_mode_state(run_id, &json) {
            self.report_store_error("save mode state", err, step);
            return false;
        }
        true
    }

    /// Record mode-relevant envelopes and text after a member message completes.
    fn mode_record_message(
        &mut self,
        member: &MemberId,
        turn: TurnId,
        visible_text: &str,
        parsed: &router::ParsedAgentOutput,
        step: &mut RuntimeStep,
    ) {
        let run_id = self.run_turns.get(&turn).copied();
        let session_meta = run_id.and_then(|id| {
            self.mode_sessions.get(&id).map(|s| {
                (
                    id,
                    s.builder.clone(),
                    s.reviewer.clone(),
                    s.phase,
                    s.participants.clone(),
                )
            })
        });

        if !parsed.reviews.is_empty() {
            let last = parsed.reviews.last().cloned().expect("non-empty");
            let approve = matches!(last.verdict, ReviewVerdictKind::Approve);
            let summary = last.summary.clone().unwrap_or_default();

            let accept =
                session_meta
                    .as_ref()
                    .is_some_and(|(_, _, reviewer, phase, _)| {
                        member == reviewer
                            && matches!(
                                *phase,
                                ModePhase::Reviewing | ModePhase::AwaitingVerdict
                            )
                    });

            if accept {
                let run_id = session_meta.as_ref().map(|(id, ..)| *id).expect("accept");
                let candidate = self.mode_sessions.get(&run_id).cloned().map(|mut session| {
                    session.pending_verdict = Some(PersistedReviewVerdict::from(&last));
                    session
                });
                let committed = match candidate
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()
                {
                    Ok(Some(mode_state)) => match self.store.commit_mode_verdict(
                        turn, member, run_id, approve, &summary, &mode_state,
                    ) {
                        Ok(_) => true,
                        Err(err) => {
                            self.report_store_error("save a review verdict", err, step);
                            false
                        }
                    },
                    Ok(None) => false,
                    Err(err) => {
                        step.events.push(RuntimeEvent::Notice(format!(
                            "could not serialize mode state for {run_id}: {err}"
                        )));
                        false
                    }
                };
                if committed {
                    if let Some(candidate) = candidate {
                        self.mode_sessions.insert(run_id, candidate);
                    }
                    step.events.push(RuntimeEvent::Verdict {
                        run: run_id,
                        member: member.clone(),
                        approve,
                        summary,
                    });
                } else if let Some(running) = self
                    .members
                    .get_mut(member)
                    .and_then(|state| state.running.as_mut())
                {
                    // Fail closed: Exited will block the mode run instead of
                    // interpreting an unaudited verdict as free-text feedback.
                    running.failed = true;
                }
            } else {
                step.events.push(RuntimeEvent::Notice(format!(
                    "{member} sent a review verdict outside an active review — ignored"
                )));
            }
        }

        if let Some((run_id, builder, reviewer, phase, participants)) = session_meta
        {
            if member == &builder && phase == ModePhase::Building {
                if let Some(session) = self.mode_sessions.get_mut(&run_id) {
                    session.builder_output = truncate_mode_text(visible_text);
                }
            } else if member == &reviewer
                && matches!(
                    phase,
                    ModePhase::Reviewing | ModePhase::AwaitingVerdict
                )
                && let Some(session) = self.mode_sessions.get_mut(&run_id)
            {
                session.reviewer_last_text = truncate_mode_text(visible_text);
            } else if phase == ModePhase::Diverging && participants.iter().any(|p| p == member) {
                if let Some(session) = self.mode_sessions.get_mut(&run_id) {
                    let text = truncate_mode_text(visible_text);
                    let round = session.round.max(1);
                    let exact_replay = session.idea_batches.iter().any(|batch| {
                        batch.round == round && &batch.author == member && batch.text == text
                    });
                    if !text.trim().is_empty() && !exact_replay {
                        session.idea_batches.push(BrainstormIdeaBatch {
                            round,
                            author: member.clone(),
                            text,
                            cards: parsed.brainstorm_cards.clone(),
                        });
                    }
                    session.idea_count = brainstorm_card_count(session);
                }
                self.persist_mode_state_quiet(run_id, step);
            } else if phase == ModePhase::Voting && participants.iter().any(|p| p == member) {
                if let Some(vote) = parsed.brainstorm_votes.last() {
                    let Some(current) = self.mode_sessions.get(&run_id).cloned() else {
                        return;
                    };
                    let candidates = brainstorm_candidate_ids(&current);
                    let unknown = vote
                        .ranked
                        .iter()
                        .filter(|candidate| {
                            !candidates.contains(&normalize_brainstorm_candidate_id(candidate))
                        })
                        .cloned()
                        .collect::<Vec<_>>();
                    if !unknown.is_empty() {
                        step.events.push(RuntimeEvent::Notice(format!(
                            "{member} submitted unknown brainstorm candidate(s): {} — ballot ignored",
                            unknown.join(", ")
                        )));
                        if let Some(running) = self
                            .members
                            .get_mut(member)
                            .and_then(|state| state.running.as_mut())
                        {
                            // An invalid structured ballot must not let the
                            // voting turn advance into synthesis.
                            running.failed = true;
                        }
                    } else {
                        let unchanged = current.votes.iter().any(|record| {
                            &record.voter == member
                                && record.ranked == vote.ranked
                                && record.summary == vote.summary
                        });
                        if !unchanged {
                            let mut candidate = current;
                            candidate.votes.retain(|record| &record.voter != member);
                            candidate.votes.push(BrainstormVoteRecord {
                                voter: member.clone(),
                                ranked: vote.ranked.clone(),
                                summary: vote.summary.clone(),
                            });
                            candidate.vote_count = candidate.votes.len() as u32;
                            match serde_json::to_string(&candidate) {
                                Ok(mode_state) => match self.store.commit_brainstorm_vote(
                                    run_id,
                                    member,
                                    &vote.ranked,
                                    &mode_state,
                                ) {
                                    Ok(()) => {
                                        self.mode_sessions.insert(run_id, candidate);
                                    }
                                    Err(err) => {
                                        self.report_store_error(
                                            "save a brainstorm vote",
                                            err,
                                            step,
                                        );
                                        if let Some(running) = self
                                            .members
                                            .get_mut(member)
                                            .and_then(|state| state.running.as_mut())
                                        {
                                            running.failed = true;
                                        }
                                    }
                                },
                                Err(err) => {
                                    step.events.push(RuntimeEvent::Notice(format!(
                                        "could not serialize mode state for {run_id}: {err}"
                                    )));
                                    if let Some(running) = self
                                        .members
                                        .get_mut(member)
                                        .and_then(|state| state.running.as_mut())
                                    {
                                        running.failed = true;
                                    }
                                }
                            }
                        }
                    }
                }
            } else if phase == ModePhase::Synthesizing {
                if let Some(session) = self.mode_sessions.get_mut(&run_id) {
                    session.brainstorm_summary = truncate_mode_text(visible_text);
                }
                self.persist_mode_state_quiet(run_id, step);
            }
        }
    }

    fn mode_mark_turn_cancelled(&mut self, turn: TurnId) {
        let Some(run_id) = self.run_turns.get(&turn).copied() else {
            return;
        };
        if let Some(session) = self.mode_sessions.get_mut(&run_id) {
            session.cancelled = true;
        }
    }

    fn mode_resume(
        &mut self,
        run: RunSummary,
        note: Option<String>,
        step: &mut RuntimeStep,
    ) {
        let state_json = match self.store.run_mode_state(run.id) {
            Ok(Some(json)) => json,
            Ok(None) | Err(_) => {
                step.events.push(RuntimeEvent::Notice(format!(
                    "could not resume {}: mode state unreadable — start a fresh run",
                    run.id
                )));
                return;
            }
        };
        let mut session: ModeSession = match serde_json::from_str(&state_json) {
            Ok(session) => session,
            Err(_) => {
                step.events.push(RuntimeEvent::Notice(format!(
                    "could not resume {}: mode state unreadable — start a fresh run",
                    run.id
                )));
                return;
            }
        };
        if session.phase == ModePhase::Done {
            step.events.push(RuntimeEvent::Notice(format!(
                "{} is already complete — start a fresh run",
                run.id
            )));
            return;
        }

        let missing = mode_resume_missing_members(&session, &self.config);
        if !missing.is_empty() {
            step.events.push(RuntimeEvent::Notice(format!(
                "could not resume {}: member(s) left the roster: {}",
                run.id,
                missing.join(", ")
            )));
            return;
        }

        match self.store.continue_run(run.id, note.as_deref()) {
            Ok(updated) => step
                .events
                .push(RuntimeEvent::RunUpdated { run: updated }),
            Err(err) => {
                self.report_store_error("continue the mode run", err, step);
                return;
            }
        }
        self.failed_runs.remove(&run.id);
        session.cancelled = false;
        session.idea_count = brainstorm_card_count(&session);

        let phase = session.phase;
        let task = session.task.clone();
        let builder = session.builder.clone();
        let reviewer = session.reviewer.clone();
        let leader = session.leader.clone();
        let plan_builder = session.plan_builder.clone();
        let iteration = session.iteration;
        let max_iterations = session.max_iterations;
        let mode = session.mode;
        let last_feedback = session.last_feedback.clone();
        let builder_output = session.builder_output.clone();

        self.mode_sessions.insert(run.id, session);

        match phase {
            ModePhase::Building => {
                let prompt = if iteration <= 1 {
                    review_task_prompt(&task)
                } else {
                    let feedback = last_feedback
                        .as_deref()
                        .filter(|s| !s.trim().is_empty())
                        .unwrap_or("(feedback unavailable)");
                    let reviewer_display = self.member_display(&reviewer);
                    review_iteration_prompt(
                        &task,
                        &reviewer_display,
                        feedback,
                        iteration,
                        max_iterations,
                    )
                };
                self.mode_dispatch(
                    run.id,
                    std::slice::from_ref(&builder),
                    prompt,
                    format!(
                        "[{mode} {} · iter {iteration}/{max_iterations}] → {builder}: {}",
                        run.id,
                        short_mode_text(&task)
                    ),
                    step,
                );
            }
            ModePhase::Planning => {
                self.mode_resume_planning(run.id, step);
            }
            ModePhase::AwaitingExecution => {
                let Some(builder) = plan_builder else {
                    self.block_mode_run(run.id, "plan mode needs a builder", step);
                    return;
                };
                self.mode_plan_dispatch_builder(run.id, &builder, step);
            }
            ModePhase::Executing => {
                let steps = match self.store.run_steps_all(run.id) {
                    Ok(steps) => steps,
                    Err(err) => {
                        self.report_store_error("load the plan checklist", err, step);
                        self.block_mode_run(run.id, "plan checklist is unavailable", step);
                        return;
                    }
                };
                let owned: Vec<&RunStepSummary> = steps
                    .iter()
                    .filter(|s| {
                        s.owner.is_some()
                            && s.status != RunStepStatus::Done
                    })
                    .collect();
                if owned.is_empty() {
                    self.mode_resume_planning(run.id, step);
                } else {
                    let dispatches = plan_owner_dispatches(run.id, &leader, owned);
                    let owners: Vec<String> =
                        dispatches.iter().map(|(m, _)| m.to_string()).collect();
                    self.mode_dispatch_multi(
                        run.id,
                        dispatches,
                        format!(
                            "[{mode} {} · iter {iteration}/{max_iterations}] → {}: execute",
                            run.id,
                            owners.join(", ")
                        ),
                        step,
                    );
                }
            }
            ModePhase::Diverging => {
                let (round, rounds, dispatches) = {
                    let session = &self.mode_sessions[&run.id];
                    (
                        session.round.max(1),
                        session.rounds.max(1),
                        self.brainstorm_generation_dispatches(session),
                    )
                };
                let stage = if round == 1 {
                    "blind seed"
                } else if round == rounds {
                    "stretch"
                } else {
                    "cross-pollinate"
                };
                self.mode_dispatch_multi(
                    run.id,
                    dispatches,
                    format!("[{mode} {} · generate {round}/{rounds}] {stage}", run.id),
                    step,
                );
            }
            ModePhase::Voting => {
                let session = &self.mode_sessions[&run.id];
                let idea_set = format_brainstorm_idea_set(session);
                let prompt = self.with_brainstorm_skill(brainstorm_vote_prompt(
                    &session.task,
                    &idea_set,
                    BRAINSTORM_VOTE_TOP_K,
                ));
                self.mode_dispatch(
                    run.id,
                    &session.participants.clone(),
                    prompt,
                    format!(
                        "[{mode} {} · vote] private top-{BRAINSTORM_VOTE_TOP_K} ranking",
                        run.id
                    ),
                    step,
                );
            }
            ModePhase::Synthesizing => {
                self.brainstorm_enter_synthesis(run.id, step);
            }
            ModePhase::Reviewing | ModePhase::AwaitingVerdict => {
                if self
                    .mode_sessions
                    .get(&run.id)
                    .is_some_and(|session| session.pending_verdict.is_some())
                {
                    let session = self.mode_sessions[&run.id].clone();
                    self.mode_handle_verdict_phase(run.id, &session, step);
                    return;
                }
                if let Some(s) = self.mode_sessions.get_mut(&run.id) {
                    s.phase = ModePhase::Reviewing;
                    s.reviewer_nudged = false;
                    s.owner_nudged = false;
                }
                if !self.persist_mode_state(run.id, step) {
                    return;
                }
                let verify_cmd = self
                    .mode_sessions
                    .get(&run.id)
                    .and_then(|s| s.verify_command.clone());
                let prompt = if mode == CollabMode::Plan {
                    let steps = match self.store.run_steps_all(run.id) {
                        Ok(steps) => steps,
                        Err(err) => {
                            self.report_store_error("load the plan checklist", err, step);
                            self.block_mode_run(run.id, "plan checklist is unavailable", step);
                            return;
                        }
                    };
                    let summary = format_lead_steps_summary(&steps);
                    plan_review_prompt(&task, &summary, verify_cmd.as_deref())
                } else {
                    let builder_display = self.member_display(&builder);
                    review_prompt(
                        &task,
                        &builder_display,
                        &builder_output,
                        verify_cmd.as_deref(),
                    )
                };
                let label = if mode == CollabMode::Plan {
                    "review plan"
                } else {
                    "review"
                };
                self.mode_dispatch(
                    run.id,
                    std::slice::from_ref(&reviewer),
                    prompt,
                    format!(
                        "[{mode} {} · iter {iteration}/{max_iterations}] → {reviewer}: {label}",
                        run.id,
                    ),
                    step,
                );
            }
            ModePhase::Verifying => {
                let configured = self
                    .mode_sessions
                    .get(&run.id)
                    .and_then(|s| s.verify_command.clone());
                if let Some(cmd) = crate::domain::mode::resolve_verify_command(
                    configured.as_deref(),
                    suggested_verify_command(&self.config.workspace),
                ) {
                    let updated = match self
                        .store
                        .update_run_status(run.id, RunStatus::Verifying)
                    {
                        Ok(updated) => updated,
                        Err(err) => {
                            self.report_store_error("resume mode verification", err, step);
                            return;
                        }
                    };
                    step.events
                        .push(RuntimeEvent::RunUpdated { run: updated });
                    step.events
                        .push(RuntimeEvent::Notice(format!("verifying {}: {cmd}", run.id)));
                    step.verify_actions.push(VerifyAction {
                        run_id: run.id,
                        command: cmd,
                        workspace: self.config.workspace.clone(),
                        cancel: Arc::new(AtomicBool::new(false)),
                    });
                } else {
                    self.finish_mode_run_approved(run.id, step);
                }
            }
            ModePhase::Done => unreachable!("completed mode runs return before resume dispatch"),
        }
    }

    fn mode_resume_planning(&mut self, run_id: RunId, step: &mut RuntimeStep) {
        let (task, leader, iteration, max_iterations, mode) = {
            let Some(session) = self.mode_sessions.get(&run_id) else {
                return;
            };
            (
                session.task.clone(),
                session.leader.clone(),
                session.iteration,
                session.max_iterations,
                session.mode,
            )
        };
        let teammates = self.plan_teammate_list();
        let base = plan_plan_prompt(&task, &teammates);
        let prompt = format!(
            "Resuming {run_id}: re-assess the checklist in /runs and continue.\n\n{base}"
        );
        self.mode_dispatch(
            run_id,
            std::slice::from_ref(&leader),
            prompt,
            format!("[{mode} {run_id} · iter {iteration}/{max_iterations}] → {leader}: plan"),
            step,
        );
    }
}

fn mode_resume_missing_members(session: &ModeSession, config: &TeamConfig) -> Vec<String> {
    let mut needed: Vec<&MemberId> = match session.mode {
        CollabMode::Review => vec![&session.builder, &session.reviewer],
        CollabMode::Plan => {
            let mut members = vec![&session.leader];
            if let Some(reviewer) = &session.plan_reviewer {
                members.push(reviewer);
            }
            if let Some(builder) = &session.plan_builder {
                members.push(builder);
            }
            members
        }
        CollabMode::Brainstorm => session.participants.iter().collect(),
        CollabMode::Team => Vec::new(),
    };
    needed.sort_by_key(|id| id.as_str());
    needed.dedup();
    needed
        .into_iter()
        .filter(|id| config.member(id).is_none())
        .map(|id| id.to_string())
        .collect()
}

fn format_lead_steps_summary(steps: &[RunStepSummary]) -> String {
    steps
        .iter()
        .map(|s| {
            let owner = s
                .owner
                .as_ref()
                .map(|o| o.to_string())
                .unwrap_or_else(|| "?".to_string());
            match &s.note {
                Some(note) if !note.trim().is_empty() => {
                    format!("#{} [{owner}] {} — {note}", s.number, s.title)
                }
                _ => format!("#{} [{owner}] {}", s.number, s.title),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Group actionable plan steps by owner and build their shared dispatch
/// prompts. New execution and resumed execution must follow the same routing
/// and checklist wording.
fn plan_owner_dispatches<'a>(
    run_id: RunId,
    leader: &MemberId,
    steps: impl IntoIterator<Item = &'a RunStepSummary>,
) -> Vec<(MemberId, String)> {
    let mut by_owner: HashMap<MemberId, Vec<(u32, String)>> = HashMap::new();
    for step in steps {
        if let Some(owner) = &step.owner {
            by_owner
                .entry(owner.clone())
                .or_default()
                .push((step.number, step.title.clone()));
        }
    }
    by_owner
        .into_iter()
        .map(|(owner, owned_steps)| {
            let prompt = step_dispatch_prompt(run_id, leader, &owned_steps);
            (owner, prompt)
        })
        .collect()
}

/// Unfinished checklist lines for the leader: `#{n} [owner] status title — note`.
fn format_unfinished_step_lines(steps: &[&RunStepSummary]) -> Vec<String> {
    steps
        .iter()
        .map(|s| {
            let owner = s
                .owner
                .as_ref()
                .map(|o| o.to_string())
                .unwrap_or_else(|| "?".to_string());
            let mut line = format!(
                "#{} [{owner}] {} {}",
                s.number,
                s.status.as_str(),
                s.title
            );
            if let Some(note) = s.note.as_ref().map(|n| n.trim()).filter(|n| !n.is_empty()) {
                line.push_str(" — ");
                line.push_str(note);
            }
            line
        })
        .collect()
}

/// Current phase of a mode session. Serialized as its snake_case string.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ModePhase {
    Building,
    Planning,
    AwaitingExecution,
    Executing,
    Diverging,
    Voting,
    Synthesizing,
    Reviewing,
    AwaitingVerdict,
    Verifying,
    Done,
}

/// One participant's append-only contribution in one brainstorm generation wave.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct BrainstormIdeaBatch {
    round: u32,
    author: MemberId,
    text: String,
    #[serde(default)]
    cards: Vec<BrainstormCard>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct BrainstormVoteRecord {
    voter: MemberId,
    ranked: Vec<String>,
    #[serde(default)]
    summary: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PersistedReviewVerdictKind {
    Approve,
    RequestChanges,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct PersistedReviewVerdict {
    verdict: PersistedReviewVerdictKind,
    #[serde(default)]
    summary: Option<String>,
}

impl From<&ReviewVerdict> for PersistedReviewVerdict {
    fn from(value: &ReviewVerdict) -> Self {
        Self {
            verdict: match value.verdict {
                ReviewVerdictKind::Approve => PersistedReviewVerdictKind::Approve,
                ReviewVerdictKind::RequestChanges => {
                    PersistedReviewVerdictKind::RequestChanges
                }
            },
            summary: value.summary.clone(),
        }
    }
}

/// One live collaboration-mode session. Persisted as the run's `mode_state` JSON;
/// field names line up with ModeStatusSummary (phase/iteration/max_iterations/round/rounds)
/// and unknown fields are tolerated by older readers.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct ModeSession {
    mode: CollabMode,
    phase: ModePhase,
    task: String,
    builder: MemberId,
    reviewer: MemberId,
    leader: MemberId,
    /// Optional execution handoff for Plan mode. Older persisted sessions
    /// retain `None`, which is blocked rather than silently inferred.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    plan_builder: Option<MemberId>,
    /// Optional Plan-only reviewer. Omission deliberately skips the review
    /// phase; `reviewer` retains a harmless derived value for shared fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    plan_reviewer: Option<MemberId>,
    participants: Vec<MemberId>,
    iteration: u32,
    max_iterations: u32,
    /// Current brainstorm generation wave for new brainstorm runs.
    round: u32,
    /// Total brainstorm generation waves for new brainstorm runs.
    rounds: u32,
    #[serde(default = "default_ideas_per_round")]
    ideas_per_round: u32,
    #[serde(default)]
    idea_count: u32,
    auto_verify: bool,
    #[serde(default = "default_auto_execute")]
    auto_execute: bool,
    /// Explicit auto-verify command from mode config (review/plan).
    #[serde(default)]
    verify_command: Option<String>,
    #[serde(default)]
    builder_output: String,
    #[serde(default)]
    reviewer_nudged: bool,
    #[serde(default)]
    owner_nudged: bool,
    #[serde(default)]
    last_feedback: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending_verdict: Option<PersistedReviewVerdict>,
    #[serde(skip)]
    reviewer_last_text: String,
    #[serde(skip)]
    cancelled: bool,
    /// Append-only brainstorm contributions. Later waves never overwrite
    /// earlier idea batches.
    #[serde(default)]
    idea_batches: Vec<BrainstormIdeaBatch>,
    /// Independent ballots collected only after all generation waves finish.
    #[serde(default)]
    votes: Vec<BrainstormVoteRecord>,
    #[serde(default)]
    vote_count: u32,
    #[serde(default)]
    brainstorm_summary: String,
}

fn default_ideas_per_round() -> u32 {
    4
}

fn default_auto_execute() -> bool {
    true
}

const MODE_TEXT_LIMIT: usize = 4000;
const BRAINSTORM_VOTE_TOP_K: usize = 5;

/// Stable anonymous label for participant index (A, B, … Z, then P27…).
fn proposal_label(index: usize) -> String {
    if index < 26 {
        ((b'A' + index as u8) as char).to_string()
    } else {
        format!("P{}", index + 1)
    }
}

fn participant_index(session: &ModeSession, member: &MemberId) -> Option<usize> {
    session.participants.iter().position(|p| p == member)
}

fn format_brainstorm_generation_context(
    session: &ModeSession,
    participant: &MemberId,
) -> String {
    let Some(index) = participant_index(session, participant) else {
        return "(no prior idea batch available)".to_string();
    };
    let latest_for = |author: &MemberId| {
        session
            .idea_batches
            .iter()
            .rev()
            .find(|batch| {
                &batch.author == author
                    && batch.round < session.round
                    && !batch.text.trim().is_empty()
            })
    };

    let mut sections = Vec::new();
    if let Some(batch) = latest_for(participant) {
        sections.push(format!(
            "Own batch R{}-{}:\n{}",
            batch.round,
            proposal_label(index),
            batch.text
        ));
    }

    let count = session.participants.len();
    if count > 1 {
        // Rotate one peer per wave instead of exposing the same all-to-all
        // transcript, preserving more independent search paths.
        let offset = ((session.round.saturating_sub(2) as usize) % (count - 1)) + 1;
        let peer_index = (index + offset) % count;
        let peer = &session.participants[peer_index];
        if let Some(batch) = latest_for(peer) {
            sections.push(format!(
                "Peer batch R{}-{}:\n{}",
                batch.round,
                proposal_label(peer_index),
                batch.text
            ));
        }
    }

    if sections.is_empty() {
        "(no prior idea batch available; create fresh directions without evaluating)".to_string()
    } else {
        sections.join("\n\n")
    }
}

fn brainstorm_labeled_batches(session: &ModeSession) -> Vec<(&BrainstormIdeaBatch, String)> {
    let mut occurrences: HashMap<(u32, MemberId), usize> = HashMap::new();
    session
        .idea_batches
        .iter()
        .filter_map(|batch| {
            let index = participant_index(session, &batch.author)?;
            let occurrence = occurrences
                .entry((batch.round, batch.author.clone()))
                .and_modify(|count| *count += 1)
                .or_insert(1);
            let base = format!("R{}-{}", batch.round, proposal_label(index));
            let label = if *occurrence == 1 {
                base
            } else {
                format!("{base}-V{occurrence}")
            };
            Some((batch, label))
        })
        .collect()
}

fn brainstorm_candidate_ids(session: &ModeSession) -> HashSet<String> {
    brainstorm_labeled_batches(session)
        .into_iter()
        .flat_map(|(batch, label)| {
            (1..=batch.cards.len().max(1))
                .map(move |item| normalize_brainstorm_candidate_id(&format!("{label}#{item}")))
        })
        .collect()
}

fn normalize_brainstorm_candidate_id(candidate: &str) -> String {
    candidate.trim().to_ascii_uppercase()
}

fn format_brainstorm_idea_set(session: &ModeSession) -> String {
    let mut sections = Vec::new();
    for (batch, label) in brainstorm_labeled_batches(session) {
        if batch.cards.is_empty() {
            // A free-text batch still represents one real, votable fallback
            // candidate, matching brainstorm_card_count's max(1) semantics.
            sections.push(format!("[{label}#1]\n{}", batch.text));
            continue;
        }
        let cards = batch
            .cards
            .iter()
            .enumerate()
            .map(|(index, card)| {
                let candidate = format!("{label}#{}", index + 1);
                let sources = if card.sources.is_empty() {
                    "none".to_string()
                } else {
                    card.sources.join(", ")
                };
                format!(
                    "[{candidate}] {}\nOperation: {}\nProposal: {}\nMechanism: {}\nSources: {sources}",
                    card.title, card.operation, card.proposal, card.mechanism
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        sections.push(cards);
    }
    if sections.is_empty() {
        "(IdeaSet is empty)".to_string()
    } else {
        sections.join("\n\n")
    }
}

fn brainstorm_card_count(session: &ModeSession) -> u32 {
    session
        .idea_batches
        .iter()
        .map(|batch| batch.cards.len().max(1) as u32)
        .sum()
}

fn brainstorm_vote_tally(session: &ModeSession) -> Vec<(String, u32, u32)> {
    let mut totals: BTreeMap<String, (u32, u32)> = BTreeMap::new();
    for ballot in &session.votes {
        let mut seen = HashSet::new();
        for (index, candidate) in ballot.ranked.iter().take(BRAINSTORM_VOTE_TOP_K).enumerate() {
            let candidate = normalize_brainstorm_candidate_id(candidate);
            if candidate.is_empty() || !seen.insert(candidate.clone()) {
                continue;
            }
            let points = BRAINSTORM_VOTE_TOP_K.saturating_sub(index) as u32;
            let entry = totals.entry(candidate).or_insert((0, 0));
            entry.0 = entry.0.saturating_add(points);
            if index == 0 {
                entry.1 = entry.1.saturating_add(1);
            }
        }
    }
    let mut ranked = totals
        .into_iter()
        .map(|(candidate, (score, first_place))| (candidate, score, first_place))
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| right.2.cmp(&left.2))
            .then_with(|| left.0.cmp(&right.0))
    });
    ranked
}

fn format_brainstorm_vote_tally(session: &ModeSession) -> String {
    let ranked = brainstorm_vote_tally(session);
    if ranked.is_empty() {
        return "(no valid structured ballots were returned)".to_string();
    }
    ranked
        .iter()
        .enumerate()
        .map(|(index, (candidate, score, first_place))| {
            format!(
                "{}. {candidate} — {score} points, {first_place} first-place vote(s)",
                index + 1
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_brainstorm_ballots(session: &ModeSession) -> String {
    if session.votes.is_empty() {
        return "(no valid ballot rationales)".to_string();
    }
    session
        .votes
        .iter()
        .map(|ballot| {
            let summary = ballot.summary.as_deref().unwrap_or("(no rationale)");
            format!(
                "@{}: {} — {summary}",
                ballot.voter,
                ballot.ranked.join(" > ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_brainstorm_response(preamble: &str, cards: &[BrainstormCard]) -> String {
    if cards.is_empty() {
        return preamble.to_string();
    }
    let rendered = cards
        .iter()
        .enumerate()
        .map(|(index, card)| {
            let sources = if card.sources.is_empty() {
                "none".to_string()
            } else {
                card.sources.join(", ")
            };
            format!(
                "### Card {} · {}\n\n- Operation: `{}`\n- Proposal: {}\n- Mechanism: {}\n- Sources: {}",
                index + 1,
                card.title,
                card.operation,
                card.proposal,
                card.mechanism,
                sources
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    if preamble.trim().is_empty() {
        rendered
    } else {
        format!("{}\n\n{rendered}", preamble.trim())
    }
}

fn truncate_mode_text(text: &str) -> String {
    truncate_mode_text_limit(text, MODE_TEXT_LIMIT)
}

fn truncate_mode_text_limit(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        text.to_string()
    } else {
        text.chars().take(limit).collect()
    }
}

fn short_mode_text(text: &str) -> String {
    const LIMIT: usize = 80;
    let trimmed = text.trim();
    if trimmed.chars().count() <= LIMIT {
        trimmed.to_string()
    } else {
        let mut s: String = trimmed.chars().take(LIMIT.saturating_sub(1)).collect();
        s.push('…');
        s
    }
}
