# Releasing

For the maintainer. A release is a tag `vX.Y.Z`; CI builds and publishes it. Nothing is
uploaded by hand.

## Steps

1. Start from an up-to-date `main` whose CI is green.
2. Bump the version: `version` under `[workspace.package]` in `Cargo.toml` and
   `version` in `flake.nix` (the `paquet` helper). Run `cargo update --workspace` so
   `Cargo.lock` follows, and nothing else changes in it.
3. In `CHANGELOG.md`, rename `## [Unreleased]` (or `## [X.Y.Z] - unreleased`) to
   `## [X.Y.Z] - YYYY-MM-DD` (the release day). Anything merged since goes under that section;
   later work starts a new `## [Unreleased]` heading above it.
4. Commit (`chore(release): X.Y.Z`), merge to `main`, check that CI passes.
5. Tag and push the tag (only the owner pushes):

   ```sh
   git tag -a vX.Y.Z -m "alpheus X.Y.Z"
   git push origin vX.Y.Z
   ```

## What the workflow does

`.github/workflows/release.yml` runs on a pushed `v*` tag.

- **build** (read-only): checks that the tag equals the workspace version, that `flake.nix`
  carries the same one and that the CHANGELOG heading is dated, extracts the
  CHANGELOG section of that version (`.github/release/notes.sh`; it fails when the section is
  missing or empty, and points relative links at the files of the tag), builds the three binaries in release mode on `ubuntu-24.04`, packages them
  (`.github/release/package.sh`: reproducible tarball, stripped binaries, `LICENSE`, `NOTICE`, `README.md`, `INSTALL`)
  as `alpheus-X.Y.Z-x86_64-linux.tar.gz` with a `SHA256SUMS` file, and uploads them as a workflow
  artifact.
- **publish** (`contents: write`, the only job with it): creates the GitHub release with the
  CHANGELOG section as its notes and attaches the tarball and `SHA256SUMS`.

A manual run (Actions, Release, Run workflow) does the **build** job only: it publishes nothing
(**publish** needs a pushed tag), and the files are in the run's `release-files` artifact. Use it
to rehearse before tagging; the tag, `flake.nix` and dated-heading checks are skipped there, so a
`- unreleased` heading passes.

The release build uses a pinned Rust toolchain (`dtolnay/rust-toolchain@1.98` in
`release.yml`); bump it deliberately, in its own commit, not by following `stable`.

## Verify

- The release page lists the tarball and `SHA256SUMS`, and the notes match the CHANGELOG section.
- `sha256sum -c SHA256SUMS` passes on the downloaded files.
- `nix build github:mornepousse/alpheus/vX.Y.Z#alpheus-demon` works.
- If a tag was wrong, delete the release and the tag (`git push origin :refs/tags/vX.Y.Z`),
  fix, and tag again; never move a tag that people may have downloaded from.
