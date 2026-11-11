# A. Technical map of the repository (state at `8b5466b`)

> **Status: descriptive snapshot inside a PROPOSED document set.** It describes observed behavior, not
> a contract. Evidence labels are defined in the [README](README.md).

## 1. Shape

Rust workspace, edition 2024, MSRV 1.88, nine crates, about 160,000 lines of Rust including tests
(about 103,000 under `src/`), 2,851 `#[test]` attributes, 119 integration test files [V][A].
External dependencies: Cranelift `=0.123.20` (codegen, frontend, jit, module) and `toml 0.8`. Nothing
else [V]. The workspace lint is `unsafe_code = "forbid"`; `aster-runtime` and
`aster-codegen-cranelift` override it to `deny` with per-site `#[allow]` [V].

```text
aster-cli ─► aster-compiler ─► aster-syntax ─► aster-diagnostics
    │              ├─► aster-hir
    │              ├─► aster-mir ─► aster-hir
    │              └─► aster-types
    └─► aster-codegen-cranelift ─► aster-mir, aster-types, aster-runtime
```

Largest source files [V]: `aster-runtime/src/context.rs` (10,436 lines),
`aster-codegen-cranelift/src/validation.rs` (8,671), `aster-compiler/src/temporary_subregions.rs`
(5,621), `aster-runtime/src/arena.rs` (4,884), `aster-codegen-cranelift/src/task_runtime.rs` (3,022).

## 2. Pipeline, source to execution

The single driver is `aster_compiler::compile` (`crates/aster-compiler/src/lib.rs`). The order after
semantic validation is fixed [V, `lib.rs:236-250`]:

```text
project load + namespace link  (project.rs)
→ generic specialization on the AST  (generics/)
→ semantic analysis  (semantic/)
→ HIR lowering  (hir_lowering/)
→ MIR lowering  (mir_lowering/)
→ loop string-concat rewrite
→ general MIR optimizer
→ canonical array-loop proof
→ escape analysis: region assignment
→ local object elimination (scalar replacement)
→ AARM ProductionV2 Temporary subregions
→ owned Persistent regions
→ [backend] MIR validation → Cranelift IR → JIT finalize → invoke
```

### Trace: an interface call (`examples/interfaces.aster`) [A]

| Stage | Location |
| --- | --- |
| Lexer keyword | `aster-syntax/src/lexer.rs:307` |
| Parser type declaration, call postfix | `parser.rs:377-379`, `parser.rs:1672`, `parser.rs:1750` |
| Semantic dispatch selection (`Dispatch::Interface`) | `semantic/general/calls.rs:962-1001`, recorded at `calls.rs:1088` |
| Class-to-interface compatibility | `semantic/general/expressions.rs:1884` |
| HIR call with member callee; implicit upcast | `hir_lowering/expressions.rs:1074`; `hir_lowering/types.rs` (about line 144) |
| MIR `CallInterface`, `MakeInterface`, slot tables | `mir_lowering/calls.rs:24-35`, `mir_lowering/expressions.rs` (about 789), `mir_lowering/module.rs:160-229` |
| Cranelift: fat pointer `{object, table}`, `call_indirect` | `aster-codegen-cranelift/src/interfaces.rs:72-143` |
| Table emission into JIT data | `interfaces.rs:7-40`, called from `execution.rs:116` |

## 3. Subsystems

### 3.1 `aster-syntax` — lexer, parser, AST

- **Responsibility.** Hand-written lexer and recursive-descent parser; spans on every node.
- **Inputs/outputs.** Source text to tokens and AST; diagnostics.
- **Facts.** 48 reserved words [A]; `protected`, `async`, `await`, `decimal`, `module`, `import` are
  tokens [V, `lexer.rs:295-345`]. `base`, `virtual`, `override`, `abstract`, `sealed`, `null`, `is`,
  `as`, `try`, `catch`, `throw`, `do`, `ref`, `out`, `yield` are ordinary identifiers [V].
  Contextual: `unsafe`, `foreign`, `test`, `where`, `get`, `set`, `value` [A].
- **Recovery.** The parser recovers at declaration, member, and statement boundaries
  (`parser.rs:2395-2452`), but lexer errors stop before parsing and parse errors stop before semantic
  analysis [A].
- **Tests.** 14 integration files.
- **Limits.** No lambda, function-type, or closure syntax of any kind. The heritage list is
  `TypeDeclaration.interfaces` only (`declarations.rs:35`) [A].

### 3.2 `aster-compiler` — front end and MIR passes

