# Aster

[![CI](https://github.com/Natteens/Aster/actions/workflows/ci.yml/badge.svg)](https://github.com/Natteens/Aster/actions/workflows/ci.yml)

Aster is an experimental native language that explores how low-level programming can feel direct
without making every program look low-level. Its code is familiar and readable, while types,
representations, errors, and runtime costs stay concrete.

The compiler is written in Rust and executes programs natively through a Cranelift JIT. Aster is
not production-ready: syntax, tooling, and the runtime model may still change.

## A first program

```aster
public class Program
{
    public static int Main()
    {
        Log("Hello, Aster!");
        return 42;
    }
}
```

After cloning the repository, install the CLI from its root with stable Rust 1.85 or newer:

```console
cargo install --path crates/aster-cli --locked --force
```

The installed executable is named `aster`:

```console
aster run examples/hello.aster
aster check examples/hello.aster
aster watch examples/hello.aster
```

The first command logs a greeting and prints `42`.

## Why Aster

Aster is guided by a few practical choices:

- **Concrete types.** Generic code is specialized before HIR and MIR, so layouts and calls do not
  depend on hidden type erasure.
- **Predictable behavior.** Evaluation order, dispatch, allocation, and value-versus-reference
  semantics should be visible in the language model.
- **Explicit failure.** Expected absence and errors travel through values such as `Option<T>` and
  `Result<T, E>` instead of implicit nulls or exceptions.
- **Usable tools.** Installation, diagnostics, examples, and the CLI are part of the language
  experience, not an afterthought.

The complete principles and their practical consequences are in the
[design goals](docs/specification/00-goals.md).

## Where the project stands

Today Aster can run single-file and multifile programs with concrete primitives, arrays, classes,
structs, interfaces, enums, properties, overloads, namespaces, and monomorphized generics. Its
standard library includes focused math and text APIs together with `Option<T>` and `Result<T, E>`.

The compiler follows a typed pipeline from AST to HIR, MIR, and Cranelift. It does not yet produce
standalone executables, manage packages, provide long-lived object ownership, or offer a garbage
collector. Automatic parallelism, threads, GPU execution, and HVM integration are research topics,
not current features. Any future work in that area must preserve determinism and make its costs
understandable; Aster is not committed to reproducing Bend's architecture.

## Learn and explore

- [Getting started](docs/getting-started.md) installs the CLI and builds a first program.
- [Runnable examples](examples/README.md) provide a short learning path.
- [Language tour](docs/language-tour.md) explains the main ideas through code.
- [Documentation index](docs/README.md) separates guides, reference, compiler internals, and
  research notes.

## Development

Aster is a Cargo workspace: the compiler, syntax tree, IRs, runtime, CLI, and Cranelift backend are
separate crates. The VS Code extension lives in [`editors/vscode`](editors/vscode/README.md).

Read [CONTRIBUTING.md](CONTRIBUTING.md) before proposing language behavior or architectural changes.
The official project is maintained at [Natteens/Aster](https://github.com/Natteens/Aster).

## License

Aster is licensed under the [Apache License 2.0](LICENSE). See [NOTICE](NOTICE) for attribution.
