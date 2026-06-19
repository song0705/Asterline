//! Drawers: overlay panels shown on top of the chat (logs, team roster, command
//! palette). Only one is open at a time.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Drawer {
    Logs,
    Team,
    Palette,
}

impl Drawer {
    pub fn title(self) -> &'static str {
        match self {
            Self::Logs => "Logs",
            Self::Team => "Team",
            Self::Palette => "Commands",
        }
    }
}