- **Project loading** (`project.rs`, `manifest.rs`, `lockfile.rs`, `git_source.rs`): one deterministic
  whole-program compilation unit; path and pinned HTTPS Git dependencies; embedded standard library
  (`StandardLibrary::embedded()`, `project.rs:207`) [V]. Spans are rebased globally
  (`project.rs:1166-1174`) [A].
- **Generics** (`generics/`): monomorphization on the AST, before semantic analysis
  (`generics.rs:70`); structural cache; interface-only nominal constraints. Only concrete
  declarations reach later layers. IMPLEMENTED.
- **Semantic analysis** (`semantic/`): names, types, overloads, visibility, flow, worker-transfer
  rules. It is the only layer that chooses a call target. Results reach HIR lowering through side
  tables keyed by `ModelNodeKey { context: String, span: Span }` and `CallableKey { owner,
  declaration_start, name, accessor }` [V, `semantic/mod.rs:23-29`, `:213-217`]. **These identities are
  positional.**
- **HIR lowering** (`hir_lowering/`): consumes the model. It still recomputes some types
  (`hir_lowering/types.rs:206-223`, binary result type) [A]; this is the duplication behind defect D1
  of the core readiness audit, since fixed in PR #71 and #74 but structurally still present.
- **MIR lowering** (`mir_lowering/`): CFG construction; `async_machine.rs` builds the restricted
  two-state machine.
- **Optimizer** (`mir_optimizer.rs`): constant propagation and folding, branch simplification, CFG
  cleanup, dead pure assignments. Plus two narrow passes (array-loop bounds proof, loop string-concat
  rewrite). **No inlining. No devirtualization** [A, confirmed by search].
- **Memory passes**: section 3.6.
- **Declaration identity** (`declaration_key.rs`, `source_declaration_index.rs`): structural keys that
  exclude spans and ordering. **Not consumed by the pipeline** (`allow(dead_code)`) [A]; matches
  `architecture.md`.
- **Tests.** 35 integration files.

### 3.3 `aster-types`, `aster-hir`, `aster-mir`

- `aster-types` (424 lines): primitive metadata, promotion, implicit conversion. One table.
- `aster-hir` (914 lines): typed, backend-independent. `Type` has no function-pointer or closure
  variant (`hir/lib.rs:11`) [A].
- `aster-mir` (1,278 lines): typed CFG. `Instruction` has explicit allocation instructions carrying
  an `AllocationRegion`, region enter/exit markers, direct `Call`, `CallInterface`, `ForeignCall`,
  `CallIntrinsic` [V]. Function values exist only as `OperandKind::Function(SymbolId)` for `Task.Run`,
  `Parallel`, and async [A, `mir/lib.rs:1057`]. `ClassDefinition` is `{symbol, name, fields}`: no base,
  no flags, no slots [V].
- **Limit.** `aster-mir` has no validator. The only MIR validator is in the backend (3.4).

### 3.4 `aster-codegen-cranelift` — validation, code generation, and (today) the scheduler

- **Validation** (`validation.rs`, `validate_module` at line 197): about 5,900 production lines,
  fail-closed; re-checks owned regions (`:1873`) and subregions (`:2512`) [A]. It runs only at backend
  entry.
- **Codegen.** `opt_level = speed` [V, `execution.rs:607`]. Direct calls between ASTER functions
  (`calls.rs:27-39`); indirect only for interfaces [A]. Every function receives the hidden
  `ExecutionContext` pointer. Aggregates use an internal ABI with a hidden destination pointer.
- **Failure propagation.** A one-byte `error_flag` in the context is tested after fallible calls
  (`functions.rs:238-264`, `calls.rs:54`, `interfaces.rs:129`) [A]. No unwinding.
- **Recursion guard.** Only for functions on a call-graph cycle (`backend.rs:60-120`), limit 1024
  [V, `context.rs:32`]. Edges come from `Call` and `CallInterface` only [A].
- **JIT lifetime.** Module freed on drop (`execution.rs:862-878`) [A]. No AOT: no `cranelift-object`,
  no `ObjectModule` [A]. `Codegen` is tied to the concrete `JITModule` [A].
- **Scheduler.** `worker_pool.rs`, `task_runtime.rs`, `task_abi.rs`, `async_abi.rs`,
  `completion_queue.rs` live in this crate. See 3.7.
- **Tests.** 62 integration files.

### 3.5 `aster-runtime` — host boundary

