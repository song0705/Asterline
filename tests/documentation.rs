use asterline::domain::team::{ApprovalSurface, BackendKind, DefaultTarget, MemberId, TeamConfig};
use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use unicode_width::UnicodeWidthStr;

const CONFIGURATION_DOC: &str = include_str!("../docs/configuration.md");
const CONFIGURATION_DOC_ZH: &str = include_str!("../docs/configuration.zh-CN.md");
const APPROVALS_DOC: &str = include_str!("../docs/approvals.md");
const APPROVALS_DOC_ZH: &str = include_str!("../docs/approvals.zh-CN.md");
const CARGO_MANIFEST: &str = include_str!("../Cargo.toml");
const CI_WORKFLOW: &str = include_str!("../.github/workflows/ci.yml");
const REAL_SMOKE_WORKFLOW: &str = include_str!("../.github/workflows/real-smoke.yml");
const RELEASE_WORKFLOW: &str = include_str!("../.github/workflows/release.yml");
const RELEASING_DOC: &str = include_str!("../docs/releasing.md");
const RELEASING_DOC_ZH: &str = include_str!("../docs/releasing.zh-CN.md");
const REAL_SMOKE_DOC: &str = include_str!("../docs/real-smoke.md");
const REAL_SMOKE_DOC_ZH: &str = include_str!("../docs/real-smoke.zh-CN.md");
const JUSTFILE: &str = include_str!("../justfile");
const WINDOWS_INSTALLER_SMOKE: &str = include_str!("../scripts/smoke-windows-installer.ps1");
const HOMEBREW_FORMULA: &str = include_str!("../packaging/homebrew/Formula/asterline.rb");
const AUR_PKGBUILD: &str = include_str!("../packaging/aur/asterline/PKGBUILD");
const AUR_SRCINFO: &str = include_str!("../packaging/aur/asterline/.SRCINFO");
const DEB_CONTROL: &str = include_str!("../packaging/deb/control.in");
const PACKAGING_README: &str = include_str!("../packaging/README.md");
const PACKAGING_README_ZH: &str = include_str!("../packaging/README.zh-CN.md");
const MACOS_PACKAGE_README: &str = include_str!("../packaging/macos/README.txt");
const MACOS_PACKAGE_README_ZH: &str = include_str!("../packaging/macos/README.zh-CN.txt");
const BUILD_MACOS_DMG: &str = include_str!("../scripts/build-macos-dmg.sh");
const PACKAGE_DEB: &str = include_str!("../scripts/package-deb.sh");
const SMOKE_DEB_PACKAGE: &str = include_str!("../scripts/smoke-deb-package.sh");
const RPM_SPEC: &str = include_str!("../packaging/rpm/asterline.spec.in");
const PACKAGE_RPM: &str = include_str!("../scripts/package-rpm.sh");
const SMOKE_RPM_PACKAGE: &str = include_str!("../scripts/smoke-rpm-package.sh");
const HOMEBREW_RELEASE_VERSION: &str = "0.2.9";
const AUR_RELEASE_VERSION: &str = "0.2.3";
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
const COMMAND_DOCS: &[(&str, &str)] = &[
    ("docs/commands.md", include_str!("../docs/commands.md")),
    (
        "docs/commands.zh-CN.md",
        include_str!("../docs/commands.zh-CN.md"),
    ),
];
const BILINGUAL_DOCUMENTS: &[(&str, &str, &str, &str)] = &[
    (
        "README.md",
        include_str!("../README.md"),
        "README.zh-CN.md",
        include_str!("../README.zh-CN.md"),
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
        include_str!("../docs/releases/v0.2.8.en.md"),
        "docs/releases/v0.2.8.md",
        include_str!("../docs/releases/v0.2.8.md"),
    ),
    (
        "packaging/README.md",
        PACKAGING_README,
        "packaging/README.zh-CN.md",
        PACKAGING_README_ZH,
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
    let release_workflow = RELEASE_WORKFLOW.replace("\r\n", "\n");

    assert!(CI_WORKFLOW.contains("Smoke test Windows installer"));
    assert!(CI_WORKFLOW.contains("./scripts/smoke-windows-installer.ps1"));
    assert!(RELEASE_WORKFLOW.contains("./scripts/build-windows-installer.ps1 -Version $version"));
    assert!(RELEASE_WORKFLOW.contains("./scripts/smoke-windows-installer.ps1"));
    assert!(
        RELEASE_WORKFLOW.contains(
            "needs: [quality, build, build-linux, package-debian, smoke-deb-ubuntu, package-rpm, smoke-rpm-fedora, package-macos, smoke-windows-installer]"
        )
    );
    assert!(RELEASE_WORKFLOW.contains("sha256sum \"${assets[@]}\" > SHA256SUMS"));
    assert!(RELEASE_WORKFLOW.contains("Validate the exact release asset set"));
    let exact_assets = release_workflow
        .split_once("          expected=(\n")
        .and_then(|(_, rest)| rest.split_once("          )\n"))
        .map(|(assets, _)| assets)
        .expect("release workflow must declare its exact asset closure");
    let ordered_assets = [
        "asterline-$version-aarch64-apple-darwin.tar.gz",
        "asterline-$version-x86_64-apple-darwin.tar.gz",
        "asterline-$version-macos-universal.dmg",
        "asterline-v$version-Linux-arm64.tar.gz",
        "asterline-v$version-Linux-arm64.deb",
        "asterline-v$version-Linux-arm64.rpm",
        "asterline-v$version-Linux-x86_64.tar.gz",
        "asterline-v$version-Linux-x86_64.deb",
        "asterline-v$version-Linux-x86_64.rpm",
        "asterline-$version-x86_64-pc-windows-msvc.zip",
        "asterline-$version-x86_64-windows-setup.exe",
    ];
    let mut previous_position = None;
    for asset in ordered_assets {
        let position = exact_assets
            .find(asset)
            .unwrap_or_else(|| panic!("release asset set must contain {asset}"));
        assert!(
            previous_position.is_none_or(|previous| position > previous),
            "release asset {asset} must remain in platform-grouped upload order"
        );
        previous_position = Some(position);
    }
    assert_eq!(
        exact_assets
            .lines()
            .filter(|line| line.trim_start().starts_with('"'))
            .count(),
        11
    );
    assert!(RELEASE_WORKFLOW.contains("$RUNNER_TEMP/release-assets.txt"));
    assert!(RELEASE_WORKFLOW.contains("gh release upload \"$GITHUB_REF_NAME\" \"${files[@]}\""));
    assert!(RELEASE_WORKFLOW.contains("dist/*.exe"));
    assert!(RELEASE_WORKFLOW.contains("dist/*.deb"));
    assert!(RELEASE_WORKFLOW.contains("dist/*.rpm"));
    assert!(RELEASE_WORKFLOW.contains("./scripts/build-macos-dmg.sh"));
    assert!(RELEASE_WORKFLOW.contains("Smoke test DMG and package payload"));
    assert!(RELEASE_WORKFLOW.contains("dist/*.dmg"));
    assert!(WINDOWS_INSTALLER_SMOKE.contains("$baselineWatch"));
    assert!(WINDOWS_INSTALLER_SMOKE.contains("$update.WaitForExit($probeMilliseconds)"));
    assert!(WINDOWS_INSTALLER_SMOKE.contains("$blocker.Kill()"));

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
            document.contains("--update"),
            "{path} must document the forced update check"
        );
        assert!(
            document.contains("macos-universal.dmg"),
            "{path} must link the universal macOS DMG"
        );
        assert!(
            document.contains("Install Asterline.pkg"),
            "{path} must explain the native macOS installer"
        );
        for linux_architecture in ["Linux-arm64", "Linux-x86_64"] {
            assert!(
                document.contains(linux_architecture),
                "{path} must name the Linux asset architecture as {linux_architecture}"
            );
        }
    }

    for (path, document) in &DOCUMENTS[..2] {
        assert!(
            document.contains("docs/installation"),
            "{path} must link to a dedicated installation guide"
        );
    }

    for (path, document) in COMMAND_DOCS {
        for option in ["--update", "--no-auto-update"] {
            assert!(
                document.contains(option),
                "{path} must document startup option {option}"
            );
        }
        assert!(
            document.contains("ast update"),
            "{path} must document the installation-aware update command"
        );
    }
}

