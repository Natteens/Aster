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
into the caller's arena or stack. Scalars and finite structs/enums containing only transferable
fields are accepted. Strings, arrays, collections, class/interface references, tasks, and any
aggregate containing one of those references are rejected. Aggregate transfer is a value copy, not
a reference alias or ownership transfer.

The ordinary overload and generic-specialization machinery resolves the target. A generic target
must infer one fully concrete specialization from the runtime argument types. Instance/bound
methods, closures, captures, and open generic targets are not Task callables.

`Task<T>.Wait()` is repeat-readable: a second wait, an alias wait, and a wait after composition
observe the same terminal result or controlled failure. Task results remain restricted to the
existing transferable scalar set.

## Deterministic composition

`Task.WaitAll(Task<T>[] tasks)` synchronously waits already-created homogeneous tasks and returns a
caller-owned `T[]`. It does not spawn helper work. Empty input returns an empty array, duplicate
handles repeat the cached result, and `result[i]` always corresponds to `tasks[i]`.

Composition waits every relevant task to a terminal state. If tasks failed, the failure at the
lowest input index is reported. Otherwise, if tasks were cancelled, the cancellation at the lowest
input index is reported. Worker completion order never selects the public failure. The result array
is allocated through the caller's `ExecutionContext` and `MemoryGovernor` only after every task
succeeds, and is fully initialized before publication.

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

## Isolation and deliberate limits

Every worker has its own `ExecutionContext`, arenas, memory-governor domain, task-control view, and
host-resource boundary. No ASTER object graph, collection backing, interface table, or owned-region
pointer crosses workers. Non-transferable values may be created and consumed entirely inside one
worker when they do not escape or reach a prohibited operation.

Terminal, filesystem, foreign, and clock intrinsics are host operations and are rejected when
directly or transitively reachable from worker bodies. Pure text, math, parsing, array, and seeded
random algorithms remain usable inside a worker when their values satisfy the existing type rules.
Mutable `Random`, `StringBuilder`, `List`, and `Dictionary` instances are not transferable.

There is no public thread handle, shared-memory synchronization, user-visible atomic, cancellation
token hierarchy, closure/delegate capture, forced thread termination, implicit parallelization, or
general nested task graph.
