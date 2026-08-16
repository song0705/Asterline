//! Prompt-attached images. The TUI stores them as sentinel lines in the user
//! message so history stays text; adapters lift those lines into native
//! multimodal blocks (Codex `localImage`, Grok ACP image, Claude/Agy path).

use std::path::{Path, PathBuf};

pub const MAX_PROMPT_IMAGES: usize = 4;
pub const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
const MARKER: &str = "[asterline-image]: ";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PromptImage {
    pub path: PathBuf,
    pub mime: String,
}

impl PromptImage {
    pub fn from_path(path: impl Into<PathBuf>) -> Option<Self> {
        let path = path.into();
        let mime = mime_from_path(&path)?;
        Some(Self { path, mime })
    }

    pub fn label(&self) -> String {
        self.path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("image")
            .to_string()
    }
}

pub fn append_prompt_image(prompt: &mut String, image: &PromptImage) {
    if !prompt.is_empty() && !prompt.ends_with('\n') {
        prompt.push('\n');
    }
    prompt.push_str(MARKER);
    prompt.push_str(&image.path.display().to_string());
}

pub fn extract_prompt_images(prompt: &str) -> (String, Vec<PromptImage>) {
    let mut text = String::new();
    let mut images = Vec::new();
    for line in prompt.lines() {
        if let Some(path) = line.strip_prefix(MARKER)
            && let Some(image) = PromptImage::from_path(path.trim())
        {
            images.push(image);
            continue;
        }
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(line);
    }
    if prompt.ends_with('\n') && !text.is_empty() {
        text.push('\n');
    }
    (text, images)
}

/// Claude / Agy print-mode prompts: keep a readable path so the CLI can Read
/// the file. Structured backends should use [`extract_prompt_images`] instead.
pub fn prompt_with_image_paths(prompt: &str) -> String {
    let (text, images) = extract_prompt_images(prompt);
    if images.is_empty() {
        return prompt.to_string();
    }
    let mut out = text;
    for image in images {
        if !image_file_is_readable(&image.path) {
            continue;
        }
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("(attached image: ");
        out.push_str(&image.path.display().to_string());
        out.push(')');
    }
    out
}

pub fn display_prompt_images(prompt: &str) -> String {
    let (text, images) = extract_prompt_images(prompt);
    if images.is_empty() {
        return prompt.to_string();
    }
    let mut out = text;
    for image in images {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('📎');
        out.push(' ');
        out.push_str(&image.label());
    }
    out
}

pub fn mime_from_bytes(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']) {
        Some("image/png")
    } else if bytes.len() >= 3 && bytes[0] == 0xff && bytes[1] == 0xd8 && bytes[2] == 0xff {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if is_tiff(bytes) {
        Some("image/tiff")
    } else {
        None
    }
}

pub fn is_tiff(bytes: &[u8]) -> bool {
    bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*")
}

pub fn mime_from_path(path: &Path) -> Option<String> {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => Some("image/png".to_string()),
        Some("jpg") | Some("jpeg") => Some("image/jpeg".to_string()),
        Some("gif") => Some("image/gif".to_string()),
        Some("webp") => Some("image/webp".to_string()),
        _ => std::fs::read(path)
            .ok()
            .as_deref()
            .and_then(mime_from_bytes)
            .map(str::to_string),
    }
}

pub fn extension_for_mime(mime: &str) -> &'static str {
    match mime {
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/tiff" => "tiff",
        _ => "png",
    }
}

pub fn looks_like_image_path(text: &str) -> Option<PathBuf> {
    let trimmed = text.trim().trim_matches(|ch| matches!(ch, '"' | '\''));
    let path = trimmed
        .strip_prefix("file://")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(trimmed));
    if !path.is_absolute()
        && !path.starts_with("~")
        && (trimmed.lines().count() != 1 || trimmed.contains(' '))
    {
        return None;
    }
    let expanded = if let Some(rest) = path.to_str().and_then(|p| p.strip_prefix("~/")) {
        dirs_home().map(|home| home.join(rest))?
    } else {
        path
    };
    if !expanded.is_file() {
        return None;
    }
    mime_from_path(&expanded)?;
    Some(expanded)
}

