# D. Executable technical roadmap (PROPOSED, revision 2)

> **Superseded in status by [06-decision-closure.md](06-decision-closure.md) and converted into tasks in [07-tasks.md](07-tasks.md).** Gates marked M below are closed by that document.
>
> **Original status: PROPOSED.** This is not the public roadmap (`docs/roadmap.md`), which stays
> unchanged. IDs refer to the [decision register](03-decision-register.md). Effort is an estimate for
> one experienced contributor, not a schedule.

Every pull request follows the repository's rule: a feature is complete only when syntax, semantics,
HIR, MIR, backend, diagnostics, tests, an example, and the reference agree. Public documents change
only in the pull request that implements the behavior.

**Gate column.** `—` needs no further decision beyond normal review. `M` waits for the maintainer's
answer to the named question. `E` waits for the named experiment. A gated row is skipped, not
blocked on: nothing ungated depends on it unless stated.

## Dependency graph

```text
BENCH-001 ─► B1 stage 2/3 + MEM-EXP-1 ─► MEM-002/003 ─┬─► MEM-004/005/007
                                                      ├─► OBJ-002..004 (also possible on H8 alone)
                                                      ├─► LANG-001 escaping closures ─► CONC-003 ─► CONC-004..008
                                                      └─► HOT-003
ARCH-002 validator ─► MEM-003, PERF-002 ─► MEM-008
ARCH-001 identity ─► ARCH-006 ─► HOT-001..004, TOOL-002
ARCH-005 ─► PERF-001 ─► PLAT-002, PLAT-004, TOOL-003   (CONC-002 independent; see 06 §4.D)
PLAT-001 (independent start)
Phase 1 additive items (independent)
```

Inheritance does not depend on counting. If MEM-001 is rejected, OBJ-002 to OBJ-004 proceed on B1's
8-byte header exactly as B1 describes.

## Phase 0 — Foundations (no language change)

| PR | Content | IDs | Effort | Risk | Gate |
| --- | --- | --- | --- | --- | --- |
| 0.1 | Correct the document contradictions | DOC-001 | S | low | — |
| 0.2 | Workloads expressible today, part 1: `longrun_frame`, `longrun_request`, `cache_churn`, `object_graph` | BENCH-001 | M | low | — |
| 0.3 | Part 2: `accessor_heavy`, `interface_dispatch`, `collection_growth`, `string_pipeline`, `frontend_large` (with its generator) | BENCH-001 | M | low | — |
| 0.4 | Harness support for task programs and a worker-count option; `task_swarm`, `parallel_scaling` | BENCH-001 | M | low | — |
| 0.5 | Class A invariants recorded and gated in CI; class B protocol documented | PERF-005 | M | low | — |
| 0.6 | HIR reads types from the semantic model; agreement test | ARCH-003 | S | low | — |
| 0.7 | MIR validator into `aster-mir` (mechanical move, then run after each pass in tests) | ARCH-002 | L | medium | — |
| 0.8 | One shared JIT module | CONC-002 | M | medium | E: CONC-EXP-1 |
| 0.9 | Structural semantic identity | ARCH-001 | L | medium | — |
| 0.10 | Diagnostic codes | ARCH-007 | S | low | — |

- **Acceptance.** Existing tests unchanged. Each workload has an independent checksum and runs in CI
  at small scale. A workload that ends in the controlled memory-cap failure at a larger scale
  records that as its deterministic outcome. The validator runs after each pass with no finding. A
  whitespace-only edit yields an identical semantic model.
- **Baseline.** Class A values recorded per target. Class B values recorded on the reference machine,
  as a starting point and not as a target.
- **Not measurable in this phase.** Virtual dispatch, closures, general async, FFI overhead, AOT size
  and startup. Their workloads arrive with their features (BENCH-001 rule).
- **Stop or replan.** If 0.8 fails its experiment, keep per-worker compilation and record the cost.
  If 0.9 cannot give stable IDs to specialization clones, stop before Phase 4 depends on it.

## Phase 1 — Everyday surface

### 1a. Additive, ungated

| PR | Content | IDs | Effort |
| --- | --- | --- | --- |
| 1.1 | `Main(string[] args)`, exit status, standard error, environment | LANG-018 | S |
| 1.2 | `do`-`while`, `%=`, block comments, exponent literals, digit separators | LANG-017 | S |
| 1.3 | Auto-properties | LANG-005 | S |
| 1.4 | `switch` on integers, chars, strings; `when` guards | LANG-004 | M |
| 1.5 | Structs and primitives satisfy interfaces statically | LANG-007 | M |
| 1.6 | Operators and indexers | LANG-011 | M |
| 1.7 | Interface inheritance; extension methods; object initializers | LANG-012, 013, 008 | M |
| 1.8 | Library growth written in ASTER: list search and removal, set, queue, stack, string case and formatting | — | M |

### 1b. Waiting for the maintainer

