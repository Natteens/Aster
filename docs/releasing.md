# Releasing Aster

Aster releases automatically from `main`. There is no release branch and no Release PR.

## Normal flow

1. Develop on a feature branch.
2. Use [Conventional Commits](https://www.conventionalcommits.org/), for example `feat: add enum cases`.
3. Make CI green and merge the change into `main`.
4. The Release workflow validates Rust, then runs semantic-release.
5. semantic-release either finds no releasable commits or creates the version, `v<version>` tag,
   GitHub Release, changelog entry, and one synchronization commit on `main`.

The release workflow only runs on `main`; feature branches and pull requests cannot publish. It
passes the repository `GITHUB_TOKEN` explicitly to semantic-release and grants the job
`contents: write`, `issues: write`, and `pull-requests: write`. Branch protection must allow GitHub
Actions to push the generated release commit, or exempt the `github-actions[bot]` actor from that
rule.

## Commit types and versions

semantic-release uses Conventional Commits to decide whether a release exists and its version.

| Commit | Effect |
| --- | --- |
| `feat:` | minor release, such as `0.1.0` to `0.2.0` |
| `fix:` or `perf:` | patch release, such as `0.1.0` to `0.1.1` |
| `feat!:` or a `BREAKING CHANGE:` footer | major release, such as `0.9.0` to `1.0.0` |
| `docs:`, `ci:`, `build:`, `test:`, `refactor:`, `chore:` | recorded in Git history, but do not create a release by themselves |

No qualifying commits means no version, tag, GitHub Release, or changelog commit is created.
During the experimental phase, `0.0.0` can remain unchanged until the first releasable commit on
`main`. The first `feat:` merged after that point produces `0.1.0`.

## One public version

`[workspace.package].version` in the root `Cargo.toml` is the Rust source of truth. Every internal
crate inherits it through `version.workspace = true`. The release `prepare` step receives the version
calculated by semantic-release and updates:

- root `Cargo.toml`;
- internal Aster path-dependency version requirements in `crates/*/Cargo.toml`;
- `CHANGELOG.md`;
- `editors/vscode/package.json` and its root entries in `package-lock.json`.

`Cargo.lock` is included in the release commit when Cargo changes it. It normally does not contain a
workspace package version, so a version-only release often leaves it untouched. The release workflow
does not publish any crate to crates.io.

The VS Code extension package reads the synchronized `package.json`; a later manual
`npm run package` therefore produces `aster-language-<version>.vsix`. This workflow deliberately
does not build or attach a VSIX.

## Checking configuration locally

Run the ordinary checks first:

```powershell
npm ci
node editors/vscode/scripts/sync-version.mjs --check
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
git diff --check
```

To preview semantic-release without creating a tag, release, or commit, use:

```powershell
npm run release:dry-run
```

The dry run needs the same full Git history and repository access as the workflow to calculate an
accurate next version. Do not run `npm run release` from a development machine; the GitHub Actions
workflow is the only publisher.
