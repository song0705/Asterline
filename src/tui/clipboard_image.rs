//! Read an image from the system clipboard and persist it for a prompt.
//!
//! Copies go under `std::env::temp_dir()/asterline-pasted/<pid>/` — that is
//! `%TEMP%` on Windows, `$TMPDIR` on macOS, `/tmp` on Linux. The OS is not
//! trusted to delete them. This process removes unused files immediately and
//! wipes its own directory on start/exit; leftover dirs from dead PIDs are
//! swept at the same time.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use sha2::{Digest, Sha256};

use crate::adapter::prompt_images::{
    MAX_IMAGE_BYTES, PromptImage, extension_for_mime, is_tiff, looks_like_image_path,
    mime_from_bytes,
};

const PASTE_DIR_NAME: &str = "asterline-pasted";

/// Sweeps dead-PID leftovers on enter and deletes this process's copies on drop.
pub struct PasteCleanup;

impl PasteCleanup {
    pub fn enter() -> Self {
        sweep_orphan_pastes();
        Self
    }
}

impl Drop for PasteCleanup {
    fn drop(&mut self) {
        clear_session_pastes();
    }
}

pub fn paste_clipboard_image(workspace: &str) -> Result<PromptImage, String> {
    let bytes = read_clipboard_image_bytes().ok_or_else(|| {
        "clipboard has no image — copy a screenshot, then paste again".to_string()
    })?;
    persist_image_bytes(workspace, &bytes)
}

const CLIPBOARD_IMAGE_HINT: &str =
    "could not read the pasted screenshot — copy the image again, then Cmd+V / Ctrl+V";

/// Copy a pasted file into the OS temp paste dir. Never keep the source path:
/// macOS screenshot paste often yields a CoreSpotlight `PasteboardHistory`
/// file that `stat`s but cannot be read by us or by the backends.
pub fn import_image_file(workspace: &str, path: &Path) -> Result<PromptImage, String> {
    if !is_ephemeral_pasteboard_path(path)
        && let Ok(bytes) = std::fs::read(path)
        && (mime_from_bytes(&bytes).is_some() || is_tiff(&bytes))
    {
        return persist_image_bytes(workspace, &bytes);
    }
    // Prefer live clipboard pixels. The pasted path is often a TCC-blocked
    // PasteboardHistory file; the bitmap is still on the pasteboard.
    paste_clipboard_image(workspace).map_err(|_| CLIPBOARD_IMAGE_HINT.to_string())
}

fn is_ephemeral_pasteboard_path(path: &Path) -> bool {
    path.components().any(|component| {
        matches!(
            component.as_os_str().to_str(),
            Some("PasteboardHistory" | "CoreSpotlight")
        )
    })
}

/// Bracketed paste: a path that looks like an image is imported; on failure
/// we take clipboard bitmap bytes instead of attaching the original path.
pub fn import_pasted_text(workspace: &str, text: &str) -> Option<Result<PromptImage, String>> {
    let path = looks_like_image_path(text)?;
    Some(import_image_file(workspace, &path))
}

pub fn persist_image_bytes(workspace: &str, bytes: &[u8]) -> Result<PromptImage, String> {
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(format!(
            "clipboard image is too large ({} bytes)",
            bytes.len()
        ));
    }
    let bytes = normalize_image_bytes(bytes)
        .ok_or_else(|| "clipboard is not a PNG/JPEG/GIF/WebP/TIFF image".to_string())?;
    let mime = mime_from_bytes(&bytes)
        .ok_or_else(|| "clipboard is not a PNG/JPEG/GIF/WebP image".to_string())?;
    sweep_orphan_pastes();
    cleanup_legacy_workspace_paste_dir(workspace);
    let dir = paste_dir();
    std::fs::create_dir_all(&dir).map_err(|err| format!("could not create paste dir: {err}"))?;
    let digest = Sha256::digest(&bytes);
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    let path = dir.join(format!("pasted-{hex}.{}", extension_for_mime(mime)));
    if !path.exists() {
        std::fs::write(&path, &bytes)
            .map_err(|err| format!("could not save pasted image: {err}"))?;
    }
    Ok(PromptImage {
        path,
        mime: mime.to_string(),
    })
}