#[test]
fn debian_packages_are_pinned_and_release_gated() {
    assert!(DEB_CONTROL.contains("Package: asterline"));
    for placeholder in ["@VERSION@", "@ARCH@", "@DEPENDS@"] {
        assert!(DEB_CONTROL.contains(placeholder));
    }
    assert!(PACKAGE_DEB.contains("dpkg-shlibdeps -O"));
    assert!(PACKAGE_DEB.contains("Source: asterline"));
    assert!(PACKAGE_DEB.contains("debian_dir=\"$work_dir/debian\""));
    assert!(PACKAGE_DEB.contains("--root-owner-group --build"));
    assert!(PACKAGE_DEB.contains("x86_64-unknown-linux-gnu:amd64"));
    assert!(PACKAGE_DEB.contains("aarch64-unknown-linux-gnu:arm64"));
    assert!(PACKAGE_DEB.contains("asterline-v${version}-Linux-${asset_arch}.deb"));
    assert!(SMOKE_DEB_PACKAGE.contains("apt-get install --yes"));
    assert!(SMOKE_DEB_PACKAGE.contains("apt-get purge --yes asterline"));
    assert!(RELEASE_WORKFLOW.contains("package-debian:"));
    assert!(RELEASE_WORKFLOW.contains("smoke-deb-ubuntu:"));
    assert!(RELEASE_WORKFLOW.contains("debian@sha256:"));
    assert!(RELEASE_WORKFLOW.contains("dist/*.deb"));
    assert!(PACKAGING_README.contains("Debian / Ubuntu"));
    assert!(PACKAGING_README.contains("v0.2.3 Release predates all Linux package assets"));
}

