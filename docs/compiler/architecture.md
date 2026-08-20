# Compiler architecture

ASTER is a Cargo workspace whose dependencies point inward:

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
depend on code-generation/runtime concerns. `foreach`, `async`, and `await` are general
language constructs. The removed `component`/`system`/`read`/`write` experiment remains
research only; see [`ecs.md`](../research/ecs.md).

### `aster-compiler`

Orchestrates project loading, lexing, parsing, linking, and validation. For a CLI root file,
the project loader discovers local namespaces recursively from `using` declarations before final
semantic analysis. Standard-library discovery follows the shared environment,
executable-relative, then embedded priority. Reserved `aster.*` namespaces cannot be shadowed
by the project. The loader creates
one deterministic compilation unit, gives linked declarations unique internal
names, and retains source ownership so diagnostics still point at the correct file. See
[`module-resolution.md`](module-resolution.md).

Generic namespace functions and generic class, struct, interface, and enum templates are specialized
after linking and before ordinary semantic validation. A structural cache reuses identical closed
types. Only concrete declarations proceed to HIR, MIR, layout calculation, interface tables, and
Cranelift; the backend has no generic ABI or type-erasure path. See
[`monomorphization.md`](monomorphization.md).

The semantic analyzer builds initial type/function/local symbol tables and checks visibility,
declarations, types, calls, expressions, variables, constants, lexical scopes, loop context,
all-path returns, worker-transfer boundaries, host-operation restrictions, and unreachable-code
warnings. Top-level `unsafe foreign` declarations participate in ordinary namespace, visibility,
overload, and callable identity resolution. A foreign call is accepted only inside a lexical
`unsafe` block and is rejected from direct or transitive Task/Parallel worker bodies.

After validation succeeds, the compiler lowers general-language AST nodes to typed HIR and then lowers
HIR to control-flow-explicit MIR.

After the narrow loop-concat representation rewrite, one general MIR optimizer simplifies the
already-typed control and scalar data flow. Its fixed, deterministic cycle propagates exact primitive
constants and direct scalar copies, folds constant primitive operations, converts known branches to
jumps, removes unreachable and trampoline blocks, and deletes dead assignments only when their
right-hand side is pure and cannot fail. Integer folding uses the language's runtime wrapping widths;
floating-point folding evaluates literal-to-literal IEEE operations without reassociation or
algebraic identities. Calls, allocations, indexed reads, collection operations, host/worker
operations, and lifetime instructions are never generic-DCE candidates. Task, async-frame, and
Parallel intrinsics also stop propagated facts at the worker-transfer boundary. Runtime-intrinsic
operands remain opaque to the optimizer so intrinsic-specific ABI shapes cannot be rewritten.

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
calls, operators, assignments, literals, variables, constants, and typed compiler-known operations
for collections, host I/O, tasks, and restricted parallel execution.

HIR represents a resolved foreign call explicitly with its declaration symbol, typed arguments,
and typed result. The source-only unsafe block has already served its semantic authorization and
lowers as an ordinary block. HIR does not embed MIR, machine instructions, Cranelift integration,
or execution.

### `aster-mir`

Owns the typed mid-level representation consumed after HIR. It retains resolved `SymbolId` values and
types while replacing structured statements with basic blocks, temporary locals, assignments, calls,
and explicit terminators: conditional branches, unconditional jumps, returns, and `End` for normal
fallthrough from `void` functions.

`if`/`else`, `while`, `for`, and `switch` no longer exist as structured MIR nodes. Their behavior is expressed as
edges between basic blocks. MIR remains backend-independent: it does not select machine instructions,
define an ABI, invoke Cranelift, or execute programs.

MIR has a typed foreign-declaration table and an opaque, effectful `ForeignCall` instruction. It
contains no raw pointer, Cranelift signature, library path, or dynamic symbol name.

MIR functions retain their visibility and optional owning type. This lets a backend enforce entry and
feature boundaries without consulting HIR or source syntax.

Executable structs use declaration-order natural layout. The JIT stores aggregates in stack slots,
copies parameters into callee-owned storage and returns aggregates through a hidden destination
pointer. This is an internal ASTER ABI, not the platform C aggregate ABI.

### `aster-codegen-cranelift`