fn paste_root() -> PathBuf {
    std::env::temp_dir().join(PASTE_DIR_NAME)
}

pub fn paste_dir() -> PathBuf {
    paste_root().join(std::process::id().to_string())
}

pub fn is_managed_paste(path: &Path) -> bool {
    let root = paste_root();
    if path.starts_with(&root) {
        return true;
    }
    match (std::fs::canonicalize(path), std::fs::canonicalize(&root)) {
        (Ok(path), Ok(root)) => path.starts_with(root),
        _ => false,
    }
}

/// Delete a file we created. User-owned originals are never removed.
pub fn remove_managed_paste(path: &Path) {
    if is_managed_paste(path) {
        let _ = std::fs::remove_file(path);
    }
}

/// Wipe this process's paste directory. Called on TUI start and exit.
pub fn clear_session_pastes() {
    let _ = std::fs::remove_dir_all(paste_dir());
}

/// Remove paste dirs whose owning process is gone, plus files from the old
/// flat layout. Does not touch another live Asterline instance.
pub fn sweep_orphan_pastes() {
    let root = paste_root();
    let Ok(entries) = std::fs::read_dir(&root) else {
        return;
    };
    let self_pid = std::process::id();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            // Previous builds wrote files directly into asterline-pasted/.
            let _ = std::fs::remove_file(path);
            continue;
        }
        let Some(pid) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        if pid != self_pid && !process_is_running(pid) {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

fn process_is_running(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // Safety: signal 0 only checks whether the process exists.
        let result = unsafe { libc::kill(pid as i32, 0) };
        if result == 0 {
            return true;
        }
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(windows)]
    {
        let output = Command::new("tasklist")
            .args(["/NH", "/FI", &format!("PID eq {pid}")])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output();
        match output {
            Ok(out) if out.status.success() => {
                String::from_utf8_lossy(&out.stdout).contains(&pid.to_string())
            }
            // If we cannot check, assume alive so we do not delete another
            // instance's in-flight copies.
            _ => true,
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        true
    }
}

/// Previous builds stored copies under `<workspace>/.asterline/pasted/`.
fn cleanup_legacy_workspace_paste_dir(workspace: &str) {
    if workspace.trim().is_empty() {
        return;
    }
    let dir = PathBuf::from(workspace).join(".asterline").join("pasted");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("pasted-"))
        {
            let _ = std::fs::remove_file(path);
        }
    }
    let _ = std::fs::remove_dir(&dir);
}

fn normalize_image_bytes(bytes: &[u8]) -> Option<Vec<u8>> {
    match mime_from_bytes(bytes) {
        Some("image/tiff") => tiff_to_png(bytes),
        Some(_) => Some(bytes.to_vec()),
        None => None,
    }
}

fn tiff_to_png(bytes: &[u8]) -> Option<Vec<u8>> {
    let dir = std::env::temp_dir().join("asterline-clip");
    let _ = std::fs::create_dir_all(&dir);
    let id = std::process::id();
    let src = dir.join(format!("in-{id}.tiff"));
    let dest = dir.join(format!("out-{id}.png"));
    std::fs::write(&src, bytes).ok()?;
    let status = Command::new("sips")
        .args(["-s", "format", "png", "--out"])
        .arg(&dest)
        .arg(&src)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok();
    let png = if status.is_some_and(|s| s.success()) {
        std::fs::read(&dest).ok()
    } else {
        None
    };
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&dest);
    png.filter(|data| mime_from_bytes(data) == Some("image/png"))
}

fn read_clipboard_image_bytes() -> Option<Vec<u8>> {
    #[cfg(target_os = "macos")]
    {
        return macos_clipboard_image();
    }
    #[cfg(target_os = "linux")]
    {
        return linux_clipboard_image();
    }
    #[cfg(windows)]
    {
        return windows_clipboard_image();
    }
    #[allow(unreachable_code)]
    None
}

