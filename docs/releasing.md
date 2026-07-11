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
   cargo test --locked
   ```

4. Commit and push the version change.
5. Create and push an annotated tag:

   ```bash
   version=0.1.0
   git tag -a "v$version" -m "Asterline v$version"
   git push origin main "v$version"
   ```

## Automated release

Pushing the tag starts `.github/workflows/release.yml`. The workflow:

1. verifies that the tag and Cargo package version match;
2. runs formatting, Clippy, and the test suite;
3. builds `asterline` and `ast` for Linux x86-64, Linux ARM64, macOS Intel,
   and macOS Apple silicon;
4. packages each target with the license and readmes;
5. creates `SHA256SUMS` and signed GitHub artifact attestations;
6. publishes a GitHub Release with generated release notes.

Monitor a release from the command line:

```bash
gh run list --workflow Release
gh run watch --exit-status
```

Do not move or reuse a published version tag. Fix the issue, increment the
version, and publish a new tag instead.
