# Getting started

Aster is currently distributed as source. You build its CLI once with Cargo, then use the `aster`
command for normal work.

## Install the CLI

Install [stable Rust](https://rustup.rs) 1.85 or newer. From the root of a cloned Aster repository,
run:

```console
cargo install --path crates/aster-cli --locked --force
```

This compiles the Aster toolchain—the compiler, CLI, runtime, and embedded standard library—and
installs an executable named `aster`, normally in
`%USERPROFILE%\.cargo\bin` on Windows or `$HOME/.cargo/bin` on Unix. Make sure that directory is on
`PATH`:

```console
aster --version
aster --help
```

The standard library is embedded in the executable, so the CLI does not need the repository as its
working directory after installation.

## Run a program

The smallest application uses a public, static, parameterless `Main` method:

```aster
public class Program
{
    public static int Main()
    {
        Log("Hello, Aster!");
        return 42;
    }
}
```

Run the checked-in version:

```console
aster run examples/hello.aster
```

The program logs `Hello, Aster!` and the CLI prints its return value, `42`.

`Main` may also be declared `public static void Main()`: it executes without producing a value, so
only the program's logs appear. See [`examples/void_main.aster`](../examples/void_main.aster).

Copy the source into your own `hello.aster` file. You can check it without executing it, run it, or
rerun it after each save:

```console
aster check hello.aster
aster run hello.aster
aster watch hello.aster
```

`Main` may return `void` or `int`. A source file that represents a library can be checked without a
`Main`; execution requires an [application entry](reference/application-entry.md).

## Grow into a project

Aster projects can spread a namespace across files. The introductory project has this shape:

```text
examples/hello_app/
  Aster.toml
  app/
    main.aster
    math.aster
```

Run it by passing the root source file:

```console
aster run examples/hello_app/app/main.aster
```

The nearest `Aster.toml` establishes the project root and selects `app.Program.Main`. Folder names
provide default namespaces, while `using` brings another namespace into scope. See
[Namespaces and usings](reference/namespaces.md) for the complete loading rules.

## Continue learning

- Follow the [runnable examples](../examples/README.md) from basics to generics and error values.
- Read the [language tour](language-tour.md) for the ideas behind those programs.
- Use the [CLI reference](reference/cli.md) for explicit function execution and IR inspection.
- Contributors should use the separate [compiler development](compiler/development.md) workflow.