#[cfg(target_os = "macos")]
fn macos_clipboard_image() -> Option<Vec<u8>> {
    macos_clipboard_via_jxa()
        .or_else(macos_clipboard_via_applescript)
        .or_else(macos_clipboard_via_pngpaste)
        .and_then(|bytes| normalize_image_bytes(&bytes))
}

#[cfg(target_os = "macos")]
fn macos_clip_path() -> PathBuf {
    let dir = std::env::temp_dir().join("asterline-clip");
    let _ = std::fs::create_dir_all(&dir);
    dir.join(format!("clip-{}.bin", std::process::id()))
}

#[cfg(target_os = "macos")]
fn macos_clipboard_via_jxa() -> Option<Vec<u8>> {
    let path = macos_clip_path();
    let encoded = serde_json::to_string(&path.to_string_lossy().as_ref()).ok()?;
    let script = format!(
        r#"
(function () {{
  ObjC.import('AppKit');
  const out = {encoded};
  const pb = $.NSPasteboard.generalPasteboard;
  const types = ['public.png', 'public.tiff', 'public.jpeg', 'public.jpeg-2000'];
  for (const type of types) {{
    const data = pb.dataForType($(type));
    if (data && data.length) {{
      data.writeToFileAtomically($(out), true);
      return 'ok';
    }}
  }}
  return 'empty';
}})()
"#
    );
    let output = Command::new("osascript")
        .args(["-l", "JavaScript", "-e", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let bytes = std::fs::read(&path).ok();
    let _ = std::fs::remove_file(&path);
    if !output.status.success() || !String::from_utf8_lossy(&output.stdout).contains("ok") {
        return None;
    }
    bytes.filter(|data| !data.is_empty() && mime_from_bytes(data).is_some())
}

#[cfg(target_os = "macos")]
fn macos_clipboard_via_applescript() -> Option<Vec<u8>> {
    let path = macos_clip_path();
    let script = format!(
        r#"set outPath to "{}"
try
  set imgData to the clipboard as «class PNGf»
on error
  try
    set imgData to the clipboard as «class TIFF»
  on error
    try
      set imgData to the clipboard as «class JPEG»
    on error
      return "empty"
    end try
  end try
end try
set f to open for access POSIX file outPath with write permission
set eof f to 0
write imgData to f
close access f
return "ok"
"#,
        path.display()
    );
    let output = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let bytes = std::fs::read(&path).ok();
    let _ = std::fs::remove_file(&path);
    if !output.status.success() || !String::from_utf8_lossy(&output.stdout).contains("ok") {
        return None;
    }
    bytes.filter(|data| !data.is_empty() && mime_from_bytes(data).is_some())
}

#[cfg(target_os = "macos")]
fn macos_clipboard_via_pngpaste() -> Option<Vec<u8>> {
    let output = Command::new("pngpaste")
        .arg("-")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() || output.stdout.is_empty() {
        return None;
    }
    mime_from_bytes(&output.stdout)?;
    Some(output.stdout)
}

#[cfg(target_os = "linux")]
fn linux_clipboard_image() -> Option<Vec<u8>> {
    for (program, args) in [
        ("wl-paste", &["--type", "image/png"][..]),
        ("wl-paste", &["--type", "image/jpeg"]),
        (
            "xclip",
            &["-selection", "clipboard", "-t", "image/png", "-o"],
        ),
        (
            "xclip",
            &["-selection", "clipboard", "-t", "image/jpeg", "-o"],
        ),
    ] {
        if let Some(bytes) = command_stdout(program, args) {
            return Some(bytes);
        }
    }
    None
}

#[cfg(windows)]
fn windows_clipboard_image() -> Option<Vec<u8>> {
    let dir = std::env::temp_dir().join("asterline-clip");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("clip-{}.png", std::process::id()));
    let script = format!(
        "Add-Type -AssemblyName System.Windows.Forms; Add-Type -AssemblyName System.Drawing; $img = [Windows.Forms.Clipboard]::GetImage(); if ($null -eq $img) {{ exit 1 }}; $img.Save('{}', [System.Drawing.Imaging.ImageFormat]::Png)",
        path.display().to_string().replace('\'', "''")
    );
    let status = Command::new("powershell")
        .args(["-NoProfile", "-Command", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    if !status.success() {
        let _ = std::fs::remove_file(&path);
        return None;
    }
    let bytes = std::fs::read(&path).ok();
    let _ = std::fs::remove_file(&path);
    bytes.filter(|data| !data.is_empty() && mime_from_bytes(data).is_some())
}

#[cfg(target_os = "linux")]
fn command_stdout(program: &str, args: &[&str]) -> Option<Vec<u8>> {
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() || output.stdout.is_empty() {
        return None;
    }
    mime_from_bytes(&output.stdout)?;
    Some(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    static PASTE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn lock_paste() -> std::sync::MutexGuard<'static, ()> {
        PASTE_LOCK.lock().unwrap_or_else(|err| err.into_inner())
    }

    #[test]
    fn persist_writes_png_under_asterline_pasted() {
        let _guard = lock_paste();
        let dir = std::env::temp_dir().join(format!("asterline-paste-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut png = vec![0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
        png.extend_from_slice(&[0, 1, 2, 3, 4, 5, 6, 7]);
        let image = persist_image_bytes(dir.to_str().unwrap(), &png).unwrap();
        assert!(image.path.starts_with(paste_dir()), "{:?}", image.path);
        assert_eq!(image.mime, "image/png");
        assert_eq!(std::fs::read(&image.path).unwrap(), png);
        let again = persist_image_bytes(dir.to_str().unwrap(), &png).unwrap();
        assert_eq!(again.path, image.path);
        let src = dir.join("source.png");
        std::fs::write(&src, &png).unwrap();
        let copied = import_image_file(dir.to_str().unwrap(), &src).unwrap();
        assert_eq!(copied.path, image.path);
        assert_ne!(copied.path, src);
        remove_managed_paste(&image.path);
        assert!(!image.path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sweep_removes_dead_pid_dir_and_keeps_ours() {
        let _guard = lock_paste();
        let png = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 2];
        let image = persist_image_bytes("", &png).unwrap();
        // Keep this in the range accepted by Windows `tasklist`; the previous
        // u32::MAX-ish value made the command fail and intentionally fail
        // closed as if the process were still alive.
        let orphan = paste_root().join("999999");
        std::fs::create_dir_all(&orphan).unwrap();
        std::fs::write(orphan.join("leftover.png"), png).unwrap();
        sweep_orphan_pastes();
        assert!(image.path.exists(), "live session copies stay");
        assert!(!orphan.exists(), "dead pid directory should be gone");
        remove_managed_paste(&image.path);
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_source_is_not_kept_as_the_attached_path() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("asterline-unreadable-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("secret.png");
        let png = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 9, 8, 7];
        std::fs::write(&src, png).unwrap();
        std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o000)).unwrap();
        let result = import_image_file(dir.to_str().unwrap(), &src);
        let _ = std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o644));
        match result {
            Ok(image) => assert_ne!(image.path, src),
            Err(err) => {
                assert!(!err.contains("secret.png"), "{err}");
                assert!(
                    err.contains("screenshot") || err.contains("clipboard"),
                    "{err}"
                );
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pasteboard_history_error_does_not_leak_system_path() {
        let path =
            PathBuf::from("/Users/me/Library/Metadata/CoreSpotlight/PasteboardHistory/shot.png");
        match import_image_file("", &path) {
            Ok(image) => {
                assert!(is_managed_paste(&image.path), "{:?}", image.path);
                assert!(!image.path.to_string_lossy().contains("PasteboardHistory"));
            }
            Err(err) => {
                assert!(!err.contains("PasteboardHistory"), "{err}");
                assert!(!err.contains("CoreSpotlight"), "{err}");
                assert!(!err.contains("/Users/me"), "{err}");
            }
        }
    }
}
