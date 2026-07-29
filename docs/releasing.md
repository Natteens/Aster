# Releasing ASTER

ASTER releases automatically from validated Conventional Commits on `main`. There is no release
branch and no manual version bump in the normal flow.

## Automatic flow

1. CI checks version consistency, formatting, Clippy, the workspace tests, the locked workspace,
   and the pure release scripts.
2. On a non-release push to `main`, semantic-release analyzes commits since the latest tag.
3. When a release is needed, semantic-release updates the canonical version, crate requirements,
   lockfiles, extension version, and changelog; it then creates
   `chore(release): <version> [skip ci]` and tag `v<version>`.
4. The same CI run calls the reusable release workflow with the release version, tag, and exact
   release commit SHA.
5. Windows x64 and Linux x64 jobs build native binaries, create deterministic archives and
   checksums, and upload their validated intermediate artifacts.
6. The workflow assembles versioned assets, stable installer aliases, sidecars, installer scripts,
   and `release-manifest.json`.
7. Final platform jobs exercise the published asset tree, including install, same-version,
   repair, update, rollback, uninstall, and CLI proof commands.
8. Only after every gate passes does the publish job create a draft GitHub Release, upload the
   exact validated inventory, verify it, and make the release public.

semantic-release is the version, changelog, commit, and tag authority. The reusable release
workflow is the only GitHub Release and binary-asset publisher.

The workflow uses the repository `GITHUB_TOKEN`. The ordinary CI and build jobs have
`contents: read`; semantic-release and the final publish job receive only the `contents: write`
permission needed for their work.

## Versions from commits

| Commit | Release effect |
| --- | --- |
| `feat:` | Minor |
| `fix:` or `perf:` | Patch |
| `feat!:` or `BREAKING CHANGE:` | Major |
| `docs:`, `ci:`, `build:`, `test:`, `refactor:`, `chore:` | No release by themselves |

No qualifying commits means no version, release commit, tag, or GitHub Release.

## Canonical version

`[workspace.package].version` in the root `Cargo.toml` is the Rust source of truth. Internal crates
inherit it. The release synchronization step updates:

- root `Cargo.toml`;
- internal path-dependency version requirements;
- workspace entries in `Cargo.lock`;
- `CHANGELOG.md`;
- `editors/vscode/package.json` and its lockfile.

The release validates the locked workspace immediately after synchronization. It does not publish
crates to crates.io or the extension to the VS Code Marketplace.

## Local validation

Run the ordinary gates:

```console
npm ci
node editors/vscode/scripts/sync-version.mjs --check
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
cargo check --workspace --locked
npm run test:release-core
npm run test:installers
git diff --check
```

Preview the next semantic-release decision with:

```console
npm run release:dry-run
```

The dry run needs full Git history and repository access. It does not create a version commit, tag,
or release. Do not run `npm run release` from a development machine.
