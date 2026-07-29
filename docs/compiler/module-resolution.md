# Namespace discovery and linking

The CLI supplies one root `.aster` file. The nearest ancestor containing `Aster.toml` is the
project root; without a manifest, the root source directory is used. A project source's namespace
is inferred from its parent directory relative to that root. Filenames do not participate, and
root-level sources use the global namespace.

An explicit `namespace app.ui;` declaration is checked against that inferred value. A `using`
maps the dotted name to a directory and loads every direct `.aster` file in stable sorted order.
This makes a namespace a set of compilation units rather than a single file. Loaded files and
namespaces are cached, and the active discovery stack detects cycles. Duplicate usings,
self-cycles, missing directories, mismatched declarations, and paths escaping the project root
produce source diagnostics.

Reserved `aster.*` usings use a separate standard-library provider. Its checked-in sources under
`stdlib/` are embedded in the compiler distribution. Project sources cannot declare `aster.*`,
official sources cannot depend on project namespaces, and a missing official namespace reports an
incomplete installation. Only project files become watch dependencies.

## Linking names

All declarations from files in one namespace share a namespace symbol table. A `using` contributes
`public` declarations and, for project-to-project access, `internal` declarations. Thus `internal`
means project visibility, not file visibility. Standard-library internals are not exposed.

Two usings that contribute the same unqualified name produce an ambiguity diagnostic. Before
semantic validation, non-root declarations receive deterministic compiler-internal qualified
names. References are rewritten against the source namespace and its direct usings. The combined
AST then enters the existing semantic, HIR, and MIR pipeline. Internal names are not ASTER syntax.

Source spans from different files occupy disjoint ranges, preserving the original file, line, and
column in diagnostics.

## Execution boundary

HIR and MIR contain resolved symbols but no paths, namespaces, usings, or manifest data.
Application entry selection happens after semantic analysis and yields a concrete `SymbolId`;
Cranelift never discovers files or parses TOML. Imported functions and types use the same MIR and
the same per-run ExecutionContext.

Watch snapshots every loaded project source and the nearest manifest. A stable dependency change
rebuilds the root graph and replaces both the JIT session and ExecutionContext. Standard-library
sources are read-only and are not watched as project files.