| PR | Content | ID | Gate |
| --- | --- | --- | --- |
| 1.9 | Constructor overloads and `this(…)` (B2 record first) | OBJ-005 | M: Q-OBJ-3 |
| 1.10 | `T?`, `null`, `??`, `?.` | LANG-003 | M: Q-LANG-1 |
| 1.11 | Static fields per domain | LANG-006 | M: Q-LANG-4 |
| 1.12 | Struct receiver by reference; `ref` parameters (breaking) | LANG-009 | M: Q-LANG-2 |
| 1.13 | Real literals default to `double` (breaking) | LANG-016 | M: Q-LANG-3 |

- **Acceptance.** Positive and negative tests, an example, a reference page per feature. Class A
  invariants unchanged or explained.
- **Note.** No 1a item, and no later ungated item, depends on a 1b item.

## Phase 2 — Memory and object model (critical path)

| PR | Content | IDs | Effort | Risk | Gate |
| --- | --- | --- | --- | --- | --- |
| 2.0 | B1 stage 1: design acceptance, first-field offset as the named constant `H`; U resolutions | OBJ-002..004 | S | low | M: Q-OBJ-1, Q-OBJ-2, Q-OBJ-4 |
| 2.1 | B1 stage 2 as an unmerged branch: header with `H` as a build parameter; size-class allocator; ownership pass and validator for class references | MEM-001..003, OBJ-001 | L | high | M: Q-MEM-0 (to include counting) |
| 2.2 | B1 stage 3 report: H8 without counting, H12 and H16 with counting | — | S | — | thresholds recorded first |
| 2.3 | Merge the accepted header | MEM-002 or B1 as written | L | medium | E: 2.2 |
| 2.4 | Counting for class references | MEM-003 | L | high | E: 2.2 |
| 2.5 | Strings and arrays | MEM-003 | L | high | E: 2.2 |
| 2.6 | Collections own and free buffers; element glue | MEM-007 | M | medium | E: 2.2 |
| 2.7 | Aggregates and interface values | MEM-003 | M | medium | E: 2.2 |
| 2.8 | `weak`; teardown report; `Memory.DetectCycles()` | MEM-004 | M | medium | M: Q-MEM-3, Q-MEM-1 |
| 2.9 | `IDisposable`, `using`, handle table, lint | MEM-005 | M | medium | — (`using` itself needs no counting) |
| 2.10 | Governor and page return by default | MEM-006 | M | medium | E: MEM-EXP-2 |
| 2.11 | Single inheritance (B1 PR 1) | OBJ-002 | L | medium | 2.3 |
| 2.12 | Virtual methods (B1 PR 2); properties if approved | OBJ-003 | L | medium | 2.11; M: Q-OBJ-2 for properties |
| 2.13 | Interfaces through hierarchies; `is`/`as` (B1 PR 3, 4) | OBJ-004 | M | low | 2.12 |
| 2.14 | Lambdas without captures; method groups | LANG-001 | L | medium | — |
| 2.15 | Capturing closures (region environments first, counted when escaping) | LANG-001 | L | high | 2.4 for escaping captures |
| 2.16 | MIR inliner, then devirtualization | PERF-002 | L | medium | E: PERF-EXP-1 |
| 2.17 | Generics in hierarchies | OBJ-006 | M | medium | 2.11 |

- **Acceptance.**
  - `longrun_*` and `cache_churn`: final live bytes equal at N and 10N iterations.
  - Tier-1 `memory_matrix` invariants unchanged.
  - Differential run with elision disabled: identical results and final live bytes.
  - One hand-written negative MIR case per validator obligation U1 to U8, including the
    field-overwritten-by-callee case.
  - B1 §13 cases P1 to P15, N1 to N22, V1 to V8, with sizes recomputed for the accepted `H`.
  - `Dispose` exactly once on each normal exit; never called by reclamation.
  - Runtime test suite under a sanitizer and the poisoning allocator.
- **Reports.** Class A and class B at 2.2, after 2.7, after 2.16.
- **Stop or replan.**
  - 2.2 fails thresholds for both H12 and H16: do not merge counting; B1 continues with H8 (2.3,
    2.11 to 2.13 unaffected); long-running memory falls back to explicit scoped arenas, as a new
    record.
  - The validator cannot be written independently of the pass: stop before 2.5.
  - 2.8 not approved: counting still lands; cycles then leak without a language answer, which must
    be stated in the reference.

## Phase 3 — Real programs

