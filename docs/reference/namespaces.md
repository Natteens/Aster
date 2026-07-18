# Namespaces and usings

A namespace says where names live. A `using` says which namespace names this file may use.
Folders provide the default organization:

```text
namespace = where names live
using     = which names this file may use
folder    = the organization convention
```

For this project:

```text
my-app/
  Aster.toml
  app/
    main.aster
    player.aster
  ui/
    menu.aster
```

both files directly under `app/` belong to namespace `app`; `ui/menu.aster` belongs to `ui`.
The filename is not part of the namespace. A source file at the project root belongs to the
global namespace.

Writing the declaration is optional. When present, it must agree with the directory:

```aster
namespace app;

using aster.math;
using aster.text;
using ui;
```

`namespace` must be first and may appear only once. All `using` declarations follow it and
precede types, functions, and variables. Namespace blocks, aliases, wildcard imports, selective
imports, reexports, and `using static` do not exist yet.

## Loading another namespace

`using ui;` loads the direct `.aster` files under `ui/` in stable path order and makes their
visible declarations available. A namespace may therefore span several files without `partial`:
the files may declare different types, but one type cannot yet be split between files.

Official namespaces start with `aster.*` and are loaded from the read-only standard library:

```aster
using aster.math;
```

A project cannot declare an `aster.*` namespace. If an official source is missing, the compiler
reports an incomplete Aster installation instead of searching the project.

Two usings that expose the same name produce an ambiguity error. Qualified source references and
aliases are future work, so the declarations must currently be renamed or reorganized.

## Visibility

- `private` is visible only inside its owning class or struct.
- `internal` is visible throughout the same Aster project, including across namespaces when the
  namespace is brought into scope with `using`.
- `public` is the API intended for other projects or packages in the future.
- `protected` remains rejected with an honest diagnostic until inheritance or another extension
  model exists.

Interfaces continue to expose public contracts. `partial` is not visibility; if added later, it
will mean that one type may be divided across files.

## Run and watch

```console
aster run examples/namespaces/app/main.aster
aster watch examples/namespaces/app/main.aster
```

The nearest `Aster.toml` establishes the project root. Without a manifest, the directory that
contains the root source is the project root. Watch observes loaded project sources and the
manifest, but not read-only standard-library files. Each rebuild creates a fresh JIT session and
ExecutionContext.

## Migration from pre-alpha syntax

The old public words `module` and `import` were removed in `0.0.0`. Use `namespace` and `using`.
The parser rejects old source with a focused migration diagnostic instead of accepting two
competing syntaxes.

Generic templates follow the same visibility and `using` rules as other declarations. A public
`Box<T>` in one namespace can be used as `Box<int>` after that namespace is brought into scope.
Specializations are cached for the whole linked project, so two files using the same closed type do
not create different runtime types.
