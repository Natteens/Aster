# Application entry

Use an application entry when a file represents a program you want to start, rather than a
library you only want to check. ASTER starts an ordinary public static method named `Main`:

```aster
using aster.io;

public class Program
{
    public static void Main()
    {
        WriteLine("Hello from ASTER!");
    }
}
```

`Main` is not an engine lifecycle hook. It runs once because the CLI asks the JIT to invoke it.
It does not create a game loop, world, task, thread, or ECS runtime.

## The convention

Without a manifest, `aster run FILE` looks in the root namespace for exactly one eligible method.
The method must be `public`, `static`, have no parameters, and return `void` or `int`. Its
declaring class must also be public. `int Main()` executes and produces an integer, which the CLI
prints. `void Main()` executes without producing a value: only the logs the program writes appear,
and the process ends successfully. Any other return type is rejected with a diagnostic.

`Main` is `static`, so it has no current instance. Instance methods access the members of the
current instance; to use instance members from `Main`, create an instance explicitly with `new`
and call through it.

```aster
public class Program
{
    public static int Main()
    {
        return 42;
    }
}
```

Namespaces reached through `using` are compiled and callable, but their `Main` methods are not conventional entry
candidates. This keeps the root file in control of the application.

## Optional `Aster.toml`

A project can select its entry explicitly with an `Aster.toml` in the project root:

```toml
schema = 1

[application]
entry = "app.Program.Main"
```

The dotted value is `namespace.Class.Main`. The selected class and method must be public, and the
method follows the same static, parameter, and return rules as the convention. When the root source
is below the manifest, the manifest directory is also the project root. For example,
`app/main.aster` belongs to namespace `app` and may use `using app.math;` for sources under
`app/math/`.

Schema `1` supports only the top-level `schema` field and the `[application]` table, which in turn
supports only `entry`. Unknown fields and unsupported schema numbers are rejected with controlled
manifest diagnostics instead of being ignored. Existing pre-schema manifests remain valid: omitting
`schema` is treated as schema `1`, while `aster new` writes the explicit version for new projects.

The manifest is intentionally small. It does not describe packages, dependencies, build profiles,
an engine, or remote downloads.

## Explicit development entry

`--function NAME` remains useful for targeted compiler tests and debugging:

```powershell
aster run examples\expressions.aster --function Run
```

An explicit function takes precedence over both `Main` and `Aster.toml`, even if the manifest is
invalid. The named function must still be a public, parameterless namespace-level function in the
root namespace. `check`, `dump-hir`, and `dump-mir` do not require an entry when no manifest exists. When a
manifest is present, those commands validate its schema, syntax, and configured target without running it.
