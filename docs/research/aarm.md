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

## AARM-2A shared MemoryGovernor

AARM-2A adds an opt-in, runtime-owned `MemoryGovernor` for research executions. Production and
default ASTER execution remain unchanged: `ExecutionContext::new()` and
`ExecutionContext::with_stats()` still use only the existing 1 GiB per-context safety limit. A
governed context is created explicitly with a shared `Arc<MemoryGovernor>` and continues to use the
current `PagedArena` and `std::alloc` page backing.

The governor limits retained arena page capacity across every participating Temporary and
Persistent arena. It does not limit logical requested bytes, live used bytes, process RSS, virtual
address space, or OS committed backing. Both authorities apply to a governed context:

```text
existing 1 GiB local context safety ceiling
AND
explicit shared governor hard ceiling
```

An arena consults the governor only when it has calculated the exact capacity of a fresh page and
has already established that its local context limit permits that page. Admission atomically
reserves the page capacity before the host allocation. Allocation from the active page and reuse of
an inactive retained page perform no governor lookup, atomic reservation, or shared-counter update.

Every successful admission creates one reservation owned by the resulting page. If host allocation
fails, the unpublished page construction drops the reservation and restores the budget. Rewind does
not release a reservation because the current allocator retains the page. Destroying the page,
normally through context teardown, releases its reservation exactly once.

The governor atomics use relaxed ordering because they protect numeric resource accounting only.
Page publication and memory ownership stay within the exclusively borrowed arena and do not rely on
the counters for cross-thread synchronization. The compare/exchange admission loop prevents budget
overshoot. After a completed operation, cumulative granted bytes minus cumulative released bytes
equals current governed capacity; a snapshot taken during a concurrent in-flight admission or
release may observe the independently loaded counters at adjacent instants. Governor snapshots are
therefore observational, not transactional: cross-field equalities are guaranteed after quiescence,
not during concurrent mutation. In particular, an admission updates current capacity before its
peak and grant counters, so a racing snapshot may transiently observe current capacity above the
sampled peak or before the matching cumulative grant.

Experimental governor telemetry records:

- `hard_limit_bytes`: fixed shared retained-capacity ceiling;
- `current_capacity_bytes`: page capacity currently owned by all participating arenas;
- `peak_capacity_bytes`: largest successfully admitted current capacity;
- `grant_events`: successful fresh-page capacity admissions;
- `denial_events`: admissions rejected before host page allocation;
- `release_events`: page reservations returned at permanent page destruction;
- `granted_bytes_cumulative`: total capacity admitted over the governor lifetime;
- `released_bytes_cumulative`: total capacity returned over the governor lifetime.

These fields describe governed allocator capacity. They are not committed memory, virtual
reservation, or RSS, and they are absent from stable `--memory-stats` output.

Public Task/Parallel execution is intentionally not governed in AARM-2A. With a naive shared
first-come admission race, host scheduling could decide which worker receives the final grant and
therefore which logical worker first encounters memory exhaustion. ASTER's deterministic worker
failure ordering requires an explicit admission or quota design before integration. AARM-2B owns
that design; AARM-2A does not change worker-pool behavior.

## AARM-2B1 deterministic Parallel partitions

AARM-2B1 adds an opt-in research execution path for governed `Parallel.For`,
`Parallel.ForEach`, and `Parallel.Reduce`. Default ASTER execution remains unchanged. Ordinary
`Task.Run`, async tasks, and `MoveNext` contexts remain ungoverned, and governed Parallel testing is
kept isolated from dynamic task admission until a later slice defines that policy.

At the start of a Parallel operation, the host reads the governor hard limit and the capacity
already retained by the governed main context, then computes:

```text
available headroom = hard limit - current retained capacity
```

The host thread is synchronously blocked inside the Parallel call after array values have been
copied to host-owned scalar storage. No callback can allocate through the main context while worker
chunks run. The captured main-context capacity is therefore stable for this isolated experimental
operation.

Available headroom is divided by logical chunk count, never worker identity. Because the current
allocator needs one 4 KiB minimum page for a fresh small context, the planner first gives whole
minimum-page entitlement to the lowest logical chunk indexes. If headroom cannot fund every chunk,
the next logical chunk receives the remaining sub-page tail and later chunks receive zero. Once
every chunk has one minimum page, surplus bytes are divided evenly and the earliest chunks receive
one extra byte until the remainder is exhausted. This page-aware initial distribution prevents
usable whole-page capacity from being stranded as sub-page fragments while every resulting local
ceiling remains byte-based for growing regular and oversized pages. Shares sum exactly to the
captured headroom. The entitlement travels in the owned `JobKind` value for that logical chunk, so
any free worker can execute it without changing its budget.

Each chunk context has two independent fail-closed authorities:

```text
fixed chunk-local retained-capacity ceiling
AND
shared MemoryGovernor hard ceiling
```

The local ceiling covers the chunk's Temporary and Persistent arenas together. It is not charged
up front, and the unchanged 1 GiB context safety ceiling still caps any larger share: governor
capacity records only real retained pages. Fresh pages must pass the effective local check before
shared admission; active-page bumps and inactive retained-page reuse perform no quota or governor
operation. Destroying the chunk context releases its real page reservations.