#[test]
fn rpm_packages_are_pinned_and_release_gated() {
    for placeholder in ["@VERSION@", "@TARGET@", "@ARCH@"] {
        assert!(RPM_SPEC.contains(placeholder));
    }
    assert!(RPM_SPEC.contains("BuildArch:      @ARCH@"));
    assert!(RPM_SPEC.contains("%global debug_package %{nil}"));
    assert!(!RPM_SPEC.contains("Requires:"));
    assert!(PACKAGE_RPM.contains("x86_64-unknown-linux-gnu:x86_64"));
    assert!(PACKAGE_RPM.contains("aarch64-unknown-linux-gnu:aarch64"));
    assert!(PACKAGE_RPM.contains("rpmbuild"));
    assert!(PACKAGE_RPM.contains("rpm --checksig \"$built_package\""));
    assert!(PACKAGE_RPM.contains("asterline-v${version}-Linux-${asset_arch}.rpm"));
    assert!(SMOKE_RPM_PACKAGE.contains("dnf install --assumeyes"));
    assert!(SMOKE_RPM_PACKAGE.contains("dnf remove --assumeyes asterline"));
    assert!(RELEASE_WORKFLOW.contains("package-rpm:"));
    assert!(RELEASE_WORKFLOW.contains("smoke-rpm-fedora:"));
    assert!(RELEASE_WORKFLOW.contains("rockylinux/rockylinux:8@sha256:"));
    assert!(RELEASE_WORKFLOW.contains("fedora:44@sha256:"));
    assert!(RELEASE_WORKFLOW.contains("dist/*.rpm"));
    assert!(PACKAGING_README.contains("matching RPM packages"));
}

#[test]
fn third_party_package_definitions_are_version_pinned_and_safe() {
    assert!(HOMEBREW_FORMULA.contains(&format!("releases/download/v{HOMEBREW_RELEASE_VERSION}")));
    assert!(HOMEBREW_FORMULA.contains("on_macos"));
    assert!(HOMEBREW_FORMULA.contains("on_linux"));
    assert!(HOMEBREW_FORMULA.contains("bin.install \"asterline\", \"ast\""));
    assert!(HOMEBREW_FORMULA.contains("doc.install \"LICENSE\""));
    for (target, checksum) in [
        (
            "aarch64-apple-darwin.tar.gz",
            "322604253de110254c119300d562c2646992465026ba85173acca82b08bcb6a4",
        ),
        (
            "x86_64-apple-darwin.tar.gz",
            "ed33fbc31705663e5a00cc03a53ebb522e0e3d0db6bb5286a3bc5e3b62162842",
        ),
        (
            "asterline-v0.2.9-Linux-arm64.tar.gz",
            "fb2afd79405f25ed697c596860a169599d6bb258e232f34fc5df6069a71de766",
        ),
        (
            "asterline-v0.2.9-Linux-x86_64.tar.gz",
            "eedde381531acfbad4a451cbdd8c5de008b4f54d6f0f42567742ae90451b802d",
        ),
    ] {
        assert!(
            HOMEBREW_FORMULA.contains(target) && HOMEBREW_FORMULA.contains(checksum),
            "Homebrew Formula must pin the {target} release asset"
        );
    }

    assert!(AUR_PKGBUILD.contains(&format!("pkgver={AUR_RELEASE_VERSION}")));
    assert!(AUR_PKGBUILD.contains("archive/${_commit}.tar.gz"));
    assert!(AUR_PKGBUILD.contains("cargo build --frozen --release --bins"));
    assert!(AUR_PKGBUILD.contains("cargo test --frozen --all-targets"));
    assert!(!AUR_PKGBUILD.contains("SKIP"));
    for dependency in ["gcc-libs", "glibc", "sqlite", "cargo"] {
        assert!(
            AUR_PKGBUILD.contains(dependency),
            "AUR package must declare {dependency}"
        );
    }
    let source_sha256 = "2f24699cc4d17dc7f075fcebe56df0253e94eb25bcf3f00526f366ef1b926fc2";
    assert!(AUR_PKGBUILD.contains(source_sha256));
    assert!(AUR_SRCINFO.contains(&format!("pkgver = {AUR_RELEASE_VERSION}")));
    assert!(AUR_SRCINFO.contains(source_sha256));
    assert!(!AUR_SRCINFO.contains("SKIP"));

    let packaging_readme = PACKAGING_README.replace("\r\n", "\n");
    assert!(packaging_readme.contains("published Homebrew tap"));
    assert!(packaging_readme.contains("v0.2.5 contains\nthe earlier Debian-only pair"));
    assert!(packaging_readme.contains("v0.2.7 is the first Release with the visible"));
    assert!(
        packaging_readme
            .contains("brew audit --strict --online --formula song0705/asterline/asterline")
    );
    assert!(PACKAGING_README.contains("GLIBC_2.39"));
    assert!(
        INSTALLATION_DOCS[0]
            .1
            .contains("v0.2.3 Linux archives predate this release")
    );
    assert!(
        INSTALLATION_DOCS[1]
            .1
            .contains("v0.2.3 Linux 归档早于上述发布保证")
    );
}

