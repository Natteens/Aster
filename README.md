# ASTER

[![CI](https://github.com/Natteens/Aster/actions/workflows/ci.yml/badge.svg)](https://github.com/Natteens/Aster/actions/workflows/ci.yml)
[![Latest release](https://img.shields.io/github/v/release/Natteens/Aster?display_name=tag&sort=semver)](https://github.com/Natteens/Aster/releases/latest)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

ASTER is an experimental native language with static nominal types, region-based memory, and a
Cranelift JIT. The compiler and runtime are written in Rust.

[Getting started](docs/getting-started.md) · [Documentation](docs/README.md) ·
[Examples](examples/README.md) · [Contributing](CONTRIBUTING.md)

> [!NOTE]
> ASTER is not 1.0 yet. Syntax, APIs, and runtime details may change while the language develops.

## Install

Official installers are available for Windows x64 and Linux x64.

**Windows PowerShell**

```powershell
irm https://github.com/Natteens/Aster/releases/latest/download/install.ps1 | iex
```

**Linux**

```sh
curl -fsSL https://github.com/Natteens/Aster/releases/latest/download/install.sh | sh
```

Open a new terminal, then check the installation:

```console
aster --version
aster doctor
```

The installers verify the release checksum before installing. They do not require Rust, Cargo, or a
clone of this repository. See [Getting started](docs/getting-started.md) for the complete flow.

## Create your first project

```console
aster new HelloAster
cd HelloAster
aster check
aster run
```

The generated program prints `Hello from ASTER!` and returns `0`.

## A small ASTER program

```aster
namespace app;

using aster.io;

public class Program
{
    public static int Main()
    {
        WriteLine("Hello from ASTER!");
        return 0;
    }
}
```

## What works today

- Static, strong, nominal types with non-null references by default.
- Classes, structs, interfaces, enums, properties, overloads, and monomorphized generics.
- Arrays, `List<T>`, `Dictionary<K, V>`, `Option<T>`, and `Result<T, E>`.
- Immutable UTF-8 strings and `char` values that represent Unicode scalars.
- Region-based memory backed by temporary and persistent arenas.
- Multifile projects, filesystem and terminal APIs, and a Cranelift JIT.
- Restricted worker-based task and parallel operations with explicit transfer boundaries.
- A CLI for project creation, diagnostics, checking, running, watching, and HIR/MIR inspection.

The [language tour](docs/language-tour.md) introduces these features through code. The
[compiler documentation](docs/README.md#compiler-internals) covers the pipeline and memory model.

## Current limits

ASTER currently distributes native toolchains for Windows x64 and Linux x64. macOS and ARM are not
official targets. There is no package manager or standalone AOT compiler yet, and the worker model
does not provide general shared-memory threads.

See the [roadmap](docs/roadmap.md) for planned work and research boundaries.

## Documentation

- [Getting started](docs/getting-started.md)
- [Language tour](docs/language-tour.md)
- [CLI reference](docs/reference/cli.md)
- [Language and standard library reference](docs/README.md#language-reference)
- [Compiler internals](docs/README.md#compiler-internals)
- [Runnable examples](examples/README.md)

## Contributing

Focused fixes, tests, and documentation improvements are welcome. Discuss new language behavior,
runtime capabilities, or substantial architecture changes before implementing them. Start with
[CONTRIBUTING.md](CONTRIBUTING.md).

## License

ASTER is licensed under the [Apache License 2.0](LICENSE). See [NOTICE](NOTICE) for attribution.
