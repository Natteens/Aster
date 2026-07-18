# Command line

Install the `aster` binary from the repository root:

```console
cargo install --path crates/aster-cli --locked --force
```

The standard library is embedded in the executable, so the CLI can run source files from any
working directory.

| Command | What it does |
| --- | --- |
| `aster check FILE` | Validates a project without running it; no manifest means no required `Main`. |
| `aster run FILE [--function NAME]` | Runs the app entry, or an explicitly named function. |
| `aster watch FILE [--function NAME]` | Re-runs the selected entry after a save. |
| `aster dump-hir FILE` | Prints typed HIR without requiring an entry. |
| `aster dump-mir FILE` | Prints control-flow MIR without requiring an entry. |

## Running an application

```console
aster run examples/hello.aster
```

Without `--function`, `run` uses the nearest optional `Aster.toml` entry or finds one conventional
public static `Main` in the root namespace. `Main` takes no parameters and returns `void` or `int`.
See [application entry](application-entry.md) for the complete rules.

## Running a function explicitly

```powershell
aster run examples\jit_basics.aster --function Calculate
```

The explicit function must be a public, parameterless namespace-level function in the root namespace. Its
supported result is printed; a `void` function prints nothing beyond the program's logs. This mode
takes precedence over `Aster.toml` and conventional `Main`; it is intended for targeted compiler
development, tests, and debugging.

## Watching a project

```console
aster watch examples/hello.aster
```

`watch` keeps the terminal open and recompiles after changes to the root, a loaded project
namespace file, or
`Aster.toml`. Compile errors do not stop it. Every rebuild starts a fresh JIT session and execution
context, so running state is not retained. Press `Ctrl+C` to stop. Add `--function NAME` to watch an
explicit test function instead.

Unsupported runtime constructs are rejected with a controlled error rather than executed
incorrectly. See [types](types.md), [namespaces](namespaces.md), and
[application entry](application-entry.md) for the current boundaries.

## Compiler development

Contributors can run the binary from the checkout without installing it by replacing `aster` with
`cargo run -p aster-cli --`. See [compiler development](../compiler/development.md) for the full
workspace validation workflow.