Consumes validated `aster-mir` exclusively. It validates the supported JIT subset, declares all ASTER
functions, translates MIR locals and basic blocks into Cranelift IR, finalizes native code in memory,
and invokes a concrete function symbol already selected outside the backend.

The current ABI maps 8/16/32/64-bit integers to matching Cranelift integer widths, `float` to
f32, `double` to f64, `bool` to an 8-bit `0`/`1` value, `char` to an i32 Unicode scalar, and
`string` to a pointer into the runtime
string ABI. Direct calls between compiled ASTER functions support all of these as parameters and
returns. Runtime intrinsics for strings, arrays, official collections, host I/O, tasks, parallel
workers, and memory scopes are bound from the `aster-runtime` registry. The `unsafe` boundary
converts Cranelift's finalized, untyped code pointer into the
exact signature already checked by the backend. Every function receives a hidden pointer to its
host-owned ExecutionContext. Array allocation, `Length`, and indexing use the small checked runtime
ABI. Class allocation uses the same context; method and constructor calls are ordinary resolved MIR
calls with an explicit object receiver. An interface value is two pointer-sized words: the non-null
object reference and a pointer to a read-only method table owned by the JIT module. Interface calls
load the concrete function from that table and use the checked ASTER calling convention. The backend
frees each JIT session after
its result is copied out. The JIT module remains alive during the call.

Semantic analysis owns overload selection and records a stable callable identity for every call.
HIR receives an already resolved static, instance, interface, constructor, getter, or setter target;
MIR and Cranelift do not repeat overload lookup. Property accessors lower as ordinary functions, and
field initializer assignments are inserted at the beginning of constructors in declaration order.
Aggregate equality is explicit MIR: structs compare typed fields recursively, class/array references
compare pointer identity, and interfaces compare their object word.

Struct instance methods use the existing aggregate ABI: HIR adds a typed `User(owner)` receiver,
MIR passes it as the first ordinary operand, and Cranelift copies that aggregate into the callee's
receiver local. Reference-bearing fields are copied with the same field-wise representation as
ordinary struct assignment; existing containment and escape proofs keep referenced allocations
alive independently of the receiver stack copy. Generic methods are specialized before semantic analysis with a structural cache
key containing the closed owner, method declaration/signature, and method arguments. Neither open
parameters nor constraint metadata reach HIR.

The complete post-lowering order is the safe loop-concat rewrite, general MIR optimization,
escape-region assignment, local-object elimination, AARM ProductionV2 Temporary selection, and
long-lived owned-region selection. This keeps lifetime markers out of the general optimizer while
giving every escape and ownership proof the final simplified scalar/control shape. The last pass
reuses escape aliases and MIR liveness to turn only fresh return-only,
same-block repeated reference families into explicit `OwnedRegionEnter`/`OwnedRegionExit` MIR.
Function-local region IDs validate marker balance but are erased at the private runtime ABI.
Cranelift validation also rejects unrelated direct or transitive Persistent effects and
invalidated-local use, then mechanically calls the context-owned LIFO checkpoint ABI; it never
discovers aliases, liveness, ownership, or source patterns.

The backend rejects unsupported MIR before code generation. Decimal source is rejected earlier by
the post-link language-surface gate because its numeric and ABI contract remains unspecified;
backend validation still rejects hand-built decimal MIR fail-closed. The backend does not inspect
AST/HIR, implement inheritance, or generate object files or link executables.
Public standard-library code reaches this backend as ordinary concrete MIR. The compact string
transforms, floating-point math operations, and collection snapshots lower through typed HIR/MIR
intrinsics with checked runtime ABI calls; Cranelift does not inspect public `Math`, `String`, or
collection source names or standard-library paths. Those operations remain opaque to the general
MIR optimizer and retain their allocation/failure ordering.

Minimal native FFI follows the same typed authority. Before JIT finalization, Cranelift validates
each foreign declaration/call and resolves its fully linked identity plus exact scalar signature
against an execution-scoped `aster-runtime` registry. The resolved wrapper is imported once into
the JIT module. Each call passes fixed-width C-ABI scalars and, for non-void results, one aligned
hidden out pointer; the wrapper returns an `i32` status. Generated code publishes a result only
after status zero and bool/char validation. Foreign calls remain opaque effect/failure and lifetime
barriers to the optimizer and memory passes. Cranelift performs no name heuristic, dynamic loading,
signature inference, or registry lookup on each executed call. It validates the registered
descriptor, not the opaque native address itself; the host's explicit unsafe registration owns that
actual C-ABI and no-unwind assertion.

