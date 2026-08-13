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

## AARM-2B3 deterministic temporal borrowing

AARM-2B3 keeps **live concurrent quota borrowing intentionally unsupported**. Pointer-stable arena
pages cannot be revoked while their context is live. A first-come enlargement for concurrently
running Parallel chunks or plain `Task.Run` contexts would therefore make scarce-memory success
depend on scheduler order. Parallel keeps fixed logical-chunk ceilings (with its existing
sequential Reduce-combine reuse), and plain Task.Run keeps fixed slot entitlements: completed tasks
release real pages, but no live task receives a larger quota.

The opt-in governed async path now permits **deterministic temporal borrowing** across proven
non-overlapping phases only:

```text
MoveNext -> context drop -> awaited inner -> context drop
         -> resumed MoveNext -> context drop -> Main resumes
```

At capture, Main's post-Wait entitlement remains the same page-aware deterministic share used by
AARM-2B2B. While Main is blocked inside `Wait`, each async phase receives the full captured
headroom up to the unchanged 1 GiB local ceiling. This is an entitlement ceiling, not a governor
grant. Its real pages are released before another phase or Main can allocate.

An execution-owned phase gate travels with each awaited-inner job. A worker may dequeue that job
while the submitting MoveNext runs, but waits before constructing its governed `ExecutionContext`
or obtaining a governor reservation. The host pump opens the gate only after `invoke_move_next`
returns, which means that fresh MoveNext context has dropped. Failure paths settle any accepted
governed inner before `Wait` returns; late completion tokens are harmlessly consumed by a later
serial pump. There is no scheduler-selected borrower, no allocation-path arbitration, no quota
pre-charge, and no borrowing into Main's frozen post-Wait guarantee.

Telemetry adds `temporal_borrowing_enabled`, `phase_context_ceiling_bytes`,
`phase_wait_events`, and `phase_borrowed_contexts`. These describe planning and gate behavior,
not committed backing, virtual reservation, RSS, or governor grants.

## AARM-2C1 stable host capacity discovery

AARM-2C1 adds an experimental, immutable `HostMemoryCapacity` snapshot for a future Auto-budget
resolver. It observes stable host/environment ceilings at execution or matrix startup only. It does
not create a governor, choose a percentage, modify the existing 1 GiB local context ceiling, or
query the operating system from allocation or page-growth paths.

The snapshot records optional `physical_total_bytes`, optional `environment_limit_bytes`, optional
`effective_capacity_bytes`, and its source. A nonzero effective capacity is the conservative
minimum of physical total and a finite environment hard limit when both are known; when only one
reliable ceiling is known it is used directly; when neither is known it remains unavailable. Zero
is never used as an unknown-capacity sentinel.

Stable capacity is **not** currently free/available memory, current RSS, arena capacity, or a
governor budget. The snapshot is a value: it does not change when other processes allocate, RSS or
cgroup usage changes, or a host administrator changes a cgroup after capture. The OS may still
reject a later host allocation independently of a future AARM logical budget.

- Windows x64 uses `GlobalMemoryStatusEx` with `MEMORYSTATUSEX::ullTotalPhys` for physical total.
  A Job Object/container hard limit is intentionally unavailable in this slice: working-set limits
  are not treated as process memory capacity, and robust Job Object limit discovery is deferred.
- Linux x64 parses `MemTotal` from `/proc/meminfo`; it never uses `MemAvailable` or `MemFree`.
  It resolves the current process's cgroup path from `/proc/self/cgroup` against the matching
  `cgroup2` or memory-controller `cgroup` mount in `/proc/self/mountinfo`, including the mount
  root. It reads finite cgroup v2 `memory.max` or v1 `memory.limit_in_bytes` from the resolved
  current cgroup through every ancestor up to that mount root; the smallest finite applicable hard
  limit wins. v2 `max`, v1's page-counter-max unlimited range, malformed input, unreadable files,
  overflow, and zero are unavailable for that individual level. A readable finite ancestor remains
  usable when another level is unavailable; if no finite level is found the environment limit is
  unavailable rather than invented.

