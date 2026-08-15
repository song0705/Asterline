//! Attaching to a member's live backend session.
//!
//! Asterline drives members non-interactively (`codex exec` etc.), but each
//! member keeps a resumable session. "Attaching" hands the whole terminal to the
//! real interactive CLI resuming that member's session — exactly like opening
//! `codex` yourself — and returns to Asterline when that CLI exits.

use crate::domain::event::{AgentSessionId, ImportedMessage};
use crate::domain::team::{BackendKind, MemberId};

/// Transcript and session identity recovered after an interactive attach.
///
/// The session is present when the backend transcript proves which native
/// session was used, or when Asterline supplied Claude with a fresh UUID.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct AttachOutcome {
    pub items: Vec<ImportedMessage>,
    pub session: Option<AgentSessionId>,
    pub notice: Option<String>,
}

/// A request to attach to a member's live backend session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttachRequest {
    pub member: MemberId,
    pub display_name: String,
    pub backend: BackendKind,
    /// The existing native session to resume, if the member already has one.
    pub session: Option<String>,
    /// A UUID that Asterline supplied to a fresh Claude CLI. This makes the
    /// resulting local transcript deterministic instead of guessing among
    /// concurrently-created files in the same workspace.
    pub fresh_session: Option<AgentSessionId>,
    pub cwd: String,
}

impl AttachRequest {
    /// The interactive program + args that resume this member's session (or
    /// start a fresh interactive session when there is none yet).
    pub fn command(&self) -> (String, Vec<String>) {
        match (self.backend, &self.session, &self.fresh_session) {
            (BackendKind::Codex, Some(session), _) => (
                "codex".to_string(),
                vec!["resume".to_string(), session.clone()],
            ),
            (BackendKind::Codex, None, _) => ("codex".to_string(), Vec::new()),
            (BackendKind::Claude, Some(session), _) => (
                "claude".to_string(),
                vec!["--resume".to_string(), session.clone()],
            ),
            (BackendKind::Claude, None, Some(session)) => (
                "claude".to_string(),
                vec!["--session-id".to_string(), session.0.clone()],
            ),
            (BackendKind::Claude, None, None) => ("claude".to_string(), Vec::new()),
            (BackendKind::Grok, Some(session), _) => (
                "grok".to_string(),
                vec!["--resume".to_string(), session.clone()],
            ),
            (BackendKind::Grok, None, _) => ("grok".to_string(), Vec::new()),
            (BackendKind::Agy, Some(session), _) => (
                "agy".to_string(),
                vec!["--conversation".to_string(), session.clone()],
            ),
            (BackendKind::Agy, None, _) => ("agy".to_string(), Vec::new()),
        }
    }

    /// The session file that can be safely imported after the CLI exits.
    pub fn transcript_session(&self) -> Option<&str> {
        self.session
            .as_deref()
            .or_else(|| self.fresh_session.as_ref().map(AgentSessionId::as_str))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_resumes_session_interactively() {
        let req = AttachRequest {
            member: MemberId::new("builder"),
            display_name: "Builder".to_string(),
            backend: BackendKind::Codex,
            session: Some("thread-1".to_string()),
            fresh_session: None,
            cwd: "/tmp/ws".to_string(),
        };
        assert_eq!(
            req.command(),
            (
                "codex".to_string(),
                vec!["resume".to_string(), "thread-1".to_string()]
            )
        );
    }

    #[test]
    fn fresh_member_launches_interactive_without_resume() {
        let req = AttachRequest {
            member: MemberId::new("builder"),
            display_name: "Builder".to_string(),
            backend: BackendKind::Codex,
            session: None,
            fresh_session: None,
            cwd: "/tmp/ws".to_string(),
        };
        assert_eq!(req.command(), ("codex".to_string(), Vec::new()));
    }

    #[test]
    fn claude_uses_resume_flag() {
        let req = AttachRequest {
            member: MemberId::new("reviewer"),
            display_name: "Reviewer".to_string(),
            backend: BackendKind::Claude,
            session: Some("sess-9".to_string()),
            fresh_session: None,
            cwd: "/tmp/ws".to_string(),
        };
        assert_eq!(
            req.command(),
            (
                "claude".to_string(),
                vec!["--resume".to_string(), "sess-9".to_string()]
            )
        );
    }

    #[test]
    fn fresh_claude_uses_an_asterline_generated_session_id() {
        let req = AttachRequest {
            member: MemberId::new("reviewer"),
            display_name: "Reviewer".to_string(),
            backend: BackendKind::Claude,
            session: None,
            fresh_session: Some(AgentSessionId(
                "3e2f3488-c08a-4d09-9cac-fc64f632a590".to_string(),
            )),
            cwd: "/tmp/ws".to_string(),
        };
        assert_eq!(
            req.command(),
            (
                "claude".to_string(),
                vec![
                    "--session-id".to_string(),
                    "3e2f3488-c08a-4d09-9cac-fc64f632a590".to_string(),
                ],
            )
        );
        assert_eq!(
            req.transcript_session(),
            Some("3e2f3488-c08a-4d09-9cac-fc64f632a590")
        );
    }

    #[test]
    fn agy_uses_conversation_flag() {
        let req = AttachRequest {
            member: MemberId::new("researcher"),
            display_name: "Researcher".to_string(),
            backend: BackendKind::Agy,
            session: Some("sess-9".to_string()),
            fresh_session: None,
            cwd: "/tmp/ws".to_string(),
        };
        assert_eq!(
            req.command(),
            (
                "agy".to_string(),
                vec!["--conversation".to_string(), "sess-9".to_string()]
            )
        );
    }

    #[test]
    fn grok_uses_resume_flag() {
        let req = AttachRequest {
            member: MemberId::new("grok"),
            display_name: "Grok".to_string(),
            backend: BackendKind::Grok,
            session: Some("sess-9".to_string()),
            fresh_session: None,
            cwd: "/tmp/ws".to_string(),
        };
        assert_eq!(
            req.command(),
            (
                "grok".to_string(),
                vec!["--resume".to_string(), "sess-9".to_string()]
            )
        );
    }
}
