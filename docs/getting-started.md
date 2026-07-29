# Getting started

The official ASTER installers download a release archive, verify its SHA-256 checksum, and install
the CLI and standard library together. You do not need Rust, Cargo, or this repository to write
ASTER programs.

## Install ASTER

ASTER currently supports Windows x64 and Linux x64.

### Windows x64

Run in PowerShell:

```powershell
irm https://github.com/Natteens/Aster/releases/latest/download/install.ps1 | iex
```

### Linux x64

Run in a POSIX shell:

```sh
curl -fsSL https://github.com/Natteens/Aster/releases/latest/download/install.sh | sh
```

Open a new terminal after installation, then verify the toolchain:

```console
aster --version
aster doctor
```

`aster doctor` checks the executable, platform, standard library, managed installation, current
`PATH`, a real compilation probe, and the current project when one is present.

The installers can also repair a damaged managed installation or update an older one. The
[release page](https://github.com/Natteens/Aster/releases/latest) contains the archives, checksums,
and installer scripts used by these commands.

## Create a project

Create the initial project and enter it:

```console
aster new HelloAster
cd HelloAster
```

The command writes this deterministic layout:

```text
HelloAster/
├── Aster.toml
└── app/
    └── main.aster
```

`app/main.aster` contains:

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

Check and run it from the project directory:

```console
aster check
aster run
```

`check` validates the manifest, project sources, standard library, HIR, MIR, and executable
boundary without running `Main`. `run` performs the same validation, executes the program, and
prints its integer result after the program output.

## Work with the project

Use the inspection commands when you need to see the compiler's typed representations:

```console
aster dump-hir
aster dump-mir
```

Watch mode currently requires an explicit source file:

```console
aster watch app/main.aster
```

Each successful rebuild starts a fresh JIT execution. Compile errors are reported without stopping
the watcher.

## Continue learning

- Follow the [runnable examples](../examples/README.md).
- Read the [language tour](language-tour.md).
- Use the [CLI reference](reference/cli.md) for every command and exit-code contract.
- Browse the [language reference](README.md#language-reference).

If you want to change the compiler itself, use the separate
[compiler development guide](compiler/development.md).
