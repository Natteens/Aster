# Tasks, parallel operations, and workers

ASTER has an explicit, restricted worker model. It is not a general shared-memory threading API.
The compiler validates every worker boundary before MIR reaches the JIT.

## Tasks

`Task.Run` schedules a directly named public free function or static method. The zero-argument form
remains valid, and runtime values may follow the target:

```aster
public int Add(int left, int right)
{
    return left + right;
}

public int Main()
{
    Task<int> task = Task.Run(Add, 20, 22);
    return task.Wait();
}
```

Arguments are evaluated exactly once, left to right, in the caller. The caller then copies their
closed scalar/value layouts into a worker-owned transfer frame; the worker never retains a pointer
into the caller's arena or stack.

### What crosses: transfer by copy

Invariant: no reference crosses a domain; transfers copy. A value crosses a worker boundary (as a
`Task.Run` argument and as a `Wait`/`WaitAll` result) when its type is

- a scalar (`bool`, `char`, integers, floats) or a `string`;
- an array `T[]`, a `List<T>` or a `Dictionary<K, V>` whose element/key/value types cross;
- a finite struct or enum whose fields (or case payloads) all cross.

`class` instances, interfaces, `Task<T>` and `decimal` are rejected at compile time (`cannot cross
a worker boundary`), including when nested inside a transferable type (`List<Box>`).

The copy is deep and independent: the value is serialized in the source domain into a
domain-independent byte buffer and materialized in the destination `ExecutionContext` with fresh
allocations owned by the destination (strings, array/list/dictionary headers and element buffers
go through the normal runtime allocation paths, so reference counts and descriptors initialize as
for any new value). No pointer into the source context survives. Mutating the copy never changes
the original and vice versa. Aliasing is not preserved (a value reachable twice is copied twice),
dictionary insertion order is preserved, and an empty container stays an empty container.

Cost: a transfer is O(size of the value) in time and transient memory (the serialized buffer plus
the materialized copy). Waiting twice on the same task materializes two independent copies.
Zero-copy sharing of immutable data is deferred (CONC-005b).

Limits of this version: `await Task.Run(...)` and async frame slots stay scalar-only, and
`Parallel.For`/`ForEach`/`Reduce` inputs and results stay scalar; using a copied type there is a
compile error.

The ordinary overload and generic-specialization machinery resolves the target. A generic target
must infer one fully concrete specialization from the runtime argument types. Instance/bound
methods, closures, captures, and open generic targets are not Task callables.

`Task<T>.Wait()` is repeat-readable: a second wait, an alias wait, and a wait after composition
observe the same terminal result or controlled failure. Task results are copied (see "What
crosses"); each wait materializes its own copy.

## Deterministic composition

`Task.WaitAll(Task<T>[] tasks)` synchronously waits already-created homogeneous tasks and returns a
caller-owned `T[]`. It does not spawn helper work. Empty input returns an empty array, duplicate
handles repeat the cached result, and `result[i]` always corresponds to `tasks[i]`.

Composition waits every relevant task to a terminal state. If tasks failed, the failure at the
lowest input index is reported. Otherwise, if tasks were cancelled, the cancellation at the lowest
input index is reported. Worker completion order never selects the public failure. The result array
is allocated through the caller's `ExecutionContext` (and its memory governor, when one is
configured) only after every task succeeds, and is fully initialized before publication.

## Cooperative cancellation

`bool Task<T>.Cancel()` requests cancellation and returns `true` only for the first accepted request
made before the task became terminal. A request before execution prevents the body from starting. A
running task observes the request explicitly with `Task.IsCancellationRequested()`; the same query
returns `false` outside a task execution context. ASTER does not inject polling on instructions,
calls, array access, or loop backedges.

An accepted request followed by normal worker completion produces the terminal `Cancelled` state.
A real ASTER runtime failure wins over cancellation. Completed, failed, and cancelled states are
immutable, and cancellation after completion does not erase a result. `Wait`, `await`, and
`WaitAll` surface cancellation through the existing controlled runtime-error convention. The
private cancellation flag is scheduler metadata only: it exposes no ASTER reference or general
atomic/shared-memory facility.

