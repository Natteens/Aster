# Compiler architecture

Aster is a Cargo workspace whose dependencies point inward:

```text
aster-cli -> aster-compiler -> aster-syntax -> aster-diagnostics
          |                 -> aster-hir
          |                 -> aster-mir -> aster-hir
          |                 -> aster-types
          |                 -> aster-diagnostics
          -> aster-codegen-cranelift -> aster-mir
                                     -> aster-types
                                     -> aster-runtime
```

## Crates

### `aster-types`

Owns backend-neutral primitive metadata: source names, fixed widths, signedness, integer
ranges, literal classification, exact implicit conversions, and binary promotion. It has no
dependency on syntax, HIR, MIR, Cranelift, or the runtime. IR layers keep their own structural
type representation and adapt primitive cases to this one shared ruleset.

### `aster-diagnostics`

Owns byte spans, error/warning severity, line/column calculation, source excerpts, and optional safe correction hints.
The CLI supplies the file name when rendering diagnostics.

### `aster-syntax`

Contains the hand-written lexer and recursive-descent parser. The AST is divided into declarations,
expressions, and statements. Expression parsing uses explicit precedence levels, with
right-associative assignment. Every token and meaningful AST node retains a source span.
Statements include lexical blocks, `if`/`else if`/`else`, `while`, C-style `for`, enum `switch`,
`break`, `continue`, and `return`.

This crate describes syntax only. It does not resolve names, infer types, or
depend on code-generation/runtime concerns. It has no ECS-specific syntax; an early ECS
experiment (`component`/`system`/`foreach`/`read`/`write`) was removed — see
`docs/future/ecs-package.md`.

### `aster-compiler`

Orchestrates project loading, lexing, parsing, linking, and validation. For a CLI root file,
the project loader discovers local namespaces recursively from `using` declarations before final
semantic analysis. Reserved `aster.*` namespaces come from embedded, checked-in standard-library
sources and cannot be shadowed by the project. The loader creates
one deterministic compilation unit, gives linked declarations unique internal
names, and retains source ownership so diagnostics still point at the correct file. See
[`module-resolution.md`](module-resolution.md).

Generic namespace functions and generic class, struct, interface, and enum templates are specialized
after linking and before ordinary semantic validation. A structural cache reuses identical closed
types. Only concrete declarations proceed to HIR, MIR, layout calculation, interface tables, and
Cranelift; the backend has no generic ABI or type-erasure path. See
[`monomorphization.md`](monomorphization.md).

`semantic::general` builds initial type/function/local symbol tables and checks visibility,
declarations, types, calls, expressions, variables, constants, logging, lexical scopes,
loop context, all-path returns, and unreachable-code warnings.

After validation succeeds, the compiler lowers general-language AST nodes to typed HIR and then lowers
HIR to control-flow-explicit MIR.

Struct literals keep resolved type and field symbols. Nominal class-to-interface conversions are
explicit in typed HIR, after semantic validation checks every required public method against its
exact contract. MIR carries interface descriptors, concrete implementation tables, and indirect
interface calls without exposing syntax nodes to the backend. It also carries struct descriptors,
aggregate construction, and base-preserving field projections instead of reducing fields to globals.
Enum values retain a resolved case and concrete payload types. Exhaustive switches lower to tag
tests, payload projections, and ordinary control-flow blocks; no enum syntax reaches Cranelift.
The postfix `?` operator resolves the official `aster.core.Result` nominally, records the concrete
`Ok`/`Error` cases and the enclosing function's `Error` case in semantic analysis, and appears in HIR
as a fully typed `PropagateResult` node. MIR reuses the same tag test, payload projection, enum
construction, and return machinery to evaluate the operand once, continue on `Ok`, and early-return
the enclosing `Result`'s `Error` — so Cranelift sees only concrete branches and enum data
(`docs/reference/result-propagation.md`).

### `aster-hir`

Owns the backend-independent high-level intermediate representation. HIR removes source-only syntax,
normalizes visibility, assigns stable-in-compilation `SymbolId` values to declarations and references,
and records checked types on expressions. It represents declarations, functions, blocks, control flow,
calls, operators, assignments, literals, variables, and constants.

HIR does not embed MIR, machine instructions, Cranelift integration, or execution.

### `aster-mir`

Owns the typed mid-level representation consumed after HIR. It retains resolved `SymbolId` values and
types while replacing structured statements with basic blocks, temporary locals, assignments, calls,
and explicit terminators: conditional branches, unconditional jumps, returns, and `End` for normal
fallthrough from `void` functions.

