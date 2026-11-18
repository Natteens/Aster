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
cargo clippy --workspace --all-targets --features aster-codegen-cranelift/aarm-telemetry -- -D warnings
cargo test --workspace --all-targets
cargo check --workspace --locked
git diff --check
```

The default workspace tests use the H8 class header. Clippy enables telemetry explicitly.
Do not enable every Cargo feature at once: the experimental H12 and H16 layouts are mutually
exclusive. CI checks the C4, C5, C6 and C6-H16 memory experiments independently on Linux.
For a change to the experimental memory implementation, select one configuration at a time,
for example:

```console
cargo clippy -p aster-codegen-cranelift -p aster-runtime -p aster-exec --all-targets --features aster-codegen-cranelift/mem-exp-c6,aster-codegen-cranelift/aarm-telemetry -- -D warnings
cargo test -p aster-codegen-cranelift -p aster-runtime -p aster-exec --all-targets --features aster-codegen-cranelift/mem-exp-c6
```

Repeat with `mem-exp-c4`, `mem-exp-c5` or `mem-exp-c6-h16` as relevant. These are research
configurations, not production features.

On Windows, project validation uses the tested GNU toolchain:

```powershell
cargo +stable-gnu test --workspace --all-targets
```

## Build directory size

Every test and example executable statically links the compiler and the JIT, so a full
`target/debug` for one configuration was 13.4 GB before the checked-in `.cargo/config.toml` and is about
3 GB with it (test executables without DWARF, no incremental cache; CI uses the same settings). Cargo names
each executable `<name>-<hash>` and never deletes the old ones, and the hash changes with
Cargo feature selections, `RUSTFLAGS`, the debug-info and incremental settings, and the toolchain
(`+stable-gnu` versus the pinned `stable`). Each switch leaves another full set behind, which is how
`target/debug` reached 66 GB.

- Use the commands above as written. Run the ordinary `cargo test` without experimental features.
  Keep experiments separate, and avoid mixing toolchains or exporting `RUSTFLAGS` or `CARGO_*`
  profile variables locally.
- Check the size with `du -sh target/debug` or Explorer. If it has grown well past one set, close
  editors that run `cargo check` (rust-analyzer) and run the maintenance script with no build in
  progress:

```powershell
scripts\prune-build-artifacts.ps1          # dry run: counts and sizes
scripts\prune-build-artifacts.ps1 -Apply   # delete stale executables and the incremental cache
```

The script first builds, or confirms, the executables for
`cargo test --workspace --all-targets` with the current settings, then deletes only the other
hashed executables in `target/debug/deps` and `target/debug/examples`, plus `target/debug/incremental`.
It never touches libraries, fingerprints, the `release` profile or `~/.cargo`, and it refuses to run
while `cargo` or `rustc` is running (checked at the start and again right before the first deletion).
It deletes nothing unless Cargo reported a successful build with a non-empty list of executables that
all exist under the target directory, and a relative `CARGO_TARGET_DIR` is resolved against the
repository root. Deleted files are rebuilt by Cargo on demand. Its failure paths are covered by
`powershell -NoProfile -File scripts\prune-build-artifacts.tests.ps1`, which only uses a temporary
directory. `cargo clean`
remains available but discards everything and costs a full rebuild.

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
