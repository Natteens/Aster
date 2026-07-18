# Technical roadmap

Aster grows in vertical slices. A language feature is complete only when source syntax,
semantic validation, HIR, MIR, the Cranelift JIT, diagnostics, tests, and an executable example
agree on its behavior. Parsing alone is not a milestone.

## A — Executable interfaces

**Status:** implemented and executable in the current JIT subset.

**Depends on:** executable classes, methods, local namespaces, and the per-run ExecutionContext.

Classes may explicitly implement one or more stateless interfaces. Interface references use
real runtime dispatch without class inheritance or default methods. The initial representation
is a non-null pair containing the object reference and an interface method table owned by the
JIT session.

**Done when:** conformance and visibility are checked; class-to-interface conversion is safe;
interface parameters, locals, returns, and calls execute; incomplete or incompatible
implementations are diagnosed; multifile dispatch is tested; no AST/HIR dependency reaches the
backend.

## B — Generics by monomorphization

**Status:** generic namespace-level functions and generic classes, structs, and interfaces are
implemented through concrete specialization. Constraints remain future work.

**Depends on:** Milestone A remaining stable under resolved callable/type identities.

Begin with generic namespace-level functions. Every concrete instantiation produces typed HIR/MIR and
native code. Type inference must have one deterministic answer. Reflection, variance, open runtime
generic values, and erased dictionary dispatch are outside the design. Closed nominal types use a
structural specialization cache before semantic HIR; their layouts and interface tables are
concrete.

**Done when:** each supported instantiation is monomorphic before MIR; layouts are concrete;
duplicate instantiations are reused; ambiguity is diagnosed; executable examples cover scalar and
value-type arguments.

## C — Everyday language ergonomics

**Status:** implemented and executable in the current JIT subset.

**Depends on:** stable member lookup and call resolution after A and B.

Add static methods, safe field initializers, simple properties, deterministic overloads, and
explicit equality rules. Scalar equality compares values. Array and class equality initially
compares identity. Struct equality is generated only after an explicit language decision and only
for comparable fields; otherwise it remains rejected.

**Done when:** each accepted construct executes through MIR/JIT, overload selection has no
order-dependent fallback, initialization remains definite, and diagnostics explain competing
candidates and access failures.

## D — Small standard library

**Status:** scalar `aster.math`, immutable-text `aster.text`, and the generic value enums in
`aster.core` are implemented and executable. More namespaces and value-vector types remain future work.

**Depends on:** B for generic collections; C for a clean public API where useful.

Build modular libraries rather than compiler keywords. `aster.math` starts with executable scalar
functions; `aster.text` starts with `String.IsEmpty` over real runtime string operations.
`Option<T>` and `Result<T, E>` are ordinary `aster.core` enums, not compiler intrinsics. Value
structs such as `float2` and `int3` come later and are not primitive types. Collections wait for
mature generics. Graphics and engine behavior are not part of this milestone.

**Done when:** APIs are ordinary Aster namespaces/runtime intrinsics with documented costs, platform
behavior, executable examples, and no primitive-type exceptions for vectors.

## E — Projects and application entry (implemented foundation)

**Depends on:** stable local namespaces and callable member resolution.

The optional minimal `Aster.toml` selects a qualified public static parameterless `Main`, while
projects without it use the same `Main` convention in the root namespace. `--function` remains
available for tests and examples. `Main` does not imply an engine lifecycle, remote registry, or
package download.

**Done when:** manifest discovery is deterministic, invalid entries are diagnosed with source
context, multifile applications run, and projects without a manifest keep working.

## F — Measured performance

**Depends on:** representative executable language features from A–E.

Create reproducible benchmarks for frontend/JIT time, calls, interface dispatch, structs, arrays,
objects, and loops. Optimize only measured bottlenecks in MIR or the backend and keep before/after
results. Do not claim superiority over other languages without comparable methodology.

**Done when:** benchmark commands, machines/configurations, raw results, and correctness checks are
documented; accepted optimizations show repeatable improvement without semantic regressions.

## G — Explicit safe concurrency

**Depends on:** mature reference/value semantics, interfaces, generics, and measured runtime costs.

Design `aster.tasks` with opt-in tasks, synchronization, controlled failures, and explicit rules
for shared mutable data. No unsafe automatic parallelization and no implicit engine scheduler.

**Done when:** ownership or sharing rules prevent data races by construction or checked runtime
contracts; task lifetime and error propagation are specified; stress tests run under supported
platforms.

## H — Optional ECS

**Depends on:** G and stable library/runtime boundaries.

Build `aster.ecs` as an optional library/runtime: entities, components, queries, resources, events,
schedules, and read/write conflict analysis. Systems run in parallel only when their declared
access is compatible and measurement says it is useful.

**Done when:** ordinary Aster programs remain independent of ECS; ECS data access is checked;
scheduling is deterministic where promised; runtime behavior and costs are documented.

## I — GPU and optional engine direction

**Depends on:** no implementation dependency yet; this is architecture planning only.

GPU programs require a separate target and restricted language subset. Normal Aster code is never
silently treated as shader or compute code. Future paths may include explicit shader modules,
compute kernels, and `wgpu` integration. A future Aster Engine would be an optional library/runtime
assembled from the language, `aster.math`, `aster.tasks`, and `aster.ecs`.

**Done for planning:** the compiler boundary, restricted subset questions, data-transfer model, and
candidate `wgpu` integration are documented. No GPU or engine implementation is claimed.

## Sequencing rule

Milestones are reviewed and verified independently. If a milestone exposes an unresolved ABI,
memory-safety, or type-system boundary, development stops at the last green vertical slice. The
next task starts from that explicit boundary instead of leaving partially accepted syntax behind.
