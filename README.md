# ASTER

<p align="center">
  <a href="https://github.com/Natteens/Aster/actions/workflows/ci.yml">
    <img alt="CI status" src="https://github.com/Natteens/Aster/actions/workflows/ci.yml/badge.svg">
  </a>
  <a href="https://github.com/Natteens/Aster/releases/latest">
    <img alt="Latest release" src="https://img.shields.io/github/v/release/Natteens/Aster?display_name=tag&sort=semver&style=flat-square">
  </a>
  <a href="./LICENSE">
    <img alt="Apache 2.0 license" src="https://img.shields.io/github/license/Natteens/Aster?style=flat-square">
  </a>
  <a href="https://github.com/Natteens/Aster/releases/latest">
    <img alt="Windows x64 and Linux x64" src="https://img.shields.io/badge/platform-Windows%20x64%20%7C%20Linux%20x64-informational?style=flat-square">
  </a>
</p>

ASTER is an experimental native language with static nominal types, region-based memory, and a
Cranelift JIT. The compiler and runtime are written in Rust.

[Getting started](docs/getting-started.md) · [Documentation](docs/README.md) ·
[Examples](examples/README.md) · [Contributing](CONTRIBUTING.md)

> [!NOTE]
> ASTER is not 1.0 yet. Syntax, APIs, and runtime details may change while the language develops.

## Install

Official installers are available for Windows x64 and Linux x64.

**🪟 Windows PowerShell**

```powershell
irm https://github.com/Natteens/Aster/releases/latest/download/install.ps1 | iex
```

**🐧 Linux**

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

- 🧱 **Type system:** static, strong, nominal types with non-null references by default.
- 🧩 **Language model:** classes, structs, interfaces, enums, properties, overloads, and monomorphized generics.
- 📦 **Collections:** arrays, `List<T>`, `Dictionary<K, V>`, `Option<T>`, and `Result<T, E>`.
- 🔤 **Text:** immutable UTF-8 strings and `char` values that represent Unicode scalars.
- 🧠 **Memory:** region-based allocation backed by temporary and persistent arenas.
- 🗂️ **Projects and I/O:** multifile projects plus filesystem and terminal APIs.
- ⚙️ **Concurrency:** restricted worker-based task and parallel operations with explicit transfer boundaries.
- 🛠️ **Tooling:** project creation, diagnostics, checking, running, watching, and HIR/MIR inspection.

The [language tour](docs/language-tour.md) introduces these features through code. The
[compiler documentation](docs/README.md#compiler-internals) covers the pipeline and memory model.

## Current limits

ASTER currently distributes native toolchains for Windows x64 and Linux x64. macOS and ARM are not
official targets. There is no package manager or standalone AOT compiler yet, and the worker model
does not provide general shared-memory threads.

See the [roadmap](docs/roadmap.md) for planned work and research boundaries.

## Documentation

- 🚀 [Getting started](docs/getting-started.md)
- 🧭 [Language tour](docs/language-tour.md)
- 🛠️ [CLI reference](docs/reference/cli.md)
- 📚 [Language and standard library reference](docs/README.md#language-reference)
- ⚙️ [Compiler internals](docs/README.md#compiler-internals)
- 🧪 [Runnable examples](examples/README.md)

## Contributing

Focused fixes, tests, and documentation improvements are welcome. Discuss new language behavior,
runtime capabilities, or substantial architecture changes before implementing them. Start with
[CONTRIBUTING.md](CONTRIBUTING.md).

## License

ASTER is licensed under the [Apache License 2.0](LICENSE). See [NOTICE](NOTICE) for attribution.
