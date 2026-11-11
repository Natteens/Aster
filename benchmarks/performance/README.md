# Performance Baseline v1

Measurement foundation for later compiler/runtime/backend work. It does not
optimize anything and makes no claim about how fast ASTER is. It records
phase-separated timings of the real production pipeline so a change can be
compared against an earlier run **on the same machine**.

## What it measures

Each workload is compiled with `aster_compiler::compile`, prepared by the
Cranelift backend (`PreparedSequentialExecution::prepare_timed`, the same
`PreparedProgram` that `aster run` builds) and invoked. No second pipeline
exists; the only backend change is a timing split inside preparation.

| Workload | Category | Exercises |
| --- | --- | --- |
| `numeric_loops` | numeric | nested `int` loops, `%`, `long` accumulation |
| `strings` | strings | concatenation, `Length`, loop-carried concat |
| `collections` | collections | `int[]` fill, `List<int>` `Add`/`Get` |
| `calls_and_interfaces` | calls | static calls, interface dispatch, class state |
| `generics` | generics | specialized generic function and `Box<T>` |
| `allocation_mixed` | allocation | reuses `benchmarks/memory/persistent.aster` (object + array + string per iteration) |
| `longrun_frame` | long_running | 64 entities updated per frame; expired entities replaced by new objects stored into a `List<Entity>` |
| `longrun_request` | long_running | per-request object with a `List<int>` and an interpolated string, processed and dropped |
| `cache_churn` | long_running | `Dictionary<int, int>` sliding window of 1,000 live keys: set, remove, lookup |
| `object_graph` | objects | complete binary tree of class nodes (depth 10) built and summed recursively, 20 times |
| `accessor_heavy` | objects | property getter and setter calls in a hot loop |
| `interface_dispatch` | objects | three interface implementations in a `List<IShape>`, dispatched 300,000 times |
| `collection_growth` | collections | `List<int>` grown to 1,024 and `Dictionary<int, int>` to 256 entries from empty, per round |
| `string_pipeline` | strings | interpolation, scalar `foreach`, concatenation |
| `frontend_generated` | frontend | deterministic generated program (`GENERATED_UNITS` = 400 classes and functions, about 13,000 lines); measures front end and code generation |

Each `Run()` returns a checksum derived independently of ASTER; a wrong value
fails the benchmark. Memory-specific measurement stays in `benchmarks/memory`.

## Long-running memory facts (BENCH-001)

`longrun_frame`, `longrun_request`, `cache_churn`, and `collection_growth` also expose `RunLong`,
which performs ten times the iterations of `Run`. The test
`crates/aster-codegen-cranelift/tests/bench_workload_memory.rs` runs both entries with memory
statistics and checks independent checksums and deterministic statistics. Run it with
`-- --nocapture` to print allocation counts and live, peak, and reserved bytes for the baseline
procedure.

Under the current region model every value these workloads stop using has escaped its function, so
live bytes grow with the iteration count; the test records that as current behavior. It is the
measurement the memory-model experiment (MEM-EXP-1 in `docs/proposals/architecture-2026/`) uses.

Not covered by this suite, because the feature or the harness support does not exist yet: virtual
dispatch, closures, general `async`, native FFI calls, AOT size and startup, and standard-library
text helpers such as `String.Split` (this harness compiles one source string without the standard
library). No workload in this directory stands in for them.

## Concurrency baseline

`perf_baseline` rejects `Task.Run`, so task and parallel workloads use a separate example:

```console
cargo run --release -p aster-codegen-cranelift --example concurrency_baseline
cargo run --release -p aster-codegen-cranelift --example concurrency_baseline -- --workers 1,2,4,8 --samples 7
```

| Workload | Exercises |
| --- | --- |
| `task_swarm` | 20 rounds of 16 `Task.Run` workers with scalar arguments, joined by `Task.WaitAll` |
| `parallel_scaling` | `Parallel.Reduce` over 200,000 `int` elements with per-element work and an associative combine |

