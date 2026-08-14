# Memory region benchmark

This directory contains the formal memory workloads and the `memory_matrix`
executor used to measure allocation behaviour and detect regressions.

## Workloads

Eight Aster workloads exercise one allocation category at a time, plus the two
original mixed programs. Each workload allocates a value and then reads it every
iteration, so the allocation cannot be optimised away, and returns a
deterministic checksum.

| Workload file | Case | Region | Per-iteration allocation | Per-iteration checksum |
| --- | --- | --- | --- | --- |
| `object_temporary.aster` | object | temporary | one object | 39 |
| `object_persistent.aster` | object | persistent | one object | 39 |
| `array_temporary.aster` | array | temporary | one array | 7 |
| `array_persistent.aster` | array | persistent | one array | 7 |
| `string_temporary.aster` | string | temporary | one dynamic string | 2 |
| `string_persistent.aster` | string | persistent | one dynamic string | 2 |
| `temporary.aster` | mixed | temporary | object + array + string | 42 |
| `persistent.aster` | mixed | persistent | object + array + string | 42 |

Temporary workloads keep the value local so escape analysis reclaims it on every
return. Persistent workloads return the value from a helper, so their allocation
instructions remain Persistent. The single object, array, and string cases now
exercise compiler-proven owned Persistent slices and rewind after each iteration;
the mixed case has overlapping returned families and intentionally retains the
conservative context lifetime.

Every workload exposes `RunSmall`, `RunMedium`, and `RunLarge`. The checksum is
always `per-iteration checksum × iterations`.

| Scale | Temporary iterations | Persistent iterations |
| --- | --- | --- |
| small | 10,000 | 10,000 |
| medium | 100,000 | 100,000 |
| large | 500,000 | 150,000 |

`large` is intended for manual local runs only.

## Commands

Human-readable summary (table plus JSON):

```console
cargo run --release -p aster-codegen-cranelift --example memory_matrix -- --scale small,medium
```

Machine-readable JSON only:

```console
cargo run --release -p aster-codegen-cranelift --example memory_matrix -- --scale small --json > memory-report.json
```

Scale selection accepts `small`, `medium`, `large`, `all`, or a comma-separated
list. With no `--scale` flag the executor runs `small,medium`.

Validate the JSON, and compare it against a local baseline once you have one
(native `node`, no extra dependency):

```console
node benchmarks/memory/compare.mjs validate memory-report.json
node benchmarks/memory/compare.mjs compare benchmarks/memory/baselines/<target>-<profile>.json memory-report.json
```

## The single-source memory-stat view still works

```console
cargo run --release -p aster-cli -- run benchmarks/memory/temporary.aster --function Run --memory-stats
cargo run --release -p aster-cli -- run benchmarks/memory/persistent.aster --function Run --memory-stats
```

## Result schema

The executor emits one JSON document. Fields are grouped so that deterministic
metrics, timing, and metadata stay separate.

```
schema_version
environment: { aster_version, os, arch, target, profile, git_revision }
results: [
  {
    case, region, scale, iterations, status,
    checksum, expected_checksum, samples,
    memory: {
      total_allocations, object_allocations, array_allocations, string_allocations,
      requested_bytes, used_bytes, reserved_bytes, peak_used_bytes, peak_reserved_bytes
    },
    timing_ms: {
      frontend_compile: { median, min, max },
      jit_and_execute:  { median, min, max },
      end_to_end:       { median, min, max }
    },
    error
  }
]
```

`git_revision` is read from `ASTER_GIT_REVISION` and is `unknown` when unset. The
comparator never requires it to match.

## Timing definitions

- `frontend_compile` is the duration of `compile(source)`. It is captured once
  per workload, so `median`, `min`, and `max` are equal.
- `jit_and_execute` is the duration of `execute_with_stats`, taken over five
  samples. It includes Cranelift code generation, finalization, and execution;
  code generation and execution are **not** measured separately.
- `end_to_end` is `jit_and_execute` plus `frontend_compile.median`.

Timing is informative only. It is never compared against a baseline, because
wall-clock time is not reproducible across machines, toolchains, or runners. Do
not read these numbers as a scientific comparison between machines.

## Blocking fields

The comparator freezes only fields that are deterministic for a given target and
profile: `checksum` and every `memory` field. On a matching target a difference
in any of these makes `compare` exit non-zero.

Cross-platform invariants are enforced by the executor, not frozen as numbers:
temporary cases finish with `used_bytes == 0` and `peak_used_bytes > 0`;
single-family Persistent cases finish with `used_bytes == 0` after owned-region rewind, while the
overlapping mixed Persistent case finishes with `used_bytes > 0`; every case keeps
`peak_used_bytes >= used_bytes`;
unused categories stay at zero; the five samples must produce identical
deterministic metrics.

## Baselines

A baseline is separated by target and profile, named `<target>-<profile>.json`.
Each records `schema_version`, `target`, `profile`, `aster_version`,
`baseline_generated_from_rev`, and the deterministic fields for the small scale.
The comparator rejects a report whose `schema_version`, `target`, or `profile`
does not match the baseline; it does not require the Git revision to match.

No baseline is shipped in the repository. Generate one locally, deliberately,
one target and profile at a time:

1. Generate a real report on the target host in the intended profile:

   ```console
   cargo run --release -p aster-codegen-cranelift --example memory_matrix -- --scale small --json > memory-report.json
   ```

2. Audit the report: confirm every case is `pass`, the checksums and
   per-category counts are expected, and the temporary and persistent
   `used_bytes` invariants hold.

3. Generate the baseline manually. `to-baseline` preserves the report's real
   `target` and `profile` and refuses a relabeling override:

   ```console
   node benchmarks/memory/compare.mjs to-baseline memory-report.json --rev <rev> > benchmarks/memory/baselines/<target>-<profile>.json
   ```

   Set `ASTER_GIT_REVISION` or pass `--rev` to record the source revision as
   metadata.

4. Compare later reports against it manually with
   `compare.mjs compare <baseline> memory-report.json`.

Regenerate a baseline with the same deliberate steps, only when an intended
change to allocation behaviour moves a deterministic field. Metrics are never
copied between targets.

## Timed comparison of the two mixed workloads

```console
cargo run --release -p aster-codegen-cranelift --example memory_regions
```

Compare results only on the same machine, Rust toolchain, Aster revision, and
build profile.
