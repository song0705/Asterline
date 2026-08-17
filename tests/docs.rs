//! Product documentation: README, bilingual links, tables, and documented config.

use asterline::domain::team::{ApprovalSurface, BackendKind, DefaultTarget, MemberId, TeamConfig};
use pulldown_cmark::{Event, Parser, Tag, TagEnd};

mod common;
use common::*;

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
fn maintained_documents_have_cross_linked_english_and_chinese_versions() {
    for (english_path, english, chinese_path, chinese) in BILINGUAL_DOCUMENTS {
        let english_name = english_path.rsplit('/').next().unwrap_or(english_path);
        let chinese_name = chinese_path.rsplit('/').next().unwrap_or(chinese_path);
        assert!(
            english.contains(chinese_name),
            "{english_path} must link to {chinese_path}"
        );
        assert!(
            chinese.contains(english_name),
            "{chinese_path} must link to {english_path}"
        );
    }

    assert!(MACOS_PACKAGE_README.contains("README.zh-CN.txt"));
    assert!(MACOS_PACKAGE_README_ZH.contains("README.txt"));
    assert!(BUILD_MACOS_DMG.contains("README.zh-CN.txt"));
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
fn codex_app_server_permission_mapping_is_documented() {
    let approvals_doc = APPROVALS_DOC.replace("\r\n", "\n");
    assert!(
        CONFIGURATION_DOC.contains("App Server `approvalPolicy` (`never` by default)"),
        "configuration must document Codex's native approval-policy transport"
    );
    assert!(
        approvals_doc.contains(
            "Agree or deny in the card above the composer (`y` / `n`); `/approve` and\n`/reject` still send the same one-time decision back to the live Codex thread."
        ),
        "approvals must document that Codex callbacks reach the user rather than being auto-resolved"
    );
}

#[test]
fn readmes_use_real_product_images_without_a_handwritten_ui_mockup() {
    for (path, document) in &DOCUMENTS[..2] {
        for image in [
            "docs/assets/chat.webp",
            "docs/assets/team.webp",
            "docs/assets/mode.webp",
            "docs/assets/review.webp",
            "docs/assets/plan.webp",
            "docs/assets/brainstorm.webp",
            "docs/assets/team-run.webp",
            "docs/assets/team-done.webp",
            "docs/assets/runs.webp",
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
