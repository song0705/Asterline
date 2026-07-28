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
                "a {} run is already active — /abort it first",
                existing.mode
            )));
            return;
        }

        let (roles, limits) = match resolve_mode_roles(&self.config, mode) {
            Ok(resolved) => resolved,
            Err(err) => {
                step.events.push(RuntimeEvent::Notice(err));
                return;
            }
        };

        if mode == CollabMode::Brainstorm && roles.participants.len() < 2 {
            step.events.push(RuntimeEvent::Notice(
                "brainstorm needs at least two participants".to_string(),
            ));
            return;
        }

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
            reviewer: roles.reviewer.clone(),
            leader: roles.leader.clone(),
            participants: roles.participants.clone(),
            iteration,
            max_iterations: limits.max_iterations,
            round,
            rounds: limits.rounds,
            ideas_per_round: limits.ideas_per_round,
            idea_count: 0,
            auto_verify: limits.auto_verify,
            verify_command: limits.verify_command.clone(),
            builder_output: String::new(),
            reviewer_nudged: false,
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
        self.record_mode_task_message(&task_targets, &task, step);

        match mode {
            CollabMode::Review => {
                step.events.push(RuntimeEvent::Notice(format!(
                    "review {run_id} started → {} (reviewer: {})",
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
                step.events.push(RuntimeEvent::Notice(format!(
                    "plan {run_id} started → {} (reviewer: {})",
                    session.leader, session.reviewer
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
    ) {
        let Ok(turn) = self.store.create_turn() else {
            return;
        };
        let _ = self.store.record_user(turn, targets, task);
        step.events.push(RuntimeEvent::TurnStarted { turn });
        step.events.push(RuntimeEvent::UserMessage {
            turn,
            targets: targets.to_vec(),
            body: task.to_string(),
        });
        step.events.push(RuntimeEvent::TurnFinished { turn });
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
                step.events
                    .push(RuntimeEvent::Notice(format!("store error: {err}")));
                return;
            }
        };
        let _ = self.store.record_user(turn, &targets, &display);
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
                if let Ok(id) = self.store.insert_approval(Some(turn), None, &kind, &prompt) {
                    self.held_approvals.insert(
                        id,
                        HeldApproval {
                            turn,
                            targets: vec![member],
                            prompt: prompt.clone(),
                            mode_run: Some(run_id),
                        },
                    );
                    step.events.push(RuntimeEvent::ApprovalRequested {
                        id,
                        member: None,
                        action: kind,
                        body: prompt,
                    });
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
                self.persist_mode_state(run_id, step);
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
            Some(ReviewVerdict {
                verdict: ReviewVerdictKind::Approve,
                summary: _,
            }) => {
                self.persist_mode_state(run_id, step);
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
                    self.persist_mode_state(run_id, step);
                    if let Ok(run) = self
                        .store
                        .update_run_status(run_id, RunStatus::Verifying)
                    {
                        step.events.push(RuntimeEvent::RunUpdated { run });
                    }
                    step.events.push(RuntimeEvent::Notice(format!(
                        "verifying {run_id}: {cmd}"
                    )));
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
            Some(ReviewVerdict {
                verdict: ReviewVerdictKind::RequestChanges,
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
                self.persist_mode_state(run_id, step);
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
                self.mode_request_changes(run_id, feedback, step);
            }
        }
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

        self.failed_runs.remove(&run_id);

        let steps = self.store.run_steps_all(run_id).unwrap_or_default();
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
            s.pending_verdict = None;
        }
        // mark_run_turn already wrote Failed; restore Running before UI events.
        if let Ok(run) = self.store.update_run_status(run_id, RunStatus::Running) {
            step.events.push(RuntimeEvent::RunUpdated { run });
        }
        self.persist_mode_state(run_id, step);

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
                step.events.push(RuntimeEvent::Notice(format!(
                    "could not load checklist for {run_id}: {err}"
                )));
                return;
            }
        };

        let owned_todos: Vec<&RunStepSummary> = steps
            .iter()
            .filter(|s| s.owner.is_some() && s.status == RunStepStatus::Todo)
            .collect();

        if owned_todos.is_empty() {
            if !session.reviewer_nudged {
                if let Some(s) = self.mode_sessions.get_mut(&run_id) {
                    s.reviewer_nudged = true;
                }
                self.persist_mode_state(run_id, step);
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

        // Mark owned todos as Doing; emit only the last RunUpdated.
        let mut last_run = None;
        for s in &owned_todos {
            if let Ok(run) =
                self.store
                    .update_run_step(run_id, s.number, RunStepStatus::Doing, None)
            {
                last_run = Some(run);
            }
        }
        if let Some(run) = last_run {
            step.events.push(RuntimeEvent::RunUpdated { run });
        }

        // Group steps by owner.
        let mut by_owner: HashMap<MemberId, Vec<(u32, String)>> = HashMap::new();
        for s in &owned_todos {
            if let Some(owner) = &s.owner {
                by_owner
                    .entry(owner.clone())
                    .or_default()
                    .push((s.number, s.title.clone()));
            }
        }

        if let Some(s) = self.mode_sessions.get_mut(&run_id) {
            s.phase = ModePhase::Executing;
        }
        self.persist_mode_state(run_id, step);

        let (max_iterations, iteration, mode) = {
            let s = &self.mode_sessions[&run_id];
            (s.max_iterations, s.iteration, s.mode)
        };
        let leader = session.leader.clone();
        let dispatches: Vec<(MemberId, String)> = by_owner
            .into_iter()
            .map(|(owner, owned_steps)| {
                let prompt = step_dispatch_prompt(run_id, &leader, &owned_steps);
                (owner, prompt)
            })
            .collect();
        let owners: Vec<String> = dispatches.iter().map(|(m, _)| m.to_string()).collect();
        self.mode_dispatch_multi(
            run_id,
            dispatches,
            format!(
                "[{mode} {run_id} · iter {iteration}/{max_iterations}] → {}: execute",
                owners.join(", ")
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
                step.events.push(RuntimeEvent::Notice(format!(
                    "could not load checklist for {run_id}: {err}"
                )));
                return;
            }
        };

        let unfinished: Vec<&RunStepSummary> = steps
            .iter()
            .filter(|s| s.status != RunStepStatus::Done)
            .collect();

        if unfinished.is_empty() {
            if let Some(s) = self.mode_sessions.get_mut(&run_id) {
                s.reviewer_nudged = false;
                s.phase = ModePhase::Reviewing;
                s.pending_verdict = None;
                s.reviewer_last_text.clear();
            }
            self.persist_mode_state(run_id, step);

            let steps_summary = format_lead_steps_summary(&steps);
            let task = session.task.clone();
            let (reviewer, max_iterations, iteration, mode, verify_cmd) = {
                let s = &self.mode_sessions[&run_id];
                (
                    s.reviewer.clone(),
                    s.max_iterations,
                    s.iteration,
                    s.mode,
                    s.verify_command.clone(),
                )
            };
            let prompt = plan_review_prompt(&task, &steps_summary, verify_cmd.as_deref());
            self.mode_dispatch(
                run_id,
                std::slice::from_ref(&reviewer),
                prompt,
                format!(
                    "[{mode} {run_id} · iter {iteration}/{max_iterations}] → {reviewer}: review"
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
        }
        self.persist_mode_state(run_id, step);

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
        self.persist_mode_state(run_id, step);
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
        self.persist_mode_state(run_id, step);
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
        self.persist_mode_state(run_id, step);
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
        self.persist_mode_state(run_id, step);
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
        self.mode_sessions.remove(&run_id);
        self.failed_runs.remove(&run_id);
        if let Ok(run) = self
            .store
            .update_run_status(run_id, RunStatus::Done)
        {
            step.events.push(RuntimeEvent::RunUpdated { run });
        }
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
        self.persist_mode_state_silent(run_id);
        self.mode_sessions.remove(&run_id);
        self.failed_runs.remove(&run_id);
        if let Ok(run) = self
            .store
            .update_run_status(run_id, RunStatus::Done)
        {
            step.events.push(RuntimeEvent::RunUpdated { run });
        }
        let notice = format!(
            "brainstorm {run_id} ranked result ready · {card_count} idea cards from {batch_count} \
             contributions across {rounds} \
             generation waves · {vote_count}/{participant_count} private ballots aggregated · \
             type a new topic to brainstorm again · /mode normal for regular chat · /runs for \
             details"
        );
        let _ = self.store.record_notice(None, &notice);
        step.events.push(RuntimeEvent::Notice(notice));
    }

    /// Block a mode run, record the reason, and free the live session.
    ///
    /// Inserts into `failed_runs` **before** any further turn completion
    /// can mark the run Done.
    fn block_mode_run(&mut self, run_id: RunId, reason: &str, step: &mut RuntimeStep) {
        self.failed_runs.insert(run_id);
        self.mode_sessions.remove(&run_id);
        match self.store.block_run(run_id, reason) {
            Ok(run) => {
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

    fn persist_mode_state(&mut self, run_id: RunId, step: &mut RuntimeStep) {
        let Some(session) = self.mode_sessions.get(&run_id) else {
            return;
        };
        let Ok(json) = serde_json::to_string(session) else {
            return;
        };
        if let Ok(run) = self.store.update_run_mode_state(run_id, &json) {
            step.events.push(RuntimeEvent::RunUpdated { run });
        }
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
                if let Some(session) = self.mode_sessions.get_mut(&run_id) {
                    session.pending_verdict = Some(last);
                }
                let _ = self.store.record_verdict(turn, member, approve, &summary);
                let _ = self
                    .store
                    .record_run_verdict_event(run_id, approve, &summary);
                step.events.push(RuntimeEvent::Verdict {
                    run: run_id,
                    member: member.clone(),
                    approve,
                    summary,
                });
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
                self.persist_mode_state_silent(run_id);
            } else if phase == ModePhase::Voting && participants.iter().any(|p| p == member) {
                let accepted = parsed.brainstorm_votes.last().is_some_and(|vote| {
                    let Some(session) = self.mode_sessions.get_mut(&run_id) else {
                        return false;
                    };
                    let unchanged = session.votes.iter().any(|record| {
                        &record.voter == member
                            && record.ranked == vote.ranked
                            && record.summary == vote.summary
                    });
                    if unchanged {
                        return false;
                    }
                    session.votes.retain(|record| &record.voter != member);
                    session.votes.push(BrainstormVoteRecord {
                        voter: member.clone(),
                        ranked: vote.ranked.clone(),
                        summary: vote.summary.clone(),
                    });
                    session.vote_count = session.votes.len() as u32;
                    true
                });
                if accepted
                    && let Some(vote) = parsed.brainstorm_votes.last()
                {
                    let _ = self.store.record_brainstorm_vote_event(
                        run_id,
                        member,
                        &vote.ranked,
                    );
                }
                self.persist_mode_state_silent(run_id);
            } else if phase == ModePhase::Synthesizing {
                if let Some(session) = self.mode_sessions.get_mut(&run_id) {
                    session.brainstorm_summary = truncate_mode_text(visible_text);
                }
                self.persist_mode_state_silent(run_id);
            }
        }
    }

    fn persist_mode_state_silent(&mut self, run_id: RunId) {
        let Some(session) = self.mode_sessions.get(&run_id) else {
            return;
        };
        let Ok(json) = serde_json::to_string(session) else {
            return;
        };
        let _ = self.store.update_run_mode_state(run_id, &json);
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

        if let Ok(updated) = self.store.continue_run(run.id, note.as_deref()) {
            step.events
                .push(RuntimeEvent::RunUpdated { run: updated });
        }
        self.failed_runs.remove(&run.id);
        session.cancelled = false;
        session.pending_verdict = None;
        session.idea_count = session.idea_batches.len() as u32;

        let phase = session.phase;
        let task = session.task.clone();
        let builder = session.builder.clone();
        let reviewer = session.reviewer.clone();
        let leader = session.leader.clone();
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
            ModePhase::Executing => {
                let steps = self.store.run_steps_all(run.id).unwrap_or_default();
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
                    let mut by_owner: HashMap<MemberId, Vec<(u32, String)>> = HashMap::new();
                    for s in owned {
                        if let Some(owner) = &s.owner {
                            by_owner
                                .entry(owner.clone())
                                .or_default()
                                .push((s.number, s.title.clone()));
                        }
                    }
                    let dispatches: Vec<(MemberId, String)> = by_owner
                        .into_iter()
                        .map(|(owner, owned_steps)| {
                            (
                                owner,
                                step_dispatch_prompt(run.id, &leader, &owned_steps),
                            )
                        })
                        .collect();
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
                if let Some(s) = self.mode_sessions.get_mut(&run.id) {
                    s.phase = ModePhase::Reviewing;
                    s.reviewer_nudged = false;
                }
                self.persist_mode_state(run.id, step);
                let verify_cmd = self
                    .mode_sessions
                    .get(&run.id)
                    .and_then(|s| s.verify_command.clone());
                let prompt = if mode == CollabMode::Plan {
                    let steps = self.store.run_steps_all(run.id).unwrap_or_default();
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
                self.mode_dispatch(
                    run.id,
                    std::slice::from_ref(&reviewer),
                    prompt,
                    format!(
                        "[{mode} {} · iter {iteration}/{max_iterations}] → {reviewer}: review",
                        run.id
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
                    if let Ok(updated) = self
                        .store
                        .update_run_status(run.id, RunStatus::Verifying)
                    {
                        step.events
                            .push(RuntimeEvent::RunUpdated { run: updated });
                    }
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
        CollabMode::Plan => vec![&session.leader, &session.reviewer],
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
    /// Explicit auto-verify command from mode config (review/plan).
    #[serde(default)]
    verify_command: Option<String>,
    #[serde(default)]
    builder_output: String,
    #[serde(default)]
    reviewer_nudged: bool,
    #[serde(default)]
    last_feedback: Option<String>,
    #[serde(skip)]
    pending_verdict: Option<ReviewVerdict>,
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

fn format_brainstorm_idea_set(session: &ModeSession) -> String {
    let mut occurrences: HashMap<(u32, MemberId), usize> = HashMap::new();
    let mut sections = Vec::new();
    for batch in &session.idea_batches {
        let Some(index) = participant_index(session, &batch.author) else {
            continue;
        };
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
        if batch.cards.is_empty() {
            sections.push(format!("[{label}]\n{}", batch.text));
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
            let candidate = candidate.trim().to_ascii_uppercase();
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
