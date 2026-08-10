# Releasing Asterline

Asterline releases are built and published by GitHub Actions. A release tag
must exactly match the package version in `Cargo.toml`.

## Prepare a release

1. Update `version` in `Cargo.toml`.
2. Run `cargo check` so `Cargo.lock` records the package version.
3. Run the local quality gate:

   ```bash
   cargo fmt --check
   cargo clippy --all-targets --locked -- -D warnings
   cargo test --all-targets --locked --no-fail-fast
   ```

4. Add `docs/releases/v<version>.md` with a user-facing summary. When this file
   is absent, the workflow falls back to GitHub-generated notes.
5. Read the package version, fetch existing tags, and confirm the release tag
   is unused:

   ```bash
   version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)"
   git fetch --tags
   test -z "$(git tag --list "v${version}")"
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

8. Only after that commit is green, create and push an annotated tag from it:

   ```bash
   version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)"
   git tag -a "v${version}" -m "Asterline v${version}"
   git push origin main "v${version}"
   ```

## Automated release

Pushing the tag starts `.github/workflows/release.yml`. The workflow:

1. verifies that the tag and Cargo package version match;
2. runs formatting and warning-free Clippy on Linux, then runs the test suite
   on Linux, macOS, and Windows;
3. builds `asterline` and `ast` for Linux x86-64, Linux ARM64, macOS Intel,
   macOS Apple silicon, and Windows x86-64 MSVC;
4. packages Unix targets as portable `.tar.gz` archives and Windows as a
   portable `.zip`;
5. combines the Intel and Apple silicon binaries into one universal macOS DMG
   containing a native `Install Asterline.pkg` for `/usr/local/bin`, and builds
   the Windows binaries as a per-user Setup `.exe`;
6. creates `SHA256SUMS` and signed GitHub artifact attestations for every
   archive and installer;
7. publishes a GitHub Release using `docs/releases/<tag>.md`, or generated
   release notes when no matching file exists.

The macOS job always verifies the DMG, mounts it, expands the package payload,
checks both Mach-O architectures, and runs the installed-layout `ast --help`.
Without Apple credentials it publishes an unsigned DMG. To enable Developer ID
signing and notarization, configure these repository secrets:

- `MACOS_CERTIFICATE_P12_BASE64`
- `MACOS_CERTIFICATE_PASSWORD`
- `MACOS_APPLICATION_IDENTITY`
- `MACOS_INSTALLER_IDENTITY`
- `APPLE_NOTARY_KEY_P8_BASE64`
- `APPLE_NOTARY_KEY_ID`
- `APPLE_NOTARY_ISSUER_ID`

The P12 must contain the named Developer ID Application and Developer ID
Installer identities. When signing is enabled, notarization credentials are
mandatory; the workflow submits the DMG with `notarytool` and staples the
ticket before upload.

The Windows build runs on `windows-latest` and links the bundled SQLite source,
so it does not rely on a runner- or user-installed `sqlite3.lib`. Inno Setup
builds the installer from `packaging/windows/asterline.iss`. Regular Windows CI
installs it into a temporary directory, runs `ast --help`, verifies the user
`Path`, uninstalls it, and confirms cleanup. Both release gating and regular CI
also run `cargo test --all-targets --locked` on Windows; keep this as a real
link-and-execute job rather than replacing it with `cargo check`.

Monitor a release from the command line:

```bash
gh run list --workflow Release
gh run watch --exit-status
```

Workflow actions are pinned to full commit SHAs. Dependabot tracks the
`github-actions` ecosystem in `.github/dependabot.yml`; review its release
notes and keep the human-readable version comment beside each SHA when
accepting an update.

Do not move or reuse a published version tag. Fix the issue, increment the
version, and publish a new tag instead.