Task callables are likewise concrete before HIR. `Task.Run` value arguments are evaluated in the
caller and lowered as ordinary typed MIR operands after the resolved callable identity. Cranelift
uses the existing aggregate layout authority to pack a caller frame; the host copies it into a
worker-owned payload before enqueue, and a generated per-signature trampoline reconstructs the
callee ABI mechanically. The parameterless form keeps its original direct entry fast path.
`TaskWaitAll`, `TaskCancel`, and `TaskCancellationRequested` are typed runtime intrinsics and remain
opaque barriers to the general MIR optimizer. Backend validation checks callable arity/signature,
closed transferable layouts, homogeneous composition types, and cancellation operand shapes before
code generation. Task control is per-runtime/per-worker scheduler metadata; no ASTER arena pointer
or Cranelift type enters the runtime contract.

### `aster-runtime`

Owns the execution boundary between JIT-compiled ASTER code and host services: the per-run
`ExecutionContext`, temporary and persistent arenas, immutable UTF-8 strings, arrays, `List<T>`,
`Dictionary<K,V>`, terminal and filesystem operations, and restricted task/parallel workers. Task
control uses private atomic terminal/request state, worker-owned argument bytes, cached repeatable
outcomes, and caller-context result-array construction for deterministic `Task.WaitAll`. The
request/terminal compare-exchange is the cancellation linearization point; the worker publishes its
completed or failed outcome through the task's completion channel only after fixing that terminal
state. Async MoveNext contexts receive an `Arc` clone of the same private control on every resume.
It also owns the central registry of exported runtime functions with backend-neutral signatures.
Separately, `ForeignRegistry` is an embedding-host-owned, cloneable value containing only canonical
declaration names, backend-neutral fixed-width scalar signatures, and opaque wrapper addresses.
It has no global state, loader, compiler types, or Cranelift types; independent executions may bind
the same declaration differently.
The crate depends on no other ASTER crate and exposes no Cranelift types.

### `aster-cli`

Builds the `aster` executable. `aster new NAME` creates the canonical project scaffold, and
`aster doctor` diagnoses the current executable, standard library, installation, `PATH`, and project.
`check`, `run`, `dump-hir`, and `dump-mir` accept either an explicit file or the project in the
current directory. Inspection commands never execute the program. `run` selects a validated
application entry after namespace resolution and semantic analysis, then gives its `SymbolId` to
the isolated backend. An optional `Aster.toml` is loaded outside Cranelift, which never sees paths
or TOML. `--function NAME` preserves explicit root-namespace execution for development. All
compilation commands resolve the root file's transitive usings. `aster watch FILE [--function NAME]`
reruns the same pipeline when the root, manifest, or any loaded dependency changes
(`docs/compiler/watch.md`).
`aster test` uses the same project compiler with the root package's conventional `tests/` source
tree added before linking. The compiler returns sorted descriptors containing only an ordinary
concrete symbol and display identity; HIR/MIR bodies remain ordinary functions. The CLI prepares
the validated main module once and invokes those symbols sequentially with fresh execution contexts
and scoped console backends. Task-using tests receive a per-test worker runtime so no task or worker
host state crosses the test boundary.

## Deliberate boundaries

Inheritance, interface inheritance/default methods, external packages, interprocedural inlining and
range/loop optimization, AOT/object generation, executable linking, and general shared-memory
concurrency remain outside the current implementation. Task and parallel APIs deliberately use
restricted worker boundaries.
`Main` is only an application entry convention; it does not imply `Start`, `Update`, a loop, or any
engine lifecycle. ECS is not part of the language or compiler — see `docs/research/ecs.md`.

Arrays and classes use a per-invocation host-owned context. Objects are non-null references with
identity and stable addresses. A narrow fresh return-only family may be reclaimed at a
compiler-proven last use; ambiguous, shared, contained, interface, and cyclic families remain live
until that invocation ends. Interface values borrow those object references and
their JIT-owned tables; they cannot outlive the invocation. Inheritance, finalization and a
general source-visible/AOT ownership model remain deliberately undecided.
