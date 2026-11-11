# MEM-EXP-1 report (closure §4.C)

Date: 2026-11-11. Machine: the development notebook (Windows 10, `stable-gnu` toolchain).
Branch: `feat/architecture-2026-execution`. Contracts: [08-mem-exp-1-c5-design.md](08-mem-exp-1-c5-design.md).
Execution record: [07-tasks.md](07-tasks.md), T-MEM-EXP-1 sections.

## Verdict

**FAIL.** Two hard gates are not met for H12 or for H16:

1. **Execution time.** `object_graph` runs at 1.353× C0 with C6 (H12) and 1.344× with C6-H16; the
   gate is ≤ 1.15× on each object-heavy workload. The other four object-heavy workloads and the
   geometric mean meet it.
2. **Tier-1 preservation.** `benchmarks/memory` class A values are not identical to C0 apart from
   header growth: the three single-case persistent workloads end with one released block left in
   its size-class free list (counted as used by the arena accounting) instead of 0 used bytes, and
   `memory_matrix` reports them as failed. The difference is reusable, not live, memory; the gate is
   applied as written.

Per closure §5 (Q-MEM-0) the fallback applies: **no reference counting in the default build; B1
with the H8 header; long-running memory through an explicit `Arena` scope type, recorded as a new
decision.** C0 remains the default. The C4 to C6 implementation stays in the repository behind its
experiment features (`mem-exp-c4`, `mem-exp-c5`, `mem-exp-c6`, `mem-exp-c6-h16`); nothing is
enabled by default.

## Gates

| Gate (closure §4.C) | Threshold | H12 (C6) | H16 (C6-H16) | Result |
| --- | --- | --- | --- | --- |
| Correctness | identical checksums, zero validator findings, whole suite under the poisoning allocator | identical; zero findings; 1,452 passed, 0 failed, 25 ignored (debug build, released blocks poisoned) | same | met for every executed test; 23 tests are ignored under counting because they measure region reclamation or a one-page budget calibrated for the single-block array layout |
| Long-running memory | final live bytes at 10N ≤ N + 64 KiB (`longrun_frame`, `longrun_request`, `cache_churn`) | 1,584 / 1,584; 448 / 448; 261,728 / 261,728 | same shape | **met** |
| Tier-1 preservation | class A identical except header-driven sizes | see class A tables | see class A tables | **not met** |
| Header choice | H12 unless C2 > C3 + 2% | C2/C3 ≤ 1.008 (stage A) | — | H12 selected (stage A) |
| Execution time | ≤ 1.15× per object-heavy workload; geometric mean ≤ 1.10× | `object_graph` 1.353; others ≤ 1.140; mean 1.062 | `object_graph` 1.344; others ≤ 1.118; mean 1.058 | **not met** |
| Elision effectiveness (soft) | executed count operations C6 ≤ 50% of C5 | 722,722 / 5,739,436 = 12.6% | — | met |
| Compile time | front end + MIR passes + validation, C6 ≤ 1.20× C0, `frontend_large` | 1.166 (`mem_exp_pass_time`: front end 247.66 ms, MIR validation 2.16 ms, ownership pass + validator 41.47 ms) | — | met |
| Peak memory | C6 peak live ≤ 1.6× C0 on `object_graph` | 294,768 vs 1,637,440 bytes = 0.18× | 327,520 = 0.20× | met |

## What was built

- MIR `Retain`/`Release`; an ownership pass (C5) and an elision pass (C6: E1 null captures, E2
  moves, E3/E5 parameters of non-releasing functions, E4 borrow windows through temporaries and
  across blocks); an independent validator (U1, U2, U3 marker check, U4 within a block).
- Runtime: H12 count word with a 3-bit kind (object, array, string, list, dictionary), counted
  arrays, strings, lists and dictionaries with element ownership glue, size classes from 16 bytes
  to 1 MiB, inline retain/release fast paths in generated code.
- Measurement tools: `mem_exp_counts` (static and executed count operations, heap facts),
  `mem_exp_pass_time` (compile-time gate input).

## Why `object_graph` misses the time gate

It allocates 40,940 nodes and 40,940 child arrays per run and frees them all (122,820 releases
with block reuse). C6 still executes 225,140 retains and 225,160 releases there, mostly in
construction (constructor arguments and array-literal elements are retained as they are stored)
and in the release work. C0 never frees anything, so it pays none of that work, and its arena peak
is 5.6× larger. Moving constructor arguments and literal elements instead of retaining them is
the remaining elision opportunity; it was not attempted after three optimisation rounds, as the
protocol requires a verdict at that point.

## Raw values

Class B protocol: release `perf_baseline` built once per configuration, 4 rounds alternating the
order of C0 and C6, `--samples 7 --warmup 2`, 28 `execution_warm` samples per workload; spread is
max − min. Control workloads that generate identical code (`numeric_loops`, `collections`) move by
up to 3.5%, which bounds the noise of this protocol on this machine.

#### Class B, C6 (H12) versus C0 (`execution_warm`, ms, median of 28 samples)

