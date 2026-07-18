# Aster

Aster is an experimental native, general-purpose language focused on compact syntax,
memory safety, and safe concurrency. The compiler is written in stable Rust with a
hand-written lexer and parser.

Aster's pre-alpha bootstrap validates source files and can JIT-compile a resolved
application entry through Cranelift. It does not generate executables yet. The
first planned public release is `0.1.0`.

```console
cargo run -p aster-cli -- check examples/my_first_program.aster
```

Run the initial native JIT example on Windows:

```console
cargo run -p aster-cli -- run examples\jit_basics.aster --function Calculate
```

Executable reference types can be tried directly as well:

```powershell
cargo run -p aster-cli -- run examples\arrays.aster --function Run
cargo run -p aster-cli -- run examples\classes_counter.aster --function Run
cargo run -p aster-cli -- run examples\class_composition.aster --function Run
cargo run --quiet -p aster-cli -- run examples\multifile\main.aster --function Run
```

Expected output:

```text
42
```

Normal applications start at one public static parameterless `Main` returning `void` or `int`:

```console
cargo run --quiet -p aster-cli -- run examples\conventional_main.aster
```

`Main` is not an engine lifecycle. Aster does not create `Start`, `Update`, an event loop, or ECS
runtime. `--function NAME` remains available for explicit examples and compiler development.

Start with [getting started](docs/getting-started.md) and the
[language tour](docs/language-tour.md). Reference pages live in
[docs/reference](docs/reference/cli.md); compiler internals for contributors live in
[docs/compiler](docs/compiler/architecture.md). See also the
[contribution guide](CONTRIBUTING.md).

Releases are fully automatic after a Conventional Commit reaches `main`. Read the short
[release guide](docs/releasing.md) for the commit types and the path from `0.0.0` to the first
public `0.1.0` release.
