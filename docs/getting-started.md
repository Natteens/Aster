# Getting started

Aster is an early programming language with a clear C#-inspired syntax. Today it checks `.aster`
files, compiles them in memory, and runs them through a native JIT. There is no standalone
executable output or game engine attached.

You need [Rust](https://rustup.rs) installed; everything else is in this repository.

## Run your first app

From the repository root:

```powershell
cargo run --quiet -p aster-cli -- run examples\conventional_main.aster
```

This finds `public static int Main()`, compiles it, and prints `42`. The first compiler build can
take a while; later runs are faster.

## Write your own file

Create `hello.aster`:

```aster
public class Program
{
    public static void Main()
    {
        Log("Hello, Aster!");
    }
}
```

Check it for errors without running anything:

```powershell
cargo run -p aster-cli -- check hello.aster
```

Run it:

```powershell
cargo run -p aster-cli -- run hello.aster
```

For an application, `Main` must be public, static, parameterless, and return `void` or `int`.
Libraries can still be checked without declaring `Main`. See the
[application entry reference](reference/application-entry.md) for the optional manifest and the
explicit `--function` mode.

## Organize a program with namespaces

The manifest example keeps its root source below the project directory:

```text
examples/hello_app/
  Aster.toml
  app/
    main.aster
    math.aster
```

Run it with:

```powershell
cargo run --quiet -p aster-cli -- run examples\hello_app\app\main.aster
```

It logs a greeting and prints `42`. Both files belong to namespace `app` because they are in the
same folder. Use `using another.namespace;` when code lives elsewhere. See
[namespaces and usings](reference/namespaces.md) for folder inference and visibility.

## Use the standard library

Official library namespaces use the `aster.*` prefix. Use `aster.math` for scalar helpers:

```aster
using aster.math;

public class Program
{
    public static int Main()
    {
        return Math.Clamp(150, 0, 100);
    }
}
```

Run the complete example with:

```powershell
cargo run --quiet -p aster-cli -- run examples\math_basics.aster
```

The result is `100`. A manifest-based project example is also available:

```powershell
cargo run --quiet -p aster-cli -- run examples\standard_library\app\main.aster
```

See the [standard library](reference/standard-library.md) and
[`aster.math`](reference/math.md) references for the available API and numeric edge cases.

For explicit absence and errors, `using aster.core;` provides `Option<T>` and `Result<T, E>`.
The runnable example and switch syntax are in the
[`Option` and `Result` reference](reference/option-result.md).

## Work with text

Strings are immutable UTF-8 values. They support concatenation, content equality, and a Unicode
scalar `Length`:

```aster
using aster.text;

string name = "Natte";
string message = "Olá, " + name + "!";
Log(message);

if (!String.IsEmpty(message))
{
    int length = message.Length;
}
```

Run the complete example with:

```powershell
cargo run --quiet -p aster-cli -- run examples\standard_text\app\main.aster
```

See the [strings reference](reference/strings.md) for allocation and Unicode details.

## Rerun on every save

```powershell
cargo run -p aster-cli -- watch examples\conventional_main.aster
```

`watch` reruns `Main` whenever the root file, a loaded project namespace file, or `Aster.toml`
changes. A broken
save reports diagnostics and keeps watching. Press `Ctrl+C` to stop it normally.

## Run an explicit test function

Existing examples and compiler tests can still choose a public parameterless namespace-level function:

```powershell
cargo run -p aster-cli -- run examples\expressions.aster --function Run
```

`--function` takes precedence over the conventional or manifest entry.

## Run the compiler tests

```powershell
cargo test --workspace
```

## Where to go next

- [Language tour](language-tour.md) — the language by example.
- [CLI reference](reference/cli.md) — checking, running, watching, and IR inspection.
- [VS Code extension](../editors/vscode/README.md) — syntax highlighting and snippets.
- [Compiler internals](compiler/architecture.md) — for contributors.
