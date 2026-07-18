# Development

Install the stable Rust toolchain and required components:

```console
rustup toolchain install stable --component rustfmt clippy
```

Build and validate the workspace:

```console
cargo build --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
cargo run -p aster-cli -- check examples/my_first_program.aster
```

CI executes formatting, linting, and tests on Windows and Linux. See `CONTRIBUTING.md`
for the commit convention.