Cancellation and terminal completion are atomically ordered. If completion becomes terminal first,
`Cancel()` returns `false` and the result stays completed. If the request is accepted first,
`Cancel()` returns `true` and a normal finish becomes cancelled. A real runtime failure remains
failed in either ordering.

ASTER intentionally provides no timeout API in this version. Timeouts would require a timer owner
and race contract beyond the existing task-owned cancellation/completion mechanism.

## Async functions

Async functions return `Task<T>`, and `await Task.Run(Target, arguments...)` uses the same concrete
transfer frame and terminal outcomes as synchronous `Wait`. Current async lowering still accepts
the documented single direct await shape. Every async step receives the same task-owned cancellation
control in its fresh execution context, including after suspension; no thread-local or process-global
current-task state is used. Nested task or parallel operations reachable from a worker body are
rejected, including through helpers and generic specializations.

## Parallel operations

The compiler recognizes:

- `Parallel.For(start, end, body)`;
- `Parallel.ForEach(array, body)`;
- `Parallel.Reduce(array, identity, accumulate, combine)`.

The callable signatures and element types are concrete before HIR and MIR. Worker execution may
complete in a different order, so programs cannot depend on worker scheduling order. Parameterized
`Task.Run` does not weaken Parallel's existing worker-transfer or nested-concurrency barriers.

`Parallel.Reduce` is reproducible: its result does not depend on the machine or worker count, even
for a non-associative `combine` such as floating-point addition. For an array of `length` elements
the chunk count is `chunks = min(length, 64)`, and chunk `i` covers the half-open index range
`[i * length / chunks, (i + 1) * length / chunks)` (integer division, computed without overflow).
Each chunk folds `accumulate` left to right starting from `identity`, and the per-chunk results are
combined with `combine` left to right in chunk order. An empty array returns `identity` without
calling `accumulate` or `combine`. The worker count only affects scheduling. Because every chunk
starts from `identity`, `identity` should be neutral for `combine`.

## Scheduling behavior

These are implementation notes; scheduling order is not part of the language contract.

- A task without a suspension point runs to completion on one thread and is never preempted.
- Each worker serves its own queue first and yields its thread to the shared injection queue after
  64 consecutive tasks when work is waiting there, so new work cannot be starved.
- `task.Wait()` and `Task.WaitAll` on the main thread block it and run nothing else. A worker
  thread blocked in a wait may run other queued tasks instead of idling, nesting at most 4 deep;
  beyond that it blocks (no program can wait inside a task yet: nested tasks remain rejected). Results and failure selection are unchanged
  (`WaitAll` order, lowest-index failure).
- There is no work stealing and there are no compensation threads in this version; whether they
  are needed is decided by measurement (CONC-EXP-2). Nested `Task.Run` inside tasks remains rejected.

## Isolation and deliberate limits

Every worker has its own `ExecutionContext`, arenas, allocation limit, task-control view, and
host-resource boundary. Default execution gives each worker context its own 1 GiB limit; a shared
memory governor exists only in research configurations. No ASTER object graph, collection backing, interface table, or owned-region
pointer crosses workers. Non-transferable values may be created and consumed entirely inside one
worker when they do not escape or reach a prohibited operation.

Terminal, filesystem, foreign, and clock intrinsics are host operations and are rejected when
directly or transitively reachable from worker bodies. Pure text, math, parsing, array, and seeded
random algorithms remain usable inside a worker when their values satisfy the existing type rules.
Mutable `Random` and `StringBuilder` instances are not transferable; `List` and `Dictionary` values
cross only by deep copy, never as a shared instance.

There is no public thread handle, shared-memory synchronization, user-visible atomic, cancellation
token hierarchy, closure/delegate capture, forced thread termination, implicit parallelization, or
general nested task graph.
