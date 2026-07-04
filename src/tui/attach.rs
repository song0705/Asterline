//! Attaching to a member's live backend session.
//!
//! Asterline drives members non-interactively (`codex exec` etc.), but each
//! member keeps a resumable session. "Attaching" hands the whole terminal to the
//! real interactive CLI resuming that member's session — exactly like opening
//! `codex` yourself — and returns to Asterline when that CLI exits.

use crate::domain::team::BackendKind;
use crate::domain::team::MemberId;

/// A request to attach to a member's live backend session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttachRequest {
    pub member: MemberId,
    pub display_name: String,
    pub backend: BackendKind,
    pub session: Option<String>,
    pub cwd: String,
}

impl AttachRequest {
    /// The interactive program + args that resume this member's session (or
    /// start a fresh interactive session when there is none yet).
    pub fn command(&self) -> (String, Vec<String>) {
        match (self.backend, &self.session) {
            (BackendKind::Codex, Some(session)) => (
                "codex".to_string(),
                vec!["resume".to_string(), session.clone()],
            ),
            (BackendKind::Codex, None) => ("codex".to_string(), Vec::new()),
            (BackendKind::Claude, Some(session)) => (
                "claude".to_string(),
                vec!["--resume".to_string(), session.clone()],
            ),
            (BackendKind::Claude, None) => ("claude".to_string(), Vec::new()),
            (BackendKind::Agy, Some(session)) => (
                "agy".to_string(),
                vec!["--conversation".to_string(), session.clone()],
            ),
            (BackendKind::Agy, None) => ("agy".to_string(), Vec::new()),
        }
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
    fn agy_uses_conversation_flag() {
        let req = AttachRequest {
            member: MemberId::new("researcher"),
            display_name: "Researcher".to_string(),
            backend: BackendKind::Agy,
            session: Some("sess-9".to_string()),
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
}
