use std::{
    collections::HashMap,
    fmt, thread,
    time::{Duration, Instant},
};

use crate::{
    adapter::cli_pty::{CliPtyAdapter, CliPtyError, CliPtyRun, CliPtySession, CliPtySessionStatus},
    types::AgentId,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PtySessionManagerError {
    MissingConfig(AgentId),
    AlreadyRunning(AgentId),
    NotRunning(AgentId),
    Pty(CliPtyError),
}

impl fmt::Display for PtySessionManagerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingConfig(agent) => write!(f, "no PTY adapter configured for {agent}"),
            Self::AlreadyRunning(agent) => write!(f, "{agent} PTY session is already running"),
            Self::NotRunning(agent) => write!(f, "{agent} PTY session is not running"),
            Self::Pty(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for PtySessionManagerError {}

impl From<CliPtyError> for PtySessionManagerError {
    fn from(error: CliPtyError) -> Self {
        Self::Pty(error)
    }
}

#[derive(Default)]
pub struct PtySessionManager {
    adapters: HashMap<AgentId, CliPtyAdapter>,
    sessions: HashMap<AgentId, CliPtySession>,
}

impl fmt::Debug for PtySessionManager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PtySessionManager")
            .field(
                "configured_agents",
                &self.adapters.keys().collect::<Vec<_>>(),
            )
            .field("running_agents", &self.sessions.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl PtySessionManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_adapter(mut self, agent: AgentId, adapter: CliPtyAdapter) -> Self {
        self.adapters.insert(agent, adapter);
        self
    }

    pub fn set_adapter(&mut self, agent: AgentId, adapter: CliPtyAdapter) {
        self.adapters.insert(agent, adapter);
    }

    pub fn is_running(&self, agent: AgentId) -> bool {
        self.sessions.contains_key(&agent)
    }

    pub fn start(&mut self, agent: AgentId) -> Result<(), PtySessionManagerError> {
        if self.sessions.contains_key(&agent) {
            return Err(PtySessionManagerError::AlreadyRunning(agent));
        }

        let adapter = self
            .adapters
            .get(&agent)
            .ok_or(PtySessionManagerError::MissingConfig(agent))?;
        let session = adapter.spawn_session()?;
        self.sessions.insert(agent, session);

        Ok(())
    }

    pub fn send_input(
        &mut self,
        agent: AgentId,
        input: &str,
    ) -> Result<(), PtySessionManagerError> {
        self.session_mut(agent)?.send_input(input)?;
        Ok(())
    }

    pub fn send_line(&mut self, agent: AgentId, line: &str) -> Result<(), PtySessionManagerError> {
        self.session_mut(agent)?.send_line(line)?;
        Ok(())
    }

    pub fn drain_output(&mut self, agent: AgentId) -> Result<String, PtySessionManagerError> {
        Ok(self.session_mut(agent)?.drain_output()?)
    }

    pub fn send_line_and_capture(
        &mut self,
        agent: AgentId,
        line: &str,
        idle_timeout: Duration,
        max_wait: Duration,
    ) -> Result<CliPtyRun, PtySessionManagerError> {
        if !self.is_running(agent) {
            self.start(agent)?;
        }

        let mut output = self.drain_output(agent)?;
        if let Some(status) = self.poll_status(agent)? {
            output.push_str(&self.drain_output(agent)?);
            self.sessions.remove(&agent);
            return Ok(run_from_status(output, status));
        }

        if let Err(error) = self.send_line(agent, line) {
            output.push_str(&self.drain_output(agent).unwrap_or_default());
            if let Ok(Some(status)) = self.poll_status(agent) {
                output.push_str(&self.drain_output(agent).unwrap_or_default());
                self.sessions.remove(&agent);
                return Ok(run_from_status(output, status));
            }
            return Err(error);
        }

        let started = Instant::now();
        let mut last_output = Instant::now();

        loop {
            let chunk = self.drain_output(agent)?;
            if !chunk.is_empty() {
                output.push_str(&chunk);
                last_output = Instant::now();
            }

            if let Some(status) = self.poll_status(agent)? {
                output.push_str(&self.drain_output(agent)?);
                self.sessions.remove(&agent);
                return Ok(run_from_status(output, status));
            }

            if !output.is_empty() && last_output.elapsed() >= idle_timeout {
                return Ok(CliPtyRun {
                    raw_output: output,
                    exit_code: 0,
                    success: true,
                });
            }

            if started.elapsed() >= max_wait {
                return Ok(CliPtyRun {
                    raw_output: output,
                    exit_code: 0,
                    success: true,
                });
            }

            thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn try_wait(
        &mut self,
        agent: AgentId,
    ) -> Result<Option<CliPtySessionStatus>, PtySessionManagerError> {
        let status = self.session_mut(agent)?.try_wait()?;
        if status.is_some() {
            self.sessions.remove(&agent);
        }
        Ok(status)
    }

    pub fn stop(&mut self, agent: AgentId) -> Result<CliPtyRun, PtySessionManagerError> {
        let mut session = self
            .sessions
            .remove(&agent)
            .ok_or(PtySessionManagerError::NotRunning(agent))?;
        Ok(session.stop()?)
    }

    pub fn stop_all(&mut self) -> Vec<(AgentId, Result<CliPtyRun, PtySessionManagerError>)> {
        let agents = self.sessions.keys().copied().collect::<Vec<_>>();
        agents
            .into_iter()
            .map(|agent| (agent, self.stop(agent)))
            .collect()
    }

    fn session_mut(
        &mut self,
        agent: AgentId,
    ) -> Result<&mut CliPtySession, PtySessionManagerError> {
        self.sessions
            .get_mut(&agent)
            .ok_or(PtySessionManagerError::NotRunning(agent))
    }

    fn poll_status(
        &mut self,
        agent: AgentId,
    ) -> Result<Option<CliPtySessionStatus>, PtySessionManagerError> {
        Ok(self.session_mut(agent)?.try_wait()?)
    }
}

fn run_from_status(output: String, status: CliPtySessionStatus) -> CliPtyRun {
    CliPtyRun {
        raw_output: output,
        exit_code: status.exit_code,
        success: status.success,
    }
}

#[cfg(test)]
mod tests {
    use std::{
        thread,
        time::{Duration, Instant},
    };

    use super::*;

    #[cfg(unix)]
    #[test]
    fn manager_starts_agent_session_and_routes_input_output() {
        let mut manager = PtySessionManager::new().with_adapter(
            AgentId::Codex,
            CliPtyAdapter::new("/bin/sh", "/tmp").with_args([
                "-lc",
                "printf 'ready\\n'; while IFS= read -r line; do if [ \"$line\" = quit ]; then printf 'bye\\n'; exit 0; fi; printf 'codex:%s\\n' \"$line\"; done",
            ]),
        );

        manager.start(AgentId::Codex).expect("session should start");
        assert!(manager.is_running(AgentId::Codex));
        assert!(wait_for_output(&mut manager, AgentId::Codex, "ready").contains("ready"));

        manager
            .send_line(AgentId::Codex, "implement")
            .expect("line should send");
        assert!(
            wait_for_output(&mut manager, AgentId::Codex, "codex:implement")
                .contains("codex:implement")
        );

        manager
            .send_line(AgentId::Codex, "quit")
            .expect("quit should send");
        assert!(wait_for_output(&mut manager, AgentId::Codex, "bye").contains("bye"));
        let status = wait_for_exit(&mut manager, AgentId::Codex);

        assert!(status.success);
        assert!(!manager.is_running(AgentId::Codex));
    }

    #[cfg(unix)]
    #[test]
    fn manager_can_send_line_and_capture_output_without_stopping_session() {
        let mut manager = PtySessionManager::new().with_adapter(
            AgentId::Codex,
            CliPtyAdapter::new("/bin/sh", "/tmp").with_args([
                "-lc",
                "while IFS= read -r line; do printf 'captured:%s\\n' \"$line\"; done",
            ]),
        );

        let run = manager
            .send_line_and_capture(
                AgentId::Codex,
                "hello",
                Duration::from_millis(50),
                Duration::from_secs(1),
            )
            .expect("line should capture");

        assert!(run.success);
        assert!(run.raw_output.contains("captured:hello"));
        assert!(manager.is_running(AgentId::Codex));
    }

    #[cfg(unix)]
    #[test]
    fn manager_stops_agent_session() {
        let mut manager = PtySessionManager::new().with_adapter(
            AgentId::Claude,
            CliPtyAdapter::new("/bin/sh", "/tmp")
                .with_args(["-lc", "printf 'ready\\n'; while true; do sleep 1; done"]),
        );

        manager
            .start(AgentId::Claude)
            .expect("session should start");
        assert!(wait_for_output(&mut manager, AgentId::Claude, "ready").contains("ready"));
        let run = manager.stop(AgentId::Claude).expect("session should stop");

        assert!(!run.success);
        assert!(!manager.is_running(AgentId::Claude));
    }

    #[test]
    fn manager_rejects_unconfigured_agent() {
        let mut manager = PtySessionManager::new();

        assert_eq!(
            manager.start(AgentId::Codex),
            Err(PtySessionManagerError::MissingConfig(AgentId::Codex))
        );
    }

    #[cfg(unix)]
    fn wait_for_output(manager: &mut PtySessionManager, agent: AgentId, needle: &str) -> String {
        let mut output = String::new();
        let started = Instant::now();

        loop {
            output.push_str(&manager.drain_output(agent).expect("output should drain"));
            if output.contains(needle) {
                return output;
            }
            if started.elapsed() > Duration::from_secs(1) {
                panic!("session output did not contain {needle:?}; output was {output:?}");
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(unix)]
    fn wait_for_exit(manager: &mut PtySessionManager, agent: AgentId) -> CliPtySessionStatus {
        let started = Instant::now();

        loop {
            if let Some(status) = manager.try_wait(agent).expect("status should poll") {
                return status;
            }
            if started.elapsed() > Duration::from_secs(1) {
                panic!("{agent} did not exit");
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}
