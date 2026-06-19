//! Backend adapters.
//!
//! The product path uses streaming adapters that translate each CLI's output
//! into [`crate::domain::AgentEvent`]. `cli_pty` is retained as a raw-terminal /
//! debug capability and is not part of the product path.

pub mod cli_pty;
