# Memory management

Aster currently uses execution-scoped arenas instead of a tracing garbage
collector or reference counting. Every JIT invocation owns one
`ExecutionContext`, and that context owns two paged bump allocators.

## Regions

The persistent arena holds values that may still be reachable after the
function that created them returns. Typical reasons are:

- returning the value;
- storing it in a field, array, struct, or enum;
- converting an object to an interface;
- passing it to a call whose summary cannot prove a non-escaping use;
- any unsupported or uncertain aliasing pattern.

The temporary arena holds objects, arrays, and dynamic strings that the
compiler proves do not escape their containing function.

A generated function with at least one temporary allocation enters a temporary
scope on entry. Every normal `Return` and `End` leaves that scope. Leaving a
scope rewinds the arena to its checkpoint, zeroes reclaimed bytes, and keeps
the pages reserved for later reuse. Nested calls use LIFO checkpoints, so a
callee cannot invalidate a caller's temporary values.

## Escape analysis

Escape analysis runs after MIR lowering and before Cranelift validation. It:

1. tracks local aliases of class, array, and string references;
2. builds summaries for direct Aster calls;
3. solves recursive call components to a monotone fixpoint;
4. classifies every dynamic allocation;
5. writes `AllocationRegion::Temporary` only for proven local candidates.

The analysis is intentionally conservative. Uncertainty selects persistent
storage. There is no silent fallback in the backend: each MIR region maps to a
specific runtime ABI function.

## Arrays and strings

An array header and its element buffer always use the same arena. Rewinding
cannot leave a live header pointing at reclaimed element storage.

String literals are not arena allocations; they live in the JIT module data
section. Concatenation, interpolation, and scalar formatting create dynamic
strings in either the persistent or temporary arena. These operations always
copy their result, including empty-operand concatenation, so the destination
region determines the lifetime.

## Statistics

`aster run <FILE> --memory-stats` prints one snapshot after execution.

- `allocations`, `objects`, `arrays`, and `strings` are cumulative logical
  allocation counts.
- `requested` is cumulative logical requested storage. It excludes arena
  alignment padding and excludes the separate array header.
- `used` is the current consumed storage across both arenas.
- `reserved` is the total page capacity retained by both arenas.
- `peak used` and `peak reserved` are maximum simultaneous values.

A successful temporary-only workload can therefore report many allocations,
non-zero requested and peak values, `used: 0 bytes`, and non-zero reserved
capacity. That means memory was reclaimed and its pages were retained for
reuse.

## Reproducible comparison

The repository includes equivalent temporary and persistent workloads under
`benchmarks/memory`.

```console
cargo run --release -p aster-codegen-cranelift --example memory_regions
```

The benchmark excludes Aster source compilation from timing, but includes
Cranelift code generation, finalization, and execution for each sample. Use it
as a same-machine regression check, not as a cross-machine performance claim.

## Current boundary

This model is designed for one bounded JIT execution. Aster does not yet
provide long-lived ownership, individual deallocation, reference counting, or
a garbage collector. Pages are released when the `ExecutionContext` is
dropped. Future ownership work must preserve the explicit MIR region contract
and controlled runtime boundary.
