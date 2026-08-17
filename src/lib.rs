pub mod adapter;
pub mod app;
pub mod domain;
mod fs_safety;
pub mod router;
pub mod runtime;
pub mod store;
pub mod tui;

mod managed_update;
#[cfg(windows)]
mod update;
