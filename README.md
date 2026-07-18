# Aster

[![CI](https://github.com/Natteens/Aster/actions/workflows/ci.yml/badge.svg)](https://github.com/Natteens/Aster/actions/workflows/ci.yml)

Aster is an experimental, general-purpose native programming language. Its compiler is written in
Rust and combines a familiar C#-inspired syntax with a pipeline built specifically for the
language.

The project explores how concrete types, monomorphized generics, and explicit absence and error
values can fit into a small native language without hiding the compiler architecture.

> Aster is not production-ready. Its syntax, runtime model, and tooling may change. Automatic
> parallelism is a future research direction, not a feature of the current compiler.

## A first program

```aster
public class Program
{
    public static int Main()
    {
        return 42;
    }
}
```

With [stable Rust](https://rustup.rs) installed (Rust 1.85 or newer), install the CLI from the
repository root:

```console
cargo install --path crates/aster-cli --locked --force
```

The installed executable is named `aster`. Run the matching example with:

```console
aster run examples/hello.aster
```

The compiler JIT-compiles the program with Cranelift and prints `42`.

## What works today

- Concrete primitive and user-defined types, including classes, structs, interfaces, enums,
  properties, arrays, and immutable strings.
- Functions, methods, constructors, control flow, namespaces, `using` declarations, and multifile
  projects.
- Generic functions and types specialized through monomorphization before HIR and MIR.
- `Option<T>`, `Result<T, E>`, exhaustive enum `switch`, and postfix `?` propagation.
- A small source-based standard library with `aster.core`, `aster.math`, and `aster.text`.
- CLI commands for checking, running, watching, and inspecting typed HIR and control-flow MIR.
- Native in-memory execution through the Cranelift JIT.

Aster does not currently produce standalone executables or object files. It also has no package
manager, finalized long-lived memory model, garbage collector, or AOT backend.

## Design direction

- Keep the source language familiar while making types and effects explicit.
- Carry concrete types through the compiler instead of relying on runtime generic erasure.
- Keep AST, HIR, MIR, runtime boundaries, and backend responsibilities visible and testable.
- Add language behavior only as complete vertical slices with diagnostics, lowering, execution,
  tests, and documentation.

Concurrency, automatic parallelism, GPU support, and optional engine-oriented libraries remain
research topics. They are not implied by the current language or runtime.

## Toolchain

```text
source -> lexer/parser -> AST -> linking/monomorphization/semantics -> HIR -> MIR -> Cranelift JIT
```

Aster remains an intentional Cargo workspace. The syntax, compiler, IRs, runtime, shared type rules,
CLI, and Cranelift backend live in separate crates. The declarative VS Code extension remains in
[`editors/vscode`](editors/vscode/README.md).

## Documentation

The [documentation index](docs/README.md) is the main map for guides, language reference,
standard-library APIs, compiler internals, development, releases, and roadmaps.

Good starting points:

- [Getting started](docs/getting-started.md)
- [Examples](examples/README.md)
- [Language tour](docs/language-tour.md)
- [CLI reference](docs/reference/cli.md)
- [Compiler architecture](docs/compiler/architecture.md)

## Contributing

Read [CONTRIBUTING.md](CONTRIBUTING.md) before proposing language behavior or architectural changes.
It documents the required checks and Conventional Commit policy.

## License

Aster is available under either the [MIT License](LICENSE-MIT) or the
[Apache License 2.0](LICENSE-APACHE), at your option.
