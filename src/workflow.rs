//! Shared workflow helpers used by the runtime and TUI.

use std::path::Path;

pub fn suggested_verify_command(workspace: &Path) -> Option<&'static str> {
    if workspace.join("Cargo.toml").is_file() {
        Some("cargo test")
    } else if workspace.join("package.json").is_file() {
        Some("npm test")
    } else if workspace.join("pyproject.toml").is_file()
        || workspace.join("pytest.ini").is_file()
        || workspace.join("tests").is_dir()
    {
        Some("pytest")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggested_verify_command_detects_common_project_files() {
        let dir =
            std::env::temp_dir().join(format!("asterline-verify-detect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        assert_eq!(suggested_verify_command(&dir), None);

        std::fs::write(dir.join("package.json"), "{}").unwrap();
        assert_eq!(suggested_verify_command(&dir), Some("npm test"));

        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        assert_eq!(suggested_verify_command(&dir), Some("cargo test"));

        std::fs::remove_file(dir.join("Cargo.toml")).unwrap();
        std::fs::remove_file(dir.join("package.json")).unwrap();
        std::fs::create_dir(dir.join("tests")).unwrap();
        assert_eq!(suggested_verify_command(&dir), Some("pytest"));

        std::fs::remove_dir_all(&dir).ok();
    }
}
