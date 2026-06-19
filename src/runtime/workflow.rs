use std::{collections::VecDeque, time::Duration};

use crate::{
    adapter::{
        AgentAdapter,
        claude_print::{ClaudePrintAdapter, ClaudePrintError, ClaudePrintRunner},
        cli_pty::CliPtyAdapter,
        codex_exec::{CodexExecAdapter, CodexExecError, CodexExecRunner},
        fake::FakeAgent,
    },
    router::{
        RoutedEvent,
        envelope::TeamMessage,
        relay::{DEFAULT_MAX_AUTO_RELAYS, RelayDecision, RelayGuard},
        route_agent_output,
    },
    runtime::pty_sessions::{PtySessionManager, PtySessionManagerError},
    store::sqlite::SqliteStore,
    types::{AgentId, Participant, RouteTarget},
};

const PTY_RESPONSE_IDLE_TIMEOUT: Duration = Duration::from_millis(100);
const PTY_RESPONSE_MAX_WAIT: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub enum WorkflowCodexBackend {
    Fake(FakeAgent),
    Exec(CodexExecAdapter),
    Pty(CliPtyAdapter),
}

#[derive(Debug)]
pub enum WorkflowClaudeBackend {
    Fake(FakeAgent),
    Print(ClaudePrintAdapter),
    Pty(CliPtyAdapter),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApprovalDecision {
    Approve,
    Reject,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PendingRelay {
    id: u64,
    thread_id: String,
    from: AgentId,
    to: AgentId,
    body: String,
}

impl ApprovalDecision {
    fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "approved",
            Self::Reject => "rejected",
        }
    }

    fn event_verb(self) -> &'static str {
        match self {
            Self::Approve => "approved",
            Self::Reject => "rejected",
        }
    }
}

impl WorkflowCodexBackend {
    pub fn fake() -> Self {
        Self::Fake(FakeAgent::codex())
    }

    pub fn exec(adapter: CodexExecAdapter) -> Self {
        Self::Exec(adapter)
    }

    pub fn pty(adapter: CliPtyAdapter) -> Self {
        Self::Pty(adapter)
    }
}

impl WorkflowClaudeBackend {
    pub fn fake() -> Self {
        Self::Fake(FakeAgent::claude())
    }

    pub fn print(adapter: ClaudePrintAdapter) -> Self {
        Self::Print(adapter)
    }

    pub fn pty(adapter: CliPtyAdapter) -> Self {
        Self::Pty(adapter)
    }
}

#[derive(Debug)]
pub struct FakeWorkflow {
    store: SqliteStore,
    relay_guard: RelayGuard,
    auto_relay_paused: bool,
    pending_relays: VecDeque<PendingRelay>,
    next_pending_relay_id: u64,
    next_thread_number: u64,
    codex: WorkflowCodexBackend,
    claude: WorkflowClaudeBackend,
    pty_sessions: PtySessionManager,
}

impl FakeWorkflow {
    pub fn in_memory() -> rusqlite::Result<Self> {
        Self::with_store(SqliteStore::in_memory()?, DEFAULT_MAX_AUTO_RELAYS)
    }

    pub fn in_memory_with_backends(
        codex: WorkflowCodexBackend,
        claude: WorkflowClaudeBackend,
    ) -> rusqlite::Result<Self> {
        Self::with_store_and_backends(
            SqliteStore::in_memory()?,
            DEFAULT_MAX_AUTO_RELAYS,
            codex,
            claude,
        )
    }

    pub fn with_store(store: SqliteStore, max_auto_relays: u8) -> rusqlite::Result<Self> {
        Self::with_store_and_backends(
            store,
            max_auto_relays,
            WorkflowCodexBackend::fake(),
            WorkflowClaudeBackend::fake(),
        )
    }

