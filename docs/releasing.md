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
| `feat!:` or `BREAKING CHANGE:` | Minor while ASTER is `0.x` |
| `docs:`, `ci:`, `build:`, `test:`, `refactor:`, `chore:` | No release by themselves |

No qualifying commits means no version, release commit, tag, or GitHub Release.

ASTER's pre-1.0 compatibility policy allows deliberate incompatible changes only in minor releases,
so the commit analyzer temporarily maps an accurate Conventional Commit breaking marker to
`minor`. Before the deliberate 1.0 transition, that override must be removed so breaking commits
again have normal major-version semantics. Do not remove the override merely to force a version.
See the [compatibility policy](reference/compatibility.md) for the source, project, CLI, standard
library, and runtime stability bands.

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

## Git Notes and interrupted releases

semantic-release 25 records the channel of every tag in a per-tag Git Note ref,
`refs/notes/semantic-release-v<version>`, and pushes it last, after the release commit and the
tag. CI runs it through `scripts/run-semantic-release.mjs`, which tolerates exactly one failure:
a rejected push of that one notes ref for the version that was just released. It does so only
after the same checks the workflow runs next (`semantic-release-result.mjs`) prove that the
release commit, the tag, and `main` agree, and it retries the push once. A tag without a note is
read as the default channel, which is what a `main` release records, so later version
calculation is unaffected. Any other error, including any other failed push, a second error, or
a release that cannot be verified, fails the job unchanged. This is only valid while the release
configuration has no publish, success, or fail steps for the tolerated failure to skip; a test
enforces that.

If a release commit and tag were created but the public GitHub Release was not, recover it with
the **Release recovery** workflow (Actions, `workflow_dispatch`, from `main`) and the existing tag,
for example `v0.67.0`. Recovery never calculates a version or creates a tag. It requires that:

- the tag is a `vMAJOR.MINOR.PATCH` release tag and the local and remote tag objects agree;
- `Cargo.toml` at the tag has exactly that version, and the tagged commit is the canonical
  `chore(release): <version> [skip ci]` commit in the `origin/main` history;
- the tag is the newest release tag, so another release cannot be substituted or re-marked as
  latest;
- no GitHub Release for the tag exists, so a published release is never overwritten.

It then calls the same reusable release workflow as the automatic path with the exact tag commit,
so the Linux and Windows builds, asset assembly, installer verification, and draft-then-publish
steps all run before anything becomes public. Running the release workflow directly with
`workflow_dispatch` still validates only and publishes nothing. A leftover draft release for the
tag (not visible to the read-only check) makes the publish step fail closed; delete the draft
first.