- **Responsibility.** `ExecutionContext`, arenas, strings, arrays, `List`, `Dictionary`,
  `StringBuilder`, console, filesystem, clocks, math, the runtime function registry, the foreign
  registry. Depends on no ASTER crate and no Cranelift type [V].
- **Layouts** [A]: class objects are **headerless** zeroed bytes (`object.rs:9`); interface value is
  `{object*, table*}` (`layouts.rs:186`); enum is an `i32` tag plus inline payload
  (`layouts.rs:67-125`); string is `{len, bytes}`; array, list, dictionary, and builder are `repr(C)`
  headers with separate buffers (`context.rs:40`, `:170`, `:200`, `:223`).
- **Globals.** ID counters (`arena.rs:46`, `context.rs:657`), zero-sized page-backend statics, one
  `OnceLock<Instant>` in `time.rs` [V].
- **`unsafe`.** Confined to ABI views and arena projection; `context.rs` has 234 `allow` sites and
  129 `SAFETY` comments [A], so not every site carries its justification.

### 3.6 Memory

| Mechanism | Status | Evidence |
| --- | --- | --- |
| One `ExecutionContext` per run, per worker job, per test; hidden first argument | IMPLEMENTED | `context.rs:610` [A]; `execution.rs:594-601` [V] |
| Temporary and Persistent `PagedArena`, 4 KiB to 64 KiB pages, combined 1 GiB cap | IMPLEMENTED | `context.rs:27` [V] |
| Page backend: `VirtualAlloc` (Windows), `mmap` (Linux) | IMPLEMENTED (default) | `arena.rs:472` [A] |
| Rewind zeroes reclaimed bytes; pages retained until the context drops | IMPLEMENTED | `arena.rs:652-670` [A] |
| Escape analysis selects the region; default is Persistent | IMPLEMENTED | `escape_analysis.rs:68` (nine escape reasons) [A] |
| Scalar replacement of trivial local objects | IMPLEMENTED | `local_object_elimination.rs` |
| Function-scope Temporary rewind | IMPLEMENTED | |
| AARM ProductionV2 fine subregions | IMPLEMENTED, narrow | Any `Call`, `CallInterface`, or `ForeignCall` in the loop rejects it [V, `temporary_subregions/validation.rs:903-916`] |
| Owned Persistent regions | IMPLEMENTED, narrow | `owned_regions.rs:94-157` [A] |
| Purge, delayed purge, pressure purge, inactive cache (AARM-4A to 4D) | **GATED: dead in every shipped path** | `allow(dead_code, reason = "… opt-in research policy")` [V, `arena.rs:713`, `:728`, `:746`]; callers are unit tests only [A] |
| `MemoryGovernor` | **GATED** | Hidden constructors and the `aarm-telemetry` feature; `aster-cli` has no reference [V] |
| Individual deallocation, reference counting, tracing, finalizers, weak references | ABSENT | |

**What is never reclaimed before the run ends** [A]: any value stored in a field, returned outside
the owned-region shape, added to a collection, wrapped in a struct or enum, converted to an
interface, or passed to a call without a non-escaping summary. Old `List` backing buffers after
growth (`context.rs:2953-3019`). Old `Dictionary` storage after a rebuild; `Remove` leaves tombstones
(`context.rs:2026`, `:2122`). A dictionary is capped at 100,000 live entries and 64 MiB.
`while (true) { Update(); Render(); }` gets no fine reclaim because the body contains calls.

The core readiness audit measured this: a loop storing the latest allocation in a field reached
824,000,032 bytes at 200,000 iterations and failed at the 1 GiB cap at 400,000
(`core-readiness-audit.md`, section 4).

**Fragile invariants** [A]: the `borrows_only` intrinsic whitelist is sound only because runtime
string operations always copy (`string.rs:141`, `:818`, `:907`); the `birth_scope_depth` promotion
test (`context.rs:1488`, `:2001`, `:3002`) relies on compile-time builder provenance with no runtime
guard. No concrete unsoundness was found; none was proven absent.

### 3.7 Concurrency