    pub fn with_store_and_backends(
        store: SqliteStore,
        max_auto_relays: u8,
        codex: WorkflowCodexBackend,
        claude: WorkflowClaudeBackend,
    ) -> rusqlite::Result<Self> {
        let mut pty_sessions = PtySessionManager::new();
        if let WorkflowCodexBackend::Pty(adapter) = &codex {
            pty_sessions.set_adapter(AgentId::Codex, adapter.clone());
        }
        if let WorkflowClaudeBackend::Pty(adapter) = &claude {
            pty_sessions.set_adapter(AgentId::Claude, adapter.clone());
        }

        Ok(Self {
            store,
            relay_guard: RelayGuard::new(max_auto_relays),
            auto_relay_paused: false,
            pending_relays: VecDeque::new(),
            next_pending_relay_id: 1,
            next_thread_number: 1,
            codex,
            claude,
            pty_sessions,
        })
    }

    pub fn handle_user_message(
        &mut self,
        target: RouteTarget,
        body: &str,
    ) -> rusqlite::Result<Vec<String>> {
        let thread_id = self.next_thread_id();
        let mut events = vec![format!("{target}: {body}")];
        let mut queue = Vec::new();
        self.insert_visible_message(
            &thread_id,
            &target.from.to_string(),
            &target.to.to_string(),
            body,
        )?;

        if let Some(action_kind) = approval_action_kind(body) {
            self.store.insert_approval(&thread_id, action_kind, body)?;
            events.push(format!(
                "Approval queued for {action_kind}: automatic relay paused"
            ));
            return Ok(events);
        }

        match target.to {
            Participant::Team => {
                events.push("Team -> Claude: default planning route".to_string());
                self.insert_visible_message(
                    &thread_id,
                    "Team",
                    "Claude",
                    "default planning route",
                )?;
                queue.push((AgentId::Claude, body.to_string()));
            }
            Participant::Agent(to_agent) => {
                if let Participant::Agent(from_agent) = target.from {
                    let message = TeamMessage {
                        to: to_agent,
                        kind: "manual".to_string(),
                        body: body.to_string(),
                    };
                    self.store
                        .insert_inter_agent_message(&thread_id, from_agent, &message)?;
                    self.insert_visible_message(
                        &thread_id,
                        &from_agent.to_string(),
                        &to_agent.to_string(),
                        body,
                    )?;
                }
                queue.push((to_agent, body.to_string()));
            }
            Participant::You => {}
        }

        events.extend(self.process_agent_queue(&thread_id, queue)?);

        Ok(events)
    }

    fn process_agent_queue(
        &mut self,
        thread_id: &str,
        mut queue: Vec<(AgentId, String)>,
    ) -> rusqlite::Result<Vec<String>> {
        let mut events = Vec::new();

        while let Some((agent, prompt)) = queue.pop() {
            let output = match self.run_agent(agent, &prompt) {
                Ok(output) => output,
                Err(message) => {
                    events.push(format!("{agent} error: {message}"));
                    continue;
                }
            };
            match route_agent_output(agent, &output) {
                Ok(RoutedEvent::InterAgent { from, message }) => {
                    self.store
                        .insert_inter_agent_message(&thread_id, from, &message)?;
                    self.insert_visible_message(
                        &thread_id,
                        &from.to_string(),
                        &message.to.to_string(),
                        &message.body,
                    )?;
                    events.push(format!("{} -> {}: {}", from, message.to, message.body));

                    if self.auto_relay_paused {
                        let relay_id =
                            self.queue_pending_relay(thread_id, from, message.to, &message.body);
                        events.push(format!(
                            "Relay paused by user for {thread_id}: {} -> {} queued as #{}",
                            from, message.to, relay_id
                        ));
                        continue;
                    }

                    match self.relay_guard.record_auto_relay(thread_id) {
                        RelayDecision::Continue { .. } => {
                            queue.push((message.to, message.body));
                        }
                        RelayDecision::Pause { count } => {
                            events.push(format!(
                                "Relay paused for {thread_id}: auto relay count {count} exceeded limit"
                            ));
                        }
                    }
                }
                Ok(RoutedEvent::VisibleOutput { from, body }) => {
                    self.insert_visible_message(&thread_id, &from.to_string(), "You", &body)?;
                    events.push(format!("{from}: {body}"));
                }
                Err(err) => events.push(format!("Router error: {err:?}")),
            }
        }

        Ok(events)
    }

