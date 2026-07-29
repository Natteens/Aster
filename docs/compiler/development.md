# Compiler development

The repository pins the stable Rust toolchain with `rustfmt` and `clippy`.

```console
rustup show active-toolchain
cargo build --workspace
```

Run the workspace gates before opening a pull request:

```console
node editors/vscode/scripts/sync-version.mjs --check
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
cargo check --workspace --locked
git diff --check
```

On Windows, project validation uses the tested GNU toolchain:

```powershell
cargo +stable-gnu test --workspace --all-targets
```

Run the local CLI without installing it:

```console
cargo run -p aster-cli -- check examples/my_first_program.aster
cargo run -p aster-cli -- run examples/hello.aster
```

Release and installer scripts require the Node version selected by CI and dependencies from
`npm ci`. Use `npm run test:release-core` for pure packaging/release tests and
`npm run test:installers` for installer lifecycles. Never run `npm run release` locally.

The [architecture guide](architecture.md) maps the crates and compiler stages. User-facing
documentation belongs in guides and reference pages; implementation details belong under
`docs/compiler`.
