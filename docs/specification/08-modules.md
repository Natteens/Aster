# 08 — Namespaces and project organization

## Objective

Define where declarations live and how a file brings names from another namespace into scope.
The compiler may use modules internally, but the accepted source language uses `namespace` and
`using`.

## Accepted syntax

```aster
namespace geometry;

public struct Point
{
    public float x;
    public float y;
}
```

```aster
namespace app;

using geometry;
```

## Accepted rules

- `namespace` is optional, appears at most once, and precedes every `using` and declaration.
- The namespace is inferred from the file's directory relative to the project root; filenames are
  excluded and root files use the global namespace.
- An explicit namespace must equal the inferred namespace.
- `using` loads all direct `.aster` files for a namespace in deterministic path order and makes
  visible names available unqualified.
- Several files may contribute different declarations to one namespace. Splitting one type across
  files with `partial` is not implemented.
- Namespace-level declarations default to `internal`. `internal` is accessible throughout the
  same project, including across namespaces; `public` is the future external API boundary.
- Ordinary namespace-level declarations cannot be `private` or `protected`.
- `aster.*` is reserved for official read-only standard-library namespaces.
- The removed `module` and `import` words are rejected with migration diagnostics.

## Invalid examples

```aster
using geometry;
namespace app; // namespace must come first
```

```aster
namespace aster.mine; // projects cannot claim official namespaces
```

```aster
module app; // use namespace app;
import geometry; // use using geometry;
```

## Open questions

- **OPEN QUESTION:** What syntax will qualify an ambiguous name directly?
- **OPEN QUESTION:** Will aliases, selective usings, or reexports be added?
- **OPEN QUESTION:** Will `partial` allow one type to span files?
- **OPEN QUESTION:** What syntax will qualify a namespace provided by more than one package?

Namespace cycles are rejected in the current discovery graph. Packages declare identity and local
path dependencies from manifest schema 2; see `../reference/packages.md`. Remote dependencies, a
lockfile, and a registry are not part of the implemented system.
