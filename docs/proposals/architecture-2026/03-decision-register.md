# C. Decision register (PROPOSED, revision 2)

> **Statuses and the nineteen questions are closed in [06-decision-closure.md](06-decision-closure.md).** This register keeps evidence and full records.
>
> **Original status: every entry is PROPOSED. None is an approved architecture decision.** IDs from
> revision 1 are preserved. Revision 2 corrects contracts, adds a review class to every entry, and
> adds question IDs (`Q-…`). Design detail is in the [master plan](02-master-plan.md); evidence
> locations are in the [repository map](01-repository-map.md).

Priority: P0 (blocks the vision), P1 (needed for real programs), P2 (important), P3 (later).
Effort: S (days), M (one to three weeks), L (one to two months), XL (longer). Risk: low, medium, high.

**Review class**

- **READY** — resolved by investigation; ready for analysis and approval; no experiment and no
  change to accepted behavior.
- **EXPERIMENT** — the recommendation stands only if a named experiment passes thresholds recorded
  beforehand.
- **MAINTAINER** — changes accepted or existing behavior, or is a policy choice; not assumed by any
  other entry.
- **DEFERRED** — recorded, not proposed for now.

## 1. Constraints taken as given

| Source | Constraint |
| --- | --- |
| Maintainer brief; `object-model-b1.md` status | Native, independent of .NET; no traditional GC; C#-like syntax; direction B (single inheritance) accepted as the objective |
| `object-model-b1.md` §2 | (1) Uniform header with a descriptor pointer at offset zero. (2) Immutable descriptors, one per closed class. (3) B1 keeps one constructor and adds `base(…)`; B2 adds overloads. (4) Closed fully specialized bases only. (5) Identity-preserving upcast |
| `specification/00-goals.md` | Concrete generics, no erasure or hidden boxing; no implicit null; no exception-based control flow; understandable cost; explicit parallelism |
| `specification/10-open-questions.md` | `var`/`const` rules; visibility set and defaults; one `Main`; ECS not in the language; no `null`; structs copy by value |
| `specification/14-platform-boundaries.md` | Language, standard library, runtime, SDK boundaries; compiler-known official types resolved in semantic analysis; initial FFI boundary |
| `research/aarm.md` frozen invariants | No tracing GC; no moving live objects; stable references; isolated worker contexts; typed backend-neutral IR; runtime free of Cranelift types; fail-closed allocation; deterministic first error |
| `reference/concurrency.md` | Worker isolation; explicit transfer; no injected cancellation polling; class references not transferable; no thread-local current-task state |
| `architecture.md` | Layer responsibilities |

Nothing in this register silently changes one of these. Where an entry proposes to change one, its
class is MAINTAINER and section 3.3 lists it.

## 2. Index

