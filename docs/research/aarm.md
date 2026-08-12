# Adaptive Region Memory (AARM)

> **AARM status:** EXPERIMENTAL
> **Production memory behavior:** UNCHANGED
> **Development branch:** `research/aarm`

AARM is a staged research program for measuring and, only after separate evidence gates, evolving
ASTER's region memory system. The current implementation remains the production authority. Nothing
in this note should be read as a shipped governor, virtual-memory backend, purge policy, or finer
lifetime model.

## Current allocator architecture

Each JIT invocation owns one `ExecutionContext`. The context owns two independent `PagedArena`
instances:

- **Temporary** stores allocations proven not to escape their function. LIFO function checkpoints
  rewind logical usage. Reclaimed bytes are zeroed, while pages remain owned by the arena for reuse.
- **Persistent** stores escaping or conservatively classified allocations until the whole context is
  dropped.

`PagedArena` is a pointer-stable paged bump allocator built on Rust's zeroed host allocations.
Regular pages grow from 4 KiB to 64 KiB. A request larger than 64 KiB receives a dedicated page.
Rewind never moves surviving allocations and does not release retained pages. Dropping the context
releases both arenas.

Worker executions create isolated `ExecutionContext` values. They do not share arenas or ASTER
references with the caller or other workers. The checked per-context allocation limit remains
exactly 1 GiB.

## Terminology

The stable `MemoryStats` fields keep their existing meaning:

- `requested_bytes` is cumulative logical storage requested by successful ASTER operations. It
  excludes arena padding and the separate array header.
- `used_bytes` is current storage consumed inside both arenas, including alignment padding and
  runtime headers according to the existing accounting rules.
- `reserved_bytes` is page capacity currently retained by both `PagedArena` instances.
- `peak_used_bytes` and `peak_reserved_bytes` are maximum simultaneous totals observed by the
  execution context.

In AARM research output the same allocator concepts use less ambiguous names:

- `requested_bytes`: cumulative logical requested storage;
- `live_used_bytes`: current consumed arena bytes;
- `arena_capacity_bytes`: capacity retained by arena pages.

The existing `reserved_bytes` field means **allocator page capacity**. It is not Windows
`MEM_RESERVE`, an OS virtual-address reservation, committed backing, RSS, or an amount available to
the whole process. AARM does not rename the stable field, but experimental output uses
`arena_capacity_bytes` to prevent that confusion.

Array accounting illustrates why the three values differ: the array element buffer contributes to
`requested_bytes`, while the header and alignment padding contribute only to used and capacity
measurements.

## AARM-1 telemetry

AARM-1 adds an internal snapshot beside stable `MemoryStats`. Allocator-event bookkeeping is
compiled only with the opt-in `aarm-telemetry` feature used by the research matrix; ordinary ASTER
builds contain no event-counter hot-path work. Stable `aster run --memory-stats` text is unchanged.

For Temporary, Persistent, and their derived total, the snapshot records:

- current and peak live used bytes;
- current and peak arena capacity bytes;
- active and inactive page capacity;
- total, active, and inactive page counts;
- active-page fast-path allocations;
- slow-path allocations that require page selection;
- inactive-page reuse events;
- successful fresh regular-page allocations;
- successful fresh oversized/dedicated-page allocations;
- rewind events and cumulative rewound bytes;
- allocation-limit denials.

The most recent rewind observation records live bytes before and after, capacity before and after,
and active/inactive retained capacity after rewind. A fresh-page event is counted only after the host
allocation succeeds. A limit denial does not increase live bytes, capacity, peaks, or logical
allocation counts.

No allocator event is called a commit. The current allocator is not an OS page backend.

## Measurement invariants

Every snapshot must satisfy:

```text
temporary.live_used_bytes <= temporary.arena_capacity_bytes
persistent.live_used_bytes <= persistent.arena_capacity_bytes

total.live_used_bytes =
    temporary.live_used_bytes + persistent.live_used_bytes

total.arena_capacity_bytes =
    temporary.arena_capacity_bytes + persistent.arena_capacity_bytes

arena_capacity_bytes =
    active_page_capacity_bytes + inactive_page_capacity_bytes

page_count = active_page_count + inactive_page_count
```