    pub fn inter_agent_message_count(&self) -> rusqlite::Result<i64> {
        self.store.inter_agent_message_count()
    }

    pub fn approval_count(&self) -> rusqlite::Result<i64> {
        self.store.approval_count()
    }

    pub fn pending_approval_count(&self) -> rusqlite::Result<i64> {
        self.store.pending_approval_count()
    }

    pub fn pending_approval_summaries(&self) -> rusqlite::Result<Vec<String>> {
        self.store.pending_approvals().map(|approvals| {
            approvals
                .into_iter()
                .map(|approval| {
                    format!(
                        "#{} [{}] {}: {}",
                        approval.id, approval.thread_id, approval.action_kind, approval.body
                    )
                })
                .collect()
        })
    }

    pub fn pending_relay_count(&self) -> usize {
        self.pending_relays.len()
    }

    pub fn pending_relay_summaries(&self) -> Vec<String> {
        self.pending_relays
            .iter()
            .map(|relay| {
                format!(
                    "#{} [{}] {} -> {}: {}",
                    relay.id, relay.thread_id, relay.from, relay.to, relay.body
                )
            })
            .collect()
    }

    pub fn replay_next_pending_relay(&mut self) -> rusqlite::Result<Option<Vec<String>>> {
        let Some(relay) = self.pending_relays.pop_front() else {
            return Ok(None);
        };

        let mut events = vec![format!(
            "Relay #{} replayed: {} -> {}: {}",
            relay.id, relay.from, relay.to, relay.body
        )];
        self.insert_visible_message(
            &relay.thread_id,
            "Relay",
            "You",
            events.first().expect("replay event exists"),
        )?;
        events.extend(self.process_agent_queue(&relay.thread_id, vec![(relay.to, relay.body)])?);

        Ok(Some(events))
    }

    pub fn reject_next_pending_relay(&mut self) -> rusqlite::Result<Option<String>> {
        let Some(relay) = self.pending_relays.pop_front() else {
            return Ok(None);
        };

        let event = format!(
            "Relay #{} rejected: {} -> {}: {}",
            relay.id, relay.from, relay.to, relay.body
        );
        self.insert_visible_message(&relay.thread_id, "Relay", "You", &event)?;

        Ok(Some(event))
    }

    pub fn decide_next_pending_approval(
        &mut self,
        decision: ApprovalDecision,
    ) -> rusqlite::Result<Option<String>> {
        let Some(approval) = self.store.pending_approvals()?.into_iter().next() else {
            return Ok(None);
        };

        let updated = self
            .store
            .set_approval_decision(approval.id, decision.as_str())?;
        if !updated {
            return Ok(None);
        }

        Ok(Some(format!(
            "Approval #{} {} for {}: {}",
            approval.id,
            decision.event_verb(),
            approval.action_kind,
            approval.body
        )))
    }

    pub fn terminal_event_count(&self) -> rusqlite::Result<i64> {
        self.store.terminal_event_count()
    }

    pub fn terminal_event_summaries(&self) -> rusqlite::Result<Vec<String>> {
        self.store.terminal_events().map(|events| {
            events
                .into_iter()
                .map(|event| {
                    let first_line = event.body.lines().next().unwrap_or_default();
                    format!("{} {}: {}", event.agent, event.stream, first_line)
                })
                .collect()
        })
    }

    pub fn terminal_event_raw_log(&self) -> rusqlite::Result<Vec<String>> {
        self.store.terminal_events().map(|events| {
            let mut lines = Vec::new();

            for event in events {
                lines.push(format!("#{} {} {}", event.id, event.agent, event.stream));
                if event.body.is_empty() {
                    lines.push("<empty>".to_string());
                    continue;
                }

                lines.extend(event.body.lines().map(str::to_string));
            }

            lines
        })
    }

    pub fn message_summaries(&self) -> rusqlite::Result<Vec<String>> {
        self.store.messages().map(|messages| {
            messages
                .into_iter()
                .map(|message| {
                    format!(
                        "{} -> {}: {}",
                        message.route_from, message.route_to, message.body
                    )
                })
                .collect()
        })
    }