#[test]
fn rust_and_dependency_policy_is_explicit_and_gated() {
    assert!(CARGO_MANIFEST.contains("rust-version = \"1.88\""));
    assert!(CARGO_MANIFEST.contains("features = [\"bundled\"]"));
    assert!(!CARGO_MANIFEST.contains("bundled-windows"));

    assert!(CI_WORKFLOW.contains("name: MSRV · Rust 1.88.0"));
    assert!(CI_WORKFLOW.contains("toolchain: 1.88.0"));
    assert!(CI_WORKFLOW.contains("toolchain: 1.93.1"));
    assert!(!CI_WORKFLOW.contains("toolchain: stable"));
    assert!(!RELEASE_WORKFLOW.contains("toolchain: stable"));

    for command in [
        "cargo fmt --check",
        "cargo clippy --all-targets --locked -- -D warnings",
        "cargo test --all-targets --locked --no-fail-fast",
        "cargo audit",
    ] {
        assert!(JUSTFILE.contains(command), "just check must run {command}");
        assert!(CI_WORKFLOW.contains(command), "CI must run {command}");
    }

    for (path, document) in DOCUMENTS {
        assert!(
            !document.contains("Rust 1.85"),
            "{path} must not advertise the obsolete Rust minimum"
        );
    }
    for (path, document) in INSTALLATION_DOCS {
        assert!(
            document.contains("Rust 1.88"),
            "{path} must advertise the Cargo MSRV"
        );
        assert!(
            document.contains("glibc 2.28"),
            "{path} must state the GNU/Linux ABI baseline"
        );
    }
}

