# Packages and dependencies

An ASTER package is a directory with an `Aster.toml`. A package declares a name, may declare an
application entry, and may depend on other packages by local path or public HTTPS Git source.

`aster check`, `run`, `dump-hir`, `dump-mir`, and `watch` never download anything and never contact
the network. Only `aster fetch` resolves or downloads Git sources.

## Declaring a package

```toml
[package]
name = "app"

[application]
entry = "app.Program.Main"
```

`[package] name` is the package's identity. It must be a valid ASTER identifier: letters, digits,
and underscores, starting with a letter or underscore. `aster new` writes this for you, replacing
any character of the project name that cannot appear in an identifier with `_`.

`[application]` is optional. A package that omits it is a library: it compiles and is checkable
without providing `Main`.

```toml
[package]
name = "math"
```

## Depending on another package

```toml
[package]
name = "app"

[application]
entry = "app.Program.Main"

[dependencies]
math = { path = "../math" }
```

The key on the left is the dependency's name and **must equal** the `[package] name` declared by
the manifest it resolves to. A mismatch is an error, so a directory rename cannot silently swap
which package you depend on.

`path` is resolved relative to **the manifest that declares it**, not the shell's working
directory, and is then canonicalized. The same checkout produces the same graph no matter where
`aster` is invoked from.

For a public Git repository, declare both its HTTPS URL and a required revision:

```toml
[dependencies]
math = { git = "https://github.com/example/math.git", rev = "main" }
```

`rev` may name a branch, a tag, or a full commit SHA. A branch and tag with the same name is
ambiguous and rejected. SSH, private authentication, local/file transports, submodules, repository
subdirectories, and implicit default branches are not supported.

Run `aster fetch` after adding or changing a Git dependency. It resolves each revision to an exact
commit, materializes the source in ASTER's user cache, resolves transitive dependencies, and writes
the root project's `Aster.lock`. Git dependencies require a usable `git` executable for fetching
and local cache validation. Commit the lockfile to source control. A lock entry records only the
package name, declared Git URL and revision, and resolved full commit:

```toml
[[package]]
name = "math"
git = "https://github.com/example/math.git"
rev = "main"
commit = "0123456789abcdef0123456789abcdef01234567"
```

The lockfile contains Git packages only, including transitives, in package-name order. Dependency
edges remain owned by each package's `Aster.toml`; dependency lockfiles are ignored. A moving
branch or tag does not change a locked build. To re-resolve one declared Git dependency, run
`aster fetch --update math`; unrelated packages remain pinned unless the updated graph requires a
different resolution.

With a valid lockfile and cache, all compilation commands work offline. A missing or stale lockfile,
or a missing or modified cache entry, fails with a diagnostic directing you to `aster fetch` rather
than repairing state or contacting a remote. A relative path dependency declared inside a Git
package must remain inside that immutable materialized source tree.

The shared Git cache lives under `%LOCALAPPDATA%\Aster\cache\git` on Windows and
`${XDG_CACHE_HOME:-$HOME/.cache}/aster/git` on Linux. Its directory names are SHA-256 keys derived
from the exact URL and locked commit; cache paths never participate in package identity.

A layout for the example above:

```text
workspace/
  app/
    Aster.toml
    app/main.aster
  math/
    Aster.toml
    math/answer.aster
```

```aster
// math/math/answer.aster
namespace math;

public int Answer()
{
    return 42;
}
```

```aster
// app/app/main.aster
namespace app;

using math;

public class Program
{
    public static int Main()
    {
        return Answer();
    }
}
```

## Using a dependency's namespaces

A dependency contributes its namespaces to the packages that declare it. `using math;` searches the
current package first, then its **direct** dependencies.

Dependencies are not transitive for `using`. If `app` depends on `service` and `service` depends on
`math`, then `app` can use `service` but not `math`. `service` may of course call `math` internally
and expose the result — that is how a public declaration flows through the graph.

A package's own namespace always wins over a dependency's namespace of the same name, so adding a
dependency cannot capture a `using` that already resolved locally. When two *dependencies* provide
the same namespace, the `using` is ambiguous and reported; ASTER has no package-qualified `using`
yet.

## Visibility across packages

Inside one package, `internal` is visible across namespaces, as before.

Across a package boundary, only `public` is accessible. `internal` never crosses a dependency edge,
and there is no `friend` or workspace-wide visibility. Reaching for a dependency's `internal`
declaration is a controlled error:

```text
error: `Secret` is internal to package `math` and is not part of its public API
help: only `public` declarations cross a package dependency boundary
```

See [visibility](../specification/15-visibility.md).

## Package identity

Two packages never merge just because they spell a namespace or type the same way. Every package's
declarations carry its declared name in their compiler-internal identity, so `alpha`'s
`shared.Value` and `beta`'s `shared.Value` stay distinct declarations.

Identity is the declared package name, and it does not depend on graph position: `math`'s
declarations are named the same way whether `math` is the package you ran `aster` on or a
dependency reached through another package. Filesystem paths deduplicate the graph but never become
part of a type's identity, and nothing about packages reaches HIR, MIR, or Cranelift — package
resolution finishes before semantic analysis.

Two different directories declaring the same package name in one graph is an error. The same
directory reached through several paths (a diamond) is loaded once, so no declaration is duplicated.

## Generics across packages

A public generic in a dependency specializes normally in the consumer, including the interface-only
constraints from `where` clauses. Specialization still produces concrete types before HIR, so a
cross-package generic introduces no open type parameter and no boxing.

```aster
// contracts/contracts/scored.aster
namespace contracts;

public interface IScored
{
    int Score();
}

public int Total<T>(T value)
    where T : IScored
{
    return value.Score();
}
```

## The root application owns the entry point

Only the package you invoke `aster` on supplies the application entry. A dependency that declares
its own `[application]` does not compete for it.

## Errors

Each of these is an ordinary compiler diagnostic, never a panic:

- a dependency path that does not exist, or is not a directory;
- a dependency directory with no `Aster.toml`;
- a malformed dependency manifest;
- a dependency manifest without `[package] name`;
- a dependency whose declared name differs from the key it is declared under;
- two packages claiming the same identity;
- a dependency cycle;
- a failure anywhere in the transitive graph;
- a `using` that resolves to no package, or to more than one dependency;
- access to a dependency's `internal` declaration.

## Watching

`aster watch` observes every source and manifest in the resolved local graph, plus the root
`Aster.lock` when present. It never polls Git remotes. Cached Git sources are immutable; change a
declared revision through `aster fetch`, then restart the watcher.

## Not implemented yet

A registry or package service, package publishing/search, semantic-version ranges and solving,
`aster add`/`remove`, private or SSH Git sources, Git subdirectory packages, build scripts, and
native package hooks are deliberately absent. Any future extension must be deliberate and follow
the [compatibility policy](compatibility.md).
