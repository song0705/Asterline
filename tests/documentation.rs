use asterline::domain::team::{ApprovalSurface, BackendKind, DefaultTarget, MemberId, TeamConfig};
use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use unicode_width::UnicodeWidthStr;

const CONFIGURATION_DOC: &str = include_str!("../docs/configuration.md");
const CI_WORKFLOW: &str = include_str!("../.github/workflows/ci.yml");
const RELEASE_WORKFLOW: &str = include_str!("../.github/workflows/release.yml");
const INSTALLATION_DOCS: &[(&str, &str)] = &[
    (
        "docs/installation.md",
        include_str!("../docs/installation.md"),
    ),
    (
        "docs/installation.zh-CN.md",
        include_str!("../docs/installation.zh-CN.md"),
    ),
];
const DOCUMENTS: &[(&str, &str)] = &[
    ("README.md", include_str!("../README.md")),
    ("README.zh-CN.md", include_str!("../README.zh-CN.md")),
    ("docs/commands.md", include_str!("../docs/commands.md")),
    (
        "docs/commands.zh-CN.md",
        include_str!("../docs/commands.zh-CN.md"),
    ),
    (
        "docs/configuration.md",
        include_str!("../docs/configuration.md"),
    ),
    ("docs/approvals.md", include_str!("../docs/approvals.md")),
    INSTALLATION_DOCS[0],
    INSTALLATION_DOCS[1],
];

#[test]
fn documented_team_json_is_valid_and_loadable() {
    let json = first_json_fence(CONFIGURATION_DOC)
        .expect("configuration documentation must contain a fenced JSON example");

    let config: TeamConfig =
        serde_json::from_str(&json).expect("documented team JSON must deserialize");
    config
        .validate()
        .expect("documented team JSON must satisfy roster invariants");

    assert_eq!(config.name, "product-team");
    assert_eq!(
        config.default_target,
        Some(DefaultTarget::Member(MemberId::new("builder")))
    );
    assert_eq!(
        config
            .members
            .iter()
            .map(|member| member.backend)
            .collect::<Vec<_>>(),
        vec![BackendKind::Codex, BackendKind::Claude, BackendKind::Grok]
    );

    assert!(
        config.modes.review.is_some(),
        "documented modes.review must be present"
    );
    assert!(
        config.modes.plan.is_some(),
        "documented modes.plan must be present"
    );
    assert!(
        config.modes.brainstorm.is_some(),
        "documented modes.brainstorm must be present"
    );
    assert_eq!(
        config.approvals.apply_to,
        Some(vec![
            ApprovalSurface::User,
            ApprovalSurface::Relay,
            ApprovalSurface::Mode
        ])
    );
    assert_eq!(
        config.approvals.gate,
        Some(vec![
            "git".to_string(),
            "shell".to_string(),
            "file".to_string()
        ])
    );
}

#[test]
fn documented_team_json_accepts_crlf_checkout() {
    let crlf = CONFIGURATION_DOC
        .replace("\r\n", "\n")
        .replace('\n', "\r\n");
    assert!(first_json_fence(&crlf).is_some());
}

#[test]
fn markdown_table_pipes_are_display_aligned() {
    for (path, document) in DOCUMENTS {
        let lines = document.lines().collect::<Vec<_>>();
        let mut index = 0;
        while index < lines.len() {
            if !is_table_row(lines[index]) {
                index += 1;
                continue;
            }

            let start = index;
            while index < lines.len() && is_table_row(lines[index]) {
                index += 1;
            }
            let block = &lines[start..index];
            if block.len() < 2 {
                continue;
            }

            let expected = unescaped_pipe_columns(block[0]);
            for (offset, line) in block.iter().enumerate().skip(1) {
                assert_eq!(
                    unescaped_pipe_columns(line),
                    expected,
                    "unaligned Markdown table boundary at {path}:{}",
                    start + offset + 1
                );
            }
        }
    }
}

#[test]
fn readmes_use_real_product_images_without_a_handwritten_ui_mockup() {
    for (path, document) in &DOCUMENTS[..2] {
        for image in [
            "docs/assets/asterline-codex-to-agy.webp",
            "docs/assets/asterline-team.webp",
        ] {
            assert!(
                document.contains(image),
                "{path} must include the real product image {image}"
            );
        }
        assert!(
            !document.contains("┌ Asterline") && !document.contains("Illustrative transcript"),
            "{path} must not embed a handwritten TUI mockup"
        );
    }
}

#[test]
fn markdown_strong_markers_are_not_rendered_literally() {
    for (path, document) in DOCUMENTS {
        let mut in_code_block = false;
        for event in Parser::new(document) {
            match event {
                Event::Start(Tag::CodeBlock(_)) => in_code_block = true,
                Event::End(TagEnd::CodeBlock) => in_code_block = false,
                Event::Text(text) if !in_code_block => {
                    assert!(
                        !text.contains("**"),
                        "literal strong delimiter in rendered text from {path}: {text}"
                    );
                }
                _ => {}
            }
        }
    }
}

#[test]
fn installers_are_tested_checksummed_and_attested() {
    assert!(CI_WORKFLOW.contains("Smoke test Windows installer"));
    assert!(RELEASE_WORKFLOW.contains("./scripts/build-windows-installer.ps1 -Version $version"));
    assert!(RELEASE_WORKFLOW.contains("sha256sum *.tar.gz *.zip *.exe *.dmg"));
    assert!(RELEASE_WORKFLOW.contains("dist/*.exe"));
    assert!(RELEASE_WORKFLOW.contains("./scripts/build-macos-dmg.sh"));
    assert!(RELEASE_WORKFLOW.contains("Smoke test DMG and package payload"));
    assert!(RELEASE_WORKFLOW.contains("dist/*.dmg"));

    for (path, document) in INSTALLATION_DOCS {
        assert!(
            document.contains("x86_64-windows-setup.exe"),
            "{path} must link the Windows Setup asset"
        );
        assert!(
            document.contains("--no-auto-update"),
            "{path} must document the update opt-out"
        );
        assert!(
            document.contains("macos-universal.dmg"),
            "{path} must link the universal macOS DMG"
        );
        assert!(
            document.contains("Install Asterline.pkg"),
            "{path} must explain the native macOS installer"
        );
    }

    for (path, document) in &DOCUMENTS[..2] {
        assert!(
            document.contains("docs/installation"),
            "{path} must link to a dedicated installation guide"
        );
    }
}

fn first_json_fence(document: &str) -> Option<String> {
    let normalized = document.replace("\r\n", "\n");
    normalized
        .split_once("```json\n")
        .and_then(|(_, rest)| rest.split_once("\n```").map(|(json, _)| json.to_string()))
}

fn is_table_row(line: &str) -> bool {
    line.starts_with('|') && line.ends_with('|')
}

fn unescaped_pipe_columns(line: &str) -> Vec<usize> {
    let mut columns = Vec::new();
    let mut prefix = String::new();
    let mut escaped = false;
    for character in line.chars() {
        if character == '|' && !escaped {
            columns.push(UnicodeWidthStr::width(prefix.as_str()));
        }
        prefix.push(character);
        escaped = character == '\\' && !escaped;
        if character != '\\' {
            escaped = false;
        }
    }
    columns
}