An early-finishing chunk does not enlarge any surviving chunk's local ceiling. This deliberately
rejects concurrent first-come borrowing: scheduler-dependent allocation success could create a
scheduler-dependent worker failure, which is incompatible with deterministic language semantics.
Dynamic Task admission and deterministic borrowing remain AARM-2B2/2B3 research problems.

`Parallel.For` and `Parallel.ForEach` retain smallest-logical-index failure selection.
`Parallel.Reduce` accumulation keeps chunk-index partial ordering and smallest logical array
position failure selection. After accumulation contexts are destroyed, each left-to-right combine
step runs alone with a newly calculated one-chunk share of the headroom available at that
deterministic sequential point. Released accumulation capacity can therefore be reused by combine
without introducing a concurrent admission race.

The AARM matrix records host-side planning data separately from governor telemetry: initial
governed capacity, available headroom, exact logical chunk shares, and their minimum/maximum. Worker
allocator snapshots provide fast/slow allocation and fresh/reused page evidence. Governor metrics
continue to describe real backing capacity only; quotas are never counted as granted capacity.

## AARM-2B2A deterministic Task.Run memory domains

AARM-2B2A adds a separate opt-in research entry point for governed plain `Task.Run`. It does not
govern async `MoveNext`, `AsyncSpawnInner`, or awaited async-inner jobs. Default ASTER execution is
unchanged. The experimental entry point rejects modules containing async execution or Parallel
operations before execution; those domains are not silently combined while their entitlement
interaction remains unproven.

Unlike Parallel chunks, the total number of future `Task.Run` submissions is not known in advance.
The first governed plain-task submission therefore freezes one execution-owned Task Memory Domain
before that job can enter the worker queue. At this capture point no task in the domain has begun
allocating. The runtime records the governor's current retained capacity, subtracts it from the hard
limit, and divides only that remaining future headroom between Main and a bounded number of
simultaneously live task contexts. Main keeps every page it already owns; its local limit becomes:

```text
Main capacity retained at capture + frozen Main future-growth entitlement
```

The limit can never be tightened below retained capacity. This removes first-come competition
between later Main growth and task growth. Main is synchronously executing the submission ABI while
the plan is installed, and all subsequent growth is checked against the frozen local authority
before shared governor admission.

The task planner uses the allocator's actual 4 KiB minimum fresh-page capacity. It first chooses a
memory concurrency limit equal to the smaller of the physical worker bound and the number of whole
minimum pages available. If less than one page remains, one task slot receives the sub-page byte
tail and therefore fails ordinary allocation deterministically rather than multiplying unusable
fragments across every worker. Each memory-active slot receives one minimum-page entitlement.
Remaining bytes are divided uniformly among those task slots plus Main; every task receives the
same extra amount and Main receives the deterministic remainder. The existing 1 GiB local ceiling
caps either entitlement. Resulting limits remain bytes, so regular-page growth and oversized pages
are admitted or rejected using their actual capacity rather than page-count approximations.

Every plain task in the frozen domain receives the same task-context ceiling. The immutable domain
travels with the logical job, not a worker identity, and any worker constructs an equivalent fresh
governed `ExecutionContext`. If useful whole-page entitlement cannot cover every physical worker,
worker preparation is deterministically capped to the domain's memory concurrency. Additional
tasks remain FIFO-queued. A queued handle owns no page, grant, or pre-charged quota. More task
handles may exist than workers or memory-active slots.

A task context owns its real page reservations only for that invocation. Completion or controlled
failure destroys the context and releases those reservations exactly once; the cached scalar
`TaskOutcome` retains no arena. A later queued task can use its same fixed entitlement after a slot
and real governor capacity are released. That is sequential reuse, not quota borrowing. Completion
never enlarges the entitlement of an already-live or later task, and `Wait` order only observes the
already-cached per-handle result.

Fresh task pages retain both fail-closed authorities:

```text
uniform frozen task-context ceiling
AND
shared MemoryGovernor hard ceiling
```

Quotas are not governor grants. Only an actual fresh page performs local checking, shared
admission, and host allocation. Active-page bump allocation and inactive-page reuse do not touch
the Task Memory Domain or governor shared state. The shared governor remains the final authority if
a planner defect or another participant invalidates the entitlement assumptions.

Task-domain telemetry records the initial governed capacity, captured headroom, Main retained and
future-growth bytes, effective Main and task local ceilings, memory concurrency limit, submissions,
contexts started/completed, task-local memory failures, task fast-path allocations, and task fresh
page allocations. These are experimental planning/counter observations, not committed or reserved
OS memory. Counter snapshots use independent relaxed atomics and are observational during active
mutation; equality assertions are made after worker quiescence.

There is no task quota borrowing or dynamic enlargement in AARM-2B2A. Async governance and a safe
unified Task/Parallel entitlement plan are explicitly deferred.

## AARM-2B2B deterministic async memory domain