    pub fn message_count(&self) -> rusqlite::Result<i64> {
        self.store.message_count()
    }

    pub fn auto_relay_paused(&self) -> bool {
        self.auto_relay_paused
    }

    pub fn set_auto_relay_paused(&mut self, paused: bool) {
        self.auto_relay_paused = paused;
    }

    pub fn toggle_auto_relay_pause(&mut self) -> bool {
        self.auto_relay_paused = !self.auto_relay_paused;
        self.auto_relay_paused
    }

    fn queue_pending_relay(
        &mut self,
        thread_id: &str,
        from: AgentId,
        to: AgentId,
        body: &str,
    ) -> u64 {
        let id = self.next_pending_relay_id;
        self.next_pending_relay_id += 1;
        self.pending_relays.push_back(PendingRelay {
            id,
            thread_id: thread_id.to_string(),
            from,
            to,
            body: body.to_string(),
        });
        id
    }

    fn next_thread_id(&mut self) -> String {
        let thread_id = format!("thread-{}", self.next_thread_number);
        self.next_thread_number += 1;
        thread_id
    }

    fn run_agent(&mut self, agent: AgentId, prompt: &str) -> Result<String, String> {
        match agent {
            AgentId::Codex if matches!(&self.codex, WorkflowCodexBackend::Pty(_)) => {
                self.run_pty_agent(AgentId::Codex, prompt)
            }
            AgentId::Codex => match &self.codex {
                WorkflowCodexBackend::Fake(adapter) => Ok(adapter.handle_user_message(prompt)),
                WorkflowCodexBackend::Exec(adapter) => {
                    let run = adapter.run_prompt(prompt).map_err(|error| {
                        self.persist_codex_error(&error);
                        format_codex_exec_error(error)
                    })?;
                    self.persist_terminal_output(AgentId::Codex, &run.raw_stdout, &run.raw_stderr);
                    Ok(run.final_message.unwrap_or_default())
                }
                WorkflowCodexBackend::Pty(_) => unreachable!("PTY handled before immutable borrow"),
            },
            AgentId::Claude if matches!(&self.claude, WorkflowClaudeBackend::Pty(_)) => {
                self.run_pty_agent(AgentId::Claude, prompt)
            }
            AgentId::Claude => match &self.claude {
                WorkflowClaudeBackend::Fake(adapter) => Ok(adapter.handle_user_message(prompt)),
                WorkflowClaudeBackend::Print(adapter) => {
                    let run = adapter.run_prompt(prompt).map_err(|error| {
                        self.persist_claude_error(&error);
                        format_claude_print_error(error)
                    })?;
                    self.persist_terminal_output(AgentId::Claude, &run.raw_stdout, &run.raw_stderr);
                    Ok(run.result)
                }
                WorkflowClaudeBackend::Pty(_) => {
                    unreachable!("PTY handled before immutable borrow")
                }
            },
        }
    }

    fn run_pty_agent(&mut self, agent: AgentId, prompt: &str) -> Result<String, String> {
        let run = self
            .pty_sessions
            .send_line_and_capture(
                agent,
                prompt,
                PTY_RESPONSE_IDLE_TIMEOUT,
                PTY_RESPONSE_MAX_WAIT,
            )
            .map_err(|error| format_pty_session_manager_error(agent, &error))?;
        self.persist_terminal_pty_output(agent, &run.raw_output);
        let visible_output = strip_pty_prompt_echo(prompt, &run.raw_output);
        if run.success {
            Ok(visible_output)
        } else {
            Err(format!(
                "{agent} PTY process exited with status {}",
                run.exit_code
            ))
        }
    }

    fn persist_terminal_pty_output(&self, agent: AgentId, output: &str) {
        if !output.is_empty() {
            let _ = self.store.insert_terminal_event(agent, "pty", output);
        }
    }

    fn persist_terminal_output(&self, agent: AgentId, stdout: &str, stderr: &str) {
        if !stdout.is_empty() {
            let _ = self.store.insert_terminal_event(agent, "stdout", stdout);
        }
        if !stderr.is_empty() {
            let _ = self.store.insert_terminal_event(agent, "stderr", stderr);
        }
    }