#[test]
fn release_keeps_integrity_gates_with_unsigned_macos_fallback() {
    let release_workflow = RELEASE_WORKFLOW.replace("\r\n", "\n");

    assert!(RELEASE_WORKFLOW.contains("Verify tag provenance and publication state"));
    assert!(RELEASE_WORKFLOW.contains("git cat-file -t"));
    assert!(RELEASE_WORKFLOW.contains("published version tags are never reused"));
    assert!(RELEASE_WORKFLOW.contains("tag_object: ${{ steps.provenance.outputs.tag_object }}"));
    assert!(RELEASE_WORKFLOW.contains("manylinux_2_28_x86_64@sha256:"));
    assert!(RELEASE_WORKFLOW.contains("manylinux_2_28_aarch64@sha256:"));
    assert!(RELEASE_WORKFLOW.contains("ar bash cc curl git objdump readelf strip tar"));
    assert!(RELEASE_WORKFLOW.contains("./scripts/verify-linux-release.sh"));
    assert!(RELEASE_WORKFLOW.contains("Publishing unsigned and unnotarized macOS DMG"));
    assert!(RELEASE_WORKFLOW.contains("if: steps.signing.outputs.enabled == 'true'"));
    assert!(MACOS_PACKAGE_README.contains("only when the Release notes say"));
    assert!(MACOS_PACKAGE_README.contains("unsigned and unnotarized"));

    let draft = RELEASE_WORKFLOW
        .find("Rebuild a clean draft GitHub Release")
        .expect("release must rebuild only its incomplete draft");
    let upload = RELEASE_WORKFLOW
        .find("Upload every release asset to the draft")
        .expect("release must upload after creating the draft");
    let verify = RELEASE_WORKFLOW
        .find("Verify draft assets")
        .expect("release must verify draft assets");
    let publish = RELEASE_WORKFLOW
        .find("Publish the complete draft")
        .expect("release must publish only after verification");
    let attest = RELEASE_WORKFLOW
        .find("Attest release artifacts")
        .expect("release assets must be attested before draft replacement");
    assert!(attest < draft);
    assert!(draft < upload && upload < verify && verify < publish);
    assert!(RELEASE_WORKFLOW.contains("gh release delete \"$GITHUB_REF_NAME\""));
    assert!(!RELEASE_WORKFLOW.contains("--cleanup-tag"));
    assert_eq!(RELEASE_WORKFLOW.matches("overwrite: true").count(), 5);
    assert_eq!(
        RELEASE_WORKFLOW
            .matches("EXPECTED_TAG_OBJECT: ${{ needs.quality.outputs.tag_object }}")
            .count(),
        2
    );
    assert_eq!(
        RELEASE_WORKFLOW
            .matches("git/tags/$remote_tag_object")
            .count(),
        2
    );
    assert_eq!(
        release_workflow.matches("          verify_tag\n").count(),
        2
    );
    assert_eq!(
        RELEASE_WORKFLOW
            .matches(".object.sha <<< \"$tag_json\")\" = \"$GITHUB_SHA\"")
            .count(),
        2
    );
    assert!(RELEASING_DOC.contains("Draft Releases are intentionally rebuildable"));
    assert!(RELEASING_DOC.contains("published Release is never replaced"));
    assert!(RELEASING_DOC.contains("before any draft mutation"));

    for (path, document) in INSTALLATION_DOCS {
        assert!(
            document.contains("v0.2.3")
                && (document.contains("unsigned") || document.contains("未签名")),
            "{path} must identify that an unsigned v0.2.3 DMG needs an explicit override"
        );
    }

    assert!(RELEASING_DOC.contains("Enable release immutability"));
    assert!(RELEASING_DOC.contains("61b56740606ec3ab52e423b3dcc4b1377babe461"));
    assert!(RELEASING_DOC.contains("c4be080788be4187b9daff91c561ecbd68f4347e"));
    assert!(RELEASING_DOC.contains("Preserve this record"));
    assert!(RELEASING_DOC.contains("moving the tag again"));
}

#[test]
fn real_smoke_has_a_controlled_executable_entrypoint() {
    assert!(REAL_SMOKE_WORKFLOW.contains("workflow_dispatch:"));
    assert!(!REAL_SMOKE_WORKFLOW.contains("pull_request:"));
    assert!(REAL_SMOKE_WORKFLOW.contains("environment: real-smoke"));
    assert!(REAL_SMOKE_WORKFLOW.contains("secrets.OPENAI_API_KEY"));
    assert!(REAL_SMOKE_WORKFLOW.contains("secrets.ANTHROPIC_API_KEY"));
    assert!(REAL_SMOKE_WORKFLOW.contains("secrets.XAI_API_KEY"));
    assert!(REAL_SMOKE_WORKFLOW.contains("runs-on: [self-hosted, asterline-real-smoke]"));
    assert!(REAL_SMOKE_WORKFLOW.contains("ASTERLINE_SMOKE_AGY=1"));
    assert!(REAL_SMOKE_WORKFLOW.contains("rustup run 1.93.1 cargo test"));
    assert!(REAL_SMOKE_DOC.contains("Prevent self-review"));
    assert!(REAL_SMOKE_DOC.contains("config.sh --ephemeral"));
    assert!(REAL_SMOKE_DOC.contains("real-smoke.yml@refs/heads/main"));
    assert_eq!(
        REAL_SMOKE_WORKFLOW
            .matches("persist-credentials: false")
            .count(),
        2
    );

    let agy_job = REAL_SMOKE_WORKFLOW
        .split_once("  agy-provider:")
        .expect("real-smoke workflow must define the isolated Agy job")
        .1;
    assert!(
        !agy_job.contains("dtolnay/rust-toolchain") && !agy_job.contains("Swatinem/rust-cache"),
        "the pre-authenticated Agy runner must not execute third-party setup actions"
    );

    for provider_source in [
        "learn.chatgpt.com/docs/codex/cli",
        "code.claude.com/docs/en/getting-started",
        "github.com/xai-org/grok-build",
    ] {
        assert!(
            REAL_SMOKE_DOC.contains(provider_source),
            "real-smoke guide must cite {provider_source}"
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
