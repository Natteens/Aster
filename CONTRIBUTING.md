# Contributing to ASTER

Focused bug fixes, tests, examples, and documentation improvements are welcome. Discuss new
language behavior, runtime capabilities, public APIs, or substantial architecture changes in an
issue before implementing them. This keeps proposals tied to a concrete problem and prevents
competing designs from arriving in the same pull request.

## Compiler setup

ASTER is a Rust workspace. Install the stable toolchain with `rustfmt` and `clippy`, then follow the
[compiler development guide](docs/compiler/development.md) for build commands and repository
structure.

Before opening a pull request, run:

```console
node editors/vscode/scripts/sync-version.mjs --check
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
cargo check --workspace --locked
git diff --check
```

Use the installed `aster` binary when validating user-facing guides. For local CLI changes, use
`cargo run -p aster-cli -- <COMMAND>`.

## Pull requests

Keep each pull request focused. Explain what changed, why it is needed, what remains out of scope,
and which commands you ran. Language or architecture changes should link to their prior discussion.
Update the canonical guide or reference page when behavior visible to users changes.

Commits follow [Conventional Commits](https://www.conventionalcommits.org/):
`type(scope): description`. Common types include `feat`, `fix`, `docs`, `test`, `refactor`, `perf`,
`build`, `ci`, and `chore`. Breaking changes use `!` or a `BREAKING CHANGE:` footer.

Do not edit released version numbers, changelog entries, or tags by hand. The automatic release
flow calculates and synchronizes versions after validated changes reach `main`; see
[Releasing ASTER](docs/releasing.md).
