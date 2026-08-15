//! Explicit, installation-aware updates for `ast update`.
//!
//! A standalone archive, a locally installed `.deb`/`.rpm`, and a source
//! build must never be overwritten by a running binary: there is no durable
//! record of the package source or of the user's intended install location.
//! Windows Setup owns its existing verified updater. On Unix, Homebrew is the
//! only package manager Asterline currently owns an installed Formula for, so
//! it is the only external updater we launch automatically.

#[cfg(not(windows))]
use std::path::{Path, PathBuf};
#[cfg(not(windows))]
use std::process::Command;

/// Run the explicit update action and return a concise terminal result.
pub(crate) fn run() -> Result<String, String> {
    #[cfg(windows)]
    {
        return crate::update::update_now().map(|outcome| outcome.to_string());
    }

    #[cfg(not(windows))]
    run_unix()
}

#[cfg(not(windows))]
const HOMEBREW_FORMULA: &str = "song0705/asterline/asterline";

#[cfg(not(windows))]
fn run_unix() -> Result<String, String> {
    let executable = std::env::current_exe()
        .map_err(|error| format!("could not locate the running Asterline binary: {error}"))?;
    let executable = std::fs::canonicalize(&executable).map_err(|error| {
        format!(
            "could not resolve the running Asterline binary {}: {error}",
            executable.display()
        )
    })?;

    let Some(prefix) = homebrew_formula_prefix()? else {
        return Ok(unmanaged_install_message());
    };
    let prefix = match std::fs::canonicalize(prefix) {
        Ok(prefix) => prefix,
        Err(_) => return Ok(unmanaged_install_message()),
    };
    if !is_below(&executable, &prefix) {
        return Ok(unmanaged_install_message());
    }

    println!("Updating Homebrew metadata...");
    run_brew(["update"])?;
    println!("Updating Asterline through Homebrew...");
    run_brew(["upgrade", HOMEBREW_FORMULA])?;
    Ok(
        "Asterline update completed through Homebrew. Restart ast to use the updated binary."
            .to_string(),
    )
}

#[cfg(not(windows))]
fn homebrew_formula_prefix() -> Result<Option<PathBuf>, String> {
    let output = match Command::new("brew")
        .args(["--prefix", HOMEBREW_FORMULA])
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("could not query Homebrew: {error}")),
    };
    if !output.status.success() {
        return Ok(None);
    }
    let prefix = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if prefix.is_empty() {
        Ok(None)
    } else {
        Ok(Some(PathBuf::from(prefix)))
    }
}

#[cfg(not(windows))]
fn run_brew<const N: usize>(args: [&str; N]) -> Result<(), String> {
    let status = Command::new("brew")
        .args(args)
        .status()
        .map_err(|error| format!("could not start Homebrew: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("Homebrew exited with status {status}"))
    }
}

#[cfg(not(windows))]
fn is_below(path: &Path, directory: &Path) -> bool {
    path.starts_with(directory)
}

#[cfg(not(windows))]
fn unmanaged_install_message() -> String {
    "This Asterline copy is not managed by Homebrew, so ast update did not replace it. \
     Update a portable archive, macOS package, or direct .deb/.rpm installation from the next \
     matching GitHub Release; rebuild source installs with cargo. For managed updates on macOS \
     or Linux, install with Homebrew: brew install song0705/asterline/asterline."
        .to_string()
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;

    #[test]
    fn homebrew_detection_requires_the_running_binary_to_be_in_the_formula_prefix() {
        let prefix = Path::new("/home/linuxbrew/.linuxbrew/Cellar/asterline/0.2.7");
        assert!(is_below(
            Path::new("/home/linuxbrew/.linuxbrew/Cellar/asterline/0.2.7/bin/ast"),
            prefix
        ));
        assert!(!is_below(Path::new("/usr/local/bin/ast"), prefix));
        assert!(!is_below(
            Path::new("/home/linuxbrew/.linuxbrew/Cellar/another-tool/0.2.7/bin/ast"),
            prefix
        ));
    }

    #[test]
    fn unmanaged_message_never_claims_to_overwrite_a_manual_installation() {
        let message = unmanaged_install_message();
        assert!(message.contains("did not replace it"));
        assert!(message.contains("portable archive"));
        assert!(message.contains("cargo"));
    }
}
