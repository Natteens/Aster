# Tasks, parallel operations, and workers

ASTER has an explicit, restricted worker model. It is not a general shared-memory threading API.
The compiler validates every worker boundary before MIR reaches the JIT.

## Tasks

`Task.Run` schedules a public, parameterless callable whose parameter and return boundary is
worker-transferable:

```aster
public int Compute()
{
    return 42;
}

public int Main()
{
    Task<int> task = Task.Run(Compute);
    return task.Wait();
}
```

Async functions return `Task<T>`, and `await` reads a compatible task result. Nested task or
parallel operations reachable from a worker body are rejected.

## Parallel operations

The compiler recognizes:

- `Parallel.For(start, end, body)`;
- `Parallel.ForEach(array, body)`;
- `Parallel.Reduce(array, identity, accumulate, combine)`.

The callable signatures and element types are concrete before HIR and MIR. Worker execution may
complete in a different order, so programs cannot depend on worker scheduling order.

## Transfer boundaries

Worker inputs and outputs are restricted to supported scalar/value layouts. Strings, arrays,
`List<T>`, `Dictionary<K, V>`, class references, interface references, and other non-transferable
aliases cannot cross the boundary. Console and filesystem operations reachable from a worker are
also rejected.

A non-transferable value may be created and consumed entirely inside a worker when it does not
escape or reach a prohibited operation. These rules prevent ASTER references from being shared
between execution contexts.

There is no public thread handle, shared-memory synchronization API, cancellation API, or implicit
parallelization.
