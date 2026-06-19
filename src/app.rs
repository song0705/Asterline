//! Application bootstrap. Replaced incrementally during the product rewrite;
//! the runtime + chat-first TUI are wired in once their phases land.

use std::io;

/// Entry point invoked from `main`.
pub fn run() -> io::Result<()> {
    eprintln!("Asterline: product rewrite in progress; runtime/TUI not yet wired.");
    Ok(())
}
