//! Narrow filesystem guards for Asterline-managed workspace state.
//!
//! Workspace paths can originate in a checkout. These helpers reject existing
//! symlinks before reading or writing control files, and use `O_NOFOLLOW` on
//! Unix for the final file open. They deliberately do not canonicalize the
//! workspace root: a user may explicitly launch from a symlinked workspace.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

pub fn ensure_workspace_directory(
    workspace: &Path,
    components: &[&str],
    private: bool,
) -> io::Result<PathBuf> {
    if !workspace.is_dir() {
        match fs::create_dir_all(workspace) {
            Ok(()) if workspace.is_dir() => {}
            Ok(()) => {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("workspace is not a directory: {}", workspace.display()),
                ));
            }
            Err(error) => return Err(error),
        }
    }

    let mut current = workspace.to_path_buf();
    for component in components {
        if component.is_empty()
            || component.contains(['/', '\\'])
            || *component == "."
            || *component == ".."
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid workspace path component: {component:?}"),
            ));
        }
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => ensure_directory(&current, &metadata)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match fs::create_dir(&current) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error),
                }
                let metadata = fs::symlink_metadata(&current)?;
                ensure_directory(&current, &metadata)?;
            }
            Err(error) => return Err(error),
        }
        if private {
            restrict_directory(&current)?;
        }
    }
    Ok(current)
}

pub fn reject_symlink(path: &Path, label: &str) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("refusing symlinked {label}: {}", path.display()),
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub fn regular_file_exists(path: &Path, label: &str) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("refusing symlinked {label}: {}", path.display()),
        )),
        Ok(metadata) => Ok(metadata.is_file()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Create a regular state file when absent, or reject a non-regular leaf.
pub fn ensure_private_regular_file(path: &Path, label: &str) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("refusing non-regular {label}: {}", path.display()),
                ));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            configure_no_follow(&mut options);
            let file = options.open(path)?;
            restrict_file(file)?;
            return Ok(());
        }
        Err(error) => return Err(error),
    }
    restrict_existing_file(path)
}

pub fn read_regular_to_string(path: &Path, label: &str) -> io::Result<String> {
    ensure_regular_file(path, label)?;
    let mut options = OpenOptions::new();
    options.read(true);
    configure_no_follow(&mut options);
    let mut file = options.open(path)?;
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    Ok(text)
}

pub fn write_regular_file(path: &Path, label: &str, contents: &str) -> io::Result<()> {
    reject_symlink(path, label)?;
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    configure_no_follow(&mut options);
    let mut file = options.open(path)?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;
    restrict_file(file)
}

fn ensure_regular_file(path: &Path, label: &str) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("refusing non-regular {label}: {}", path.display()),
        ));
    }
    Ok(())
}

fn ensure_directory(path: &Path, metadata: &fs::Metadata) -> io::Result<()> {
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("refusing non-directory workspace path: {}", path.display()),
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn configure_no_follow(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;

    options.custom_flags(libc::O_NOFOLLOW).mode(0o600);
}

#[cfg(not(unix))]
fn configure_no_follow(_options: &mut OpenOptions) {}

#[cfg(unix)]
fn restrict_directory(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn restrict_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn restrict_file(file: File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = file.metadata()?.permissions();
    permissions.set_mode(0o600);
    file.set_permissions(permissions)
}

#[cfg(not(unix))]
fn restrict_file(_file: File) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn restrict_existing_file(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o600);
    fs::set_permissions(path, permissions)
}

#[cfg(not(unix))]
fn restrict_existing_file(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn private_workspace_state_rejects_a_symlinked_directory() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let root = std::env::temp_dir().join(format!("asterline-fs-safety-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        let state = ensure_workspace_directory(&root, &[".asterline"], true).unwrap();
        assert_eq!(
            fs::metadata(&state).unwrap().permissions().mode() & 0o777,
            0o700
        );

        fs::remove_dir(&state).unwrap();
        let target = root.join("outside");
        fs::create_dir_all(&target).unwrap();
        symlink(&target, &state).unwrap();
        assert!(ensure_workspace_directory(&root, &[".asterline"], true).is_err());

        fs::remove_dir_all(root).ok();
    }
}