fn image_file_is_readable(path: &Path) -> bool {
    std::fs::read(path)
        .ok()
        .as_deref()
        .and_then(mime_from_bytes)
        .is_some()
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Codex App Server `turn/start` items: text plus `localImage` paths.
pub fn codex_user_input(prompt: &str) -> Vec<serde_json::Value> {
    let (text, images) = extract_prompt_images(prompt);
    let mut items = Vec::new();
    if !text.is_empty() {
        items.push(serde_json::json!({ "type": "text", "text": text }));
    }
    for image in images {
        if !image_file_is_readable(&image.path) {
            continue;
        }
        items.push(serde_json::json!({
            "type": "localImage",
            "path": image.path.display().to_string(),
        }));
    }
    if items.is_empty() {
        items.push(serde_json::json!({ "type": "text", "text": "" }));
    }
    items
}

/// Grok ACP `session/prompt` content blocks. Image bytes are inlined as
/// base64. Unreadable files are dropped — never forwarded as a fake path.
pub fn grok_prompt_blocks(prompt: &str) -> Vec<serde_json::Value> {
    let (text, images) = extract_prompt_images(prompt);
    let mut blocks = Vec::new();
    if !text.is_empty() {
        blocks.push(serde_json::json!({ "type": "text", "text": text }));
    }
    for image in images {
        match std::fs::read(&image.path) {
            Ok(bytes) if !bytes.is_empty() && bytes.len() <= MAX_IMAGE_BYTES => {
                if let Some(mime) = mime_from_bytes(&bytes) {
                    blocks.push(serde_json::json!({
                        "type": "image",
                        "mimeType": mime,
                        "data": encode_base64(&bytes),
                    }));
                }
            }
            _ => {}
        }
    }
    if blocks.is_empty() {
        blocks.push(serde_json::json!({ "type": "text", "text": "" }));
    }
    blocks
}

pub fn encode_base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((n >> 6) & 63) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(n & 63) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_temp_png(label: &str) -> (std::path::PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "asterline-prompt-img-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("shot.png");
        std::fs::write(&path, [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']).unwrap();
        (dir, path)
    }

    #[test]
    fn round_trips_sentinel_lines() {
        let (dir, path) = write_temp_png("roundtrip");
        let mut prompt = "look at this".to_string();
        append_prompt_image(
            &mut prompt,
            &PromptImage {
                path: path.clone(),
                mime: "image/png".to_string(),
            },
        );
        let (text, images) = extract_prompt_images(&prompt);
        assert_eq!(text, "look at this");
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].path, path);
        assert!(display_prompt_images(&prompt).contains("📎 shot.png"));
        assert!(
            prompt_with_image_paths(&prompt)
                .contains(&format!("(attached image: {})", path.display()))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mime_detects_png_and_jpeg() {
        let png = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
        assert_eq!(mime_from_bytes(&png), Some("image/png"));
        assert_eq!(
            mime_from_bytes(&[0xff, 0xd8, 0xff, 0xe0]),
            Some("image/jpeg")
        );
        assert_eq!(mime_from_bytes(b"not an image"), None);
        assert_eq!(mime_from_bytes(b"II*\0rest"), Some("image/tiff"));
    }

    #[test]
    fn base64_encodes_known_vector() {
        assert_eq!(encode_base64(b"Man"), "TWFu");
        assert_eq!(encode_base64(b"Ma"), "TWE=");
        assert_eq!(encode_base64(b"M"), "TQ==");
    }

    #[test]
    fn codex_input_lifts_sentinels_to_local_image() {
        let (dir, path) = write_temp_png("codex");
        let items = codex_user_input(&format!("look\n[asterline-image]: {}", path.display()));
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["type"], "text");
        assert_eq!(items[0]["text"], "look");
        assert_eq!(items[1]["type"], "localImage");
        assert_eq!(items[1]["path"], path.to_string_lossy().as_ref());
        let skipped = codex_user_input("look\n[asterline-image]: /tmp/missing-shot.png");
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0]["text"], "look");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn grok_blocks_embed_base64_when_file_exists() {
        let dir = std::env::temp_dir().join(format!("asterline-grok-img-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("shot.png");
        let png = [
            0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 1, 2, 3, 4,
        ];
        std::fs::write(&path, png).unwrap();
        let mut prompt = "see".to_string();
        append_prompt_image(
            &mut prompt,
            &PromptImage {
                path: path.clone(),
                mime: "image/png".to_string(),
            },
        );
        let blocks = grok_prompt_blocks(&prompt);
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[0]["text"], "see");
        assert_eq!(blocks[1]["type"], "image");
        assert_eq!(blocks[1]["mimeType"], "image/png");
        assert_eq!(blocks[1]["data"], encode_base64(&png));
        let missing = grok_prompt_blocks("see\n[asterline-image]: /tmp/missing-shot.png");
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0]["type"], "text");
        assert_eq!(missing[0]["text"], "see");
        assert!(
            !missing[0]["text"]
                .as_str()
                .unwrap()
                .contains("attached image")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
