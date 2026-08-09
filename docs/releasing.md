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

6. Commit and push the version change and release notes.
7. Create and push an annotated tag from that release commit:

   ```bash
   version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)"
   git tag -a "v${version}" -m "Asterline v${version}"
   git push origin main "v${version}"
   ```

## Automated release

Pushing the tag starts `.github/workflows/release.yml`. The workflow:

1. verifies that the tag and Cargo package version match;
2. runs formatting, Clippy, and the test suite;
3. builds `asterline` and `ast` for Linux x86-64, Linux ARM64, macOS Intel,
   macOS Apple silicon, and Windows x86-64 MSVC;
4. packages Unix targets as `.tar.gz` and Windows as `.zip`, including the
   license and readmes;
5. creates `SHA256SUMS` and signed GitHub artifact attestations;
6. publishes a GitHub Release using `docs/releases/<tag>.md`, or generated
   release notes when no matching file exists.

The Windows build runs on `windows-latest` and links the bundled SQLite source,
so it does not rely on a runner- or user-installed `sqlite3.lib`. The regular CI
workflow also runs `cargo test --all-targets --locked` on Windows; keep this as
a real link-and-execute job rather than replacing it with `cargo check`.

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
