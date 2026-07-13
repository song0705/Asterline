use asterline::domain::team::{ApprovalSurface, BackendKind, DefaultTarget, MemberId, TeamConfig};
use unicode_width::UnicodeWidthStr;

const CONFIGURATION_DOC: &str = include_str!("../docs/configuration.md");
const DOCUMENTS: &[(&str, &str)] = &[
    ("README.md", include_str!("../README.md")),
    ("README.zh-CN.md", include_str!("../README.zh-CN.md")),
    ("docs/commands.md", include_str!("../docs/commands.md")),
    (
        "docs/configuration.md",
        include_str!("../docs/configuration.md"),
    ),
    ("docs/approvals.md", include_str!("../docs/approvals.md")),
];

#[test]
fn documented_team_json_is_valid_and_loadable() {
    let json = CONFIGURATION_DOC
        .split_once("```json\n")
        .and_then(|(_, rest)| rest.split_once("\n```").map(|(json, _)| json))
        .expect("configuration documentation must contain a fenced JSON example");

    let config: TeamConfig =
        serde_json::from_str(json).expect("documented team JSON must deserialize");
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
        config.modes.lead.is_some(),
        "documented modes.lead must be present"
    );
    assert!(
        config.modes.roundtable.is_some(),
        "documented modes.roundtable must be present"
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
