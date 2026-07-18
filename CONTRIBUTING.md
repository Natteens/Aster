# Contributing to Aster

Aster is experimental, but changes should still have a clear scope. Discuss new language behavior,
runtime capabilities, or substantial architectural changes before implementing them. Small fixes,
tests, and documentation improvements can go directly to a focused pull request.

Use stable Rust with the `rustfmt` and `clippy` components. Before submitting a change, run:

```console
node editors/vscode/scripts/sync-version.mjs --check
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
git diff --check
```

These Cargo commands validate the compiler checkout. User-facing guides use the installed `aster`
binary; contributors may run `cargo run -p aster-cli -- <COMMAND>` when testing local CLI changes.

Commits must follow [Conventional Commits](https://www.conventionalcommits.org/):
`type(scope): description`. Common types are `feat`, `fix`, `docs`, `test`, `refactor`, `perf`,
`build`, `ci`, and `chore`. Breaking changes use `!` or a `BREAKING CHANGE:` footer.

Do not hand-edit released version numbers or tags. The automated process is documented in the
[release guide](docs/releasing.md); only pushes to `main` may publish a release.
