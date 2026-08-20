# Command-line interface

The `aster` CLI creates projects, checks and runs source, watches files, diagnoses installations,
and prints the compiler's typed intermediate representations.

| Command | Purpose |
| --- | --- |
| `aster new <NAME>` | Create a project in a new child directory. |
| `aster doctor` | Diagnose the local toolchain and current project without modifying them. |
| `aster check [FILE]` | Validate a project or source file without executing it. |
| `aster run [FILE]` | Compile and run an application. |
| `aster test` | Compile and run root-package tests. |
| `aster watch <FILE>` | Recompile and rerun when a loaded project file changes. |
| `aster dump-hir [FILE]` | Print typed HIR after successful validation. |
| `aster dump-mir [FILE]` | Print control-flow MIR after successful validation. |
| `aster --help` | Print the command summary. |
| `aster --version` | Print the CLI version. |

## Projects and source files

Without `[FILE]`, `check`, `run`, `dump-hir`, and `dump-mir` find the nearest `Aster.toml` in the
current directory or one of its ancestors, then start from `app/main.aster` below that manifest.
This means the project commands keep working from nested directories inside the project.

```console
aster check
aster run
```

`aster test` is a package command with no file mode. It finds the nearest manifest and discovers
only that package's conventional `tests/` sources. See [testing](testing.md).

Passing one source file selects it explicitly:

```console
aster check examples/hello.aster
aster run examples/hello.aster
```

For an explicit source, the nearest `Aster.toml` in that source file's ancestors establishes the
project root and application entry. Without a manifest, `run` finds one conventional public static
`Main` in the root namespace. `Main` is parameterless and returns `void` or `int`. See
[application entry](application-entry.md) for the complete manifest and entry rules.

## Development options

`run` can select a public, parameterless namespace-level function:

```console
aster run examples/expressions.aster --function Run
```

The selected function takes precedence over the manifest and conventional `Main`, including when
the relevant manifest is invalid. This option is intended for compiler development and focused
examples. `--memory-stats` prints execution-context allocation metrics after a successful run.

`watch` accepts the same `--function <NAME>` selection but always requires a source file:

```console
aster watch examples/watch_demo.aster
```

Every rebuild uses a fresh JIT session and execution context. A failed rebuild writes diagnostics
to stderr and the watcher keeps observing for a later valid edit. Press `Ctrl+C` to stop.

## Output and exit codes

ASTER uses three process exit codes:

| Code | Meaning |
| --- | --- |
| `0` | The command completed successfully. |
| `1` | Valid command arguments led to an operational, compilation, or runtime failure. |
| `2` | The command or its arguments were invalid. |

Normal output, including program output and HIR/MIR dumps, goes to stdout. Usage, filesystem,
compiler, runtime, manifest, and standard-library errors go to stderr. A failed dump does not emit
partial IR.

## Compiler checkout

Contributors can run the local binary by replacing `aster` with
`cargo run -p aster-cli --`. See [compiler development](../compiler/development.md) for the Rust
toolchain and workspace gates. Cargo is not required for a normal installed toolchain.
