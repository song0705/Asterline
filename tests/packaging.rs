//! Installer, Debian, RPM, and third-party package definitions.

mod common;
use common::*;

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