Each sample is one complete execution at an explicit worker count (main and worker JIT
preparation, run, teardown) through the hidden `execute_with_worker_count` seam; phases are not
separated. Checksums are independent of ASTER and must not depend on the worker count; the test
`tests/concurrency_baseline.rs` checks that at 1, 2, and 4 workers. Medians are informative and
comparable only on one machine. Suspended-task memory is not measurable with the current
restricted `async`.

## Running

```console
cargo run --release -p aster-codegen-cranelift --example perf_baseline
cargo run --release -p aster-codegen-cranelift --example perf_baseline -- --json > perf.json
cargo run --release -p aster-codegen-cranelift --example perf_baseline -- --category generics --samples 10 --warmup 2
```

Flags: `--json` (JSON only), `--warmup N` (default 1, 0..=10000), `--samples N`
(default 5, 1..=10000), `--workload a,b`, `--category c`. Invalid values exit
with status 2; a failed workload exits with status 1. Use `--release`; debug
numbers are not meaningful. Set `ASTER_GIT_REVISION` to record a revision
(otherwise `null`; Git is never required).

## Phases

All values are wall-clock nanoseconds (`unit: "ns"`) from `Instant`.

| Phase | Mode | Boundary |
| --- | --- | --- |
| `frontend_compile` | cold | one `compile(source)`: lex, parse, semantics, HIR, MIR, MIR optimizer |
| `backend_validate` | cold | MIR validation, Task.Run rejection, entry selection |
| `backend_codegen` | cold | JIT setup, declarations, Cranelift compilation of all functions, excluding `finalize_definitions` |
| `backend_finalize` | cold | `finalize_definitions` and entry-address lookup |
| `backend_codegen_and_finalize` | cold | `backend_codegen + backend_finalize` (contiguous intervals), excluding validation |
| `execution_cold` | cold | first invocation on freshly finalized code, no compilation |
| `execution_warm` | warm | repeated invocation of one already-prepared artifact, no compilation |
| `end_to_end_cold` | cold | one outer timer around compile, prepare, finalize and first invocation |

`end_to_end_cold` is measured independently, not summed from the others, so it
exceeds their sum by timer/API overhead. Every cold sample is a complete fresh
pipeline run; warm samples reuse one artifact (a fresh execution context per
invocation). Warmup runs are executed and discarded in both modes.

## Output

```
schema_version, kind
metadata: aster_version, git_revision|null, os, arch, profile, rustc_version|null,
          timing_unit, warmup, samples, phases{name: {mode, boundary}}
run:      started_unix_seconds            (nondeterministic)
benchmarks[]: name, category, status, error,
          invariants: entry, expected_checksum, checksum, mir_function_count
          timings: phase -> {mode, unit, count, min, median, max, mean, samples[]}
```

`metadata` and `invariants` are deterministic for a given revision, target and
profile; `run` and `timings` are noisy. Tests assert only invariants, shape,
sample counts and structural ordering (the outer timer encloses its inner
intervals), never durations.

## Comparing runs responsibly

Compare medians from the same machine, power state, toolchain, profile and
background load, ideally repeating each run several times. Wall-clock time is
not comparable across machines or runners, and there is no cross-machine
normalization, statistical regression test, or CI gate. A difference between
two runs is a lead to investigate, not proof of a speedup or regression.

## Limitations

- Execution timings use the prepared execution path with memory statistics
  disabled (`invoke`), so memory-stat collection overhead is excluded;
  `aster run` collects statistics and will be slightly slower.
- Codegen vs. finalize is Cranelift JIT-level: compilation happens when each
  function is defined, finalize is relocation/permission setup.
- Task.Run workloads are not supported by the prepared-execution seam.
- Single-source `compile(source)` only; project and multifile front ends are
  not measured. The standard library is not loaded separately.
- Inner phase timers add small overhead to `end_to_end_cold`.
- Memory statistics are not collected here; see `benchmarks/memory`.
