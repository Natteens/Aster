# Command line

The compiler ships one binary, `aster`. During development, replace `aster` with
`cargo run -p aster-cli --`.

| Command | What it does |
| --- | --- |
| `aster check FILE` | Validates a project without running it; no manifest means no required `Main`. |
| `aster run FILE [--function NAME]` | Runs the app entry, or an explicitly named function. |
| `aster watch FILE [--function NAME]` | Re-runs the selected entry after a save. |
| `aster dump-hir FILE` | Prints typed HIR without requiring an entry. |
| `aster dump-mir FILE` | Prints control-flow MIR without requiring an entry. |

## Running an application

```powershell
aster run examples\conventional_main.aster
```

Without `--function`, `run` uses the nearest optional `Aster.toml` entry or finds one conventional
public static `Main` in the root namespace. `Main` takes no parameters and returns `void` or `int`.
See [application entry](application-entry.md) for the complete rules.

## Running a function explicitly

```powershell
aster run examples\jit_basics.aster --function Calculate
```

The explicit function must be a public, parameterless namespace-level function in the root namespace. Its
supported result is printed, and `void` prints `function completed successfully (void)`. This mode
takes precedence over `Aster.toml` and conventional `Main`; it is useful for examples and compiler
development.

## Watching a project

```powershell
aster watch examples\conventional_main.aster
```

`watch` keeps the terminal open and recompiles after changes to the root, a loaded project
namespace file, or
`Aster.toml`. Compile errors do not stop it. Every rebuild starts a fresh JIT session and execution
context, so running state is not retained. Press `Ctrl+C` to stop. Add `--function NAME` to watch an
explicit test function instead.

Unsupported runtime constructs are rejected with a controlled error rather than executed
incorrectly. See [types](types.md), [namespaces](namespaces.md), and
[application entry](application-entry.md) for the current boundaries.
