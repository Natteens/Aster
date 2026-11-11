# G. Executable tasks (revision 3)

> Tasks implement the decisions in [06-decision-closure.md](06-decision-closure.md). One task is one
> reviewable change. A task is **done** only when its gate passes; "done" is recorded in the status
> column with the validation performed. Public documentation changes only in the task that makes the
> behavior exist.

Common gate for every task, in addition to its own: `cargo fmt --all --check`; `cargo clippy` on the
touched crates with `-D warnings`; the touched crates' tests; no unrelated change in the diff.
Reinforced gate (memory, ownership, concurrency, FFI, hot reload): the whole workspace test suite,
the concurrency stress suites, and, once they exist, the poisoning allocator and sanitizer runs.

Status: `todo`, `in progress`, `done` (with date), `blocked (reason)`.

## Phase 0 — Foundations

| ID | Objective | Contract affected | Files and layers | Depends on | Minimal change | Tests (+ / −) | Benchmarks | Docs | Gate | Risk | Status |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| T-BENCH-1 | Workloads expressible today: long-running object update, simulated requests, dictionary churn, object graph, property access, interface calls, collection growth, string pipeline, large-program compilation | None (benchmarks only) | `benchmarks/performance/*.aster`; `aster-codegen-cranelift/examples/perf_baseline.rs`; `aster-codegen-cranelift/tests/perf_baseline.rs`; new test `bench_workload_memory.rs` | — | Add `.aster` workloads with independent checksums; register them; generate `frontend_large` deterministically; record memory invariants in a test | + checksum per workload; + generated source is deterministic and compiles; + memory facts recorded; − unknown workload rejected (existing) | the workloads themselves | `benchmarks/performance/README.md` | perf_baseline tests and the new memory test pass; clippy clean | low | **done 2026-11-10** (see report) |
| T-BENCH-2 | Harness support for task and parallel programs, and a worker-count option | None | `perf_baseline.rs` or a new `concurrency_baseline.rs` example; uses the existing worker-count entry points | T-BENCH-1 | Execution through `execute_resolved_with_aarm_parallel_workers` style entry points, which already accept a worker count | + `task_swarm`, `parallel_scaling` checksums at 1, 2, 4 workers; − invalid worker count rejected | `task_swarm`, `parallel_scaling` | README | tests pass | low | **done 2026-11-10** (see log) |
| T-BENCH-3 | Baseline procedure: class A values per target, class B protocol | PERF-005 | `benchmarks/performance/README.md`, `compare.mjs`-style comparator if needed | T-BENCH-1 | Document and, if needed, extend the existing JSON comparator to perf_baseline | + comparator rejects mismatched target | — | README | review | low | todo |
| T-DOC-1 | Correct the factual contradictions listed in map §4 | DOC-001 | `docs/compiler/grammar.md`, `watch.md`, `memory-management.md`, `reference/concurrency.md`, `research/aarm.md` | — | Edit only the wrong statement in each pair; no new behavior described | Each correction checked against code (see log) | — | those files | review | low | **done 2026-11-10** (see log) |
| T-ARCH-3 | HIR reads every expression type from the semantic model | ARCH-003 | `aster-compiler/src/hir_lowering/types.rs`, `expressions.rs` | — | Replace `binary_type` recomputation by a model lookup; agreement test over all fixtures and examples | + agreement test; − a seeded disagreement fails the test | `frontend_large` class A | `architecture.md` | compiler tests | low | **done 2026-11-10** (see log) |
| T-ARCH-2a | Move backend-neutral MIR checks into `aster-mir` without behavior change | ARCH-002 | `aster-mir/src/validate.rs` (new); `aster-codegen-cranelift/src/validation.rs` | — | Mechanical move of structural, typing, CFG checks; backend calls them | Existing validation tests unchanged and passing | — | `architecture.md` | workspace tests | medium | **done 2026-11-10, partial coverage** (types only; see log) |
| T-ARCH-2b | Run the MIR validator after every compiler MIR pass in tests and debug builds | ARCH-002 | `aster-compiler/src/lib.rs` | T-ARCH-2a | `debug_assertions`-gated call after each pass | + all tests pass with zero findings; − a test pass that corrupts MIR is caught | — | `architecture.md` | workspace tests | low | **done 2026-11-10** (see log) |
| T-ARCH-2c | Move the remaining backend-neutral checks into `aster-mir`: CFG targets, owned-region and temporary-subregion executability, worker host-operation reachability, nominal shape checks | ARCH-002 | `aster-mir/src/validate.rs`; `aster-codegen-cranelift/src/validation.rs` | T-ARCH-2a | Mechanical move with unchanged messages | Existing tests; one invalid-MIR case per moved check | — | `architecture.md` | workspace tests | medium | **done 2026-11-10** (see log; ARCH-002 still partial) |
| T-ARCH-2d | Complete ARCH-002: move interface metadata; run CFG, shape, worker, and region checks after each pass in the phase where each applies | ARCH-002 | `aster-mir/src/validate/executable.rs`; `aster-compiler/src/lib.rs` | T-ARCH-2c | Phased `validate_module`; per-pass `check_mir_at` | Invalid-MIR and phase-gating cases; corpus | — | `architecture.md` | workspace tests with and without features | medium | **done 2026-11-10** (see log) |
| T-CONC-EXP-1 | Can one finalized JIT module serve every worker thread | CONC-002 | `aster-codegen-cranelift/src/worker_pool.rs`, `execution.rs` (experiment branch) | — | Prototype: finalize once, send entry addresses to workers; run concurrency suites | Concurrency suites identical | `task_swarm`, startup time | report in `docs/proposals/` | suites pass; report written | medium | **done 2026-11-10: PASS** (see log) |
| T-CONC-2 | Adopt the shared module | CONC-002 | same | T-CONC-EXP-1 passes | — | — | — | `architecture.md` | reinforced gate | medium | **done 2026-11-10** (see log) |
| T-ARCH-1 | Structural semantic identity | ARCH-001 | `aster-compiler/src/semantic/mod.rs`, `semantic/general/*`, `hir_lowering/*`, `declaration_key.rs` | — | Replace `ModelNodeKey`/`CallableKey` span fields with declaration key plus per-declaration expression index | + identical model after a whitespace edit; + all pipeline tests | `frontend_large` | `architecture.md` | workspace tests | medium | **done 2026-11-10** (see log) |
| T-ARCH-7 | Diagnostic codes | ARCH-007 | `aster-diagnostics`, all diagnostic sites | — | Add `code` field; assign codes by layer | + golden output; − duplicate code test | — | `reference/cli.md` | workspace tests | low | todo |
| T-ARCH-5 | Scheduler into `aster-exec` behind a code provider | ARCH-005 | new `crates/aster-exec`; `worker_pool.rs`, `task_runtime.rs`, `task_abi.rs`, `async_abi.rs`, `completion_queue.rs` | — (independent of CONC-002, see closure §4.D) | Move code; inject the provider | Concurrency suites unchanged | `task_swarm` | `architecture.md` | reinforced gate | medium | todo |

## Phase 1 — Everyday surface

Each task: syntax, semantics, HIR, MIR, backend if needed, diagnostics, positive and negative tests,
example, reference page. Gate: workspace tests; class A invariants unchanged or explained.

| ID | Objective | Decision | Depends on | Risk | Status |
| --- | --- | --- | --- | --- | --- |
| T-LANG-18 | `Main(string[] args)`, exit status, standard error, environment | LANG-018 | — | low | todo |
| T-LANG-17 | `do`-`while`, `%=`, block comments, exponents, digit separators | LANG-017 | — | low | todo |
| T-LANG-16 | Real literal defaults to `double` (breaking; migrate examples and stdlib) | LANG-016 | T-LANG-17 | low | todo |
| T-OBJ-5 | Constructor overloads and `this(…)` (B2 record first) | OBJ-005 | — | low | todo |
| T-LANG-5 | Auto-properties and `init` | LANG-005 | — | low | todo |
| T-LANG-4a | `switch` on integers, chars, strings | LANG-004 | — | low | todo |
| T-LANG-4b | `is T x` patterns and `when` guards | LANG-004 | T-LANG-4a | low | todo |
| T-LANG-3 | `T?`, `null`, `??`, `?.` | LANG-003 | T-LANG-4b | medium | todo |
| T-LANG-6 | Static fields per domain | LANG-006 | — | medium | todo |
| T-LANG-9 | Struct receiver by reference; `ref` parameters (breaking) | LANG-009 | — | medium | todo |
| T-LANG-7 | Structs and primitives satisfy interfaces statically | LANG-007 | — | medium | todo |
| T-LANG-11 | Operators and indexers | LANG-011 | — | low | todo |
| T-LANG-12 | Interface inheritance | LANG-012 | — | low | todo |
| T-LANG-13 | Extension methods | LANG-013 | — | low | todo |
| T-LANG-8 | Object initializers | LANG-008 | T-LANG-5 | low | todo |
| T-STD-1 | List search and removal; set, queue, stack; string case and formatting, written in ASTER | — | T-LANG-7 | low | todo |