    fn insert_visible_message(
        &self,
        thread_id: &str,
        route_from: &str,
        route_to: &str,
        body: &str,
    ) -> rusqlite::Result<i64> {
        self.store
            .insert_message(Some(thread_id), route_from, route_to, body)
    }

    fn persist_codex_error(&self, error: &CodexExecError) {
        if let CodexExecError::ProcessFailed { stdout, stderr, .. } = error {
            self.persist_terminal_output(AgentId::Codex, stdout, stderr);
        }
    }

    fn persist_claude_error(&self, error: &ClaudePrintError) {
        if let ClaudePrintError::ProcessFailed { stdout, stderr, .. } = error {
            self.persist_terminal_output(AgentId::Claude, stdout, stderr);
        }
    }
}

fn format_codex_exec_error(error: CodexExecError) -> String {
    match error {
        CodexExecError::InvalidJsonLine { message, .. } => {
            format!("Codex emitted invalid JSONL: {message}")
        }
        CodexExecError::MissingEventType(_) => "Codex emitted an event without a type".to_string(),
        CodexExecError::ProcessFailed { status, stderr, .. } => {
            format!("Codex process failed with status {status:?}: {stderr}")
        }
        CodexExecError::Io(message) => format!("Codex process could not start: {message}"),
    }
}

fn format_claude_print_error(error: ClaudePrintError) -> String {
    match error {
        ClaudePrintError::InvalidJson { message } => {
            format!("Claude emitted invalid JSON: {message}")
        }
        ClaudePrintError::MissingResult => {
            "Claude JSON output did not include a result".to_string()
        }
        ClaudePrintError::ProcessFailed { status, stderr, .. } => {
            format!("Claude process failed with status {status:?}: {stderr}")
        }
        ClaudePrintError::Io(message) => format!("Claude process could not start: {message}"),
    }
}

fn format_pty_session_manager_error(agent: AgentId, error: &PtySessionManagerError) -> String {
    format!("{agent} PTY failed: {error}")
}

fn strip_pty_prompt_echo(prompt: &str, output: &str) -> String {
    let Some(rest) = output.strip_prefix(prompt) else {
        return output.to_string();
    };

    if let Some(rest) = rest.strip_prefix("\r\n") {
        rest.to_string()
    } else if let Some(rest) = rest.strip_prefix('\n') {
        rest.to_string()
    } else {
        output.to_string()
    }
}