`aarm_memory_matrix` captures this snapshot once before running its cases and emits it once at the
top level of its research JSON. This remains separate from its process RSS samples. AARM-2C2, not
AARM-2C1, decides whether and how an Auto governor budget uses the snapshot.

## AARM-2C2 frozen Auto governor budget

AARM-2C2 adds an opt-in research resolver: **Auto hard limit = stable effective host capacity**,
clamped only when the current process cannot represent that `u64` value as `usize`. It applies no
percentage, host reserve, page-size rounding, worker multiplier, free-memory input, RSS input, or
dynamic retuning. Unknown or zero effective capacity is a controlled pre-execution failure rather
than a fallback budget.

One Auto execution discovers capacity once, resolves once, and creates one explicit shared
`MemoryGovernor`. Main and the existing supported Parallel, plain Task.Run, or async domain paths
receive clones of that one authority. Existing unsupported mixed-domain shapes remain rejected.
The resolved limit is frozen for the top-level execution; a later cgroup change may affect OS
allocation outcomes but does not rewrite that logical hard limit.

The Auto limit is a retained ASTER `PagedArena` capacity ceiling, not a target and not a process RSS
guarantee. JIT code/data, Rust host allocations, worker stacks, libraries, async host frames,
compiler/backend allocations, and other process overhead remain outside the governor. Future
pressure/purge work may reduce physical backing, but must not silently change this frozen logical
limit. The current per-`ExecutionContext` 1 GiB local safety ceiling remains in force.

Matrix schema version 8 emits the stable `host_memory_capacity` snapshot and the independently
named `auto_memory_budget` resolution. It includes small integration cases for plain, Parallel,
Task.Run, and async governed execution without consuming the machine-sized Auto ceiling.

## AARM-2C3 explicit reproducible memory-budget override

AARM-2C3 completes the experimental AARM-2 governor foundation with two frozen policies:

- **Auto** discovers stable effective host capacity once, clamps only to process `usize`
  representability, and then freezes the resulting governor limit.
- **Explicit** accepts one caller-provided positive decimal byte count and freezes that exact logical
  governor limit. It performs no host-capacity discovery, percentage calculation, reserve
  subtraction, host/cgroup clamp, free-memory or RSS query, or page-size rounding. An explicit
  value that cannot fit `usize` is rejected rather than silently clamped.

Both policies create one explicit shared `MemoryGovernor` per supported experimental top-level
execution. The governor remains an ASTER retained-`PagedArena` capacity ceiling, not a process RSS
guarantee or memory-consumption target. An explicit limit may exceed discovered host capacity; the
OS can still reject backing earlier through the existing controlled host-allocation path. The 1 GiB
per-`ExecutionContext` local safety ceiling remains authoritative alongside the shared limit.

The research-only matrix accepts `--budget-bytes 67108864` or
`--budget-bytes=67108864`. The value must be one positive base-10 `u64` byte count; zero, signs,
fractions, suffixes, overflow, and duplicate overrides are rejected. Without it, the matrix uses
Auto. Explicit runs expose `host_memory_capacity: null` deliberately: execution behavior did not
query or depend on the host snapshot. Schema version 9 replaces `auto_memory_budget` with one
`memory_budget` object identifying `auto` or `explicit`, requested explicit bytes where applicable,
the resolved hard limit, and address-width handling.

**AARM-2 MemoryGovernor research foundation: COMPLETE.** It consists of shared governor accounting
(2A); deterministic Parallel (2B1), Task.Run (2B2A), async (2B2B), and temporal borrowing (2B3)
domains; stable capacity discovery (2C1); frozen Auto (2C2); and this explicit reproducible
override (2C3). This does not make AARM production-complete: AARM-3 through AARM-6 remain.

## AARM-3A PageBackend abstraction foundation