| Aspect | Today | Evidence |
| --- | --- | --- |
| Pool | Per run, created only if the module uses tasks; `available_parallelism()` OS threads, default stacks | `execution.rs:903` [V] |
| Code per worker | **Each worker JIT-compiles the whole module again** | `worker_pool.rs:959-965` [V] |
| Queue | One global unbounded `Mutex` + `Condvar` FIFO; no stealing | [A] |
| `Task.Run` | Named public free or static function; scalar and finite struct/enum arguments copied to a worker-owned frame; handle is an `i64` id | `concurrency.md`; `semantic/general/calls.rs:1652-1788` [A] |
| `Wait` | Blocks the calling OS thread on `mpsc::recv` | [A] |
| Nested tasks | Rejected statically, including through helpers | `nested_concurrency.rs` |
| Cancellation | Cooperative, atomic state machine | `worker_pool.rs:31-200` [A] |
| `Parallel.For/ForEach/Reduce` | At most `worker_count` contiguous chunks, so `Reduce` grouping depends on the CPU count | [A] |
| Async | Static, parameterless, scalar result, linear body, **exactly one `await Task.Run`**; two states; frame is host `Vec<Option<ExecutionValue>>` (scalars); each `MoveNext` runs in a fresh context; driven by a blocking pump on the caller | `declarations.rs:1205-1330`, `async_machine.rs`, `task_runtime.rs:145-160`, `execution.rs:360-381` [A] |
| Worker memory | Each worker context has its own 1 GiB cap; no aggregate budget by default | [A] |
| Host operations in workers | Console, filesystem, clock, and foreign calls rejected | `concurrency.md` |
| Shared state, locks, atomics, channels | ABSENT by design | |

### 3.8 Language surface

IMPLEMENTED [A, verified against lexer, parser, examples]: classes with one constructor, computed
properties, static methods and static classes; structs with methods (a mutating method changes only
its receiver copy [V, `reference/structs.md:25`]); class-only interfaces; enums with payloads;
exhaustive enum `switch` statement and expression; generics on functions, methods, classes, structs,
interfaces, enums with interface-only `where`; `Option<T>`, `Result<T, E>`, postfix `?`; arrays and
jagged arrays; compiler-known `List<T>` and `Dictionary<K, V>`; named and default arguments;
overloads; bitwise and shift operators; hex and binary literals; `$"…"` interpolation; `foreach` over
arrays, lists, and strings; namespaces and `using`; `test void`; `unsafe foreign` scalar
declarations.

ABSENT [A]: class inheritance; constructor overloads and chaining; auto-properties; static fields;
indexers; object initializers; lambdas, closures, delegates, events; `ref`/`out`/`params`; local
functions; `try`/`catch`/`throw`; `do`-`while`; `switch` on integers, strings, or chars; general
pattern matching; `is`/`as`/`typeof`/`nameof`; `null`, `T?`, `??`, `?.`; tuples, records; attributes,
reflection; operator overloading; struct interface implementation; interface inheritance and default
methods; iterators; `using`/`Dispose`/`defer`; block comments; `%=`; exponent literals and digit
separators; `Set`, `Queue`, `Stack`.

Notable divergence from C#: an unsuffixed `1.5` is `float` [V, `specification/02-types.md:87`, listed
there as an open question].

### 3.9 Standard library, host services, FFI

- Eight namespaces: `core`, `io`, `math`, `text`, `collections`, `random`, `testing`, `time`; 45
  intrinsic bindings in `standard_library.rs` [A].
- Present: console, whole-file filesystem operations, clocks, OS entropy, logging, math.
- Absent: program arguments, environment, process exit status (the `Main` result is printed, not used
  as exit status [A]), standard error writer, sleep, timers, sockets, streaming file handles.
- **FFI.** Fixed-width scalars only, through a host-owned `ForeignRegistry`. `aster run` never
  constructs a registry (`aster-cli/src` has no reference [V]), so **a CLI user cannot call any native
  function**; only a Rust embedding host can. No dynamic loading. No pointers, strings, structs, or
  callbacks.

### 3.10 Tooling

- CLI [A]: `new`, `fetch`, `doctor`, `check`, `run [--function] [--memory-stats]`, `test`, `watch`,
  `dump-hir`, `dump-mir`. **No `build`, `fmt`, or language server.**
- `aster watch`: polls metadata every 150 ms, recompiles the whole project, new JIT, new context. No
  cache (`watch.rs`) [A].
- Diagnostics: severity, message, span, optional help. **No error codes** (`diagnostics/lib.rs:35-40`)
  [A]. Runtime errors carry a message only: no stack trace, no source position [A].
- Editors: a VS Code grammar and snippets; the grammar misses several keywords and types [A].
- Platforms: Windows x64 and Linux x64. CI: Ubuntu on push, Windows on pull requests and weekly.
  Releases through semantic-release.
- Benchmarks: `perf_baseline` (six small workloads, phase-separated), `memory_matrix`,
  `aarm_memory_matrix`. No committed baseline, no CI gate.

## 4. Document contradictions found

