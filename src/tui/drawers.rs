//! Drawers: overlay panels shown on top of the chat (logs, team roster, command
//! palette). Only one is open at a time.

use crate::domain::team::MemberId;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Drawer {
    Logs,
    Team,
    Palette,
    Diff,
    MemberLogs(MemberId),
}

impl Drawer {
    pub fn title(&self) -> &'static str {
        match self {
            Self::Logs => "Logs",
            Self::Team => "Team",
            Self::Palette => "Commands",
            Self::Diff => "Working-tree diff",
            Self::MemberLogs(_) => "Member Logs",
        }
    }
}