AARM-3A introduces the runtime-private `PageBackend` seam between `PagedArena` pages and host
backing allocation. The sole active backend is the system-allocator fallback, which preserves the
previous exact `Layout` + `alloc_zeroed` + `dealloc` behavior: page capacity is unchanged, fresh
pages are zeroed and pointer-stable, rewind still zeroes reclaimed bytes, inactive pages remain
retained for reuse, and page destruction releases the backing once.

`PageBackend` is a host-memory mechanism, not a budget authority. `MemoryGovernor` remains the
only shared capacity-policy authority, and governor admission still occurs before a fresh backing
allocation. A failed backend allocation drops its governor reservation without publishing a page.
Neither active-page bump allocation nor inactive-page reuse touches the backend.

Virtual-reserve/backing separation is **not** implemented. The fallback backend means
`MemoryStats.reserved_bytes`, AARM `arena_capacity_bytes`, and governor current capacity retain
their existing meaning: retained logical page capacity backed by the current host allocator. They
are not OS virtual-reservation or committed-backing metrics. At this AARM-3A foundation point,
native VM backends and automatic purge/decommit were still deferred.

## AARM-3B/3C native virtual-memory PageBackends

On Windows, AARM-3B selects `WindowsVirtualPageBackend` as the one production page backend.
Fresh pages use `VirtualAlloc` with `MEM_RESERVE | MEM_COMMIT` and `PAGE_READWRITE`; their
reservation is released once by `VirtualFree(base, 0, MEM_RELEASE)` through the existing backing
RAII owner.

The Windows backend preserves ASTER page semantics. A fresh page is committed and zeroed before
publication, its base remains stable until release, and its logical capacity remains the exact
ASTER request even when Windows internally rounds virtual-memory operations. Rewind does not
decommit, release a reservation, or release a governor grant: inactive pages remain committed and
reusable after the existing reclaimed-byte zeroing.

The backend also contains tested low-level `MEM_DECOMMIT` and fixed-address `MEM_COMMIT`
mechanics for future work. Decommit retains the reservation; recommit must return the original
base and exposes zeroed bytes. Those primitives are not connected to ordinary arena lifecycle in
this slice. `reserved_bytes` and `arena_capacity_bytes` still mean logical retained page capacity,
not Windows virtual reservation or committed/backed bytes.

On Linux, AARM-3C selects `LinuxAnonymousPageBackend`. Fresh pages are anonymous private writable
`mmap` mappings, released exactly once by `munmap` through the same RAII owner. The mapping extent
is page-rounded internally for the Linux API, while page capacity, arena capacity, and governor
charging remain the exact logical ASTER request. Fresh mappings are zero-filled and retain a stable
base until final release. Other targets continue to use `SystemAllocatorPageBackend`.

The Linux backend contains a tested low-level `MADV_DONTNEED` discard primitive for a complete live
anonymous mapping. Unlike Windows `MEM_DECOMMIT`, this leaves the Linux mapping addressable; a later
access faults in demand-zero contents without an explicit recommit. The two operations are not
treated as interchangeable. Ordinary arena rewind does not call either platform primitive: it still
zeroes reclaimed bytes, retains inactive backing for immediate reuse, and keeps governor capacity
charged.

`reserved_bytes` and `arena_capacity_bytes` remain logical retained page capacity, not Windows
reservation, Linux mapping extent, resident/backed bytes, or RSS. Automatic arena
decommit/discard and purge policy are not implemented.

## AARM-3D page backing state and VM telemetry

AARM-3D makes host backing state explicit inside the runtime-private `PageBacking` owner. A page
is either **Retained** or **Discarded**. A discarded page is always inactive with a zero cursor;
it keeps the same page object, logical capacity, governor reservation, and virtual extent. The
state changes only after the selected backend operation succeeds. Before a discarded page returns
to ordinary arena allocation, the backend restores it and the runtime verifies the original base
address before marking it retained again. A restore failure leaves the page inactive and discarded,
without changing logical arena or governor accounting.

