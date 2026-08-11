# Namespace discovery and linking

The CLI supplies one root `.aster` file. The nearest ancestor containing `Aster.toml` is the root
package; without a manifest, the root source directory is used. A source's namespace is inferred
from its parent directory relative to its own package root. Filenames do not participate, and
package-root sources use the global namespace.

An explicit `namespace app.ui;` declaration is checked against that inferred value. A `using`
maps the dotted name to a directory and loads every direct `.aster` file in stable sorted order.
This makes a namespace a set of compilation units rather than a single file. Loaded files and
namespaces are cached per package, and the active discovery stack detects cycles. Duplicate usings,
self-cycles, missing directories, mismatched declarations, and paths escaping the owning package
root produce source diagnostics.

## The package graph

A package declares `[package] name` and may declare path or locked Git dependencies. The whole
graph is resolved from manifests **before any source is read**, so
namespace discovery can never wander outside the declared graph — a `using` reaches only the
current package and its direct dependencies.

Each dependency path is resolved relative to the manifest that declares it and canonicalized, so
the graph does not depend on the working directory. Dependencies are visited in declared-name
order, and directory enumeration is sorted, so discovery, linking, and diagnostics are
deterministic. Manifest tables are sorted rather than trusted to preserve TOML order.

Git resolution is a separate source-materialization step owned by `aster fetch`. Normal compilation
reads the root `Aster.lock`, validates the exact cached commit, and gives its canonical local source
directory to this same graph builder; it never resolves a remote revision. Dependency lockfiles are
ignored. A path dependency originating inside a materialized Git source must remain within that
source root.

The same canonical directory reached through several graph paths becomes one package and is loaded
once. Two directories claiming the same package name, a name that disagrees with the key it is
declared under, a cycle, a missing path, a non-package directory, and an unsupported or malformed
dependency manifest are all controlled errors. Nothing in the compiler graph path performs network
access.

A malformed *root* manifest stays non-fatal during loading, because application-entry selection
reports it and `--function` deliberately bypasses that. Dependency manifests fail closed.

Reserved `aster.*` usings use a separate standard-library provider. Its checked-in sources under
`stdlib/` are embedded in the compiler distribution. Project sources cannot declare `aster.*`,
official sources cannot depend on project namespaces, and a missing official namespace reports an
incomplete installation. Only project files become watch dependencies.

## Linking names

All declarations from files in one namespace of one package share a namespace symbol table. A
`using` always contributes `public` declarations, and contributes `internal` declarations only when
the providing namespace belongs to the *same package*. Thus `internal` means package visibility,
not project-wide or file visibility. Standard-library internals are not exposed.

Two usings that contribute the same unqualified name produce an ambiguity diagnostic. A package's
own namespace takes precedence over a dependency's namespace of the same name, matching the
existing rule that local declarations shadow imported ones.

Before semantic validation, every declaration receives a deterministic compiler-internal qualified
name. A package's declared `[package] name` prefixes its declarations independent of whether that
package is the graph root, a direct dependency, or a transitive one: the same declaration gets the
same identity regardless of graph position. This is why two packages that spell the same namespace
and type can never collapse into one declaration, and cross-package identity collisions are
reported rather than merged. A manifest-less direct-file compilation uses an implicit empty root
identity and keeps its bare/namespace-only naming. Package *names* participate in manifest-backed
identity; filesystem paths never do.

Manifest-backed declarations follow that rule even in the literal file passed to
`compile_project`. Source-facing entry selectors such as `--function NAME` resolve their spelling
to the linked symbol before execution; they do not define nominal identity.

References are rewritten against the source namespace and its direct usings. The combined AST then
enters the existing semantic, HIR, and MIR pipeline. Internal names are not ASTER syntax.

Source spans from different files occupy disjoint ranges, preserving the original file, line, and
column in diagnostics.

## Execution boundary

HIR and MIR contain resolved symbols but no paths, namespaces, packages, usings, or manifest data.
Package resolution finishes before semantic analysis, and generic specialization still produces
concrete types, so a cross-package generic introduces no open parameter downstream. Application
entry selection happens after semantic analysis and yields a concrete `SymbolId`; only the root
package supplies it, and a dependency declaring its own `[application]` does not compete. Cranelift
never discovers packages, files, or TOML. Imported functions and types use the same MIR and the
same per-run ExecutionContext.

Watch snapshots every loaded source, every manifest in the resolved graph, and the root lockfile
when present, through one owning abstraction
(`ProjectCompilation::dependency_paths`). A stable change rebuilds the graph and replaces both the
JIT session and ExecutionContext. Standard-library sources are read-only and are not watched.