`if`/`else`, `while`, `for`, and `switch` no longer exist as structured MIR nodes. Their behavior is expressed as
edges between basic blocks. MIR remains backend-independent: it does not select machine instructions,
define an ABI, invoke Cranelift, or execute programs.

MIR functions retain their visibility and optional owning type. This lets a backend enforce entry and
feature boundaries without consulting HIR or source syntax.

Executable structs use declaration-order natural layout. The JIT stores aggregates in stack slots,
copies parameters into callee-owned storage and returns aggregates through a hidden destination
pointer. This is an internal Aster ABI, not the platform C aggregate ABI.

### `aster-codegen-cranelift`

Consumes validated `aster-mir` exclusively. It validates the supported JIT subset, declares all Aster
functions, translates MIR locals and basic blocks into Cranelift IR, finalizes native code in memory,
and invokes a concrete function symbol already selected outside the backend.

The current ABI maps 8/16/32/64-bit integers to matching Cranelift integer widths, `float` to
f32, `double` to f64, `bool` to an 8-bit `0`/`1` value, `char` to an i32 Unicode scalar, and
`string` to a pointer into the runtime
string ABI. Direct calls between compiled Aster functions support all of these as parameters and
returns. Runtime intrinsics (logging, string equality, concatenation, and Unicode length) are bound from the `aster-runtime`
registry. The `unsafe` boundary converts Cranelift's finalized, untyped code pointer into the
exact signature already checked by the backend. Every function receives a hidden pointer to its
host-owned ExecutionContext. Array allocation, `Length`, and indexing use the small checked runtime
ABI. Class allocation uses the same context; method and constructor calls are ordinary resolved MIR
calls with an explicit object receiver. An interface value is two pointer-sized words: the non-null
object reference and a pointer to a read-only method table owned by the JIT module. Interface calls
load the concrete function from that table and use the checked Aster calling convention. The backend
frees each JIT session after
its result is copied out. The JIT module remains alive during the call.

Semantic analysis owns overload selection and records a stable callable identity for every call.
HIR receives an already resolved static, instance, interface, constructor, getter, or setter target;
MIR and Cranelift do not repeat overload lookup. Property accessors lower as ordinary functions, and
field initializer assignments are inserted at the beginning of constructors in declaration order.
Aggregate equality is explicit MIR: structs compare typed fields recursively, class/array references
compare pointer identity, and interfaces compare their object word.

The backend rejects unsupported MIR before code generation. `decimal` is preserved by the
frontend but rejected here until a real decimal runtime exists. The backend does not inspect
AST/HIR, implement inheritance, or generate object files or link executables.
Public standard-library code reaches this backend as ordinary concrete MIR.
The one math domain-error bridge is a typed MIR intrinsic; Cranelift does not inspect `Math` names
or standard-library paths.

### `aster-runtime`

Owns the execution boundary between JIT-compiled Aster code and host services: the per-run
ExecutionContext and fixed-array arena, the immutable
UTF-8 string ABI (`docs/compiler/runtime-abi.md`), context-owned immutable string concatenation,
Unicode-scalar length, the standard logging sink, and a central registry of
exported runtime functions with backend-neutral signatures. It depends on no other Aster crate
and exposes no Cranelift types; future runtime modules (files, time, windowing, audio,
networking) add registry entries instead of backend special cases.

### `aster-cli`

Builds the `aster` executable. `aster check FILE` validates a source file. `aster dump-hir FILE` prints
typed HIR, while `aster dump-mir FILE` prints control-flow MIR. Both inspection commands run the validated
frontend pipeline and never execute the program. `aster run FILE` selects a validated application
entry after namespace resolution and semantic analysis, then gives its `SymbolId` to the isolated
backend. An optional `Aster.toml` is loaded outside Cranelift, which never sees paths or TOML.
`--function NAME` preserves explicit root-namespace execution for development. All commands resolve
the root file's transitive usings. `aster watch FILE [--function NAME]`
reruns the same pipeline when the root, manifest, or any loaded dependency changes
(`docs/compiler/watch.md`).

## Deliberate boundaries

Inheritance, interface inheritance/default methods, external packages, MIR optimization, AOT/object
generation, executable linking, and concurrency remain outside this phase.
`Main` is only an application entry convention; it does not imply `Start`, `Update`, a loop, or any
engine lifecycle. ECS is not part of the language or compiler — see `docs/future/ecs-package.md`.

Arrays and classes use a per-invocation host-owned context. Objects are non-null references with
identity and live until that invocation ends. Interface values borrow those object references and
their JIT-owned tables; they cannot outlive the invocation. Inheritance, finalization and a
long-lived/AOT ownership model remain deliberately undecided.