The platform contracts deliberately remain distinct:

- Windows discard is `MEM_DECOMMIT`. The reservation survives but its range is inaccessible until
  a fixed-base `MEM_COMMIT` restore succeeds.
- Linux discard is `MADV_DONTNEED`. The anonymous mapping remains addressable and later access
  obtains demand-zero contents; there is no Linux recommit operation.
- The system-allocator fallback has no meaningful virtual extent or backing-state measurement, so
  those experimental values are explicitly unavailable rather than fabricated from logical
  capacity.

Production rewind remains unchanged. It zeroes reclaimed bytes, retains inactive backing, retains
the governor reservation, and makes no discard/decommit request. The discard/restore seam is
exercised only by controlled runtime tests in this slice; automatic discard, purge, and hysteresis
remain deferred.

With the `aarm-telemetry` research feature, each Temporary/Persistent region and its derived total
now expose these optional fields:

- `virtual_extent_bytes`: known VM mapping/reservation extent;
- `backing_retained_bytes`: known extent currently retained by the backing mechanism;
- `backing_discarded_bytes`: known extent discarded but still owned;
- `peak_backing_retained_bytes`: observed high-water retained backing extent.

On Windows and Linux, a known region satisfies `virtual_extent_bytes =
backing_retained_bytes + backing_discarded_bytes`. Totals use checked aggregation and become
unavailable if any participating region is unavailable. Logical `arena_capacity_bytes` and
governor charging remain exact ASTER requested capacities, even when the OS mapping/reservation is
page-rounded. On Windows, retained backing denotes AARM page extents still committed by the backend.
On Linux, it denotes mapping extent not explicitly `MADV_DONTNEED`-discarded by AARM; it is not
RSS, residency, or a physical-memory guarantee. These backing fields are neither RSS nor a promise
of immediate physical-residency change.

The research matrix schema is **10**. It serializes the four optional backing fields as numbers or
`null`, retains process RSS as a separate whole-process observation, and does not add backing state
or VM work to active-page allocation or inactive retained-page reuse.

## AARM-3E PageBackend integration and hardening

AARM-3E closes the AARM-3 infrastructure validation pass without changing production allocation
policy. Deterministic stress coverage exercises retained/discarded transitions, exact logical
governor charging, regular geometric pages, modest oversized pages, best-fit inactive reuse,
restore/discard failure atomicity, mixed retained/discarded teardown, and telemetry identities.
The tests keep the critical boundary explicit: discarded pages are restored at their original base
and return to **Retained** before publication, while a failed restore leaves the page inactive and
discarded without a replacement backing allocation or a new governor grant.

This validates PageBackend infrastructure only. Ordinary rewind still zeroes reclaimed bytes and
retains all inactive backing. AARM-4 owns every production decision about automatic discard,
purge eligibility, delay, hysteresis, and pressure response.

## AARM-4A fully-dead page eligibility and controlled purge

AARM-4A adds a runtime-private, caller-owned `PagedArena` operation that attempts to discard every
currently eligible inactive page in deterministic arena order. A page is eligible only when it is
outside the active prefix, has a zero cursor, is **Retained**, and its selected backend explicitly
supports discard. The arena owns that liveness decision; the backend only performs the platform
operation and `PageBacking` records the resulting Retained/Discarded state.

On Windows, controlled purge uses `MEM_DECOMMIT` and preserves the reservation until same-base
reuse preparation. On Linux it uses `MADV_DONTNEED`; the anonymous mapping remains owned and
addressable, so no explicit recommit exists. The system-allocator fallback reports backing as
unsupported and leaves it retained. Individual discard failures leave their page retained and the
operation continues with later eligible pages; no multi-page transaction or rollback is implied.

Controlled purge does not change logical arena capacity, virtual extent, or MemoryGovernor charges.
It only moves known VM backing extent between retained and discarded telemetry. Ordinary rewind,
temporary-scope exit, allocation, and context teardown do not invoke it automatically.

