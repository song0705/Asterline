//! SQLite-backed event-source persistence.

pub mod sqlite;

pub use sqlite::{SqliteStore, StoredApproval};
