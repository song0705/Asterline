//! Release, MSRV, and real-smoke workflow policy.

mod common;
use common::*;

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