Per-region peaks are local maxima. Total peaks remain the maximum simultaneous combined value from
the execution context; they are not fabricated by adding region peaks that may have occurred at
different times.

## Process RSS

The release-only `aarm_memory_matrix` reads process memory only at workload measurement points,
never inside allocator hot paths.

- Windows x64 uses `GetProcessMemoryInfo`: `WorkingSetSize` for current RSS and
  `PeakWorkingSetSize` for the process-lifetime peak.
- Linux x64 reads `VmRSS` and `VmHWM` from `/proc/self/status`.
- Other hosts report these values as unavailable.

RSS is a whole-process observation. It includes stacks, Rust and native runtime state, JIT code and
data, loaded libraries, arena pages, and other process structures. It is not committed arena memory
and cannot be attributed entirely to ASTER arenas. The OS peak is a process-lifetime high-water mark,
not necessarily a workload-local peak; the matrix separately samples current RSS while direct burst
and worker payloads are live when the platform supports it.

## Baseline matrix

`aarm_memory_matrix` is release-only and supports `small`, `medium`, and manual `large` scales. Its
workloads cover tiny allocations, long-scope temporary retention, helper-scoped control, temporary
burst and rewind, repeated burst/reuse, persistent retention, and isolated 1/4/16-context worker
shapes. Default burst sizes are measured in tens of MiB or less; `large` must be selected explicitly.

```console
cargo run --release -p aster-codegen-cranelift --features aarm-telemetry --example aarm_memory_matrix -- --scale small
cargo run --release -p aster-codegen-cranelift --features aarm-telemetry --example aarm_memory_matrix -- --scale small --json
```

Structured output separates allocator telemetry, process RSS, checksum, scale, and informational
elapsed time. Timing is never a correctness gate. Generated reports and machine-local baselines are
not committed.

## Metrics that do not exist yet

AARM-1 does not expose or infer:

- `virtual_reserved_bytes`;
- `committed_backing_bytes`;
- purge or decommit events and bytes;
- governor borrows or denials;
- decay, hysteresis, or pressure state.

These fields remain absent until an architecture exists that can measure them accurately.

## Program phases

```text
AARM-1 observability
-> AARM-2 MemoryGovernor on current allocator
-> AARM-3 OS PageBackend
-> AARM-4 delayed purge/hysteresis
-> AARM-5 MIR lifetime refinement
-> AARM-6 integration/production decision
```

Each phase requires its own architecture review and focused commit on the long-lived
`research/aarm` branch. Published AARM history is not rebased or force-pushed, and the branch does
not produce releases.

## Frozen architectural invariants

Every AARM experiment must preserve:

```text
No tracing GC.
No moving live objects.
No silent pointer relocation.
Native stable references remain valid.
Temporary/Persistent ownership remains conservative.
Worker ExecutionContexts remain isolated.
HIR/MIR remain typed and backend-neutral.
Runtime remains free of Cranelift types.
Fail-closed checked allocation remains authoritative.
First-error semantics remain deterministic.
```

There is no permitted behavioral shortcut around the current allocation limit, region selection,
rewind semantics, worker isolation, runtime errors, or language behavior.

## Measurement gates

Before an AARM phase can change production memory behavior it must provide:

1. deterministic allocator telemetry tests with no timing thresholds;
2. safe release matrix results at representative scales;
3. same-machine before/after measurements for normal release workloads;
4. evidence that instrumentation or policy does not materially regress normal paths;
5. focused allocation, rewind, collection, string, worker, and first-error compatibility tests;
6. broad workspace, release-core, version-sync, formatting, lint, and diff validation;
7. explicit documentation of unavailable measurements and remaining risk.

Performance claims require repeated same-machine measurements. Noise is not an improvement, and an
observed regression must be redesigned before the relevant phase is accepted.