AARM-2B2B adds a separate opt-in research entry point for async execution. Default ASTER execution
is unchanged. The entry point rejects independent plain `Task.Run` and Parallel operations before
execution; only the `AsyncSpawnInner` job produced by the validated single-`await Task.Run(...)`
state machine participates in this domain.

The Async Memory Domain freezes at the first unresolved governed `Task<T>.Wait()`, immediately
before the pump's first `MoveNext` step. `AsyncSpawn` remains lazy, so Main may allocate between
creating the handle and waiting. Capture records Main's real retained capacity and the governor's
remaining headroom, then releases the Main borrow before JIT execution. Main keeps every retained
page and receives a fixed future-growth entitlement for execution after `Wait` returns.

Future headroom is assigned in canonical role order:

```text
MoveNext -> awaited inner -> Main future growth
```

The page-aware planner first assigns whole allocator minimum pages in that order. If the remaining
headroom is smaller than a minimum page, the next role receives the byte tail rather than
fragmenting it across every role. Once all three roles have a page, surplus is split evenly with
earlier roles receiving the deterministic remainder. The local 1 GiB context ceiling still caps
each role; representable headroom beyond role caps remains enforced by the governor rather than
being reported as an entitlement. A zero-byte context can execute allocation-free code and fails
with a controlled role-specific error on its first fresh allocation.

MoveNext and awaited-inner entitlements are simultaneously included in the frozen plan. This is
required because a worker can begin `AsyncSpawnInner` while the submitting MoveNext context is
still alive. Every MoveNext invocation nevertheless receives a fresh context, just as before;
dropping the step releases its real pages, and a resumed step receives the same fixed entitlement.
The awaited-inner policy travels with its logical job, never worker identity, and its fresh worker
context releases real page reservations before the scalar `TaskOutcome` is cached.

Entitlements are not pre-charged governor capacity. Only actual fresh pages pass the unchanged
sequence of local limit check, shared governor admission, and host allocation. Active-page bumps
and inactive-page reuse perform no async-domain or governor shared-state operation. Released pages
may be reused by a later step or serial async handle with the same fixed limit; they never enlarge
another live entitlement, so async quota borrowing is not implemented.

The host `AsyncTask.frame` is Rust-owned scalar storage, not arena capacity, OS commitment, virtual
reservation, or RSS-controlled memory. Compiler semantic validation rejects reference-typed locals
before suspension; MIR validation independently requires every async frame slot to be worker
transferable. Awaited inner results and published async results use the same fixed-width scalar
transport, so neither an arena pointer nor worker arena ownership can survive context teardown.

Async-domain telemetry records the frozen Main/MoveNext/inner plan, handle and context lifecycle
counters, role-specific memory failures, allocator fast/fresh page events, and peak simultaneous
governed async contexts. Governor telemetry remains the authority for real retained capacity.
Relaxed event-counter snapshots are observational during mutation and are asserted only after
workers quiesce.

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
shapes. AARM-2A extends it with paired governed/control tiny-allocation and page-growth cases,
manually governed 1/4/16-context aggregates, shared-limit denial, and teardown/reuse. Default burst
sizes are measured in tens of MiB or less; `large` must be selected explicitly. AARM-2B1 adds
ordinary/governed `Parallel.For`, `ForEach`, and `Reduce` comparisons at 1/4/16 worker shapes,
tight and uneven logical partitions, repeated deterministic denial, and sequential Reduce combine
headroom reuse.
AARM-2B2A adds ordinary/governed empty-task, fast-path-heavy small-allocation, moderate-allocation,
and task-swarm controls at 1/2/4/16 worker shapes. It also covers more-task-than-worker queueing,
concurrent Main/task growth, teardown and sequential reuse, a tight page-aware domain, and repeated
deterministic task-local denial.
AARM-2B2B adds ordinary/governed trivial async, allocation before and after suspension, awaited
inner allocation, Main post-Wait growth, page-tight domain plans, repeated Wait, and multiple
serial async-handle cases.

```console
cargo run --release -p aster-codegen-cranelift --features aarm-telemetry --example aarm_memory_matrix -- --scale small
cargo run --release -p aster-codegen-cranelift --features aarm-telemetry --example aarm_memory_matrix -- --scale small --json
```

Structured output separates allocator telemetry, Parallel planning, Task/Async Memory Domain planning,
process RSS, checksum, scale, and informational elapsed time. Timing is never a correctness gate.
Generated reports and machine-local baselines are not committed.

## Metrics that do not exist yet

AARM-2B2B does not expose or infer:

- `virtual_reserved_bytes`;
- `committed_backing_bytes`;
- purge or decommit events and bytes;
- adaptive governor quotas, borrows, or host-memory policy;
- decay, hysteresis, or pressure state.

These fields remain absent until an architecture exists that can measure them accurately.

## Program phases

```text
AARM-1 observability
-> AARM-2A shared MemoryGovernor foundation on current allocator
-> AARM-2B1 deterministic Parallel chunk partitions
-> AARM-2B2A deterministic plain Task.Run memory domains
-> AARM-2B2B async task memory domains
-> AARM-2B3 deterministic quota borrowing research
-> AARM-2C host-adaptive policy research
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