| PR | Content | IDs | Effort | Risk | Gate |
| --- | --- | --- | --- | --- | --- |
| 3.1 | `aster-exec` crate | ARCH-005 | M | low | — |
| 3.2 | `Codegen` generic over `Module`; object emission | PERF-001 | L | medium | — |
| 3.3 | Static runtime, linking, `aster build`; JIT/AOT differential test | PERF-001 | L | medium | E: PERF-EXP-2 (reported, not blocking) |
| 3.4 | FFI: library loading and linking, scalar signatures | PLAT-001 | M | medium | — |
| 3.5 | FFI: `ptr<T>`, `CString`, C-layout structs | PLAT-001 | L | high | — |
| 3.6 | FFI: synchronous callbacks with user data; posting callbacks; handle wrappers | PLAT-001 | L | high | M: Q-CONC-2 for callbacks without user data |
| 3.7 | General async state machine | CONC-003 | XL | high | 2.4, 2.15; M: Q-CONC-0 |
| 3.8 | Scheduler: turn budget, helping wait, nested tasks | CONC-004 | L | medium | E: CONC-EXP-2 for stealing |
| 3.9 | Transfer by copy of strings, arrays, collections | CONC-005 | L | medium | — |
| 3.10 | `Channel<T>`, `TaskScope`, cancellation, `TryWait`, domain failure rule | CONC-006, CONC-007 | L | medium | M: Q-CONC-3, Q-CONC-1 |
| 3.11 | Reactor; timers; sockets; streaming files | CONC-008 | XL | high | M: Q-CONC-5 |
| 3.12 | Reproducible `Parallel.Reduce` | CONC-009 | S | low | M: Q-CONC-4 |
| 3.13 | Positions in runtime failures; debug information in AOT | TOOL-003 | L | medium | — |
| 3.14 | macOS and arm64 | PLAT-004 | L | medium | — |
| 3.15 | Tuples; pattern-based `foreach` | LANG-010, LANG-014 | M | low | — |

- **Acceptance.** Every example and JIT test gives identical output as AOT. One test per row of the
  blocking table (master plan §8.4). Reentrancy across `await` observed and documented. A failure in
  a child domain reaches a supervisor through `TryWait` and leaves the supervisor running. A callback
  on a foreign thread does not execute ASTER code. An echo server holds flat memory under load.
- **Workloads added here.** General async (`task_swarm` extended), FFI call overhead, AOT size and
  startup: first measurements, no prior baseline.
- **Stop or replan.** Frame liveness not validatable: frames hold all locals. C aggregate ABI
  unreliable on a platform: pointers only there, documented.

## Phase 4 — Development experience

| PR | Content | IDs | Effort | Risk |
| --- | --- | --- | --- | --- |
| 4.1 | Semantic analysis after syntax errors | ARCH-007 | M | low |
| 4.2 | Formatter | TOOL-001 | M | low |
| 4.3 | Declaration fingerprints; front-end reuse | ARCH-006 | L | high |
| 4.4 | Machine-code reuse in the JIT | ARCH-006 | L | high |
| 4.5 | Language server | TOOL-002 | XL | medium |
| 4.6 | Development profile: slots keyed by declaration and signature; stable descriptors; session-immortal literals | HOT-001, HOT-002 | L | medium |
| 4.7 | Body and signature reload with state kept; all generations retained; explicit restart | HOT-003, HOT-004 | L | high |
| 4.8 | Type ABI change under the four conditions; global safe point with timeout | HOT-003, HOT-004 | L | high |
| 4.9 | Generation unloading under the unload rule (only if retained code size is shown to matter) | HOT-004 | M | high |
| 4.10 | Live-instance migration | HOT-003 | L | high |
| 4.11 | Generic body checking | ARCH-004 | M | medium |

- **Prerequisites.** 0.9; Phase 2 through 2.7 (heap independent of the JIT module); 1.11 if statics
  are approved; 3.7.
- **Acceptance.** Editing one body in `frontend_large` recompiles one declaration. A frame loop keeps
  its state across a body edit. A signature change leaves a running old caller correct. A type change
  with a live instance restarts and names the failed condition. A suspended task finishes on the code
  that suspended it. Release builds contain no slot load (class A code-shape test).
- **Stop or replan.** Whole-program passes defeat reuse: cache their summaries per declaration; if
  still insufficient, limit hot reload to body changes of non-generic functions and say so.

## Phase 5 — Long-term research (no commitment)

| Topic | Prerequisite | Open question |
| --- | --- | --- |
| WebAssembly backend | Stable MIR; runtime without threads or virtual memory | Cost of a second backend |
| Mobile | arm64 AOT; platform linkers | Toolchain integration |
| Move of unique graphs; page handoff | CONC-005 | Cost of the uniqueness check (CONC-005b) |
| Shared immutable data | MEM-003 | Atomic count cost (CONC-010) |
| Backup cycle collector | Field experience with MEM-004 | Compatibility with the no-GC decision (MEM-009) |
| Budgeted deferred free | MEM-003 | Weakens the fixed release point |
| Second backend, PGO, tiering | A measured gap PERF-002 cannot close | Complexity budget |
| Package registry | Stable manifest | Hosting and trust (PLAT-003) |
| `yield` iterators | CONC-003 lowering | Shared state-machine pass |
| GPU, ECS, engine | FFI, closures | Libraries only |

## Sequencing summary

| Group | Items |
| --- | --- |
| Can start now, ungated | 0.1 to 0.7, 0.9, 0.10; 1.1 to 1.8; 2.9; 2.14; 3.1 to 3.5; 4.1, 4.2 |
| Needs an experiment first | 0.8; 2.3 to 2.7; 2.10; 2.16; 3.8 (stealing) |
| Needs the maintainer first | 1.9 to 1.13; 2.0; 2.1 (counting part); 2.8; 3.6 (part), 3.7, 3.10, 3.11, 3.12 |
| Critical path | BENCH-001, ARCH-002, B1 stage 2/3 with MEM-EXP-1, MEM-003, LANG-001, CONC-003, CONC-008 |
