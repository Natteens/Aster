# Contributing to Aster

Install stable Rust with the `rustfmt` and `clippy` components. Before submitting a
change, run:

```console
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
```

Commits must follow [Conventional Commits](https://www.conventionalcommits.org/):
`type(scope): description`. Common types are `feat`, `fix`, `docs`, `test`,
`refactor`, `perf`, `build`, `ci`, and `chore`. Breaking changes use `!` or a
`BREAKING CHANGE:` footer.

Do not hand-edit released version numbers or tags. The automatic release process is described in
[`docs/releasing.md`](docs/releasing.md). Only pushes to `main` may publish a release.