fn approval_action_kind(body: &str) -> Option<&'static str> {
    let lower = body.to_ascii_lowercase();
    let words = lower.split_whitespace().collect::<Vec<_>>();

    if words.iter().any(|word| *word == "git")
        || lower.starts_with("git ")
        || lower.contains(" git ")
    {
        return Some("git");
    }

    if words
        .iter()
        .any(|word| matches!(*word, "shell" | "bash" | "sh" | "zsh" | "command"))
        || lower.contains("run `")
    {
        return Some("shell");
    }

    if lower.contains("edit file")
        || lower.contains("modify file")
        || lower.contains("write file")
        || lower.contains("delete file")
        || lower.contains("remove file")
        || words
            .iter()
            .any(|word| matches!(*word, "rm" | "mv" | "chmod" | "chown"))
    {
        return Some("file");
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn team_message_routes_to_claude_then_codex_and_persists_relays() {
        let mut workflow = FakeWorkflow::with_store(SqliteStore::in_memory().unwrap(), 2).unwrap();
        let target = RouteTarget::new(Participant::You, Participant::Team);

        let events = workflow
            .handle_user_message(target, "build the parser")
            .expect("workflow should run");

        assert!(
            events
                .iter()
                .any(|event| event == "You -> Team: build the parser")
        );
        assert!(
            events
                .iter()
                .any(|event| event == "Team -> Claude: default planning route")
        );
        assert!(events.iter().any(|event| event.contains("Claude -> Codex")));
        assert!(events.iter().any(|event| event.contains("Codex -> Claude")));
        assert!(events.iter().any(|event| event.contains("Relay paused")));
        assert_eq!(workflow.inter_agent_message_count().unwrap(), 3);
        assert_eq!(workflow.message_count().unwrap(), 5);
    }

    #[test]
    fn manual_agent_to_agent_message_is_persisted_before_delivery() {
        let mut workflow = FakeWorkflow::with_store(SqliteStore::in_memory().unwrap(), 0).unwrap();
        let target = RouteTarget::new(
            Participant::Agent(AgentId::Codex),
            Participant::Agent(AgentId::Claude),
        );

        let events = workflow
            .handle_user_message(target, "review this plan")
            .expect("workflow should run");

        assert!(
            events
                .iter()
                .any(|event| event == "Codex -> Claude: review this plan")
        );
        assert_eq!(workflow.inter_agent_message_count().unwrap(), 2);
        assert_eq!(workflow.message_count().unwrap(), 3);
    }

    #[test]
    fn risky_requests_are_queued_for_approval_without_auto_relay() {
        let mut workflow = FakeWorkflow::with_store(SqliteStore::in_memory().unwrap(), 5).unwrap();
        let target = RouteTarget::new(Participant::You, Participant::Team);

        let events = workflow
            .handle_user_message(target, "run git status before planning")
            .expect("workflow should run");

        assert!(
            events
                .iter()
                .any(|event| event == "You -> Team: run git status before planning")
        );
        assert!(
            events
                .iter()
                .any(|event| event == "Approval queued for git: automatic relay paused")
        );
        assert_eq!(workflow.approval_count().unwrap(), 1);
        assert_eq!(workflow.inter_agent_message_count().unwrap(), 0);
        assert_eq!(workflow.message_count().unwrap(), 1);
    }

    #[test]
    fn pending_approval_can_be_summarized_and_decided() {
        let mut workflow = FakeWorkflow::with_store(SqliteStore::in_memory().unwrap(), 5).unwrap();
        let target = RouteTarget::new(Participant::You, Participant::Team);

        workflow
            .handle_user_message(target, "run git status before planning")
            .expect("workflow should run");

        assert_eq!(
            workflow.pending_approval_summaries().unwrap(),
            vec!["#1 [thread-1] git: run git status before planning"]
        );

        let event = workflow
            .decide_next_pending_approval(ApprovalDecision::Approve)
            .expect("approval decision should save")
            .expect("pending approval should exist");

        assert_eq!(
            event,
            "Approval #1 approved for git: run git status before planning"
        );
        assert_eq!(workflow.pending_approval_count().unwrap(), 0);
        assert_eq!(
            workflow
                .decide_next_pending_approval(ApprovalDecision::Reject)
                .expect("empty queue should query"),
            None
        );
    }

    #[test]
    fn message_summaries_return_visible_log() {
        let mut workflow = FakeWorkflow::with_store(SqliteStore::in_memory().unwrap(), 0).unwrap();
        let target = RouteTarget::new(Participant::You, Participant::Team);

        workflow
            .handle_user_message(target, "plan")
            .expect("workflow should run");

        let messages = workflow.message_summaries().unwrap();
        assert!(
            messages
                .iter()
                .any(|message| message == "You -> Team: plan")
        );
        assert!(
            messages
                .iter()
                .any(|message| message == "Team -> Claude: default planning route")
        );
    }

    #[test]
    fn terminal_raw_log_preserves_multiline_body() {
        let store = SqliteStore::in_memory().unwrap();
        store
            .insert_terminal_event(AgentId::Codex, "stderr", "first line\nsecond line")
            .expect("terminal event should insert");
        let workflow = FakeWorkflow::with_store(store, 5).unwrap();

        assert_eq!(
            workflow.terminal_event_raw_log().unwrap(),
            vec!["#1 Codex stderr", "first line", "second line"]
        );
    }

    #[test]
    fn user_paused_relay_records_inter_agent_message_without_auto_delivery() {
        let mut workflow = FakeWorkflow::with_store(SqliteStore::in_memory().unwrap(), 5).unwrap();
        workflow.set_auto_relay_paused(true);
        let target = RouteTarget::new(Participant::You, Participant::Team);

        let events = workflow
            .handle_user_message(target, "plan")
            .expect("workflow should run");

        assert!(workflow.auto_relay_paused());
        assert!(events.iter().any(|event| event.contains("Claude -> Codex")));
        assert!(events.iter().any(|event| {
            event == "Relay paused by user for thread-1: Claude -> Codex queued as #1"
        }));
        assert!(!events.iter().any(|event| event.contains("Codex -> Claude")));
        assert_eq!(
            workflow.pending_relay_summaries(),
            vec!["#1 [thread-1] Claude -> Codex: Implement the plan for: plan"]
        );
        assert_eq!(workflow.pending_relay_count(), 1);
        assert_eq!(workflow.inter_agent_message_count().unwrap(), 1);
        assert_eq!(workflow.message_count().unwrap(), 3);
    }

    #[test]
    fn manual_agent_to_agent_delivery_still_works_when_auto_relay_is_paused() {
        let mut workflow = FakeWorkflow::with_store(SqliteStore::in_memory().unwrap(), 0).unwrap();
        workflow.set_auto_relay_paused(true);
        let target = RouteTarget::new(
            Participant::Agent(AgentId::Codex),
            Participant::Agent(AgentId::Claude),
        );

        let events = workflow
            .handle_user_message(target, "review this plan")
            .expect("workflow should run");

        assert!(
            events
                .iter()
                .any(|event| event == "Codex -> Claude: review this plan")
        );
        assert!(events.iter().any(|event| {
            event == "Relay paused by user for thread-1: Claude -> Codex queued as #1"
        }));
        assert_eq!(workflow.inter_agent_message_count().unwrap(), 2);
    }

    #[test]
    fn pending_relay_can_be_replayed_under_user_control() {
        let mut workflow = FakeWorkflow::with_store(SqliteStore::in_memory().unwrap(), 5).unwrap();
        workflow.set_auto_relay_paused(true);
        let target = RouteTarget::new(Participant::You, Participant::Team);

        workflow
            .handle_user_message(target, "plan")
            .expect("workflow should run");

        let events = workflow
            .replay_next_pending_relay()
            .expect("relay replay should query")
            .expect("pending relay should exist");

        assert_eq!(
            events.first(),
            Some(&"Relay #1 replayed: Claude -> Codex: Implement the plan for: plan".to_string())
        );
        assert!(events.iter().any(|event| event.contains("Codex -> Claude")));
        assert_eq!(
            workflow.pending_relay_summaries(),
            vec![
                "#2 [thread-1] Codex -> Claude: Please review Codex next step for: Implement the plan for: plan"
            ]
        );
        assert_eq!(workflow.pending_relay_count(), 1);
        assert_eq!(workflow.inter_agent_message_count().unwrap(), 2);
    }

    #[test]
    fn pending_relay_can_be_rejected() {
        let mut workflow = FakeWorkflow::with_store(SqliteStore::in_memory().unwrap(), 5).unwrap();
        workflow.set_auto_relay_paused(true);
        let target = RouteTarget::new(Participant::You, Participant::Team);

        workflow
            .handle_user_message(target, "plan")
            .expect("workflow should run");

        let event = workflow
            .reject_next_pending_relay()
            .expect("relay reject should query")
            .expect("pending relay should exist");

        assert_eq!(
            event,
            "Relay #1 rejected: Claude -> Codex: Implement the plan for: plan"
        );
        assert_eq!(workflow.pending_relay_count(), 0);
        assert_eq!(
            workflow
                .reject_next_pending_relay()
                .expect("empty relay queue should query"),
            None
        );
        assert_eq!(workflow.inter_agent_message_count().unwrap(), 1);
    }

    #[test]
    fn real_backend_errors_are_visible_events() {
        let mut workflow = FakeWorkflow::with_store_and_backends(
            SqliteStore::in_memory().unwrap(),
            5,
            WorkflowCodexBackend::exec(CodexExecAdapter::new("/tmp").with_binary("missing-codex")),
            WorkflowClaudeBackend::fake(),
        )
        .unwrap();
        let target = RouteTarget::new(Participant::You, Participant::Agent(AgentId::Codex));

        let events = workflow
            .handle_user_message(target, "summarize")
            .expect("workflow should run");

        assert!(
            events
                .iter()
                .any(|event| event.contains("Codex error: Codex process could not start"))
        );
    }

    #[test]
    fn process_failure_raw_output_is_persisted_as_terminal_events() {
        let mut workflow = FakeWorkflow::with_store_and_backends(
            SqliteStore::in_memory().unwrap(),
            5,
            WorkflowCodexBackend::exec(CodexExecAdapter::new("/tmp").with_binary("/bin/sh")),
            WorkflowClaudeBackend::fake(),
        )
        .unwrap();
        let target = RouteTarget::new(Participant::You, Participant::Agent(AgentId::Codex));

        workflow
            .handle_user_message(target, "summarize")
            .expect("workflow should run");

        assert_eq!(workflow.terminal_event_count().unwrap(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn pty_backend_output_is_visible_and_persisted_as_terminal_event() {
        let mut workflow = FakeWorkflow::with_store_and_backends(
            SqliteStore::in_memory().unwrap(),
            5,
            WorkflowCodexBackend::pty(CliPtyAdapter::new("/bin/sh", "/tmp").with_args([
                "-lc",
                "while IFS= read -r line; do printf 'Codex PTY:%s\\n' \"$line\"; done",
            ])),
            WorkflowClaudeBackend::fake(),
        )
        .unwrap();
        let target = RouteTarget::new(Participant::You, Participant::Agent(AgentId::Codex));

        let events = workflow
            .handle_user_message(target, "summarize")
            .expect("workflow should run");

        assert!(
            events
                .iter()
                .any(|event| event.contains("Codex: Codex PTY:summarize"))
        );
        assert_eq!(workflow.terminal_event_count().unwrap(), 1);
        assert!(
            workflow
                .terminal_event_raw_log()
                .unwrap()
                .iter()
                .any(|event| event.contains("Codex PTY:summarize"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn pty_backend_reuses_managed_session_between_messages() {
        let mut workflow = FakeWorkflow::with_store_and_backends(
            SqliteStore::in_memory().unwrap(),
            5,
            WorkflowCodexBackend::pty(CliPtyAdapter::new("/bin/sh", "/tmp").with_args([
                "-lc",
                "count=0; while IFS= read -r line; do count=$((count + 1)); printf 'Codex PTY:%s:%s\\n' \"$count\" \"$line\"; done",
            ])),
            WorkflowClaudeBackend::fake(),
        )
        .unwrap();
        let target = RouteTarget::new(Participant::You, Participant::Agent(AgentId::Codex));

        let first = workflow
            .handle_user_message(target, "first")
            .expect("workflow should run");
        let second = workflow
            .handle_user_message(target, "second")
            .expect("workflow should run");

        assert!(
            first
                .iter()
                .any(|event| event.contains("Codex: Codex PTY:1:first"))
        );
        assert!(
            second
                .iter()
                .any(|event| event.contains("Codex: Codex PTY:2:second"))
        );
        assert_eq!(workflow.terminal_event_count().unwrap(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn pty_backend_nonzero_exit_is_visible_error_with_raw_output_persisted() {
        let mut workflow = FakeWorkflow::with_store_and_backends(
            SqliteStore::in_memory().unwrap(),
            5,
            WorkflowCodexBackend::pty(
                CliPtyAdapter::new("/bin/sh", "/tmp")
                    .with_args(["-lc", "printf 'before-fail\\n'; exit 9"]),
            ),
            WorkflowClaudeBackend::fake(),
        )
        .unwrap();
        let target = RouteTarget::new(Participant::You, Participant::Agent(AgentId::Codex));

        let events = workflow
            .handle_user_message(target, "summarize")
            .expect("workflow should run");

        assert!(
            events
                .iter()
                .any(|event| { event == "Codex error: Codex PTY process exited with status 9" })
        );
        assert_eq!(workflow.terminal_event_count().unwrap(), 1);
        assert!(
            workflow
                .terminal_event_raw_log()
                .unwrap()
                .iter()
                .any(|event| event.contains("before-fail"))
        );
    }
}