```text
default automatic purge: DISABLED
delayed purge/hysteresis: implemented only as an opt-in internal policy
pressure-triggered purge: NOT IMPLEMENTED
```

## AARM-4B delayed purge with hysteresis

AARM-4B layers an opt-in, runtime-private policy over AARM-4A's whole-page transition. The policy
is owned by `PagedArena` and is **disabled by default** for ordinary ASTER execution. Enabling it
requires explicit internal/research configuration of three monotonic durations:

- **inactive delay**: minimum time since a whole page entered the inactive suffix;
- **repurge cooldown**: minimum time since a discarded page was restored before it may be
  discarded again;
- **sweep interval**: minimum time between arena-owned maintenance scans.

Only pages newly moved from the active prefix into the inactive suffix receive a new inactivity
timestamp. Unrelated rewinds do not reset older inactive pages, and cursor-only rewind within a
still-active page never makes that page eligible. Retained inactive reuse clears its inactivity
timestamp without a VM call. A restored page records the restore time only after same-address reuse
preparation succeeds; the cooldown then protects it until monotonic time naturally expires.

Rewind is a safe maintenance point only when the policy is explicitly enabled: it first completes
normal rewind state, timestamps newly inactive pages, and then evaluates a due sweep. Research code
can also invoke maintenance at a supplied `Instant`, which makes boundary and hysteresis tests
deterministic without sleeps or a background thread. AARM-4A's immediate caller-owned purge remains
unchanged.

```text
policy default: disabled
pressure-triggered purge: NOT IMPLEMENTED
hot-cache tuning: NOT IMPLEMENTED
production/default timing values: NOT CHOSEN
```

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

when VM backing telemetry is known:
virtual_extent_bytes = backing_retained_bytes + backing_discarded_bytes
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
burst and rewind, repeated burst/reuse, persistent retention, and isolated 1/2/4/8/16-context worker
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
AARM-2B3 adds phase-serialized temporal borrowing controls for before-await, awaited-inner, and
resumed-MoveNext allocations under the same tight governor hard limit.

```console
cargo run --release -p aster-codegen-cranelift --features aarm-telemetry --example aarm_memory_matrix -- --scale small
cargo run --release -p aster-codegen-cranelift --features aarm-telemetry --example aarm_memory_matrix -- --scale small --json
```

Structured output separates allocator telemetry (including optional AARM-3D backing state), Parallel
planning, Task/Async Memory Domain planning, process RSS, checksum, scale, and informational elapsed
time. Timing is never a correctness gate.
Generated reports and machine-local baselines are not committed.

## Metrics that do not exist yet

AARM-3D does not expose or infer:

- operating-system committed/resident bytes;
- virtual reservation telemetry for the system allocator fallback;
- purge or automatic discard events and bytes;
- adaptive governor quotas, borrows, or host-memory policy;
- structured delayed-purge timing/counter telemetry or pressure state.

These fields remain absent until an architecture exists that can measure them accurately.

## Program phases

```text
AARM-1 observability
-> AARM-2A shared MemoryGovernor foundation on current allocator
-> AARM-2B1 deterministic Parallel chunk partitions
-> AARM-2B2A deterministic plain Task.Run memory domains
-> AARM-2B2B async task memory domains
-> AARM-2B3 deterministic temporal borrowing
-> AARM-2C1 stable host capacity discovery
-> AARM-2C2 Auto governor budget policy
-> AARM-2C3 explicit reproducible governor budget override
-> AARM-3A PageBackend abstraction with system allocator fallback
-> AARM-3B Windows VirtualAlloc PageBackend
-> AARM-3C Linux virtual-memory PageBackend
-> AARM-3D page-state and reserve/backing telemetry
-> AARM-3E integration and hardening
-> AARM-4A fully-dead eligibility and controlled purge
-> AARM-4B delayed purge/hysteresis
-> AARM-4C pressure-triggered purge
-> AARM-4D oversized/hot-cache policy and tuning
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
