# Releasing Asterline

[简体中文](releasing.zh-CN.md)

Asterline releases are built and published by GitHub Actions. A release tag
must exactly match the package version in `Cargo.toml`. The normal and release
workflows use Rust 1.93.1; the explicit MSRV job enforces the package's declared
Rust 1.88 minimum.

Before the next release, a repository administrator must enable **Settings →
General → Releases → Enable release immutability**. GitHub documents that this
setting protects only future releases, not existing ones. It locks the published
release's tag and assets, so the workflow creates a draft, uploads and checks
every asset, and only then publishes it. See [GitHub's immutable release
guidance](https://docs.github.com/en/code-security/how-tos/secure-your-supply-chain/establish-provenance-and-integrity/prevent-release-changes).

## Prepare a release

1. Update `version` in `Cargo.toml`.
2. Run `cargo check` so `Cargo.lock` records the package version.
3. Install the CI-pinned audit tool once, then run the portable local quality
   gate:

   ```bash
   cargo install cargo-audit --version 0.22.2 --locked
   just check
   ```

   `just check` covers format, warning-free Clippy, all-target tests, and the
   dependency audit. Platform and installer jobs remain Actions-only.

4. Add `docs/releases/v<version>.md` with a user-facing summary. When this file
   is absent, the workflow falls back to GitHub-generated notes.
5. Read the package version, fetch existing tags, and confirm the release tag
   is unused:

   ```bash
   version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)"
   git fetch --tags
   test -z "$(git tag --list "v${version}")"
   ! git ls-remote --exit-code --tags origin "refs/tags/v${version}" >/dev/null 2>&1
   ! gh release view "v${version}" --repo song0705/Asterline >/dev/null 2>&1
   ```

6. Commit and push the version change and release notes without a tag.
7. Wait for the regular CI workflow on that exact commit to pass on Linux,
   macOS, and Windows. Do not create a release tag while any required job is
   pending or failing:

   ```bash
   commit="$(git rev-parse HEAD)"
   run_id="$(gh run list --workflow CI --commit "$commit" --limit 1 \
     --json databaseId --jq '.[0].databaseId')"
   test -n "$run_id"
   gh run watch "$run_id" --exit-status
   ```

8. Manually run the **Release** workflow from that candidate commit, supplying the version without
   the `v` prefix. It builds, packages, and smoke-tests every release asset but creates no tag,
   Release, or Homebrew PR. Do not push another commit to `main` while it runs; confirm the run
   still targets the `$commit` saved in step 7:

   ```bash
   gh workflow run Release --ref main -f version="$version"
   run_id=""
   for _ in $(seq 1 30); do
     run_id="$(gh run list --workflow Release --event workflow_dispatch --commit "$commit" \
       --limit 1 --json databaseId --jq '.[0].databaseId')"
     test -n "$run_id" && break
     sleep 2
   done
   test -n "$run_id"
   gh run watch "$run_id" --exit-status
   test "$(git rev-parse HEAD)" = "$commit"
   ```

9. Only after the preflight is green, create and push an annotated tag from it:

   ```bash
   version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)"
   git tag -a "v${version}" -m "Asterline v${version}"
   test "$(git cat-file -t "refs/tags/v${version}")" = tag
   test "$(git rev-parse "refs/tags/v${version}^{commit}")" = "$(git rev-parse HEAD)"
   git push origin main "v${version}"
   ```

## Automated release

The manually dispatched Release preflight runs items 1–8 below but skips publication and the
Homebrew update. Only a tag push starts the full `.github/workflows/release.yml`; the quality
gate rejects it unless the same commit has a successful preflight:

1. verifies that an annotated tag matches the Cargo version, resolves to the
   triggering commit, is contained in `origin/main`, has a successful preflight
   for that same commit, and has no published Release;
2. runs formatting and warning-free Clippy on Linux, then runs the test suite
   on Linux, macOS, and Windows;
3. builds Linux x86-64 and ARM64 in digest-pinned, supported PyPA
   `manylinux_2_28` containers, and builds macOS Intel, macOS Apple silicon, and
   Windows x86-64 MSVC on native runners;
4. packages Unix targets as portable `.tar.gz` archives, Windows as a portable
   `.zip`, and each verified GNU/Linux architecture as a portable archive,
   Debian package, and RPM package with the same visible `Linux-arm64` or
   `Linux-x86_64` prefix;
5. installs, runs, and removes each Debian package in Debian 12 and Ubuntu
   24.04, and each RPM package in Rocky Linux 8 and Fedora 44, before it can be
   published;
6. combines the Intel and Apple silicon binaries into one universal macOS DMG
   containing a native `Install Asterline.pkg` for `/usr/local/bin`, and builds
   the Windows binaries as a per-user Setup `.exe`;
7. installs the generated Windows Setup, runs `ast --help`, verifies `/WAITPID`
   update waiting, and uninstalls it before allowing publication;
8. creates `SHA256SUMS` and signed GitHub artifact attestations for every
   archive and installer, after rejecting any missing or unexpected asset;
9. removes any incomplete draft left by an earlier attempt without deleting or
   moving the tag, creates a clean draft using `docs/releases/<tag>.md` (or
   generated notes), uploads every asset, compares the uploaded asset names
   with `dist`, and only then publishes the complete draft.

The `publish` job records the validated annotated-tag object from `quality`.
Immediately before any draft mutation and again immediately before publication,
it asks the GitHub Git Data API for the remote ref and tag object, requires the
same tag-object SHA, and requires that object to target the triggering
`GITHUB_SHA`. A tag moved while the slower packaging jobs run therefore fails
closed.

Draft Releases are intentionally rebuildable. If a run fails during draft
creation or asset upload, rerun the same tagged workflow: after all build,
smoke, checksum, and attestation gates pass again, `publish` deletes only that
draft and recreates it cleanly. It never passes `--cleanup-tag` and verifies
that the remote annotated-tag object is unchanged after draft deletion. A
published Release is never replaced; make a new version instead. Build artifact
uploads also use the Action's explicit `overwrite` mode so a full workflow
rerun replaces artifacts from the failed attempt instead of colliding with
their names.

The macOS job always verifies the DMG, mounts it, expands the package payload,
checks both Mach-O architectures, and runs the installed-layout `ast --help`.
When all of these repository secrets are present, it Developer ID-signs the
binaries, package, and DMG, then notarizes and staples the DMG:

- `MACOS_CERTIFICATE_P12_BASE64`
- `MACOS_CERTIFICATE_PASSWORD`
- `MACOS_APPLICATION_IDENTITY`
- `MACOS_INSTALLER_IDENTITY`
- `APPLE_NOTARY_KEY_P8_BASE64`
- `APPLE_NOTARY_KEY_ID`
- `APPLE_NOTARY_ISSUER_ID`

The P12 must contain the named Developer ID Application and Developer ID
Installer identities. If any secret is absent, the workflow emits a warning and
publishes an unsigned, unnotarized DMG instead; its binaries are ad-hoc signed
only. `SHA256SUMS` and GitHub artifact attestations still cover that file, but
they do not replace Developer ID signing or Apple notarization.

## Homebrew Formula updates

Before releasing, configure `HOMEBREW_TAP_TOKEN` as an Actions secret in
`song0705/Asterline`. Use a fine-grained token restricted to
`song0705/homebrew-asterline`, with **Contents: Read and write** and **Pull
requests: Read and write** permissions. Do not use a broad personal token.
Set the token without placing it in shell history:

```bash
gh secret set HOMEBREW_TAP_TOKEN --repo song0705/Asterline
```

After the GitHub Release is published, the workflow downloads its `SHA256SUMS`,
updates the four prebuilt archive URLs and checksums in `Formula/asterline.rb`,
validates the Formula on macOS, and opens or updates an
`automation/asterline-v<version>` pull request in the tap. The Release quality
gate refuses to start without this secret, so every published version has a
corresponding Formula-update attempt.

The Linux archives intentionally target `*-unknown-linux-gnu`, not musl. The
release workflow uses PyPA's maintained `manylinux_2_28` images, which provide a
glibc 2.28 build baseline for x86-64 and ARM64. A post-build script rejects
binaries that request newer GLIBC symbols or dynamically link `libsqlite3`;
`rusqlite` uses its cross-platform `bundled` feature. Consequently the archive
requires glibc 2.28 or newer, embeds SQLite, and does not support Alpine/musl.
The image references are pinned by digest; review and update both digests when
PyPA publishes a maintained replacement. See [PyPA's supported image and ABI
matrix](https://github.com/pypa/manylinux#manylinux_2_28-almalinux-8-based).

After the GNU/Linux archives pass that gate, `package-debian` unpacks each one
inside a digest-pinned Debian 12 container, uses `dpkg-shlibdeps` to derive its
runtime `Depends`, and builds `asterline-v<version>-Linux-arm64.deb` or
`asterline-v<version>-Linux-x86_64.deb`. It installs, executes, and purges each
package there, then `smoke-deb-ubuntu` repeats that test on native Ubuntu 24.04
runners.

`package-rpm` packages those same verified archives in digest-pinned Rocky Linux
8 containers as `asterline-v<version>-Linux-arm64.rpm` or
`asterline-v<version>-Linux-x86_64.rpm`. The RPM spec retains automatic shared
library requirements and rejects a system SQLite dependency. `smoke-rpm-fedora`
then installs, executes, and removes each asset in a fresh digest-pinned Fedora
44 container. The resulting `.deb` and `.rpm` files are Release assets only; do
not describe them as APT or DNF repositories until separately managed signing
keys and repositories are in place.

The Windows build runs on `windows-latest` and links the bundled SQLite source,
so it does not rely on a runner- or user-installed `sqlite3.lib`. Inno Setup
builds the installer from `packaging/windows/asterline.iss`. Regular Windows CI
and the release workflow both call `scripts/smoke-windows-installer.ps1`, which
installs into a temporary directory, runs `ast --help`, verifies the user
`Path`, measures an ordinary update as a no-wait baseline, proves `/WAITPID`
stays blocked beyond that window, uninstalls, and confirms cleanup. `publish`
explicitly depends on the release smoke job. Both release gating and regular
CI also run `cargo test --all-targets --locked` on Windows; keep this as a real
link-and-execute job rather than replacing it with `cargo check`.

Real provider compatibility is a separate, paid, manually approved gate. Use
`.github/workflows/real-smoke.yml` and follow [the real-smoke runner and
credential guide](real-smoke.md); it is deliberately not a pull-request job.

Monitor a release from the command line:

```bash
gh run list --workflow Release
gh run watch --exit-status
```

Workflow actions are pinned to full commit SHAs. Rust's normal toolchain is also
pinned to 1.93.1 instead of the drifting `stable` channel, while the MSRV job is
pinned to 1.88.0. Review release notes and keep the human-readable Action
version comment beside each SHA when accepting an update. Rust toolchain and
manylinux digest updates remain deliberate maintainer work.

Do not move or reuse a published version tag. Fix the issue, increment the
version, and publish a new tag instead.

## Historical provenance note: v0.2.2

The successful [v0.2.2 release workflow
run](https://github.com/song0705/Asterline/actions/runs/31376054346) recorded
source commit `61b56740606ec3ab52e423b3dcc4b1377babe461`. The public `v0.2.2`
tag now resolves to `c4be080788be4187b9daff91c561ecbd68f4347e`. Those commits have
the same Git tree, `d99f356a23a2d4b38dc0045759a159db3de23816`, but different commit
identities. This is a historical provenance anomaly, not permission to repair
history by moving the tag again. Preserve this record, leave the remote tag
where it is, and use a new version for every future correction. Release
immutability protects only releases published after an administrator enables
it; it cannot retroactively repair v0.2.2.