| # | Contradiction | Sources |
| --- | --- | --- |
| 1 | "Class inheritance is not adopted" (ACCEPTED) versus direction B "ACCEPTED as the objective" | `specification/10-open-questions.md` versus `compiler/object-model-b1.md` |
| 2 | Governor and delayed-purge policy described as live; neither is reachable by default | `reference/concurrency.md:48`, `:97`; `compiler/memory-management.md:306` [A] |
| 3 | Arena "built on Rust's zeroed host allocations" versus `VirtualAlloc`/`mmap` | `research/aarm.md:24` versus `:472-506` and `arena.rs` |
| 4 | `decimal` listed as a valid type; the compiler rejects it | `compiler/grammar.md:124`, `:157` [A] |
| 5 | Function specialization cache key described differently from the code | `compiler/monomorphization.md` versus `generics/specialization.rs:479-488` [A] |
| 6 | "Compile errors keep the watcher running" holds only after a first successful build | `compiler/watch.md` [A] |
| 7 | Entry point ACCEPTED in one chapter and listed as open in another | `specification/05`, `10` [A] |
| 8 | Chapter 13 missing; chapter 01 lists 16 keywords against 48; `CHANGELOG.md` has headers only | [A] |
| 9 | "No package manager" beside documented Git dependencies and `aster fetch` | `README.md`, `reference/packages.md` [A] |

## 4a. Benchmark harness facts (relevant to BENCH-001)

Checked for revision 2, to establish which workloads can be written today.

| Fact | Evidence |
| --- | --- |
| Scale is chosen by entry function (`RunSmall`, `RunMedium`, `RunLarge`), not by an argument | `benchmarks/memory/README.md`; `memory_matrix.rs` (`entry_function`) [V] |
| `perf_baseline` compiles a single source string; projects and multi-file front ends are not measured | `benchmarks/performance/README.md`, limitations [V] |
| `perf_baseline` does not support `Task.Run` workloads | Same [V] |
| Worker count is selectable only through research entry points | `aster-codegen-cranelift/src/lib.rs` (`execute_resolved_with_aarm_parallel_workers` and related, about lines 511-601) [V] |
| Timing is never frozen; only checksums and `MemoryStats` are | `compiler/memory-management.md`, "Reproducible comparison" [V] |
| `List<T>` has `Add`, `Get`/index, `Set`, `RemoveAt`, `Clear`; no `Remove`, `Contains`, `IndexOf` | `reference/collections.md` [V][A] |
| A recursive class field of type `Option<Node>` is not exercised by any test or example found | Search of tests, examples, reference [V]. UNVERIFIED whether it compiles |
| No static fields; no `Main` arguments | §3.8, §3.9 |

Consequence: object, collection, string, interface, and parallel workloads are expressible now.
Virtual dispatch, closures, general async, FFI overhead, and AOT size are not; the decision register
(BENCH-001) names a proxy or states that none exists.

## 5. What is architecturally sound today

1. Layering is real. The runtime has no compiler or Cranelift types. The backend consumes MIR only.
2. Semantic analysis is the single authority for names and call targets; HIR and MIR carry resolved
   symbols.
3. Generics are concrete end to end. No erasure, no boxing, no generic ABI.
4. Failure is fail-closed: unsupported MIR is rejected before code generation, and runtime failures
   are controlled, never undefined behavior in the sampled paths.
5. Memory decisions are explicit MIR (`AllocationRegion`, enter/exit instructions). The backend maps
   them mechanically.
6. Worker isolation is strict and compiler-checked. No data race is expressible.
7. No hidden global mutable state in the compiler or backend.
8. Evidence culture: decision records, phase-gated research, deterministic benchmarks.

## 6. Structural limits that block the product vision

| Limit | Blocks |
| --- | --- |
| Memory lifetime equals one bounded execution; escaping values are never freed | Servers, games, desktop, any long-running program |
| Headerless objects, no runtime type identity | Inheritance, downcasts, hot-reload migration, any per-object management |
| No function values | Callbacks, events, LINQ-style APIs, UI, general task targets |
| Async is one fixed shape; no I/O integration; no scheduler beyond a FIFO pool | Networking, services, responsive applications |
| FFI is scalar-only and unreachable from the CLI | Desktop, games, system libraries |
| JIT only | Distribution, startup, binary size |
| Positional semantic identity; whole-program recompilation; parse errors stop the pipeline | Incremental compilation, language server, hot reload |
| No inlining or devirtualization; every property access is a call | Generated-code quality for object-oriented code |
| MIR validation only inside the backend | Trust in compiler passes; a second backend |
