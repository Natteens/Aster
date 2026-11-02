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

Each `Run()` returns a checksum derived independently of ASTER; a wrong value
fails the benchmark. Memory-specific measurement stays in `benchmarks/memory`.

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
