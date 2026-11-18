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
The method must be `public`, `static`, return `void` or `int`, and take either no parameters or
exactly one `string[] args`. Its declaring class must also be public. Any other parameter list or
return type is rejected with a diagnostic, and two eligible `Main` overloads in the candidate set
are ambiguous.

```aster
public class Program
{
    public static int Main(string[] args)
    {
        return args.Length;
    }
}
```

### Arguments

`Main(string[] args)` receives the program arguments. `args` never contains the program name, and
`args.Length` is `0` when none are given. Each argument is passed unchanged, except that invalid
Unicode (an unpaired UTF-16 surrogate on Windows, an invalid UTF-8 sequence on POSIX) is replaced
with U+FFFD. A `Main()` that takes no parameters simply ignores any arguments.

`aster run` takes the program arguments after a `--` separator:

```console
aster run app/main.aster -- first "second argument" --not-an-aster-flag
```

Everything after the first `--` belongs to the program, even words that look like `aster run`
options. Options for `aster run` itself (`--function`, `--memory-stats`) go before the separator.

### Exit status

The `int` returned by `Main` is the process exit status. `aster run` passes it to the operating
system unchanged: Windows observes the full 32-bit value, POSIX observes the low 8 bits (`256`
looks like `0`, `-1` like `255`). It is not printed. `void Main()` ends the process with status `0`.
A compile error, an entry-selection error or a runtime failure (an unhandled runtime error such
as division by zero) ends it with status `1` and a diagnostic on standard error, and a command-line
usage error ends it with `2`; see [the CLI reference](cli.md#output-and-exit-codes). A program
whose own exit status is `1` therefore cannot be told apart from a runtime failure by the status
alone; its own diagnostics belong on standard error ([`WriteError`](io.md#standard-error)).

`--function NAME` is a development option and keeps its own behavior: a non-`void` result is
printed and the process exits `0`.

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

`aster run` on this program prints nothing and exits with status `42`.

Namespaces reached through `using` are compiled and callable, but their `Main` methods are not conventional entry
candidates. This keeps the root file in control of the application.

## `Aster.toml`

A project can select its entry explicitly with an `Aster.toml` in the project root:

```toml
[package]
name = "app"

[application]
entry = "app.Program.Main"
```

The dotted value is `namespace.Class.Main`. The selected class and method must be public, and the
method follows the same static, parameter (`Main()` or `Main(string[] args)`), and return rules as
the convention. When the root source
is below the manifest, the manifest directory is also the project root. For example,
`app/main.aster` belongs to namespace `app` and may use `using app.math;` for sources under
`app/math/`.

Every manifest declares `[package] name`. `[application]` is optional; omitting it makes the package
a reusable source package with no executable entry. Only the root package supplies the application
entry, and a dependency that declares its own `[application]` does not compete for it. See
[packages and dependencies](packages.md).

`aster test` does not select an application entry: it invokes discovered `test void` functions
instead. A library package can therefore run its own tests without `[application]` or `Main`; see
[testing](testing.md).

ASTER does not put a schema, edition, or format-version field in `Aster.toml`. A `schema` field is
rejected with a controlled migration diagnostic rather than activating alternate semantics.

The manifest stays small. It does not describe build profiles, an engine, remote downloads, or any
dependency source other than a local path.

## Explicit development entry

`--function NAME` remains useful for targeted compiler tests and debugging:

```powershell
aster run examples\expressions.aster --function Run
```

An explicit function takes precedence over both `Main` and `Aster.toml`, even if the manifest is
invalid. The named function must still be a public, parameterless namespace-level function in the
root namespace. `check`, `dump-hir`, and `dump-mir` do not require an entry when no manifest exists. When a
manifest is present, those commands validate its syntax and configured target without running it.