## Phase 2 — Memory and object model

| ID | Objective | Decision | Depends on | Gate | Risk | Status |
| --- | --- | --- | --- | --- | --- | --- |
| T-OBJ-B1-1 | B1 stage 1: update B1 record with closure answers and the constant `H` | OBJ-002..004 | — | review | low | todo |
| T-MEM-EXP-1 | Branch with configurations C0 to C6, ownership pass and independent validator for class references; report | MEM-001..003, OBJ-001 | T-BENCH-1, T-ARCH-2b | closure §4.C thresholds | high | **in progress**: stage A done; stage B (C4) done; stage C runtime substrate done; C5 first increment (class ownership pass and U1-U4 validator) done 2026-11-11; C6, collections, timing not done; no overall verdict |
| T-MEM-2 | Merge the chosen header | MEM-002 or B1 H8 | T-MEM-EXP-1 | reinforced | medium | blocked (experiment) |
| T-MEM-3a | Counting for class references; store sequence §4.B; failure contract §4.A | MEM-003 | T-MEM-2 | reinforced + negative MIR cases of §4.B | high | blocked (experiment) |
| T-MEM-3b | Strings and arrays | MEM-003 | T-MEM-3a | reinforced | high | blocked |
| T-MEM-7 | Collections own and free buffers; element glue | MEM-007 | T-MEM-3b | reinforced | medium | blocked |
| T-MEM-3c | Aggregates and interface values | MEM-003 | T-MEM-3b | reinforced | medium | blocked |
| T-MEM-4 | `weak`, teardown report, `DetectCycles` | MEM-004 | T-MEM-3c | reinforced | medium | blocked |
| T-MEM-5 | `IDisposable`, `using`, handle table, safety-net close, lint | MEM-005 | — (`using`); T-MEM-3a for the safety net | reinforced | medium | todo |
| T-MEM-6 | Governor and page return by default | MEM-006 | MEM-EXP-2 | reinforced | medium | todo |
| T-OBJ-2 | Single inheritance (B1 PR 1) | OBJ-002 | T-MEM-2 or B1 H8 | B1 cases | medium | blocked |
| T-OBJ-3 | Virtual methods and properties | OBJ-003 | T-OBJ-2 | B1 cases | medium | blocked |
| T-OBJ-4 | Interfaces through hierarchies; `is`/`as` | OBJ-004 | T-OBJ-3, T-LANG-4b | B1 cases | low | blocked |
| T-LANG-1a | Non-capturing lambdas and method groups | LANG-001 | — | workspace | medium | todo |
| T-LANG-1b | Capturing closures (region environments; counted when escaping) | LANG-001 | T-LANG-1a; T-MEM-3c for escaping | reinforced | high | blocked |
| T-PERF-2 | MIR inliner then devirtualization (PERF-EXP-1 first) | PERF-002 | T-ARCH-2b | experiment threshold | medium | todo |
| T-OBJ-6 | Generics in hierarchies | OBJ-006 | T-OBJ-3 | B1 cases | medium | blocked |

## Phase 3 — Real programs

| ID | Objective | Decision | Depends on | Risk | Status |
| --- | --- | --- | --- | --- | --- |
| T-PERF-1a | `Codegen` generic over `Module`; object emission | PERF-001 | T-ARCH-5 | medium | todo |
| T-PERF-1b | Static runtime, link, `aster build`; JIT/AOT differential test | PERF-001 | T-PERF-1a | medium | todo |
| T-PLAT-1a | FFI: libraries in `Aster.toml`, scalar signatures from the CLI | PLAT-001 | — | medium | todo |
| T-PLAT-1b | FFI: `ptr<T>`, `CString`, C-layout structs | PLAT-001 | T-PLAT-1a | high | todo |
| T-PLAT-1c | FFI: callbacks (synchronous, posting), callback thread-local | PLAT-001, Q-CONC-2 | T-PLAT-1b, T-LANG-1a | high | todo |
| T-CONC-3 | General async state machine | CONC-003 | T-MEM-3c, T-LANG-1b, T-ARCH-5 | high | blocked |
| T-CONC-4 | Scheduler: turn budget, helping wait, nested tasks | CONC-004 | T-ARCH-5 | medium | todo |
| T-CONC-5 | Transfer by copy of strings, arrays, collections | CONC-005 | T-ARCH-5 | medium | todo |
| T-CONC-7 | Domain failure contract §4.A for existing tasks; `TryWait`; `TaskScope`; channels | CONC-006, CONC-007 | T-CONC-4 | medium | todo |
| T-CONC-8 | Reactor, timers, sockets, streaming files | CONC-008 | T-CONC-3 | high | blocked |
| T-CONC-9 | Reproducible `Parallel.Reduce` | CONC-009 | — | low | todo |
| T-TOOL-3 | Source positions in runtime failures; debug information in AOT | TOOL-003 | — / T-PERF-1b | medium | todo |
| T-PLAT-4 | macOS and arm64 | PLAT-004 | T-PERF-1b | medium | todo |
| T-LANG-10 | Tuples | LANG-010 | — | low | todo |
| T-LANG-14 | Pattern-based `foreach` | LANG-014 | T-LANG-7 | low | todo |

## Phase 4 — Development experience

| ID | Objective | Decision | Depends on | Risk | Status |
| --- | --- | --- | --- | --- | --- |
| T-ARCH-7b | Semantic analysis after syntax errors | ARCH-007 | T-ARCH-7 | low | todo |
| T-TOOL-1 | Formatter | TOOL-001 | — | low | todo |
| T-ARCH-6a | Declaration fingerprints; front-end reuse | ARCH-006 | T-ARCH-1 | high | todo |
| T-ARCH-6b | Machine-code reuse in the JIT | ARCH-006 | T-ARCH-6a | high | todo |
| T-TOOL-2 | Language server | TOOL-002 | T-ARCH-6a, T-ARCH-7b | medium | todo |
| T-HOT-1 | Development profile: slots, stable descriptors, session literals | HOT-001, HOT-002 | T-ARCH-6b | medium | todo |
| T-HOT-2 | Body and signature reload; restart fallback | HOT-003, HOT-004 | T-HOT-1, T-MEM-3c, T-LANG-6 | high | blocked |
| T-HOT-3 | Type ABI change under the four conditions | HOT-003 | T-HOT-2, T-CONC-3 | high | blocked |
| T-ARCH-4 | Generic body checking | ARCH-004 | T-ARCH-1 | medium | todo |

## Order of execution

1. T-BENCH-1 (done), T-BENCH-2, T-BENCH-3.
2. T-DOC-1.
3. T-ARCH-3.
4. T-ARCH-2a, T-ARCH-2b.
5. T-CONC-EXP-1.
6. T-ARCH-1.
7. T-OBJ-B1-1, T-MEM-EXP-1.
8. Phase 2 merges per experiment result; Phase 1 tasks interleaved as independent work.
9. Phase 3, then Phase 4.

## Validation log

### T-BENCH-1 — done 2026-11-10

- Added `benchmarks/performance/{longrun_frame,longrun_request,cache_churn,object_graph,accessor_heavy,interface_dispatch,collection_growth,string_pipeline}.aster`
  and the generated `frontend_generated` workload (400 units, about 13,000 lines) in
  `crates/aster-codegen-cranelift/examples/perf_baseline.rs`.
- Expected checksums were computed by an independent Python model of each workload, not by ASTER.
- New test `crates/aster-codegen-cranelift/tests/bench_workload_memory.rs`: checksums at N and 10N,
  deterministic memory statistics, and the current-behavior fact that live bytes grow with the
  iteration count. Observed on Windows x64 (debug test profile), used bytes at N / 10N:
  `longrun_frame` 65,628 / 653,112; `longrun_request` 2,559,195 / 25,599,196; `cache_churn`
  1,212,016 / 11,017,840; `collection_growth` 1,516,000 / 15,160,000.
- `string_pipeline` was narrowed: the single-source harness does not load the standard library, so
  `String.Split`/`Replace`/`Join` are not measurable there. Recorded in the benchmark README.
- Gates: `cargo fmt --all --check`; `cargo clippy -p aster-codegen-cranelift --all-targets -- -D warnings`;
  `cargo test -p aster-codegen-cranelift --test perf_baseline --test bench_workload_memory --test memory_matrix`
  (13 + 4 + 8 passed). No benchmark timings were collected.
- Not done here (T-BENCH-2): `task_swarm` and `parallel_scaling`, which need a harness path for task
  programs and a worker-count option.

### T-DOC-1 — done 2026-11-10

Each correction was checked against the code before editing:

