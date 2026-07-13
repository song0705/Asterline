// Mode-run handlers (review, lead, roundtable).
// Included at the bottom of team_runtime.rs so private fields stay accessible.

impl TeamRuntime {
    fn handle_run_mode(
        &mut self,
        mode: CollabMode,
        task: String,
        overrides: Vec<(String, String)>,
        step: &mut RuntimeStep,
    ) {
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

        let (roles, limits) = match resolve_mode_roles(&self.config, mode, &overrides) {
            Ok(resolved) => resolved,
            Err(err) => {
                step.events.push(RuntimeEvent::Notice(err));
                return;
            }
        };

        if mode == CollabMode::Roundtable && roles.participants.len() < 2 {
            step.events.push(RuntimeEvent::Notice(
                "roundtable needs at least two participants".to_string(),
            ));
            return;
        }

        let (phase, iteration, round) = match mode {
            CollabMode::Review => (ModePhase::Building, 1, 0),
            CollabMode::Lead => (ModePhase::Leading, 1, 0),
            CollabMode::Roundtable => (ModePhase::Rounds, 0, 1),
        };

        let session = ModeSession {
            mode,
            phase,
            task: task.clone(),
            builder: roles.builder.clone(),
            reviewer: roles.reviewer.clone(),
            leader: roles.leader.clone(),
            participants: roles.participants.clone(),
            moderator: roles.moderator.clone(),
            iteration,
            max_iterations: limits.max_iterations,
            round,
            rounds: limits.rounds,
            auto_verify: limits.auto_verify,
            builder_output: String::new(),
            reviewer_nudged: false,
            last_feedback: None,
            pending_verdict: None,
            reviewer_last_text: String::new(),
            cancelled: false,
            transcripts: Vec::new(),
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
            CollabMode::Lead => Some(&session.leader),
            CollabMode::Roundtable => session
                .moderator
                .as_ref()
                .or_else(|| session.participants.first()),
        };

        let run = match self.store.create_mode_workflow_run(
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
        step.events.push(RuntimeEvent::WorkflowRunUpdated { run });

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
            CollabMode::Lead => {
                step.events.push(RuntimeEvent::Notice(format!(
                    "lead {run_id} started → {} (reviewer: {})",
                    session.leader, session.reviewer
                )));
                let leader = session.leader.clone();
                self.mode_sessions.insert(run_id, session);
                let teammates = self.lead_teammate_list();
                let prompt = lead_plan_prompt(&task, &teammates);
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
            CollabMode::Roundtable => {
                let n = session.participants.len();
                step.events.push(RuntimeEvent::Notice(format!(
                    "roundtable {run_id} started · {n} participants · {} rounds",
                    session.rounds
                )));
                let participants = session.participants.clone();
                let rounds = session.rounds;
                self.mode_sessions.insert(run_id, session);
                let prompt = roundtable_prompt(&task, 1, rounds);
                self.mode_dispatch(
                    run_id,
                    &participants,
                    prompt,
                    format!("[{mode} {run_id} · round 1/{rounds}] discussion"),
                    step,
                );
            }
        }
    }

    fn lead_teammate_list(&self) -> Vec<(String, String)> {
        self.config
            .members
            .iter()
            .map(|m| (m.id.to_string(), m.role.clone()))
            .collect()
    }

    /// Dispatch one mode phase as a single turn (approval-gated enqueue).
    fn mode_dispatch(
        &mut self,
        run_id: WorkflowRunId,
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
        run_id: WorkflowRunId,
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
        self.workflow_turns.insert(turn, run_id);

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

    fn mode_on_turn_complete(&mut self, run_id: WorkflowRunId, step: &mut RuntimeStep) {
        let Some(session) = self.mode_sessions.get(&run_id) else {
            return;
        };
        match session.mode {
            CollabMode::Review => self.mode_review_on_turn_complete(run_id, step),
            CollabMode::Lead => self.mode_lead_on_turn_complete(run_id, step),
            CollabMode::Roundtable => self.mode_roundtable_on_turn_complete(run_id, step),
        }
    }

    fn mode_review_on_turn_complete(&mut self, run_id: WorkflowRunId, step: &mut RuntimeStep) {
        let failed = self.failed_workflow_runs.contains(&run_id);
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
                let prompt = review_prompt(
                    &session.task,
                    &builder_display,
                    &session.builder_output,
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

    /// Shared approve / request_changes / nudge path for Review and Lead review phases.
    fn mode_handle_verdict_phase(
        &mut self,
        run_id: WorkflowRunId,
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
                let auto_verify = self
                    .mode_sessions
                    .get(&run_id)
                    .map(|s| s.auto_verify)
                    .unwrap_or(false);
                if auto_verify
                    && let Some(cmd) =
                        suggested_verify_command(&self.config.workspace).map(ToString::to_string)
                {
                    if let Some(session) = self.mode_sessions.get_mut(&run_id) {
                        session.phase = ModePhase::Verifying;
                    }
                    self.persist_mode_state(run_id, step);
                    if let Ok(run) = self
                        .store
                        .update_workflow_status(run_id, WorkflowRunStatus::Verifying)
                    {
                        step.events.push(RuntimeEvent::WorkflowRunUpdated { run });
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

    fn mode_lead_on_turn_complete(&mut self, run_id: WorkflowRunId, step: &mut RuntimeStep) {
        let failed = self.failed_workflow_runs.contains(&run_id);
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
            ModePhase::Leading => self.mode_lead_on_leading_complete(run_id, &session, step),
            ModePhase::Executing => self.mode_lead_on_executing_complete(run_id, &session, step),
            ModePhase::Reviewing | ModePhase::AwaitingVerdict | ModePhase::Verifying => {
                self.mode_review_on_turn_complete(run_id, step);
            }
            _ => {}
        }
    }

    fn mode_lead_on_leading_complete(
        &mut self,
        run_id: WorkflowRunId,
        session: &ModeSession,
        step: &mut RuntimeStep,
    ) {
        let steps = match self.store.workflow_steps_all(run_id) {
            Ok(steps) => steps,
            Err(err) => {
                step.events.push(RuntimeEvent::Notice(format!(
                    "could not load checklist for {run_id}: {err}"
                )));
                return;
            }
        };

        let owned_todos: Vec<&WorkflowStepSummary> = steps
            .iter()
            .filter(|s| s.owner.is_some() && s.status == WorkflowStepStatus::Todo)
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
                    lead_nudge_prompt(),
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

        // Mark owned todos as Doing; emit only the last WorkflowRunUpdated.
        let mut last_run = None;
        for s in &owned_todos {
            if let Ok(run) =
                self.store
                    .update_workflow_step(run_id, s.number, WorkflowStepStatus::Doing, None)
            {
                last_run = Some(run);
            }
        }
        if let Some(run) = last_run {
            step.events.push(RuntimeEvent::WorkflowRunUpdated { run });
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
        let dispatches: Vec<(MemberId, String)> = by_owner
            .into_iter()
            .map(|(owner, owned_steps)| {
                let prompt = step_dispatch_prompt(run_id, &owned_steps);
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

    fn mode_lead_on_executing_complete(
        &mut self,
        run_id: WorkflowRunId,
        session: &ModeSession,
        step: &mut RuntimeStep,
    ) {
        let steps = match self.store.workflow_steps_all(run_id) {
            Ok(steps) => steps,
            Err(err) => {
                step.events.push(RuntimeEvent::Notice(format!(
                    "could not load checklist for {run_id}: {err}"
                )));
                return;
            }
        };

        let unfinished: Vec<&WorkflowStepSummary> = steps
            .iter()
            .filter(|s| s.status != WorkflowStepStatus::Done)
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
            let prompt = lead_review_prompt(&task, &steps_summary);
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

        let unfinished_lines: Vec<String> = unfinished
            .iter()
            .map(|s| format!("#{} {} {}", s.number, s.status.as_str(), s.title))
            .collect();

        if let Some(s) = self.mode_sessions.get_mut(&run_id) {
            s.iteration = next_iteration;
            s.phase = ModePhase::Leading;
            s.reviewer_nudged = false;
        }
        self.persist_mode_state(run_id, step);

        let task = session.task.clone();
        let max_iterations = session.max_iterations;
        let mode = session.mode;
        let leader = session.leader.clone();
        let prompt = lead_progress_prompt(&task, &unfinished_lines, next_iteration, max_iterations);
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

    fn mode_roundtable_on_turn_complete(
        &mut self,
        run_id: WorkflowRunId,
        step: &mut RuntimeStep,
    ) {
        let failed = self.failed_workflow_runs.contains(&run_id);
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
            ModePhase::Rounds => {
                if session.round < session.rounds {
                    let next_round = session.round.saturating_add(1);
                    let digests = self.roundtable_digest_dispatches(&session, next_round);
                    if let Some(s) = self.mode_sessions.get_mut(&run_id) {
                        s.round = next_round;
                        s.transcripts.clear();
                    }
                    self.persist_mode_state(run_id, step);
                    let rounds = session.rounds;
                    let mode = session.mode;
                    self.mode_dispatch_multi(
                        run_id,
                        digests,
                        format!("[{mode} {run_id} · round {next_round}/{rounds}] discussion"),
                        step,
                    );
                } else if let Some(moderator) = session.moderator.clone() {
                    if let Some(s) = self.mode_sessions.get_mut(&run_id) {
                        s.phase = ModePhase::Moderating;
                    }
                    self.persist_mode_state(run_id, step);
                    let transcript = self.format_roundtable_transcript(&session);
                    let prompt = moderator_prompt(&session.task, &transcript);
                    let mode = session.mode;
                    self.mode_dispatch(
                        run_id,
                        std::slice::from_ref(&moderator),
                        prompt,
                        format!("[{mode} {run_id}] → {moderator}: moderate"),
                        step,
                    );
                } else {
                    self.finish_mode_run_roundtable(run_id, step);
                }
            }
            ModePhase::Moderating => {
                self.finish_mode_run_roundtable(run_id, step);
            }
            _ => {}
        }
    }

    fn roundtable_digest_dispatches(
        &self,
        session: &ModeSession,
        next_round: u32,
    ) -> Vec<(MemberId, String)> {
        session
            .participants
            .iter()
            .map(|participant| {
                let others = self.roundtable_others_digest(session, participant);
                let prompt = roundtable_digest_prompt(
                    &session.task,
                    next_round,
                    session.rounds,
                    &others,
                );
                (participant.clone(), prompt)
            })
            .collect()
    }

    fn roundtable_others_digest(&self, session: &ModeSession, participant: &MemberId) -> String {
        session
            .participants
            .iter()
            .filter(|other| *other != participant)
            .map(|other| {
                let display = self.member_display(other);
                let text = session
                    .transcripts
                    .iter()
                    .find(|(id, _)| id == other)
                    .map(|(_, t)| t.as_str())
                    .unwrap_or("(no reply)");
                format!("{display}: {text}")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn format_roundtable_transcript(&self, session: &ModeSession) -> String {
        session
            .participants
            .iter()
            .map(|member| {
                let display = self.member_display(member);
                let text = session
                    .transcripts
                    .iter()
                    .find(|(id, _)| id == member)
                    .map(|(_, t)| t.as_str())
                    .unwrap_or("(no reply)");
                format!("{display}: {text}")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn mode_request_changes(
        &mut self,
        run_id: WorkflowRunId,
        feedback: String,
        step: &mut RuntimeStep,
    ) {
        let (mode, target, task, max_iterations, next_iteration, reviewer) = {
            let Some(session) = self.mode_sessions.get(&run_id) else {
                return;
            };
            let next_iteration = session.iteration.saturating_add(1);
            let target = match session.mode {
                CollabMode::Lead => session.leader.clone(),
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
                CollabMode::Lead => {
                    session.phase = ModePhase::Leading;
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
            CollabMode::Lead => lead_iteration_prompt(
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

    fn finish_mode_run_approved(&mut self, run_id: WorkflowRunId, step: &mut RuntimeStep) {
        self.mode_sessions.remove(&run_id);
        self.failed_workflow_runs.remove(&run_id);
        if let Ok(run) = self
            .store
            .update_workflow_status(run_id, WorkflowRunStatus::Done)
        {
            step.events.push(RuntimeEvent::WorkflowRunUpdated { run });
        }
        step.events.push(RuntimeEvent::Notice(format!(
            "{run_id} approved — done"
        )));
    }

    fn finish_mode_run_roundtable(&mut self, run_id: WorkflowRunId, step: &mut RuntimeStep) {
        self.mode_sessions.remove(&run_id);
        self.failed_workflow_runs.remove(&run_id);
        if let Ok(run) = self
            .store
            .update_workflow_status(run_id, WorkflowRunStatus::Done)
        {
            step.events.push(RuntimeEvent::WorkflowRunUpdated { run });
        }
        step.events.push(RuntimeEvent::Notice(format!(
            "roundtable {run_id} finished"
        )));
    }

    /// Block a mode run, record the reason, and free the live session.
    ///
    /// Inserts into `failed_workflow_runs` **before** any further turn completion
    /// can mark the run Done.
    fn block_mode_run(&mut self, run_id: WorkflowRunId, reason: &str, step: &mut RuntimeStep) {
        self.failed_workflow_runs.insert(run_id);
        self.mode_sessions.remove(&run_id);
        match self.store.block_workflow_run(run_id, reason) {
            Ok(run) => {
                step.events.push(RuntimeEvent::WorkflowRunUpdated { run });
                step.events
                    .push(RuntimeEvent::Notice(format!("{run_id} blocked: {reason}")));
            }
            Err(err) => step.events.push(RuntimeEvent::Notice(format!(
                "could not block mode run {run_id}: {err}"
            ))),
        }
    }

    fn block_all_mode_sessions(&mut self, reason: &str, step: &mut RuntimeStep) {
        let ids: Vec<WorkflowRunId> = self.mode_sessions.keys().copied().collect();
        for run_id in ids {
            self.block_mode_run(run_id, reason, step);
        }
    }

    fn persist_mode_state(&mut self, run_id: WorkflowRunId, step: &mut RuntimeStep) {
        let Some(session) = self.mode_sessions.get(&run_id) else {
            return;
        };
        let Ok(json) = serde_json::to_string(session) else {
            return;
        };
        if let Ok(run) = self.store.update_workflow_mode_state(run_id, &json) {
            step.events.push(RuntimeEvent::WorkflowRunUpdated { run });
        }
    }

    /// Record mode-relevant envelopes and text after a member message completes.
    fn mode_record_message(
        &mut self,
        member: &MemberId,
        turn: TurnId,
        visible_text: &str,
        reviews: &[ReviewVerdict],
        step: &mut RuntimeStep,
    ) {
        let run_id = self.workflow_turns.get(&turn).copied();
        let session_meta = run_id.and_then(|id| {
            self.mode_sessions.get(&id).map(|s| {
                (
                    id,
                    s.builder.clone(),
                    s.reviewer.clone(),
                    s.phase,
                    s.mode,
                    s.participants.clone(),
                )
            })
        });

        if !reviews.is_empty() {
            let last = reviews.last().cloned().expect("non-empty");
            let approve = matches!(last.verdict, ReviewVerdictKind::Approve);
            let summary = last.summary.clone().unwrap_or_default();

            let accept = session_meta.as_ref().is_some_and(|(_, _, reviewer, phase, _, _)| {
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
                    .record_workflow_verdict_event(run_id, approve, &summary);
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

        if let Some((run_id, builder, reviewer, phase, _mode, participants)) = session_meta {
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
            } else if phase == ModePhase::Rounds
                && participants.iter().any(|p| p == member)
                && !visible_text.trim().is_empty()
            {
                if let Some(session) = self.mode_sessions.get_mut(&run_id) {
                    let truncated =
                        truncate_mode_text_limit(visible_text, ROUNDTABLE_TRANSCRIPT_LIMIT);
                    if let Some(entry) = session.transcripts.iter_mut().find(|(id, _)| id == member)
                    {
                        entry.1 = truncated;
                    } else {
                        session.transcripts.push((member.clone(), truncated));
                    }
                }
                // Keep transcripts on disk so /continue can rebuild digests.
                self.persist_mode_state_silent(run_id);
            }
        }
    }

    fn persist_mode_state_silent(&mut self, run_id: WorkflowRunId) {
        let Some(session) = self.mode_sessions.get(&run_id) else {
            return;
        };
        let Ok(json) = serde_json::to_string(session) else {
            return;
        };
        let _ = self.store.update_workflow_mode_state(run_id, &json);
    }

    fn mode_mark_turn_cancelled(&mut self, turn: TurnId) {
        let Some(run_id) = self.workflow_turns.get(&turn).copied() else {
            return;
        };
        if let Some(session) = self.mode_sessions.get_mut(&run_id) {
            session.cancelled = true;
        }
    }

    fn mode_resume(
        &mut self,
        run: WorkflowRunSummary,
        note: Option<String>,
        step: &mut RuntimeStep,
    ) {
        let state_json = match self.store.workflow_mode_state(run.id) {
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

        let missing = mode_resume_missing_members(&session, &self.config);
        if !missing.is_empty() {
            step.events.push(RuntimeEvent::Notice(format!(
                "could not resume {}: member(s) left the roster: {}",
                run.id,
                missing.join(", ")
            )));
            return;
        }

        if let Ok(updated) = self.store.continue_workflow_run(run.id, note.as_deref()) {
            step.events
                .push(RuntimeEvent::WorkflowRunUpdated { run: updated });
        }
        self.failed_workflow_runs.remove(&run.id);
        session.cancelled = false;
        session.pending_verdict = None;

        let phase = session.phase;
        let task = session.task.clone();
        let builder = session.builder.clone();
        let reviewer = session.reviewer.clone();
        let participants = session.participants.clone();
        let moderator = session.moderator.clone();
        let iteration = session.iteration;
        let max_iterations = session.max_iterations;
        let mode = session.mode;
        let last_feedback = session.last_feedback.clone();
        let builder_output = session.builder_output.clone();
        let round = session.round;
        let rounds = session.rounds;

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
            ModePhase::Leading => {
                self.mode_resume_leading(run.id, step);
            }
            ModePhase::Executing => {
                let steps = self.store.workflow_steps_all(run.id).unwrap_or_default();
                let owned: Vec<&WorkflowStepSummary> = steps
                    .iter()
                    .filter(|s| {
                        s.owner.is_some()
                            && s.status != WorkflowStepStatus::Done
                    })
                    .collect();
                if owned.is_empty() {
                    self.mode_resume_leading(run.id, step);
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
                            (owner, step_dispatch_prompt(run.id, &owned_steps))
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
            ModePhase::Rounds => {
                if round <= 1 {
                    let prompt = roundtable_prompt(&task, 1, rounds);
                    self.mode_dispatch(
                        run.id,
                        &participants,
                        prompt,
                        format!("[{mode} {} · round 1/{rounds}] discussion", run.id),
                        step,
                    );
                } else {
                    // Rebuild digests from persisted transcripts of the prior round.
                    let digests = {
                        let s = &self.mode_sessions[&run.id];
                        self.roundtable_digest_dispatches(s, round)
                    };
                    self.mode_dispatch_multi(
                        run.id,
                        digests,
                        format!("[{mode} {} · round {round}/{rounds}] discussion", run.id),
                        step,
                    );
                }
            }
            ModePhase::Moderating => {
                if let Some(moderator) = moderator {
                    let transcript = {
                        let s = &self.mode_sessions[&run.id];
                        self.format_roundtable_transcript(s)
                    };
                    let prompt = moderator_prompt(&task, &transcript);
                    self.mode_dispatch(
                        run.id,
                        std::slice::from_ref(&moderator),
                        prompt,
                        format!("[{mode} {}] → {moderator}: moderate", run.id),
                        step,
                    );
                } else {
                    self.finish_mode_run_roundtable(run.id, step);
                }
            }
            ModePhase::Reviewing | ModePhase::AwaitingVerdict => {
                if let Some(s) = self.mode_sessions.get_mut(&run.id) {
                    s.phase = ModePhase::Reviewing;
                    s.reviewer_nudged = false;
                }
                self.persist_mode_state(run.id, step);
                let prompt = if mode == CollabMode::Lead {
                    let steps = self.store.workflow_steps_all(run.id).unwrap_or_default();
                    let summary = format_lead_steps_summary(&steps);
                    lead_review_prompt(&task, &summary)
                } else {
                    let builder_display = self.member_display(&builder);
                    review_prompt(&task, &builder_display, &builder_output)
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
                if let Some(cmd) =
                    suggested_verify_command(&self.config.workspace).map(ToString::to_string)
                {
                    if let Ok(updated) = self
                        .store
                        .update_workflow_status(run.id, WorkflowRunStatus::Verifying)
                    {
                        step.events
                            .push(RuntimeEvent::WorkflowRunUpdated { run: updated });
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
        }
    }

    fn mode_resume_leading(&mut self, run_id: WorkflowRunId, step: &mut RuntimeStep) {
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
        let teammates = self.lead_teammate_list();
        let base = lead_plan_prompt(&task, &teammates);
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
        CollabMode::Lead => vec![&session.leader, &session.reviewer],
        CollabMode::Roundtable => {
            let mut ids: Vec<&MemberId> = session.participants.iter().collect();
            if let Some(m) = &session.moderator {
                ids.push(m);
            }
            ids
        }
    };
    needed.sort_by_key(|id| id.as_str());
    needed.dedup();
    needed
        .into_iter()
        .filter(|id| config.member(id).is_none())
        .map(|id| id.to_string())
        .collect()
}

fn format_lead_steps_summary(steps: &[WorkflowStepSummary]) -> String {
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

/// Current phase of a mode session. Serialized as its snake_case string.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ModePhase {
    Building,
    Leading,
    Executing,
    Rounds,
    Moderating,
    Reviewing,
    AwaitingVerdict,
    Verifying,
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
    moderator: Option<MemberId>,
    iteration: u32,
    max_iterations: u32,
    round: u32,
    rounds: u32,
    auto_verify: bool,
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
    /// Per-participant latest text of the current roundtable round (truncated).
    #[serde(default)]
    transcripts: Vec<(MemberId, String)>,
}

const MODE_TEXT_LIMIT: usize = 4000;
const ROUNDTABLE_TRANSCRIPT_LIMIT: usize = 1200;

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
