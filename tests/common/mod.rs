//! Shared fixtures for crate-level documentation and release tests.
//!
//! `include_str!` paths are relative to this file (`tests/common/`).
//! Each integration crate only uses a subset of these fixtures.
#![allow(dead_code)]

use unicode_width::UnicodeWidthStr;

pub const CONFIGURATION_DOC: &str = include_str!("../../docs/configuration.md");
pub const CONFIGURATION_DOC_ZH: &str = include_str!("../../docs/configuration.zh-CN.md");
pub const APPROVALS_DOC: &str = include_str!("../../docs/approvals.md");
pub const APPROVALS_DOC_ZH: &str = include_str!("../../docs/approvals.zh-CN.md");
pub const CARGO_MANIFEST: &str = include_str!("../../Cargo.toml");
pub const CI_WORKFLOW: &str = include_str!("../../.github/workflows/ci.yml");
pub const REAL_SMOKE_WORKFLOW: &str = include_str!("../../.github/workflows/real-smoke.yml");
pub const RELEASE_WORKFLOW: &str = include_str!("../../.github/workflows/release.yml");
pub const RELEASING_DOC: &str = include_str!("../../docs/releasing.md");
pub const RELEASING_DOC_ZH: &str = include_str!("../../docs/releasing.zh-CN.md");
pub const REAL_SMOKE_DOC: &str = include_str!("../../docs/real-smoke.md");
pub const REAL_SMOKE_DOC_ZH: &str = include_str!("../../docs/real-smoke.zh-CN.md");
pub const JUSTFILE: &str = include_str!("../../justfile");
pub const WINDOWS_INSTALLER_SMOKE: &str = include_str!("../../scripts/smoke-windows-installer.ps1");
pub const HOMEBREW_FORMULA: &str = include_str!("../../packaging/homebrew/Formula/asterline.rb");
pub const AUR_PKGBUILD: &str = include_str!("../../packaging/aur/asterline/PKGBUILD");
pub const AUR_SRCINFO: &str = include_str!("../../packaging/aur/asterline/.SRCINFO");
pub const DEB_CONTROL: &str = include_str!("../../packaging/deb/control.in");
pub const PACKAGING_README: &str = include_str!("../../packaging/README.md");
pub const PACKAGING_README_ZH: &str = include_str!("../../packaging/README.zh-CN.md");
pub const MACOS_PACKAGE_README: &str = include_str!("../../packaging/macos/README.txt");
pub const MACOS_PACKAGE_README_ZH: &str = include_str!("../../packaging/macos/README.zh-CN.txt");
pub const BUILD_MACOS_DMG: &str = include_str!("../../scripts/build-macos-dmg.sh");
pub const PACKAGE_DEB: &str = include_str!("../../scripts/package-deb.sh");
pub const SMOKE_DEB_PACKAGE: &str = include_str!("../../scripts/smoke-deb-package.sh");
pub const RPM_SPEC: &str = include_str!("../../packaging/rpm/asterline.spec.in");
pub const PACKAGE_RPM: &str = include_str!("../../scripts/package-rpm.sh");
pub const SMOKE_RPM_PACKAGE: &str = include_str!("../../scripts/smoke-rpm-package.sh");
pub const HOMEBREW_RELEASE_VERSION: &str = "0.2.9";
pub const AUR_RELEASE_VERSION: &str = "0.2.3";
pub const INSTALLATION_DOCS: &[(&str, &str)] = &[
    (
        "docs/installation.md",
        include_str!("../../docs/installation.md"),
    ),
    (
        "docs/installation.zh-CN.md",
        include_str!("../../docs/installation.zh-CN.md"),
    ),
];
pub const COMMAND_DOCS: &[(&str, &str)] = &[
    ("docs/commands.md", include_str!("../../docs/commands.md")),
    (
        "docs/commands.zh-CN.md",
        include_str!("../../docs/commands.zh-CN.md"),
    ),
];
pub const BILINGUAL_DOCUMENTS: &[(&str, &str, &str, &str)] = &[
    (
        "README.en.md",
        include_str!("../../README.en.md"),
        "README.md",
        include_str!("../../README.md"),
    ),
    (
        "docs/README.en.md",
        include_str!("../../docs/README.en.md"),
        "docs/README.md",
        include_str!("../../docs/README.md"),
    ),
    (
        "docs/installation.md",
        INSTALLATION_DOCS[0].1,
        "docs/installation.zh-CN.md",
        INSTALLATION_DOCS[1].1,
    ),
    (
        "docs/commands.md",
        COMMAND_DOCS[0].1,
        "docs/commands.zh-CN.md",
        COMMAND_DOCS[1].1,
    ),
    (
        "docs/configuration.md",
        CONFIGURATION_DOC,
        "docs/configuration.zh-CN.md",
        CONFIGURATION_DOC_ZH,
    ),
    (
        "docs/approvals.md",
        APPROVALS_DOC,
        "docs/approvals.zh-CN.md",
        APPROVALS_DOC_ZH,
    ),
    (
        "docs/real-smoke.md",
        REAL_SMOKE_DOC,
        "docs/real-smoke.zh-CN.md",
        REAL_SMOKE_DOC_ZH,
    ),
    (
        "docs/releasing.md",
        RELEASING_DOC,
        "docs/releasing.zh-CN.md",
        RELEASING_DOC_ZH,
    ),
    (
        "docs/releases/v0.2.8.en.md",
        include_str!("../../docs/releases/v0.2.8.en.md"),
        "docs/releases/v0.2.8.md",
        include_str!("../../docs/releases/v0.2.8.md"),
    ),
    (
        "packaging/README.md",
        PACKAGING_README,
        "packaging/README.zh-CN.md",
        PACKAGING_README_ZH,
    ),
];
pub const DOCUMENTS: &[(&str, &str)] = &[
    ("README.md", include_str!("../../README.md")),
    ("README.en.md", include_str!("../../README.en.md")),
    ("docs/README.md", include_str!("../../docs/README.md")),
    ("docs/README.en.md", include_str!("../../docs/README.en.md")),
    ("docs/commands.md", include_str!("../../docs/commands.md")),
    (
        "docs/commands.zh-CN.md",
        include_str!("../../docs/commands.zh-CN.md"),
    ),
    (
        "docs/configuration.md",
        include_str!("../../docs/configuration.md"),
    ),
    ("docs/approvals.md", include_str!("../../docs/approvals.md")),
    INSTALLATION_DOCS[0],
    INSTALLATION_DOCS[1],
];

pub fn first_json_fence(document: &str) -> Option<String> {
    let normalized = document.replace("\r\n", "\n");
    normalized
        .split_once("```json\n")
        .and_then(|(_, rest)| rest.split_once("\n```").map(|(json, _)| json.to_string()))
}

pub fn is_table_row(line: &str) -> bool {
    line.starts_with('|') && line.ends_with('|')
}

pub fn unescaped_pipe_columns(line: &str) -> Vec<usize> {
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