| Workload | C0 median | C0 spread | C6 median | C6 spread | C6/C0 |
| --- | ---: | ---: | ---: | ---: | ---: |
| numeric_loops | 6.348 | 0.481 | 6.163 | 0.631 | 0.971 |
| strings | 6.632 | 0.277 | 6.551 | 1.334 | 0.988 |
| collections | 4.615 | 0.737 | 4.749 | 1.225 | 1.029 |
| calls_and_interfaces | 2.760 | 0.140 | 3.138 | 0.648 | 1.137 |
| generics | 3.942 | 0.401 | 3.005 | 0.654 | 0.762 |
| allocation_mixed | 8.032 | 0.743 | 11.814 | 2.732 | 1.471 |
| longrun_frame | 1.831 | 0.121 | 2.088 | 0.391 | 1.140 |
| longrun_request | 7.833 | 1.170 | 7.550 | 1.148 | 0.964 |
| cache_churn | 3.974 | 0.467 | 4.164 | 0.483 | 1.048 |
| object_graph | 2.330 | 0.458 | 3.152 | 0.425 | 1.353 |
| accessor_heavy | 5.596 | 0.311 | 5.909 | 1.559 | 1.056 |
| interface_dispatch | 4.194 | 0.369 | 4.365 | 0.879 | 1.041 |
| collection_growth | 2.047 | 0.552 | 1.920 | 0.484 | 0.938 |
| string_pipeline | 9.793 | 0.222 | 9.620 | 1.038 | 0.982 |
| frontend_generated | 0.052 | 0.005 | 0.064 | 0.008 | 1.240 |

Geometric mean C6/C0: 1.062

#### Class B, C6-H16 versus C0 (`execution_warm`, ms, median of 28 samples)

| Workload | C0 median | C0 spread | C6 median | C6 spread | C6/C0 |
| --- | ---: | ---: | ---: | ---: | ---: |
| numeric_loops | 6.307 | 0.454 | 6.088 | 0.733 | 0.965 |
| strings | 6.609 | 0.297 | 6.760 | 3.631 | 1.023 |
| collections | 4.662 | 0.667 | 4.563 | 1.217 | 0.979 |
| calls_and_interfaces | 2.764 | 0.106 | 3.135 | 0.067 | 1.134 |
| generics | 4.070 | 0.882 | 2.979 | 0.097 | 0.732 |
| allocation_mixed | 8.122 | 2.189 | 11.895 | 0.658 | 1.465 |
| longrun_frame | 1.845 | 0.159 | 2.063 | 0.057 | 1.118 |
| longrun_request | 7.525 | 0.728 | 7.518 | 0.259 | 0.999 |
| cache_churn | 3.978 | 0.220 | 4.100 | 0.236 | 1.031 |
| object_graph | 2.337 | 0.154 | 3.140 | 0.240 | 1.344 |
| accessor_heavy | 5.610 | 0.279 | 5.863 | 0.384 | 1.045 |
| interface_dispatch | 4.184 | 0.340 | 4.329 | 0.341 | 1.035 |
| collection_growth | 2.045 | 0.380 | 1.916 | 0.141 | 0.937 |
| string_pipeline | 9.458 | 0.261 | 9.640 | 0.449 | 1.019 |
| frontend_generated | 0.052 | 0.004 | 0.064 | 0.006 | 1.235 |

Geometric mean C6/C0: 1.058

#### Class A, `memory_matrix --scale small`, C0

| Case | Region | Status | Checksum | Allocations | Requested | Used | Reserved | Peak used |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| object | temporary | pass | 390000 | 10000 | 40000 | 0 | 4096 | 4 |
| object | persistent | pass | 390000 | 10000 | 40000 | 0 | 4096 | 4 |
| array | temporary | pass | 70000 | 10000 | 40000 | 0 | 4096 | 20 |
| array | persistent | pass | 70000 | 10000 | 40000 | 0 | 4096 | 20 |
| string | temporary | pass | 20000 | 10000 | 100000 | 0 | 4096 | 10 |
| string | persistent | pass | 20000 | 10000 | 100000 | 0 | 4096 | 10 |
| mixed | temporary | pass | 420000 | 30000 | 180000 | 0 | 4096 | 42 |
| mixed | persistent | pass | 420000 | 30000 | 180000 | 319968 | 323584 | 319982 |

#### Class A, `memory_matrix --scale small`, C6 (H12)

| Case | Region | Status | Checksum | Allocations | Requested | Used | Reserved | Peak used |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| object | temporary | pass | 390000 | 10000 | 160000 | 0 | 4096 | 16 |
| object | persistent | fail | 390000 | 10000 | 160000 | 32 | 4096 | 32 |
| array | temporary | pass | 70000 | 10000 | 40000 | 0 | 4096 | 36 |
| array | persistent | fail | 70000 | 10000 | 40000 | 96 | 4096 | 96 |
| string | temporary | pass | 20000 | 10000 | 260000 | 0 | 4096 | 26 |
| string | persistent | fail | 20000 | 10000 | 260000 | 64 | 4096 | 64 |
| mixed | temporary | pass | 420000 | 30000 | 460000 | 0 | 4096 | 82 |
| mixed | persistent | pass | 420000 | 30000 | 460000 | 144 | 4096 | 144 |

#### Class A, `memory_matrix --scale small`, C6-H16

| Case | Region | Status | Checksum | Allocations | Requested | Used | Reserved | Peak used |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| object | temporary | pass | 390000 | 10000 | 240000 | 0 | 4096 | 24 |
| object | persistent | fail | 390000 | 10000 | 240000 | 48 | 4096 | 48 |
| array | temporary | pass | 70000 | 10000 | 40000 | 0 | 4096 | 36 |
| array | persistent | fail | 70000 | 10000 | 40000 | 96 | 4096 | 96 |
| string | temporary | pass | 20000 | 10000 | 260000 | 0 | 4096 | 26 |
| string | persistent | fail | 20000 | 10000 | 260000 | 64 | 4096 | 64 |
| mixed | temporary | pass | 420000 | 30000 | 540000 | 0 | 4096 | 90 |
| mixed | persistent | pass | 420000 | 30000 | 540000 | 176 | 4096 | 176 |