| Document | Was | Evidence |
| --- | --- | --- |
| `compiler/memory-management.md` | Rewound capacity "follows the existing delayed-purge/governor policy" | `enable_delayed_purge` is called only from unit tests in `aster-runtime/src/arena.rs`; `ExecutionContext::new()` passes no governor (`context.rs:694-706`) |
| `reference/concurrency.md` (two places) | `MemoryGovernor` and "memory-governor domain" as part of default execution | Same; `parallel_context` uses `ExecutionContext::new` without a budget (`execution.rs`) |
| `research/aarm.md` | Arena "built on Rust's zeroed host allocations" | `default_page_backend` selects `VirtualAlloc`/`mmap` (`arena.rs:472-483`) |
| `compiler/grammar.md` | Intro omitted hexadecimal and binary literals; `decimal` listed without its semantic rejection | Lexer support (PR #79); `decimal_deferred` in `semantic/mod.rs:259`, `:293` |
| `compiler/watch.md` | Compile errors keep the watcher running, including on the first build | `watch.rs:92-95` returns an error when the first build fails |

Left unchanged on purpose: specification chapter 10 ("class inheritance is not adopted") stays until
the implementing change, as B1 §12 requires; the monomorphization cache-key description concerns
generic functions and matches the code (the method key in `specialization.rs:479-488` is a separate
cache).

### T-ARCH-3 — done 2026-11-10

- Semantic analysis records `Model::binary_operand_types` (`semantic/mod.rs`) from
  `binary_operand_type` in `semantic/general/expressions.rs`, using the same central promotion table.
- HIR lowering (`hir_lowering/expressions.rs`, binary lowering) converts operands to the recorded
  type and derives the result from it; `binary_type` no longer promotes. A `debug_assert_eq!`
  compares the recorded type with the table on every compiled binary expression, which turns the
  whole test corpus into the agreement test.
- **Defect found by the agreement check and fixed:** property accessor bodies were analyzed under
  the model context `Owner::get_Name@<accessor start>` but lowered under `Owner::Name@<property
  start>`, so no decision recorded inside a getter or setter reached HIR (HIR silently re-derived
  types there). New shared helper `semantic::accessor_context`, used by HIR lowering.
- New test `crates/aster-codegen-cranelift/tests/accessor_semantic_decisions.rs` (small-integer
  promotion in a getter; list index and shift in a getter and a setter). Both fail without the
  context fix and pass with it.
- Probed and confirmed not affected: the transitive worker host-operation check already sees
  getter bodies.
- Remaining recomputation (not in this task's scope): conditional expressions (`conditional_type`)
  and unary promotion still use the table directly in HIR.
- Gates: `cargo fmt --all --check`; `cargo clippy -p aster-compiler -p aster-codegen-cranelift
  --all-targets -- -D warnings`; `cargo test --workspace --all-targets` (2,738 passed, 0 failed),
  then the new test file (2 passed).

### T-BENCH-2 — done 2026-11-10

- Workloads `benchmarks/performance/task_swarm.aster` (320 `Task.Run` calls joined by `WaitAll`)
  and `parallel_scaling.aster` (`Parallel.Reduce` over 200,000 elements); checksums from an
  independent Python model.
- New hidden seam `aster_codegen_cranelift::execute_with_worker_count` (`lib.rs`, with
  `execution::execute_resolved_with_worker_count`): the existing `TaskRuntime` with an explicit
  worker count; zero is a controlled error. It is not reachable from the language or the CLI.
- New example `examples/concurrency_baseline.rs` (`--workers`, `--samples`) and test
  `tests/concurrency_baseline.rs`: identical checksums at 1, 2, and 4 workers; zero workers
  rejected; argument validation.
- Gates: `cargo fmt --all --check`; `cargo clippy -p aster-codegen-cranelift --all-targets -- -D
  warnings`; `cargo test -p aster-codegen-cranelift --test concurrency_baseline` (3 passed). No
  timings collected.

### Point documentation fixes — 2026-11-10

- `specification/10-open-questions.md`: mojibake in the "Enums and switch" heading; removed the
  index entry listing program entry-point selection as open, since chapter 05 accepts it.
- `specification/05-functions.md`: the objective said the chapter selects no entry point while its
  "Application entry" section is ACCEPTED; now points to that section.
- `README.md`: "no package manager" next to documented Git dependencies and `aster fetch`; now "no
  package registry", naming the dependency sources that exist.
- Not changed: `stdlib/aster/io/io.aster` comment "ASTER 1.0 exposes no owned file handle" (source of
  the embedded standard library; left for a standard-library change); `Assert.Fail` reporting
  "values are not equal" is a behavior defect, not documentation, and is recorded for a later task.

### Workspace gate after T-BENCH-1, T-DOC-1, T-ARCH-3, T-BENCH-2 — 2026-11-10

`cargo fmt --all --check`; `cargo clippy --workspace --all-targets -- -D warnings`;
`cargo test --workspace --all-targets --no-fail-fast`: 2,743 passed, 0 failed (Windows x64).

### T-ARCH-2a — done 2026-11-10

- New module `crates/aster-mir/src/validate.rs` (moved, not rewritten, from
  `aster-codegen-cranelift/src/validation.rs`): `MirTypeIndex`, the typed instruction, rvalue,
  operand, place, and constant checks, plus `validate_function_types` and `validate_types`.
- `ValidationError::{Invalid, Unsupported}`. The backend converts it with `From`, producing exactly
  the previous messages (including "Cranelift JIT does not yet support …" for `Unsupported`).
- The `mir::Type` to `aster_types::Primitive` adapter (`primitive`) and `type_name` moved to
  `aster-mir`; the backend re-exports them. `aster-mir` now depends on `aster-types`.
- Kept in the backend on purpose: entry selection, foreign ABI, executable value types, intrinsic
  shapes, layouts, worker host-operation reachability, CFG target checks, array-bounds proofs, and
  the owned-region and temporary-subregion executability validators. They are candidates for a
  later slice (worker reachability, CFG targets, region validators are backend-neutral); moving
  them was not needed for T-ARCH-2b.
- Error ordering: unchanged; the backend calls the moved checks at the same points.
- New tests `crates/aster-mir/tests/validate.rs` (7): one valid module and six invalid ones (wrong
  place type, constant of the wrong kind, duplicate function symbol, undeclared class, field symbol
  shared by two structs, and a global place classified as unsupported rather than invalid).
- Gates: workspace tests 2,743 passed before the new tests; `cargo clippy -p aster-mir -p
  aster-codegen-cranelift --all-targets -- -D warnings`; `cargo fmt --all --check`.

### T-ARCH-2b — done 2026-11-10

- `aster_compiler::check_mir` runs `aster_mir::validate::validate_types` after MIR lowering and after
  each of the seven following passes, only under `debug_assertions`; it panics on `Invalid` and
  leaves `Unsupported` to the backend. The existing compiler worker turns the panic into a
  diagnostic naming the pass.
- Finding during the first run: a valid program with a module-level global produces MIR that is
  well-formed but unsupported; the first version panicked on it. Fixed by the
  `Invalid`/`Unsupported` split above.
- Tests: `check_mir_tests` in `crates/aster-compiler/src/lib.rs` (a valid pipeline passes; a pass
  that changes local types is caught and named).
- Gates: `cargo test --workspace --all-targets --no-fail-fast` 2,750 passed, 0 failed, with the
  check active in every test compilation; `cargo clippy --workspace --all-targets -- -D warnings`;
  `cargo fmt --all --check`.

### ARCH-002 coverage note — 2026-11-10

ARCH-002 is **not complete**. T-ARCH-2a moved only the nominal type index and the typing checks.
These backend-neutral validations still live only in `aster-codegen-cranelift/src/validation.rs`
and therefore do not run after each compiler pass:

- CFG structure: block targets and terminator checks (`validate_terminator`, `validate_block_target`);
- owned-region executability (`validate_executable_owned_regions` and helpers);
- temporary-subregion executability (`validate_executable_temporary_subregions` and helpers);
- worker host-operation reachability (`validate_no_console_io_in_workers`);
- interface metadata and nominal shape checks (enum construction, struct literals).

They are the next ARCH-002 slice (call it T-ARCH-2c). The MEM-003 validator obligations depend on
the region validators being backend-neutral, so T-ARCH-2c precedes T-MEM-EXP-1.

### T-CONC-EXP-1 — done 2026-11-10: PASS

Question: can one finalized JIT module serve every worker thread (CONC-002)?

Prototype, behind the new default-off Cargo feature `shared-jit-experiment` in
`aster-codegen-cranelift` (no public or default behavior changes):

- `worker_pool.rs`: `SharedProgram(PreparedProgram)` with `unsafe impl Send` and `Sync`; the pool
  prepares it once (`ExecutionPool::shared_program`, under the workers mutex) and hands an `Arc` to
  each worker; `worker_loop` takes an optional shared program and otherwise prepares its own as
  before.

Safety evidence for `Send`/`Sync`:

| Property | Evidence |
| --- | --- |
| Program not mutated after finalization | `PreparedProgram` fields are the `JITModule` and two entry maps; the module is touched only in `Drop` (`execution.rs`: `self.jit` appears only in `Drop`) |
| No writable JIT data | No data object in the backend is declared writable (search for `writable`/TLS in `src/` found none) |
| No shared ASTER state | Each job creates its own `ExecutionContext` (`parallel_context`, task and async paths) |
| Safe unloading | `Arc` ownership: the JIT memory is freed once, when the last holder (pool or worker thread) drops it; the test below observes the holder count drop to the pool's own reference after shutdown joins the workers |

Validation:

- New unit test `workers_share_one_prepared_program_and_release_it_at_shutdown` (feature-gated):
  4 workers → 5 holders (pool + 4 workers); 64 concurrent tasks complete with the right value; 1
  holder after `shutdown`.
- `cargo test -p aster-codegen-cranelift --all-targets`: default 1,439 passed; with the feature
  1,440 passed (the extra test); 0 failed in both.
- Stress: five consecutive runs with the feature of `concurrency_stress`, `task_run`, `parallel`,
  `task_ergonomics` (cancellation), `async_runtime`, `concurrency_baseline`, `foreign_ffi`,
  `recursion_guard` (160 tests) and the `worker_pool`/`task_runtime` unit tests (69 tests,
  including simulated preparation failure and worker panic): 0 failures.
- Clippy `-D warnings` clean with and without the feature.

Informative timing (release, Windows x64, one run of 7 samples each, median; not a gate):

| Workload | Workers | Per-worker compilation | Shared module |
| --- | ---: | ---: | ---: |
| `task_swarm` | 1 | 44.1 ms | 41.3 ms |
| `task_swarm` | 4 | 22.4 ms | 18.5 ms |
| `task_swarm` | 8 | 24.0 ms | 15.6 ms |
| `parallel_scaling` | 1 | 44.4 ms | 41.6 ms |
| `parallel_scaling` | 4 | 18.4 ms | 18.2 ms |
| `parallel_scaling` | 8 | 17.3 ms | 14.6 ms |

Decision: **PASS** against the closure criterion (one module serves every worker thread with
identical results in the concurrency suites). The prototype stays behind the feature. T-CONC-2
(adoption) is unblocked and must also: share the main thread's program (today the main thread
still compiles once more, so the prototype reaches two compilations instead of N + 1), turn the
"no writable data" fact into a checked assertion at preparation, and run the reinforced gate.

### T-CONC-2 — done 2026-11-10: shared JIT module adopted by default

Safety audit (beyond text search):

| Question | How it was checked | Result |
| --- | --- | --- |
| Does anything touch the `JITModule` after finalization through `&PreparedProgram`? | Every method of `impl PreparedProgram` was listed with the fields it reads: `self.jit` appears only in preparation and `Drop`; all `&self` methods read only `entries`/`task_entries` | No. The `JITModule`'s own `RefCell` symbol table (why it is not `Sync`) is never reached after finalization |
| Writable or thread-local JIT data? | Every data declaration in the backend is `declare_anonymous_data(false, false)` (interface tables in `interfaces.rs`, constants in `values.rs`); no named data declarations exist | None |
| Page permissions | cranelift-jit 0.123.20 source: `finalize` makes code read+execute and read-only data read-only (`memory/system.rs`), with `pipeline_flush_mt` for cross-thread instruction coherence | Immutable after finalization |
| Lazy or patched code (GOT/PLT, hot-swap)? | `is_pic=false` is forced by cranelift-jit; no hot-swap mode; symbols resolve at finalization | None |
| Cross-thread publication | The `Arc<SharedProgram>` reaches workers through `thread::spawn`, which orders the finalized writes before use | Ordered |
| Shared ASTER state | Each job and each `MoveNext` step creates its own `ExecutionContext`; runtime globals are atomics only | None shared |
| FFI | Foreign wrappers are bound at finalization; foreign calls are still rejected in workers (semantic and backend checks unchanged) | Unchanged |
| Unloading | `Arc`: freed once on the last release; `ExecutionPool::shutdown` joins every worker before the pool drops its reference; worker panics are caught inside the thread | Exactly once, after the last use |

Change:

- `execution.rs`: `SharedProgram` (with the audited `unsafe impl Send`/`Sync` and the SAFETY text
  above) and `Deref` to `PreparedProgram`.
- `worker_pool.rs`: the pool holds one program (`adopt_program` from the caller, else prepared once
  by `shared_program`); workers no longer compile.
- `task_runtime.rs`: `TaskRuntime::with_program`; the async `driver` is the same `Arc`, keeping the
  take/restore guard that rejects nested pumping.
- Production paths (`execute_resolved`, `execute_resolved_with_worker_count`,
  `execute_resolved_with_foreign_registry`) and the test runner (`PreparedTestProgram`, which
  previously recompiled per worker for every test with tasks) share the main program. Research
  entry points behind `aarm-telemetry` still prepare once for the main thread and once for the pool.
- The `shared-jit-experiment` feature is removed.
- Compilations per execution with tasks: previously 1 (main) + 1 (async driver, if any) + N
  (workers); now 1.

Tests and gates:

- New: `an_adopted_program_is_the_one_every_worker_runs` (pointer identity and holder counts),
  `async_driver_reuses_the_callers_program`; `workers_share_one_prepared_program_and_release_it_at_shutdown`
  now runs by default.
- `cargo test --workspace --all-targets`: 2,755 passed; with `--all-features`: 2,865 passed; 0
  failed.
- Five consecutive runs of the concurrency integration suites (160), pool/runtime/task ABI unit
  tests (75), and the CLI suite (127, including `aster test`): 0 failures.
- `cargo clippy --workspace --all-targets -- -D warnings`, with and without `--all-features`;
  `cargo fmt --all --check`.
- Not run: sanitizers or Miri (JIT code is outside Miri's model; no sanitizer toolchain is
  configured on this Windows host).

Informative release timing (one run, 7 samples, median): `task_swarm` 40.4 / 16.2 / 13.5 ms and
`parallel_scaling` 42.3 / 17.4 / 13.6 ms at 1 / 4 / 8 workers.

### T-ARCH-2c — done 2026-11-10

- New module `crates/aster-mir/src/validate/executable.rs` (1,743 lines moved unchanged from the
  backend; the move set was computed as the call closure of the roots below, ignoring comments,
  and contains no backend helper): owned-region executability and invalidated-use checks,
  owned-region call effects, temporary-subregion executability (CFG, cycles, fine ownership
  state), worker host-operation reachability, enum construction and struct literal shapes, and
  the CFG block-target check.
- The backend imports them and calls them at the same points; messages are identical
  (`ValidationError` → `BackendError` keeps the "Cranelift JIT does not yet support" wording for
  unsupported constructs). The unit test of `FineExecutionState` moved with the type.
- Kept in the backend, because they depend on what Cranelift can lower: the rest of
  `validate_terminator` (operand and return-value ABI checks), executable value types, intrinsic
  shapes, entry selection, foreign ABI, array-bounds proofs, explicit array initialization, task
  argument transfer layouts. `validate_interface_metadata` is backend-neutral but returns the
  method maps the backend's per-function validation consumes; it stays until those maps move.
- New tests `crates/aster-mir/tests/validate_executable.rs` (4): unknown block target, enum
  construction of an undeclared enum, aggregate of an undeclared struct, worker body reaching
  console output (and the same module accepted once the host call is removed). The region
  validators keep their existing negative coverage in the backend tests, which now exercise the
  moved code.
- Gates: `cargo test --workspace --all-targets` 2,755 passed, 0 failed; `cargo clippy --workspace
  --all-targets -- -D warnings` with and without `--all-features`; `cargo fmt --all --check`.

**ARCH-002 status after T-ARCH-2c: still partial.** Remaining gaps:

1. `validate_interface_metadata` still lives only in the backend.
2. The compiler's per-pass check (`check_mir`, T-ARCH-2b) runs only the typing checks. The moved
   region, shape, worker-reachability, and block-target checks run at backend entry, not after
   each compiler pass. Wiring them into `check_mir` (with the owned-effect summary they need) is
   the next ARCH-002 step and belongs before T-MEM-EXP-1.

### T-ARCH-1 — done 2026-11-10

Design (chosen to preserve behavior by construction):

- `ModelNodeKey { context, node, span }`, identity `(context, node)`:
  - `context` is structural: `Owner::Name(param types)`, `Owner::get_P()`, `Owner::set_P(T)`,
    `Owner::#field:F` (previously `Owner::Name@<byte offset>`).
  - `node` is `NodeRef::Ordinal(n)`, the pre-order ordinal among the context's expressions,
    statements, blocks, switch cases, and variable declarations. The index maps each span to its
    first ordinal, so two nodes share a key exactly when they shared a span before: every lookup
    resolves as it did with span keys. `NodeRef::Position` remains as a fallback for an unindexed
    node; the stability test requires zero fallbacks for the covered program.
  - `span` is kept for diagnostics and the deterministic report order of the worker checks; it is
    not part of the identity.
- `CallableKey` gains `signature` (parameter type spelling); `declaration_start` stays only for
  deterministic candidate ordering in overload diagnostics and is not part of the identity.
- Semantic analysis registers the node index of every context it analyzes
  (`Model::register_function`, `Model::register_expression`); every key in semantic analysis and
  HIR lowering comes from `Model::node_key`. HIR lowering no longer builds keys itself.

Tests (`semantic::structural_identity_tests`, built on generic specializations `Box<int>` and
`Box<string>`, overloads `Add(int)`/`Add(long)`, a property with getter and setter, a field
initializer, a constructor, an enum switch, list indexing, `Task.Run`, and interpolation):

- every key is identical after whitespace changes, added comments, an unrelated declaration
  inserted first, and two functions swapped; zero positional fallbacks;
- the fingerprint covers each of those features;
- a structural edit inside one function changes keys only in that function's context.

Gates: `cargo test --workspace --all-targets` 2,762 passed, 0 failed; `cargo clippy --workspace
--all-targets -- -D warnings` with and without `--all-features`; `cargo fmt --all --check`.

Cost not measured: indexing clones each analyzed function body once.

### T-ARCH-2d — done 2026-11-10: ARCH-002 complete

- `validate_interface_metadata` (and `InterfaceMethods`) moved to `aster_mir::validate::executable`.
- New `validate_cfg` (unique block ids, existing entry, declared `Goto`/`Branch` targets) and
  `validate_module(module, Phase)`, which runs interface metadata, CFG, enum and struct shapes, and
  worker reachability at every phase, subregion executability from `Phase::TemporarySubregions`,
  and owned-region executability at `Phase::Final`.
- `aster_compiler::check_mir_at` runs typing plus `validate_module` after MIR lowering and after
  each pass with the phase that applies (`Lowered` through local object elimination,
  `TemporarySubregions` after the AARM pass, `Final` after owned regions).
- `ValidationError::Rejected` added: a worker host operation is a policy rejection of well-formed
  MIR (the backend reports it with the same message as before; the compiler check does not stop on
  it, because valid programs reach the backend's controlled rejection and tests depend on it).
- Region validators report violations as `Unsupported` (the backend's "does not yet support"
  wording for hand-built MIR). The compiler check treats any executability error except `Rejected`
  as a pass defect, so a pass that emits an unbalanced or non-executable region stops compilation.

Defect found and fixed while wiring the checks: **a struct literal naming its fields out of
declaration order (`Position { y: 2, x: 1 }`) compiled but failed at `aster run`** with "function
`Main` has an `Aggregate` for `Position` with a field that does not match its declaration". MIR
lowering emitted the `Aggregate` in source order while the MIR contract is declaration order. Fixed
in `mir_lowering/module.rs` (`order_struct_aggregates`): operands are permuted after evaluation, the
same rule enum payloads use, so field expressions still run in source order. Regression tests:
`crates/aster-codegen-cranelift/tests/struct_literal_order.rs` (result and evaluation order).

Tests: `crates/aster-mir/tests/validate_executable.rs` adds CFG cases (duplicate block, missing
entry, unknown branch target) and phase gating (an unbalanced owned-region exit passes at `Lowered`
and `TemporarySubregions` and fails at `Final`; an unbalanced subregion exit fails from
`TemporarySubregions` on); `check_mir_tests::an_unbalanced_owned_region_is_caught_in_the_final_phase`.
The whole test corpus compiles with every applicable check after every pass.

Gates: `cargo test --workspace --all-targets` 2,768 passed, 0 failed; with `--all-features`
2,878 passed, 0 failed; `cargo clippy --workspace --all-targets -- -D warnings` with and without `--all-features`;
`cargo fmt --all --check`.

ARCH-002 is complete: every backend-neutral check lives in `aster-mir`, the backend keeps only
lowering-capability checks (ABI value types, intrinsic shapes, entry selection, foreign ABI,
array-bounds proofs, explicit array initialization, task argument layouts, terminator operand
checks), and the compiler runs the neutral checks after every pass in debug builds.

### T-MEM-EXP-1 stage A (C0 to C3, header layout) — 2026-11-10

Scope of this stage: the header factor only, with semantics unchanged. C4 (size-class allocator),
C5 (counting without elision), and C6 (counting with elision) are not built yet, so the overall
MEM-EXP-1 gates (long-running memory, execution time of C6, elision, compile time, peak memory)
**have no verdict** and reference counting stays out of the default build.

Implementation (experiment only; default build is C0 and unchanged):

- `aster-codegen-cranelift` features `mem-exp-h8` (C1), `mem-exp-h12` (C2), `mem-exp-h16` (C3),
  mutually exclusive (`compile_error!` otherwise).
- `layouts.rs`: `OBJECT_HEADER_BYTES`; class objects (only classes, never structs) place their
  first field at the header size with 8-byte alignment; field offsets and allocation sizes follow
  from the existing layout authority.
- `calls.rs`: the allocation sequence stores a non-zero 64-bit descriptor word at offset 0 right
  after the runtime returns the zeroed object (a stand-in for the B1 descriptor pointer; the count
  word stays zero because counting does not exist in C1 to C3).

Correctness (`cargo test -p aster-codegen-cranelift --all-targets`, 2 threads, per configuration):
1,441 passed and 2 failed in each of C1, C2, C3. Both failures pin byte sizes, as the closure
expected: `layouts::layout_tests::lays_out_class_references_as_pointers` asserts a class size of
16; `long_lived_ownership::a_live_alias_prevents_the_overlapping_family_from_reclaiming` asserts
`used_bytes < 16_000` for 1,000 to 2,000 retained one-`int` objects. In both, the computed
values and checksums are correct.

Class A (deterministic, `bench_workload_memory` `--nocapture`, Windows x64), used bytes at N / 10N:

| Workload | C0 | C1 (H8) | C2 (H12) | C3 (H16) |
| --- | ---: | ---: | ---: | ---: |
| `longrun_frame` | 65,628 / 653,112 | 98,472 / 979,752 | 98,472 / 979,752 | 131,296 / 1,306,336 |
| `longrun_request` | 2,559,195 / 25,599,196 | 2,719,086 / 27,198,163 | 2,719,086 / 27,198,163 | 2,878,951 / 28,797,197 |
| `cache_churn` | 1,212,016 / 11,017,840 | same | same | same |
| `collection_growth` | 1,516,000 / 15,160,000 | same | same | same |

`cache_churn` and `collection_growth` allocate no class objects, so they are unchanged, as they
must be. For classes whose fields have alignment 4 or less, H12 costs exactly what H8 costs (the
first field lands at 12 and the size rounds to 8); H16 costs one more word per object.

Class B (release, `perf_baseline`, `execution_warm`, one executable per configuration run
interleaved C0, C1, C2, C3 for 3 rounds of 7 samples after 2 warm-up runs; median of 21 samples):

| Workload | C0 ms | C1/C0 | C2/C1 | C3/C1 | C2/C3 | C0 spread ms |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `accessor_heavy` | 5.564 | 1.009 | 0.994 | 0.998 | 0.996 | 0.568 |
| `allocation_mixed` | 9.463 | 1.039 | 0.971 | 1.030 | 0.943 | 0.641 |
| `cache_churn` (no objects) | 4.069 | 0.973 | 0.994 | 1.086 | 0.916 | 0.617 |
| `calls_and_interfaces` | 2.761 | 0.999 | 1.001 | 0.998 | 1.003 | 0.633 |
| `interface_dispatch` | 4.065 | 1.006 | 0.996 | 1.004 | 0.992 | 2.428 |
| `longrun_frame` | 1.833 | 1.053 | 0.958 | 0.964 | 0.993 | 0.515 |
| `longrun_request` | 7.513 | 0.986 | 1.010 | 1.001 | 1.008 | 1.579 |
| `numeric_loops` (no objects) | 6.592 | 0.949 | 1.049 | 1.107 | 0.947 | 1.193 |
| `object_graph` | 2.158 | 1.051 | 1.001 | 1.079 | 0.928 | 0.413 |

Checksums identical in all configurations. **Noise floor**: `numeric_loops` and `cache_churn`
generate identical code in every configuration, yet vary by up to 11% and 9%. Differences between
separately built binaries on this machine are therefore of the same size as the 5% threshold.

Verdicts against the closure (§4.C), stated with their confidence:

| Gate | Threshold | Result | Verdict |
| --- | --- | --- | --- |
| Header choice | H12 unless C2 is more than 2% slower than C3 on object workloads | C2/C3 at most 1.008 on every workload | **H12 selected** |
| Header cost | C2 (or C3) at most 1.05x C1 on every workload | C2/C1 at most 1.049 (on a control workload); C3/C1 up to 1.107 (also a control) | **C2: PASS (nominal, low confidence: below the noise floor). C3: not decided** (its only exceedances are on control workloads or within noise) |
| Correctness (stage A part) | identical checksums | identical | **PASS** |
| Memory | (no stage-A threshold) | H12 = H8 for alignment-4 fields; +1 word for H16 | recorded |

Consequences and next steps:

- H12 is the header for stages B and C.
- Before stage C's time gates, the class B protocol needs a lower noise floor: hardware counters
  (executed instructions) where available, more rounds, or a build that pins code alignment. The
  current protocol cannot resolve a 5% difference on this machine.
- Stage B (C4): a size-class allocator for escaping allocations, never freed, behind a feature.
- Stage C (C5, C6): the ownership pass, the independent validator obligations U1 to U8, and the
  store sequence of §4.B. This is the large part of the experiment.

### T-MEM-EXP-1 stage B (C4) and stage C runtime substrate — 2026-11-10

H12 remains the **candidate** header, not a decision; every configuration keeps the default build
unchanged.

Size-pinning tests, made configuration-aware without weakening them:

- `layouts::layout_tests::lays_out_class_references_as_pointers` now asserts the exact offsets and
  size for the active header (C0 0/8/16, H8 8/16/24, H12 12/16/24, H16 16/24/32).
- `long_lived_ownership::a_live_alias_prevents_the_overlapping_family_from_reclaiming` scales its
  byte bounds by the configuration's object stride (8, 16, 16, 24).
- With these, C1, C2, C3, and C4 have **no failing test**.

Stage B, C4 (`aster-codegen-cranelift` feature `mem-exp-c4` = `mem-exp-h12` +
`aster-runtime/mem-exp-heap`):

- `crates/aster-runtime/src/object_heap.rs`: `ObjectHeap`, 15 size classes from 16 to 2,048 bytes.
  A fresh block is carved from the Persistent arena (stable address, released with the context);
  a released block goes to its class's free list and is zeroed when reused; debug builds poison it
  (`0xDB`) on release. Larger objects use the existing arena path. In C4 nothing is released.
- `ExecutionContext::allocate_object` routes Persistent class objects through the heap when the
  feature is on.
- Constraint recorded in code: owned-region rewinds of the Persistent arena are compatible with C4
  (free lists stay empty), but **must be disabled in any configuration that releases blocks** (C5,
  C6), or a rewound block could be handed out again.
- Correctness: `cargo test -p aster-codegen-cranelift --all-targets --features mem-exp-c4`
  (2 threads): 1,443 passed, 0 failed.
- Class A: identical to C2 on the suite (the benchmark objects are 24 and 32 bytes, exactly on
  size classes, so the heap adds no rounding).
- Class B (release, C2 and C4 interleaved, 4 rounds of 7 samples, medians): checksums identical;
  C4/C2 = 0.920 `accessor_heavy`, 1.049 `allocation_mixed`, 0.996 `cache_churn`, 0.994
  `calls_and_interfaces`, 1.016 `interface_dispatch`, 0.923 `longrun_frame`, 1.116
  `longrun_request`, 1.028 `numeric_loops`, 1.028 `object_graph`. Workloads that never reach the
  heap move by up to 8%, so only `longrun_request` (the most allocation-intensive workload, 110,000
  escaping allocations per run) stands slightly above the noise floor. **Inconclusive**; no stage-B
  threshold exists, and none is claimed.

Stage C runtime substrate (same feature, not reachable from generated code yet):

- H12 count word at offset 8 (29-bit count, 3 reserved flag bits); `IMMORTAL` sentinel;
  `retain` saturates to `IMMORTAL` (leaks, never frees early); `release` uses an explicit work list,
  reads the immutable `ObjectDescriptor` (size and reference-field offsets) through the descriptor
  word, releases children, and returns the block to the heap. `ObjectDescriptor` is `Sync` because
  descriptors are immutable read-only data.
- Tests `crates/aster-runtime/tests/object_heap.rs` (6, all passing): reuse is zeroed and size
  classes do not mix; a 200,001-object chain is released without native recursion; a shared child
  survives its first parent; a strong cycle is never released and never freed early (MEM-004);
  immortal objects ignore counting and counts saturate; oversized objects bypass the heap.

Not started (stage C proper), and therefore **no PASS for MEM-001 to MEM-003**:

1. MIR `Retain`/`Release`/`Drop` instructions and their lowering to the substrate, with count
   initialization at allocation (1 for counted, `IMMORTAL` for region objects) and per-class
   descriptor data emitted as read-only module data.
2. The ownership pass (master plan §5.5) and the store sequence (closure §4.B).
3. The independent validator for obligations U1 to U6 in `aster_mir::validate` (U7 and U8 are
   runtime obligations, partly covered by the substrate tests above).
4. Disabling owned-region lowering in C5 and C6.
5. A lower-noise class B protocol (hardware counters or more rounds) before the time gates.
6. Strings, arrays, and collections: until they are counted, `longrun_request` and `cache_churn`
   cannot meet the long-running gate even with class objects counted.

### T-MEM-EXP-1 stage C, C5 — first increment implemented (2026-11-11)

Contract and recorded corrections: [08-mem-exp-1-c5-design.md](08-mem-exp-1-c5-design.md).
Scope: counting for class and interface references in locals, parameters, returns, and class
fields (including inline struct fields). References stored into arrays, collections, enums,
statics, or runtime storage receive a count that is never released (leak, never early free).

Implementation (feature `aster-codegen-cranelift/mem-exp-c5` = `mem-exp-c4` +
`aster-runtime/mem-exp-rc`; C0 unchanged):

- MIR `Retain`/`Release` (`crates/aster-mir/src/lib.rs`) with typed validation; conservative arms
  in compiler passes (they run before insertion).
- Ownership pass `crates/aster-mir/src/ownership.rs` (`insert_ownership`); independent validator
  `crates/aster-mir/src/validate/ownership.rs` (`validate_ownership`, U1, U2, U3 marker check, U4
  within a block). Both run in `PreparedProgram::prepare_timed_with_foreign_registry` under C5; any
  finding is a `BackendError`.
- Runtime: count initialization (1, or `IMMORTAL` for Temporary and above 2,048 bytes),
  `aster_rt_object_retain`/`aster_rt_object_release`, retain/release counters in
  `ObjectHeapStats`, exposed as `MemoryStats::object_heap`.
- Codegen: per-class descriptors owned by the prepared program (`layouts::ObjectDescriptors`),
  descriptor address written at allocation, `Retain`/`Release` lowering, zero-initialized
  reference-bearing stack slots.
- Defects found by integration and fixed: scalar returns read through a released `this` (U2);
  `StringBuilder` values are `Type::Class` with a native header (now excluded from leaves and
  descriptors); fine-subregion rewinds left counted locals pointing into rewound memory (test
  aborted on "release of an object whose count is already zero"; region markers are now removed
  and the validator rejects any that remain, U3); validator minted distinct values for repeated
  reads of one array element.

Gates run (CARGO_BUILD_JOBS=2, RUST_TEST_THREADS=2, debug builds, so released blocks are
poisoned with 0xDB):

- `aster-codegen-cranelift --all-targets`: C0 1,443 passed, 0 failed; C4 1,443 passed, 0 failed;
  C5 1,427 passed, 0 failed, 24 ignored (2 pre-existing, 22 region-reclamation tests ignored under
  C5 with an explicit reason: C5 disables region rewinds). The checkpoint commit message says
  1,429 passed; that figure was a counting error, corrected here.
- `tests/mem_exp_c5.rs` (7): released blocks observed for loop field replacement, loop local
  reassignment, callee frames; shared objects survive; interface identity preserved.
- `aster-runtime` 337 (default) and 346 (`mem-exp-rc`) passed; `aster-compiler` 707 passed;
  `aster-mir` 42 passed; clippy clean on the four crates (default and C5); rustfmt clean.

### T-MEM-EXP-1 stage C, C6 elision — implemented (2026-11-11)

Contract: §6 of [08-mem-exp-1-c5-design.md](08-mem-exp-1-c5-design.md). `insert_ownership_elided`
applies E1 (null capture), E2 (move from a single-use lowering temporary), E3 (leaf-function
parameters); the validator gained liveness-based holders, token-only balance at return, and a
borrowed-parameter mode restricted to functions with no call and no non-parameter release.
Feature `mem-exp-c6` = `mem-exp-c5` + elision. Report tool:
`cargo +stable-gnu run -p aster-codegen-cranelift --example mem_exp_counts --features mem-exp-c6`.

Gates: `aster-mir` 39 + 14 passed; `aster-codegen-cranelift --all-targets`: C6 1,427 passed,
0 failed, 24 ignored; C5 rerun 1,427 passed, 0 failed, 24 ignored.

Count report (debug build, deterministic; static = Retain + Release instructions in the module,
executed = non-null, non-immortal runtime operations, `Run` entry):

| Workload | Checksum C5 = C6 | Static C5 / C6 | Executed retains C5 / C6 | Executed releases C5 / C6 |
| --- | --- | --- | --- | --- |
| accessor_heavy | 2508936383 | 15 / 3 | 0 / 0 | 0 / 0 |
| calls_and_interfaces | 300900000 | 26 / 8 | 800,006 / 400,002 | 800,008 / 400,004 |
| generics | 300333 | 22 / 6 | 0 / 0 | 0 / 0 |
| interface_dispatch | 7025000 | 33 / 21 | 600,090 / 300,060 | 600,090 / 300,060 |
| longrun_frame | 16351259 | 20 / 12 | 648,206 / 260,103 | 648,206 / 260,103 |
| longrun_request | 45498889 | 20 / 8 | 320,000 / 60,000 | 340,000 / 80,000 |
| object_graph | 184230 | 36 / 19 | 245,600 / 163,720 | 245,620 / 163,740 |

Workloads without class references (collections, cache_churn, collection_growth, numeric_loops,
string_pipeline, strings) emit no count operation; all checksums equal C0's expected values.
Executed operations, suite total: C6/C5 = 2,387,792 / 5,247,826 = 45.5% (soft gate ≤ 50%: met on
the total; per workload object_graph is 66.7% and interface_dispatch 50.0%).

Heap facts in C6 (`Run`): `longrun_request` reuses 19,998 of 20,000 released blocks (2 fresh), but
its `used_bytes` still grows (2,079,156 at N, 20,798,521 at 10N) because strings and arrays are not
counted. `longrun_frame` (4,103 fresh, 0 released) and `object_graph` (40,922 fresh, 20 released)
keep their objects in arrays or lists, whose references are never released in this increment. The
long-running gate therefore cannot pass yet; this is a measured consequence, not a verdict.

### T-MEM-EXP-1 stage C, `List<T>` element ownership (U8, first collection) — implemented (2026-11-11)

Contract: §7 of [08-mem-exp-1-c5-design.md](08-mem-exp-1-c5-design.md). Lists with reference
elements own one count per leaf; the runtime releases on `Set`, `RemoveAt`, `RemoveRange`, `Clear`
and retains on `ToArray`, `GetRange`, `AddRange`, using a per-element-type descriptor set after
`AllocateList`. The list header itself is still uncounted (its elements leak when it dies).
Validator: list reads are borrowed until `ListSet`/`ListRemoveAt`/`ListClear` or any call; E3 does
not apply to functions with those instructions.

Gates: `aster-codegen-cranelift --all-targets` C0 1,443 and C4 1,443 passed, 0 failed; C5 and C6
1,432 passed, 0 failed, 24 ignored (including 4 new end-to-end list tests and the new
`counted_frame_workload_stays_flat_between_n_and_ten_n`); `aster-runtime` 337 (default) and 354
(`mem-exp-rc`, including 8 `list_rc` tests); `aster-mir` 58; clippy and rustfmt clean.

Measured (C6, debug, deterministic): `longrun_frame` used bytes 1,584 at N and 1,584 at 10N
(peak 2,632), 40,759 objects released and 40,757 blocks reused at 10N: **the long-running gate
shape holds for this workload**. `longrun_request` still grows (2,239,071 → 22,397,708: strings
and arrays), `cache_churn` still grows (dictionary and strings), `object_graph` releases 20 of
40,922 objects (children in `Node[]` arrays). Checksums identical in C5 and C6; executed count
operations C6/C5 = 2,391,831 / 5,251,865 = 45.5%.

### T-MEM-EXP-1 stage C, counted arrays (U8, second collection) — implemented (2026-11-11)

Contract: §8 of [08-mem-exp-1-c5-design.md](08-mem-exp-1-c5-design.md). Under `mem-exp-rc` arrays
use the uniform header (descriptor @0, count @8 with `ARRAY_FLAG`, length, element size, data @24;
32-byte header), Persistent headers and data blocks up to 2,048 bytes come from the size-class heap,
larger data blocks stay reserved (`unreturned_array_blocks`). Arrays own their elements; the
release work list releases element leaves and both blocks. Pass and validator: `Array` is a leaf,
`Index` is owning, fresh array producers are owned sources, E3 excludes functions with element
stores. Two existing runtime tests that pinned the single-allocation array layout are gated off
under `mem-exp-rc` (`array_allocation_preflights_the_complete_header_and_payload`, and the final
stats assertion of `array_layout_overflow_and_preflight_errors_are_fail_closed_and_first_wins`).

Gates: `aster-codegen-cranelift --all-targets` C0 1,443 and C4 1,443 passed; C5 and C6 1,435
passed, 0 failed, 24 ignored; `aster-runtime` 337 (default) and 362 (`mem-exp-rc`, incl. 9
`array_rc`); `aster-mir` 70; `aster-compiler` 707; clippy and rustfmt clean.

Measured (C6, debug, deterministic): `object_graph` releases 122,820 objects and arrays and reuses
110,538 blocks; used bytes 294,768 (was 1,964,528 before arrays were counted). `longrun_frame`
unchanged (flat at 1,584). `longrun_request` still grows (strings). Checksums identical in C5 and
C6; executed count operations C6/C5 = 2,760,271 / 5,579,385 = 49.5% (soft gate ≤ 50%, close).

Known leaks by design in this increment: arrays passed to intrinsics (consumed operand), Temporary
arrays' elements, dictionary contents, enum payloads, a dropped list's elements.

### T-MEM-EXP-1 stage C, counted strings — implemented (2026-11-11)

Contract: §9 of [08-mem-exp-1-c5-design.md](08-mem-exp-1-c5-design.md). Under `mem-exp-rc` the
string header is `{descriptor @0 (null), count @8 with STRING_FLAG, len @16, payload @24}`;
`ExecutionContext::allocate_string_storage` is the only runtime construction path (count 1 for a
Persistent size-class block; `IMMORTAL` for Temporary, constant, and above 2,048 bytes). No runtime
entry point returns an existing string, so every string result is owned. Pass and validator:
`String` is a leaf, string intrinsic results and `StringBuilderToString` are owned, intrinsic
arguments are borrowed except stored operands (`ListInsert` element, `DictionaryGetOrAdd` key and
value, `AsyncStoreSlot` value), and `DictionaryGetOr`/`DictionaryGetOrAdd`/`AsyncLoadSlot`/
`AsyncAwaitResult`/`TaskWait` results are borrowed (this also corrects the array increment, which
treated a `DictionaryGetOr` array result as owned). The validator no longer reports a false U2 for
a borrowed intrinsic result retained right after the call. Two tests that pinned the 8-byte string
header (`string_methods`, `jit::static_string_concat_chain_allocates_once`) now derive sizes from
`ASTER_STR_PAYLOAD_OFFSET`.

Gates: `aster-codegen-cranelift --all-targets` C0 1,443 and C4 1,443 passed; C5 and C6 1,442
passed, 0 failed, 24 ignored; `aster-runtime` 337 (default) and 371 (`mem-exp-rc`, incl. 9
`string_rc`); `aster-mir` 82; `aster-compiler` 707; clippy and rustfmt clean.

Measured (C6): `longrun_request` releases and reuses 40,000 blocks at N (strings and requests), but
used bytes still grow 1,760,224 → 17,600,224: the `List<int>` headers and buffers of each request
are not counted. Checksums identical in C5 and C6; executed count operations C6/C5 = 2,880,271 /
5,659,385 = 50.9% (**soft elision gate ≤ 50% now missed** by 0.9 points, because strings added
operations that E1 to E3 do not remove). The string header grows from 8 to 24 bytes.

### T-MEM-EXP-1 stage C, counted `List<T>` headers — implemented (2026-11-11)

Contract: §10 of [08-mem-exp-1-c5-design.md](08-mem-exp-1-c5-design.md). The count word's three
reserved bits are now one kind field (0 object, 1 array, 2 string, 3 list, 4 dictionary reserved).
`AsterList` under `mem-exp-rc`: element descriptor @0, count @8 (kind 3); Persistent headers and
buffers up to 2,048 bytes are heap-served, growth returns the old buffer, release frees elements,
buffer, and header. Pass and validator: `List` is a leaf, `AllocateList` is owned, list operands are
borrowed.

Gates: `aster-codegen-cranelift --all-targets` C0 1,443 and C4 1,443 passed; C5 and C6 1,446
passed, 0 failed, 24 ignored; `aster-runtime` 337 (default) and 378 (`mem-exp-rc`); `aster-mir` 93;
clippy and rustfmt clean. The recorded-fact test now lists `longrun_frame` and `longrun_request`
as counted-flat and asserts the 64 KiB gate shape for both
(`counted_workloads_stay_flat_between_n_and_ten_n`).

Measured (C6, debug, deterministic): `longrun_request` used bytes **448 at N and 448 at 10N**
(peak 478), 900,000 blocks reused at 10N; `longrun_frame` 1,584 at both; `object_graph` 294,768.
`cache_churn` still grows (dictionaries). Checksums identical in C5 and C6; executed count
operations C6/C5 = 2,960,271 / 5,739,385 = 51.6% (soft elision gate ≤ 50% missed).

### T-MEM-EXP-1 stage C, counted dictionaries — implemented (2026-11-11)

Contract: §11 of [08-mem-exp-1-c5-design.md](08-mem-exp-1-c5-design.md). `AsterDictionary` under
`mem-exp-rc`: `{key, value}` descriptor pair @0, count @8 (kind 4); the dictionary owns live keys
and values (glue on `Add`, `Set`, `GetOrAdd`, `Remove`, `Clear`, drop); heap-served header and
storage up to 2,048 bytes. Pass and validator: `Dictionary` is a leaf, `AllocateDictionary` owned,
dictionary mutations invalidate borrowed reads and exclude E3.

Gates: `aster-codegen-cranelift --all-targets` C0 1,443 and C4 1,443 passed; C5 and C6 1,450
passed, 0 failed, 24 ignored; `aster-runtime` 337 (default) and 395 (`mem-exp-rc`, incl. 17
`dictionary_rc`); `aster-mir` 104; clippy and rustfmt clean.

Measured (C6): `cache_churn` still grows (1,212,512 → 11,018,336 used bytes): its 1,000-key window
needs a storage block above 2,048 bytes, and each rehash or compaction leaves the old large block
reserved. `collection_growth` grows for the same reason (large list buffers). Checksums identical;
executed count operations C6/C5 = 2,960,322 / 5,739,436 = 51.6%.

### T-MEM-EXP-1 stage C, large block classes — implemented (2026-11-11)

Contract: §12 of [08-mem-exp-1-c5-design.md](08-mem-exp-1-c5-design.md). Nine power-of-two classes
from 4 KiB to 1 MiB serve buffers (array data, list buffers, dictionary storage) and strings
through one lookup (`ObjectHeap::buffer_class_of`); objects and headers keep the small classes.
Strings up to 1 MiB are counted. One lib test that calibrates a one-page AARM budget for the
single-block array layout is ignored under C5
(`task_abi::tests::wait_all_allocation_denial_exposes_no_partial_result_and_keeps_handles_valid`).

Gates: `aster-codegen-cranelift --all-targets` C0 1,443 and C4 1,443 passed; C5 and C6 1,449
passed, 0 failed, 25 ignored; `aster-runtime` 337 (default), 343 (`mem-exp-heap`), and the
`mem-exp-rc` suites (336 unit, 10 `array_rc`, 18 `dictionary_rc`, 16 `list_rc`, 6 `object_heap`,
3 `object_rc`, 10 `string_rc`); clippy and rustfmt clean.

Measured (C6, debug, deterministic; used bytes at N / 10N):

| Workload | Used N | Used 10N | Peak N | Peak 10N |
| --- | ---: | ---: | ---: | ---: |
| `longrun_frame` | 1,584 | 1,584 | 2,640 | 2,640 |
| `longrun_request` | 448 | 448 | 477 | 478 |
| `cache_churn` | 261,728 | 261,728 | 261,728 | 261,728 |
| `collection_growth` | 32,448 | 32,448 | 444,448 | 4,152,448 |

The long-running gate shape (final live bytes at 10N ≤ N + 64 KiB) holds for `longrun_frame`,
`longrun_request`, and `cache_churn` in this debug measurement; `bench_workload_memory` asserts it
for all four workloads. Checksums identical; executed count operations C6/C5 = 51.6%.

### T-MEM-EXP-1 class B, first C0/C6 measurement (2026-11-11)

Protocol: release `perf_baseline` built once per configuration (C0 default, C6 `mem-exp-c6`),
4 rounds alternating order, `--samples 7 --warmup 2`, median of 28 `execution_warm` samples per
workload, on the notebook (first class B run on this machine).

| Workload | C0 ms | C6/C0 |
| --- | ---: | ---: |
| numeric_loops (control) | 6.093 | 0.998 |
| strings | 5.728 | 1.394 |
| collections | 4.648 | 0.978 |
| calls_and_interfaces | 2.742 | 10.642 |
| generics | 3.959 | 0.751 |
| allocation_mixed | 8.225 | 7.509 |
| longrun_frame | 1.812 | 11.202 |
| longrun_request | 7.445 | 2.904 |
| cache_churn | 3.927 | 0.985 |
| object_graph | 2.168 | 12.599 |
| accessor_heavy | 5.582 | 1.046 |
| interface_dispatch | 4.242 | 6.569 |
| collection_growth | 2.064 | 0.824 |
| string_pipeline | 9.123 | 1.051 |

Geometric mean 2.287. Compile time on `frontend_generated`: front end 1.001; front end + backend
validation (which includes the ownership pass and validator under C6) + codegen 1.114 (gate ≤ 1.20:
met, conservatively, since codegen is included). **As built, C6 fails the execution-time gate**
(≤ 1.15 per object workload, ≤ 1.10 geometric mean). Cause identified in the implementation, not
in counting as such: every `Retain`/`Release` is an out-of-line call into the runtime, and
`release` allocates a work-list `Vec` on every call; list and dictionary glue allocate a `Vec` per
mutation. No verdict is recorded yet: the closure asks for the correct implementation to be
measured, and the inline fast path below is part of the C5/C6 design (§3 calls retain and release
"test the sentinel"), not a threshold change.

### T-MEM-EXP-1 inline count fast paths and second class B measurement (2026-11-11)

Contract: §13 of [08-mem-exp-1-c5-design.md](08-mem-exp-1-c5-design.md). Generated code now
retains and releases inline (load, sentinel test, store) and calls `aster_rt_object_release_last`
only when a count reaches zero; counters are incremented inline at exported context offsets, so
executed counts are unchanged (C6 total 2,960,322). The release work list reuses a heap-owned
buffer; glue collects leaf words into an 8-slot inline buffer.

Gates: C5 and C6 `--all-targets` 1,449 passed, 0 failed, 25 ignored; C0 and C4 1,443 passed;
`aster-runtime` 337 / 399 (`mem-exp-rc`); `aster-mir` 104; clippy clean.

Class B (same protocol as the first run):

| Workload | C0 ms | C6/C0 first | C6/C0 now |
| --- | ---: | ---: | ---: |
| numeric_loops (control) | 6.157 | 0.998 | 0.997 |
| strings | 5.804 | 1.394 | 1.115 |
| collections | 4.701 | 0.978 | 1.005 |
| calls_and_interfaces | 2.774 | 10.642 | 1.291 |
| generics | 4.032 | 0.751 | 0.738 |
| allocation_mixed | 8.288 | 7.509 | 1.385 |
| longrun_frame | 1.839 | 11.202 | 1.400 |
| longrun_request | 7.434 | 2.904 | 1.057 |
| cache_churn | 3.927 | 0.985 | 1.037 |
| object_graph | 2.191 | 12.599 | 1.551 |
| accessor_heavy | 5.574 | 1.046 | 1.065 |
| interface_dispatch | 4.247 | 6.569 | 1.173 |
| collection_growth | 2.086 | 0.907 | 0.907 |
| string_pipeline | 9.087 | 1.077 | 1.077 |

Geometric mean 1.116. Against the hard gate (≤ 1.15 per object-heavy workload, ≤ 1.10 geometric
mean): `longrun_request` 1.057 and `accessor_heavy` 1.065 meet it; `longrun_frame` 1.400,
`object_graph` 1.551, `interface_dispatch` 1.173 and the mean do not. The C6 measured here
implements only part of the elision the design specifies (§5.5 borrow windows and G1 callee
summaries are missing), so the next step implements them (§14) before the verdict.

### T-MEM-EXP-1 C6 round 2 (E4 borrow windows, E5 call summaries) and third class B (2026-11-11)

Contract: §14 of [08-mem-exp-1-c5-design.md](08-mem-exp-1-c5-design.md). Pass and validator
compute the non-releasing summary independently (greatest fixpoint; whitelist of read-only or
allocating intrinsics listed in `ownership.rs`); calls to non-releasing functions do not
invalidate; borrowed locals (single block windows) carry no count.

Gates: C5 and C6 `--all-targets` 1,452 passed, 0 failed, 25 ignored; `aster-mir` 122; clippy
clean. Executed count operations (`Run` entries): C6 1,480,322 vs C5 5,739,436 = **25.8%**
(soft elision gate ≤ 50%: met). Checksums identical.

Class B (third run, same protocol):

| Workload | C6/C0 run 2 | C6/C0 run 3 | Gate |
| --- | ---: | ---: | --- |
| longrun_frame | 1.400 | 1.392 | ≤ 1.15: not met |
| longrun_request | 1.057 | 0.986 | met |
| object_graph | 1.551 | 1.536 | ≤ 1.15: not met |
| interface_dispatch | 1.173 | 1.032 | met |
| accessor_heavy | 1.065 | 1.058 | met |
| geometric mean | 1.116 | 1.086 | ≤ 1.10: met |

Compile time, measured directly (`mem_exp_pass_time`, release, median of 15 on
`frontend_generated`): front end 249.98 ms, MIR validation 2.19 ms, ownership pass + validator
53.41 ms: **1.212** (gate ≤ 1.20: not met, by 1.2 points).

Causes of the remaining gaps, identified: `longrun_frame` keeps all 260,103 retains because the
`ListGet` result is lowered into a temporary that is then copied into `entity`, which E4 does not
follow; `object_graph` loads `children` before the loop and reads it in the loop body, which a
single-block window does not cover; the pass and validator use linear searches (places, locals,
cache) that dominate their time.

Not done, so **no PASS for MEM-001 to MEM-003**:

1. Final round: E4 through single-use temporaries and across blocks by dominance with no
   invalidating instruction on any path; faster pass and validator. Then class B and the §4.C
   verdict, recorded whatever the result.
2. Enum payload drop glue and closure environments (enums currently leak their references).
3. Region reclamation together with counting (H2 pins); C5 and C6 currently lose it.
4. Class B timing protocol (release builds, interleaved rounds) and the §4.C thresholds.

Next action: item 1 (class B timing and the §4.C verdict); improve elision for the soft gate.