| ID | Decision | Recommendation | Class | Pri | Effort | Risk | Needs |
| --- | --- | --- | --- | --- | --- | --- | --- |
| BENCH-001 | Representative workload suite | Workloads expressible today, with stated proxies for the rest | READY | P0 | M | low | none |
| ARCH-001 | Semantic identity | Structural IDs replace span keys | READY | P0 | L | medium | none |
| ARCH-002 | MIR validator location | Backend-neutral validation in `aster-mir` | READY | P0 | L | medium | none |
| ARCH-003 | Type authority in HIR lowering | Read from the semantic model | READY | P1 | S | low | none |
| ARCH-004 | Generic body checking | One constraint-based check per body | READY | P2 | M | medium | ARCH-001 |
| ARCH-005 | Scheduler placement | New `aster-exec` crate | READY | P1 | M | low | none (corrected in 06 §4.D) |
| ARCH-006 | Incremental compilation | Per-declaration reuse | READY | P2 | XL | high | ARCH-001 |
| ARCH-007 | Diagnostics | Error codes; analysis after syntax errors | READY | P2 | M | low | none |
| MEM-001 | Memory model | Scalar replacement, regions, per-domain counted heap | EXPERIMENT + MAINTAINER (direction) | P0 | XL | high | BENCH-001, MEM-EXP-1 |
| MEM-002 | Count word in the object header | Amend B1 §4.1: H12 (fallback H16); descriptor pointer stays at offset 0 | EXPERIMENT + MAINTAINER (B1 amendment) | P0 | L | medium | MEM-001, OBJ-001 |
| MEM-003 | Ownership in MIR | Owned/borrowed rules of master plan §5.5, validated independently | EXPERIMENT | P0 | XL | high | MEM-002, ARCH-002 |
| MEM-004 | Cycles | `weak`; leak diagnostics; no collector | MAINTAINER (policy) | P0 | M | medium | MEM-003 |
| MEM-005 | Destruction and resources | Reclamation, drop glue, and `Dispose` separated; no implicit `Dispose` | READY | P1 | M | medium | MEM-003 |
| MEM-006 | Budgets and page return | Enable the gated governor and purge policy | EXPERIMENT | P1 | M | medium | MEM-001 |
| MEM-007 | Collections | Headers own and free their buffers; element glue | READY (with MEM-003) | P0 | M | medium | MEM-003 |
| MEM-008 | Region proofs through calls | Inline first; no new proof kind | EXPERIMENT | P2 | M | medium | PERF-002 |
| MEM-009 | Backup cycle collector | Not proposed | DEFERRED | P3 | L | high | MEM-004 |
| OBJ-001 | Header experiment | B1 stage 2 and 3 measure H8, H12, H16 in one report | READY (process) | P0 | L | medium | BENCH-001 |
| OBJ-002 | Single inheritance | B1 with U1, U2, U6, U8, U10; U5 reserved `base` | READY; U5 is MAINTAINER (breaking) | P0 | L | medium | OBJ-001 |
| OBJ-003 | Virtual dispatch | B1; properties included (U9 changed) | MAINTAINER (differs from B1's proposal) | P0 | L | medium | OBJ-002 |
| OBJ-004 | Downcast and type test | `is T x`; `as T` yields `Option<T>` (U4); lookup by search (U3) | READY; U3 choice EXPERIMENT | P1 | M | low | OBJ-003 |
| OBJ-005 | Constructor overloads and `this(…)` | B2 record; order relative to B1 | MAINTAINER (ordering) | P1 | M | low | none |
| OBJ-006 | Generic bases | Closed generic base in the generics step (U7) | READY | P2 | M | medium | OBJ-002 |
| LANG-001 | Function values | Lambdas and closures typed as `Func<…>`/`Action<…>` | READY | P0 | XL | high | MEM-003 for escaping captures |
| LANG-002 | Error model | Keep `Result`/`Option`/`?`; no exceptions | READY (confirms accepted) | P1 | S | low | TOOL-003 |
| LANG-003 | Nullable spelling | `T?`, `null`, `??`, `?.` as sugar | **MAINTAINER** | P1 | M | medium | none |
| LANG-004 | `switch` and patterns | Scalar and string subjects; `is T x`; guards | READY | P1 | M | low | none |
| LANG-005 | Auto-properties | Desugar to field and accessors | READY | P1 | S | low | none |
| LANG-006 | Static state | Per-domain statics | **MAINTAINER** | P1 | M | medium | none |
| LANG-007 | Structs and constraints | Static satisfaction; no boxing conversion | READY | P1 | M | medium | none |
| LANG-008 | Object initializers | Desugar | READY | P2 | S | low | none |
| LANG-009 | Struct mutation and `ref` | Receiver by reference; second-class `ref` | **MAINTAINER (breaking)** | P1 | M | medium | none |
| LANG-010 | Tuples | Anonymous structs | READY | P2 | M | low | none |
| LANG-011 | Operators and indexers | Statically resolved | READY | P1 | M | low | none |
| LANG-012 | Interface inheritance | Yes; default methods deferred | READY | P2 | M | low | none |
| LANG-013 | Extension methods | After instance candidates of the whole base chain | READY | P2 | S | low | none |
| LANG-014 | Iteration protocol | Pattern-based `foreach`; `yield` later | READY | P2 | M | medium | LANG-007 |
| LANG-015 | Reflection and attributes | None at run time | READY | P2 | S | low | none |
| LANG-016 | Real literal default | `double` | **MAINTAINER (breaking)** | P1 | S | low | none |
| LANG-017 | Small syntax gaps | Add | READY | P1 | S | low | none |
| LANG-018 | Entry point | `Main(string[] args)`; exit status | READY (extends the accepted `Main`) | P1 | S | low | none |
| CONC-001 | Concurrency model | Domains as actors; contracts of master plan §8 | MAINTAINER (direction) | P0 | — | high | MEM-001 |
| CONC-002 | Shared code module | Compile once | EXPERIMENT | P1 | M | medium | none |
| CONC-003 | General `async`/`await` | Full state machine; frames as heap objects | READY (after MEM-003) | P0 | XL | high | MEM-003, LANG-001 |
| CONC-004 | Scheduler | Per-thread queues plus injection | EXPERIMENT (stealing, compensation) | P1 | L | medium | ARCH-005 |
| CONC-005 | Transfer | Copy of value-like data; class instances do not cross | READY | P1 | L | medium | MEM-003 |
| CONC-005b | Move of unique graphs; page handoff | Not proposed | DEFERRED | P3 | L | high | CONC-005 |
| CONC-006 | Channels | Bounded `Channel<T>` | READY | P1 | M | low | CONC-003, CONC-005 |
| CONC-007 | Structured concurrency and failure | `TaskScope`; failure terminates the domain; `TryWait` | READY; domain-failure rule is MAINTAINER | P1 | L | medium | CONC-003 |
| CONC-008 | Asynchronous I/O | Runtime reactor | READY; dependency policy is MAINTAINER | P1 | XL | high | CONC-003, CONC-004 |
| CONC-009 | Reproducible `Reduce` | Chunk plan independent of core count | **MAINTAINER (observable change)** | P2 | S | low | none |
| CONC-010 | Shared immutable data | Not proposed | DEFERRED | P3 | L | high | MEM-003 |
| PERF-001 | AOT | `cranelift-object`, static runtime, `aster build` | EXPERIMENT (size, startup) | P0 | XL | medium | ARCH-005 |
| PERF-002 | Inlining and devirtualization | MIR passes | EXPERIMENT | P1 | L | medium | ARCH-002 |
| PERF-003 | Backend choice | Cranelift only through 1.0 | READY | P2 | — | low | none |
| PERF-004 | Tiered JIT and PGO | Not now | DEFERRED | P3 | — | low | PERF-001 |
| PERF-005 | Measurement discipline | Deterministic invariants gate; performance metrics advise | READY | P1 | M | low | BENCH-001 |
| HOT-001 | Reload indirection | Development profile only; slots keyed by declaration and signature | READY | P2 | L | medium | ARCH-001 |
| HOT-002 | Stable descriptors and literals | Session-stable storage in development | READY | P2 | M | medium | OBJ-003, HOT-001 |
| HOT-003 | State and type changes | State kept; type ABI change under four conditions, else restart | READY | P2 | L | high | MEM-001, LANG-006 |
| HOT-004 | Safe points and unloading | Turn boundaries; global safe point with timeout; retain generations in v1 | READY | P2 | L | high | CONC-003 |
| PLAT-001 | General FFI | Libraries, `ptr<T>`, C-layout structs, callbacks | READY; callback thread-local is MAINTAINER | P0 | XL | high | none |
| PLAT-002 | Binary interface | C ABI is the only public ABI | READY | P2 | S | low | PERF-001 |
| PLAT-003 | Package registry | Not proposed | DEFERRED | P3 | XL | medium | none |
| PLAT-004 | More targets | macOS and arm64 after AOT; others research | READY | P2/P3 | L | medium | PERF-001 |
| TOOL-001 | Formatter | `aster fmt` | READY | P2 | M | low | none |
| TOOL-002 | Language server | On ARCH-001 and ARCH-007 | READY | P2 | XL | medium | ARCH-001, ARCH-007 |
| TOOL-003 | Debug information | Positions in failures; DWARF/CodeView in AOT | READY | P1 | L | medium | PERF-001 |
| TOOL-004 | Profiling | Frame pointers and perf map | READY | P3 | S | low | PERF-001 |
| DOC-001 | Document corrections | Fix the contradictions in map §4 | READY | P1 | S | low | none |

## 3. What needs a decision, by kind

### 3.1 Experiments (thresholds recorded by the maintainer before the run)

| Experiment | Question | Decides |
| --- | --- | --- |
| MEM-EXP-1 (= B1 stage 3) | H8 without counting versus H12 and H16 with counting: time, peak and final live bytes, count operations emitted, elided, executed | MEM-001, MEM-002, MEM-003, OBJ-001 |
| CONC-EXP-1 | Can one finalized JIT module serve all worker threads | CONC-002 |
| PERF-EXP-1 | MIR inlining gain on `accessor_heavy`; compile-time cost | PERF-002, MEM-008 |
| PERF-EXP-2 | AOT binary size, startup, run time against JIT | PERF-001 |
| CONC-EXP-2 | Task memory and spawn cost; scaling; need for stealing or compensation threads | CONC-003, CONC-004 |
| MEM-EXP-2 | Fast-path cost of the governor and purge policy when enabled by default | MEM-006 |
| OBJ-U3 | Interface lookup by search versus fixed index | OBJ-004 |

### 3.2 Questions that are the maintainer's choice

| ID | Question | Options | Recommendation |
| --- | --- | --- | --- |
| Q-MEM-0 | Accept reference counting as the third tier, subject to MEM-EXP-1 | Yes; no (keep regions, add explicit scoped arenas) | Yes |
| Q-MEM-1 | Release point of an owned local | Last use; end of lexical scope | Last use, with `Memory.KeepAlive`. Observable only through `weak` |
| Q-MEM-2 | May the runtime close a standard-library handle when its wrapper is reclaimed | No (teardown plus diagnostics); yes | No |
| Q-MEM-3 | Cycles are the programmer's responsibility | Yes; require a collector (MEM-009) | Yes |
| Q-OBJ-1 | Amend B1 §4.1 to allow a count word (H12 or H16) | Yes after MEM-EXP-1; no (H8) | Yes, decided at stage 3 |
| Q-OBJ-2 | Virtual properties with virtual methods (U9) | Methods only; both | Both |
| Q-OBJ-3 | B2 (constructor overloads) before or after B1 | Before; after | Before |
| Q-OBJ-4 | `base` reserved (U5) | Reserved; contextual | Reserved |
| Q-LANG-1 | `T?`/`null` sugar (LANG-003) | Yes; no | Yes |
| Q-LANG-2 | Struct receiver by reference (LANG-009) | Yes; no | Yes |
| Q-LANG-3 | Real literal default `double` (LANG-016) | Yes; no | Yes |
| Q-LANG-4 | Static fields per domain (LANG-006) | Per domain; none | Per domain |
| Q-CONC-0 | Domains as the permanent model; no shared-memory threads | Yes; no | Yes |
| Q-CONC-1 | At `Main` return, running worker domains are | Abandoned; awaited | Abandoned after cancellation is requested |
| Q-CONC-2 | One runtime-internal thread-local for FFI callbacks | Allow; user-data only | Allow, narrowly |
| Q-CONC-3 | A runtime failure terminates | The whole domain; only the task | The whole domain |
| Q-CONC-4 | `Reduce` chunk plan independent of core count (CONC-009) | Yes; document as machine-dependent | Yes |
| Q-CONC-5 | A small I/O polling dependency in the runtime | Allow; in-tree | Decide with CONC-008 |
| Q-LANG-5 | Exceptions stay rejected | Confirm | Confirm |

### 3.3 Breaking changes, all pending

`base` reserved (OBJ-002/U5); class instance size (MEM-002); real literal `double` (LANG-016); struct
receiver by reference (LANG-009); `Reduce` grouping (CONC-009). No READY entry depends on any of
them.

## 4. Full records

### BENCH-001 — Representative workloads, checked against today's syntax

1. **Problem.** No workload represents a long-running or object-oriented program.
2. **Evidence.** Six small workloads in `benchmarks/performance`; no baseline; readiness audit D6,
   P2-5.
3. **Alternatives.** Decide by argument; port external suites; add focused workloads.
4. **Recommendation.** Add focused workloads to the existing harnesses.
5. **Harness facts that constrain the design.**
   - Scale is selected by entry function (`RunSmall`, `RunMedium`, `RunLarge`), not by an argument.
   - `perf_baseline` compiles one source string; no project or multi-file front end.
   - `perf_baseline` does not support `Task.Run` workloads (`benchmarks/performance/README.md`,
     limitations).
   - Worker count is selectable only through the research entry points
     (`execute_resolved_with_aarm_parallel_workers` and related, `aster-codegen-cranelift/src/lib.rs`).
   - No static fields: state is passed through objects.
   - One context is capped at 1 GiB and a dictionary at 100,000 entries and 64 MiB.
6. **Feasibility of each workload today.**

| Workload | Measures | Expressible today | Note or proxy |
| --- | --- | --- | --- |
| `longrun_frame` | Steady-state memory of a frame loop that replaces entities | Yes: classes, `List<T>` with index assignment and `RemoveAt` | Today memory grows; `RunLarge` is expected to end in the controlled cap failure. **That outcome is recorded as a deterministic result, not treated as a harness error** |
| `longrun_request` | Per-request build, process, drop | Yes | Second variant: each request in `Task.Run` built from an integer seed, since strings cannot cross today |
| `cache_churn` | Dictionary insert and remove at steady size | Yes, with `int` or `string` keys | Capped at 100,000 live entries; tombstones accumulate; record where it fails |
| `object_graph` | Tree build and traversal | Yes with `Node[] children` | A recursive `Option<Node>` field is UNVERIFIED; use arrays |
| `accessor_heavy` | Property reads in a hot loop | Yes (computed properties) | Auto-properties absent; write accessors by hand |
| `interface_dispatch` | Indirect calls | Yes | Extends `calls_and_interfaces` |
| `collection_growth` | Growth and old-buffer retention | Yes | |
| `string_pipeline` | Split, replace, build | Yes | No string indexing; uses the library calls |
| `parallel_scaling` | Speedup by worker count | Yes, scalar bodies | Needs the research worker-count entry points promoted to a benchmark-visible option |
| `task_swarm` | Spawn and join cost | Partly: `Task.Run` with scalars and `WaitAll` | Needs a harness path for task programs. Suspended-task memory is **not measurable** today |
| `frontend_large` | Front-end time on a large program | Yes | Source produced by a deterministic generator; single file |
| Virtual dispatch | — | **No** | Proxy: `interface_dispatch`. Real workload added by the pull request that implements OBJ-003 |
| Closures | — | **No** | Proxy: a class with one method behind an interface. Real workload with LANG-001 |
| General async | — | **No** | No proxy. Workload added with CONC-003; its baseline is its own first measurement |
| FFI call overhead | — | **No** from the CLI | Rust-host registry only; added with PLAT-001 |
| AOT size and startup | — | **No** | Added with PERF-001 |

7. **Rule for features that do not exist yet.** The pull request that introduces a feature adds its
   workload and records the first measurement. A proxy is labeled as a proxy in every report and is
   never used to claim a result about the real feature.
8. **Contract.** Every workload returns a checksum computed independently of ASTER and reports the
   class A invariants of PERF-005. Timings are class B.
9. **Dependencies.** None.
10. **Steps.** Workloads in two pull requests; harness support for task programs and worker count;
    baseline procedure.
11. **Risk low, priority P0, effort M.**
12. **Invalidation.** None. A workload that cannot be written is recorded as a language gap.

### MEM-001 — Memory model

1. **Problem.** Escaping values are never reclaimed during a run.
2. **Evidence.** Map §3.6; readiness audit §4; `temporary_subregions/validation.rs:903-916`;
   `context.rs:2953-3019`.
3. **Alternatives and comparison.** Master plan §5.2.
4. **Recommendation.** Four storage classes fixed at allocation (eliminated, region, counted,
   immortal); no run-time promotion (master plan §5.3).
5. **Contract.**
   - A value proven local keeps today's behavior and cost.
   - Acyclic counted garbage is freed at the release point the compiled program fixes.
   - Reclamation moves nothing and scans nothing.
   - Heap invariants H1 to H5 hold (master plan §5.4).
   - The cost of freeing is proportional to what is freed, at that point.
6. **Consequences.** Runtime: a size-class allocator beside the arenas. MIR: ownership
   instructions. Compiler: an ownership pass after escape analysis; escape analysis also decides
   pins (H2) and the collection rule (H3). Validator: obligations U1 to U8. Owned Persistent
   regions become redundant for counted types and are retired only after measurement.
7. **Dependencies.** BENCH-001; OBJ-001; ARCH-002.
8. **Steps.** MEM-EXP-1 for class references; report; strings and arrays; collections; aggregates
   and interface values; `weak`; retire "Persistent until exit" for migrated kinds.
9. **Tests.** Final live bytes equal at N and 10N; tier-1 invariants unchanged; differential run
   with elision disabled; poisoning allocator; sanitizer.
10. **Risk high, priority P0, effort XL. Class: EXPERIMENT and MAINTAINER (Q-MEM-0).**
11. **Invalidation.** Thresholds fail; or the validator cannot check the borrow rules without
    source annotations. Fallback: regions plus explicit scoped arenas; B1 continues with H8.

### MEM-002 — Count word (amendment to B1 §4.1)

1. **Problem.** Counting needs a per-object count; B1's header has only the descriptor pointer.
2. **Evidence.** B1 §4.1 rules 1, 2, 6; `object.rs:9`.
3. **Alternatives.** H8, H12, H16; side table; 32-bit descriptor index (rejected: it would replace
   the approved pointer at offset zero).
4. **Comparison.** Master plan §6.2, including computed sizes for B1's worked examples.
5. **Recommendation.** H12; H16 as fallback. **Approved decision 1 is preserved in both.**
6. **Contract.** Descriptor pointer at offset 0. Count word at offset 8. First field at
   `align_up(H, a)`. Only the allocation sequence writes offset 0; only allocator, retain, and
   release write the count word. Strings, arrays, and collections carry the count in their own
   headers and are outside B1.
7. **Consequences.** B1 §4.1 rules 1, 2, 6, the worked table, §4.4, cases P11, P12, V2. Layout
   code; pinned-size tests; memory statistics.
8. **Dependencies.** MEM-001; OBJ-001.
9. **Steps.** B1 stage 1 with `H` as a named constant; stage 2 with `H` as a build parameter; stage
   3 report.
10. **Tests.** B1 layout cases recomputed per variant; validator rejects any other store to
    offsets 0 to 11 (or 15).
11. **Risk medium, priority P0, effort L. Class: EXPERIMENT and MAINTAINER (Q-OBJ-1).**
12. **Invalidation.** MEM-001 rejected, or size cost above threshold: H8, B1 unchanged.

### MEM-003 — Ownership as explicit, independently validated MIR

1. **Problem.** Where count operations go must be decided once and be checkable.
2. **Evidence.** Region operations are already explicit MIR; `memory-management.md` names
   retain/release placement as the missing evidence.
3. **Alternatives.** Backend inserts counts (violates layering); runtime smart pointers (cannot
   elide); a MIR pass.
4. **Recommendation.** A MIR pass, with the rules of master plan §5.5.
5. **Contract (summary; full text in master plan §5.5 and §5.6).**
   - Owned sources: allocation, call result, upgraded load, weak read.
   - Borrowed sources: parameters; loads inside their borrow window.
   - G1: a borrowed argument needs a guarantor the callee cannot release.
   - Consuming: stores into counted objects, return, transfer.
   - Store order: load old, store new, release old.
   - Release after last use, or after region exit for pins.
   - Balance on every normal path; failure paths exempt because the domain terminates.
   - Aggregates through copy and drop glue; collections call element glue; closures own their
     environment; async frames own everything live across a suspension; async calls retain their
     arguments.
6. **Consequences.** New MIR instructions `Retain`, `Release`, `Drop`, weak load and store; glue
   functions per type; runtime collection entry points take glue; validator obligations U1 to U8.
7. **Dependencies.** MEM-002; ARCH-002.
8. **Steps.** Class references; strings and arrays; aggregates; interface values; collections;
   closures; async frames.
9. **Tests.** Golden MIR per rule; one negative validator case per obligation, hand-written; the
   field-overwritten-by-callee case for G1; differential runs.
10. **Risk high, priority P0, effort XL. Class: EXPERIMENT.**
11. **Invalidation.** The validator cannot be written independently of the pass.

### MEM-004 — Cycles, weak references, leak detection

1. **Problem.** Counting does not free strong cycles.
2. **Alternatives.** Automatic collector; `weak` plus diagnostics; forbid cyclic types.
3. **Recommendation.** `weak` plus diagnostics.
4. **Contract.** Master plan §5.8: a cycle lives until its domain ends and is never a dangling
   reference; only acyclic garbage has a fixed release point; weak read is retain-or-none in one
   step; weak store is an escape; weak cell allocated on first use; tools and their limits as
   tabulated there.
5. **Consequences.** Parser (modifier), semantic analysis, MIR (weak load and store), runtime (weak
   cell, per-domain table), development allocator (allocation sites, enumeration, reference maps).
6. **Dependencies.** MEM-003. Independent of LANG-003: the field type is `Option<T>` unless the
   sugar is approved.
7. **Tests.** Parent and child freed; observer list freed; weak read after free yields none;
   deliberate cycle appears in the teardown report with types and sites; `DetectCycles` finds it.
8. **Risk medium, priority P0, effort M. Class: MAINTAINER (Q-MEM-3, Q-MEM-1).**
9. **Invalidation.** Field experience shows cycle leaks are common despite the tools; then MEM-009
   is put to the maintainer.

### MEM-005 — Reclamation, drop glue, and `Dispose`

1. **Problem.** Revision 1 let drop glue call `Dispose`. That is a finalizer by another name.
2. **Alternatives.** (a) Implicit `Dispose` at count zero. (b) Explicit `Dispose` only. (c) Explicit,
   plus the runtime closing standard-library handles at reclamation.
3. **Comparison.** (a) runs user code at optimizer-chosen points, inside stores, and cannot apply to
   region objects. (c) runs no user code but ties closing time to release placement.
4. **Recommendation.** (b). (c) is Q-MEM-2.
5. **Contract.** Master plan §5.7: three separate mechanisms; `Dispose` runs only when called;
   `using` runs it on every normal exit and not after a runtime failure; per-domain handle table
   closed at teardown; warning for an undisposed local; development report of open handles.
6. **Consequences.** `IDisposable` in `aster.core`; `using` statement and declaration; handle table
   in the runtime; a lint.
7. **Dependencies.** None for `using` itself; MEM-003 for drop glue.
8. **Tests.** `Dispose` exactly once on fallthrough, `return`, `break`, `continue`, `?`; not after a
   failure; handle closed at teardown when leaked; no call to `Dispose` when a counted wrapper is
   reclaimed.
9. **Risk medium, priority P1, effort M. Class: READY.**
10. **Invalidation.** None.

### OBJ-001 — One measurement report for B1 stage 3 and counting

1. **Problem.** Measuring the B1 header alone, then changing it for counting, migrates layout,
   tests, and benchmarks twice.
2. **Recommendation.** Keep B1's stages. Stage 2 implements the first-field offset as a parameter;
   stage 3 measures H8 (no counting), H12, and H16 (with counting) in one report.
3. **Contract.** B1 is not blocked on MEM-001: stage 1 acceptance needs no measurement, and if
   MEM-001 is rejected B1 proceeds with H8 as written.
4. **Dependencies.** BENCH-001.
5. **Risk medium, priority P0, effort L. Class: READY (process).**
6. **Cross-check.** The eighteen relations between B1 and the new proposals are tabulated in master
   plan §6.3. No approved B1 decision is contradicted.

### CONC-001 — Domains as actors

1. **Problem.** A fixed set of intrinsics over a FIFO pool cannot express a server or a responsive
   application.
2. **Alternatives.** Shared-memory threads with locks (races; atomic counts); stackful coroutines (a
   stack per task; stack switching in the backend); ownership typing (needs the surface the language
   avoids); stackless async on isolated domains.
3. **Recommendation.** Stackless async on isolated domains.
4. **Contract.** Master plan §8:
   - §8.2 turns and reentrancy: a synchronous call never yields; state may change across `await`.
   - §8.3 a task without `await` occupies its domain; no preemption.
   - §8.4 blocking table; `Wait()` never runs tasks of its own domain; same-domain `Wait()` on an
     incomplete task is a controlled failure.
   - §8.5 fairness at turn granularity.
   - §8.6 a failure terminates its domain; domain lifetimes.
   - §8.7 transfer by copy; class instances do not cross.
   - §8.8 FFI callbacks.
5. **Consequences.** CONC-002 to CONC-009; the static ban on nested tasks is lifted in steps.
6. **Dependencies.** MEM-001.
7. **Tests.** Existing stress and determinism suites at every step; a test per row of the blocking
   table; reentrancy across `await` observed; failure isolation between domains.
8. **Risk high, priority P0. Class: MAINTAINER (Q-CONC-0, Q-CONC-1, Q-CONC-3).**
9. **Invalidation.** A required workload needs shared mutable memory that transfer and channels
   cannot serve; then CONC-010 is promoted.

### CONC-003 — General `async`/`await`

1. **Problem.** One accepted shape; frames are host vectors of scalars; each step runs in a fresh
   context, so no reference survives an `await`.
2. **Recommendation.** A MIR-to-MIR pass after optimization: split at each `await`, compute live
   locals with the existing liveness analysis, emit a typed frame object and a resume function.
3. **Contract.** `async` on any function or method, with parameters; `await` in any expression
   position; `Task<T>` for any `T` within a domain and for transferable `T` across domains. All
   steps of a task run in its domain. The ownership rules for frames are those of MEM-003.
4. **Dependencies.** MEM-003, LANG-001, ARCH-005.
5. **Tests.** Existing async tests; each control-flow shape; a reference held across `await`;
   cancellation at each state.
6. **Risk high, priority P0, effort XL. Class: READY after MEM-003.**
7. **Invalidation.** Frame liveness cannot be validated; then frames hold all locals.

### PERF-001 — Ahead-of-time compilation

1. **Problem.** No distributable program.
2. **Alternatives.** Ship the JIT; `cranelift-object` plus system linker; a C or LLVM backend.
3. **Recommendation.** `cranelift-object`.
4. **Contract.** `aster build` produces a native executable with the program and the needed runtime
   as a static library and no compiler. Behavior equals `aster run`.
5. **Dependencies.** ARCH-005.
6. **Tests.** Every example and JIT test re-run as AOT with identical output.
7. **Risk medium, priority P0, effort XL. Class: EXPERIMENT for size and startup; the direction is
   READY.**

### PERF-005 — Measurement discipline

1. **Problem.** Revision 1 called for an "instruction-count gate" without separating exact
   invariants from performance.
2. **Recommendation.** Two classes, never mixed (master plan §9.3).
   - **A. Deterministic invariants**: checksums, allocation counts and bytes, final and peak live
     bytes, count operations emitted and executed, MIR instruction counts, specialization counts,
     code size for a pinned Cranelift, validator findings. Compared for equality. May gate CI. Not
     performance.
   - **B. Performance metrics**: wall time, resident memory, hardware counters. Reference machine,
     interleaved A/B, median and spread. Advisory only; never a gate.
3. **Contract.** A claim of speed or memory benefit needs class B evidence. A change in a class A
   value is explained in the pull request that causes it.
4. **Dependencies.** BENCH-001.
5. **Risk low, priority P1, effort M. Class: READY.**

### HOT-001 to HOT-004 — Hot reload

1. **Problem.** `aster watch` restarts; nothing survives.
2. **Recommendation.** Development-profile indirection with ABI-keyed slots, stable descriptors,
   session-immortal literals, turn-boundary safe points, and restart whenever a rule is not provably
   met.
3. **Contract.** Master plan §10:
   - §10.2 what is indirect, and why literals move out of generations.
   - §10.3 the table of change classes.
   - §10.4 safe points; global safe point with timeout.
   - §10.5 four conditions for a type ABI change without migration.
   - §10.6 every kind of reference into an old generation and the unload rule; first version
     retains all generations.
4. **Dependencies.** ARCH-001, ARCH-006, MEM-001, LANG-006, OBJ-003, CONC-003.
5. **Tests.** Body change with state preserved; signature change with an old caller still running;
   virtual body change with a live object; type change with a live instance forces restart and
   names the condition; a suspended task finishes on old code; release build has no slot loads.
6. **Risk high, priority P2. Class: READY.**
7. **Invalidation.** Whole-program passes make incremental recompilation too slow for the loop;
   then hot reload is limited to body changes of non-generic functions.

### PLAT-001 — General FFI

1. **Problem.** No native library is callable from the CLI; only scalars cross.
2. **Recommendation.** A declarative C ABI in the language, keeping host registration for embedders.
3. **Contract.** Library named on the declaration and in `Aster.toml`; `ptr<T>` and `nint` only
   inside `unsafe`; C-layout structs; explicit `CString`; static functions convertible to C function
   pointers under master plan §8.8; a foreign call fails only by returning.
4. **Steps.** Library loading for scalar signatures; pointers and `CString`; C-layout structs;
   callbacks; handle wrappers.
5. **Risk high, priority P0, effort XL. Class: READY; Q-CONC-2 for callbacks without user data.**

## 5. Compact records

| ID | Problem and evidence | Choice and contract | Tests | Reversal trigger |
| --- | --- | --- | --- | --- |
| ARCH-001 | Side tables keyed by span (`semantic/mod.rs:23-29`, `:213-217`); `DeclarationKey` unused | `DeclarationKey` for declarations plus a per-declaration expression index; spans only for diagnostics | Identical model after a whitespace-only edit | Clones from monomorphization cannot get distinct IDs cheaply |
| ARCH-002 | Only validator is in the backend | Structural, type, CFG, region, and ownership checks in `aster-mir`; ABI support checks stay | Existing tests move unchanged; run after each pass | The split duplicates layout knowledge |
| ARCH-003 | `hir_lowering/types.rs:206-223` | HIR reads the model's type; corpus agreement test | Agreement over all fixtures | None |
| ARCH-004 | Bodies checked only per specialization | Keep specialization; add one constraint-based check | Error at the declaration | Constraints too weak |
| ARCH-005 | Scheduler in the Cranelift crate | `aster-exec`, no Cranelift dependency | Existing suites | None |
| ARCH-006 | Whole-program recompilation | Fingerprints; reuse of HIR, MIR, code | Same output with and without cache | Whole-program passes defeat reuse |
| ARCH-007 | No codes; parse errors stop analysis | Stable codes; analysis on the recovered tree | Golden diagnostics | None |
| MEM-006 | Governor and purge unreachable; workers × 1 GiB | Default aggregate budget; idle pages returned | Research tests become production tests | Fast-path regression (MEM-EXP-2) |
| MEM-007 | Old buffers leak (`context.rs:2953-3019`, `:2026`) | Header owns the buffer; growth frees; element glue; `Remove` compacts on a threshold | Steady churn flat | None |
| MEM-008 | Call barrier in fine reclaim | Inline first, same proofs | AARM matrix with inlined bodies | Inliner too conservative |
| OBJ-002 | No inheritance | B1 §5 to §8 | B1 §13 cases | Stage 3 thresholds fail |
| OBJ-003 | — | B1 plus virtual properties; a virtual property on `this` in a constructor is rejected | B1 cases plus accessor overrides | Maintainer keeps U9 as methods only |
| OBJ-004 | No downcast | `is T x`; `as T` yields `Option<T>` | B1 V4 | None |
| OBJ-005 | One constructor | Overloads; `this(…)`; initializers run once, in the constructor that calls `base(…)` | Chaining order; cycle error | None |
| OBJ-006 | U7 | With generics in hierarchies | `class A : Box<int>` | Specialization ordering invasive |
| LANG-001 | No function values | `{code, environment}`; C# capture semantics; distinct concrete type per signature; capture of `this` in a constructor rejected | Capture observed after mutation; stored closure outlives creator | None |
| LANG-002 | No positions in failures | Keep `Result`; a failure is not catchable inside its domain | Failure text has file and line | Maintainer chooses exceptions |
| LANG-004 | Enum-only `switch` | Scalar and string subjects; `is T x`; guards. Exhaustiveness for enums only | Lowering | None |
| LANG-005 | No auto-properties | Private field plus trivial accessors | Accessor inlined to a field access | None |
| LANG-007 | Only classes implement interfaces | Static satisfaction; conversion to an interface value is an error | Generic sort over `int` and a struct | Demand for boxed struct values |
| LANG-008 | — | `new T(args) { A = 1 }` | Evaluation order | None |
| LANG-010 | — | Anonymous struct; deconstruction | Layout equals the equivalent struct | None |
| LANG-011 | — | Static operator methods; indexers | Resolution | None |
| LANG-012 | — | Interfaces extend interfaces | Dispatch through a base interface | None |
| LANG-013 | — | Considered after instance members of the whole base chain | Shadowing | None |
| LANG-014 | Compiler-known `foreach` | Pattern-based, statically dispatched | No allocation for a user collection | None |
| LANG-015 | — | No runtime metadata; `nameof` | No member-name tables in the binary | A library needs compile-time generation |
| LANG-017 | — | `do`-`while`, `%=`, block comments, exponents, separators | Lexer and parser | None |
| LANG-018 | `Main` takes nothing; result printed | `Main()` or `Main(string[] args)`; `int` is the exit status | CLI tests | None |
| CONC-002 | Each worker compiles the module (`worker_pool.rs:959-965`) | One module owned by the execution; outlives every domain | One compilation per run | CONC-EXP-1 fails |
| CONC-004 | Global FIFO; blocking waits | Per-thread queues plus injection; turn budget; helping wait with bounded depth | Fairness and throughput workloads | Imbalance measured |
| CONC-005 | Scalars only (`calls.rs:1652-1788`) | Copy of strings, arrays, lists, dictionaries, structs, enums of transferables; serialize then materialize | Copy independence; class instance rejected | None |
| CONC-006 | — | Bounded `Channel<T>` | Ordering, backpressure, close | None |
| CONC-007 | Flat tasks | `TaskScope`; cancellation down; first failure cancels siblings; `TryWait` | Failure and cancellation matrices | None |
| CONC-008 | No sockets or timers | Reactor; suspend on I/O | Echo server; timers | Dependency policy |
| PERF-002 | No inlining | Size-budgeted inliner; devirtualize on `sealed` and exact type; closed hierarchy only in whole-program AOT | `accessor_heavy` A/B | Compile-time cost in the JIT profile |
| PERF-003 | — | Cranelift only | — | A measured gap PERF-002 cannot close |
| PLAT-002 | — | C ABI only | Header generation | None |
| PLAT-004 | Two targets | macOS and arm64 after AOT | CI matrix | None |
| TOOL-001 | — | One style, no options | Idempotence | None |
| TOOL-002 | — | On the incremental front end | Protocol tests | None |
| TOOL-003 | — | Address-to-source table; DWARF and CodeView | Failure shows file and line | None |
| DOC-001 | Map §4 | Correct the wrong document in each pair | Review | None |

Entries of class MAINTAINER that are not expanded above (LANG-003, LANG-006, LANG-009, LANG-016,
CONC-009) are specified in master plan §7.1 with what each breaks and what happens if it is not
approved.
