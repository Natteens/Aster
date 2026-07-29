# Memory management

ASTER currently uses execution-scoped arenas instead of a tracing garbage
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

The temporary arena holds objects, arrays, lists, dictionaries, and dynamic strings that the
compiler proves do not escape their containing function.

A generated function with at least one temporary allocation enters a temporary
scope on entry. Every normal `Return` and `End` leaves that scope. Leaving a
scope rewinds the arena to its checkpoint, zeroes reclaimed bytes, and keeps
the pages reserved for later reuse. Nested calls use LIFO checkpoints, so a
callee cannot invalidate a caller's temporary values.

## Escape analysis

Escape analysis runs after MIR lowering and before Cranelift validation. It:

1. tracks local aliases of class, array, list, dictionary, and string references;
2. builds summaries for direct ASTER calls;
3. solves recursive call components to a monotone fixpoint;
4. classifies every dynamic allocation;
5. writes `AllocationRegion::Temporary` only for proven local candidates.

The analysis is intentionally conservative. Uncertainty selects persistent
storage. There is no silent fallback in the backend: each MIR region maps to a
specific runtime ABI function.

## Collections and strings

An array header and its element buffer always use the same arena. Rewinding
cannot leave a live header pointing at reclaimed element storage.

`List<T>` and `Dictionary<K,V>` headers and their current backing buffers also share one selected
region. Growth can retain older arena buffers until the containing temporary scope rewinds or the
execution context is dropped; there is no individual deallocation.

String literals are not arena allocations; they live in the JIT module data
section. Concatenation, interpolation, and scalar formatting create dynamic
strings in either the persistent or temporary arena. These operations always
copy their result, including empty-operand concatenation, so the destination
region determines the lifetime.

## Statistics

`aster run <FILE> --memory-stats` prints one snapshot after execution.

- `allocations`, `objects`, `arrays`, and `strings` are cumulative logical allocation counts.
  Collection headers and backing buffers use the existing object/array accounting categories.
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

The repository includes eight formal workloads under `benchmarks/memory`: an
object, array, dynamic string, and mixed program in both a temporary and a
persistent variant. Each workload runs at `small`, `medium`, and `large` scales
and returns a deterministic checksum.

The `memory_matrix` executor compiles each workload, runs a warmup, then takes
timed samples. It verifies the checksum, the per-category allocation counts, the
expected region, and the temporary and persistent `used_bytes` invariants, and
emits a structured JSON document.

```console
cargo run --release -p aster-codegen-cranelift --example memory_matrix -- --scale small,medium
cargo run --release -p aster-codegen-cranelift --example memory_matrix -- --scale small --json
```

The JSON separates deterministic metrics, timing, and metadata. Only the
checksum and the `MemoryStats` fields are frozen in a baseline, and only for a
matching target and profile. Timing (`frontend_compile`, `jit_and_execute`,
`end_to_end`) is informative: it includes Cranelift code generation,
finalization, and execution, is never frozen, and is never compared, because
wall-clock time is not reproducible across machines.

Validation and comparison are manual. Run `memory_matrix`, validate the JSON
with `node benchmarks/memory/compare.mjs validate`, optionally generate a local
baseline for the real target and profile with `compare.mjs to-baseline`, and
compare later reports with `compare.mjs compare`. The comparator rejects a
mismatched schema, target, or profile, reports per-field deterministic
differences, and ignores timing. No baseline is shipped in the repository and
metrics are never copied between targets. `large` is for manual local runs only.
See `benchmarks/memory/README.md` for the schema and the baseline procedure.

The older two-workload timing example remains available:

```console
cargo run --release -p aster-codegen-cranelift --example memory_regions
```

Use these benchmarks as a same-machine regression check, not as a cross-machine
performance claim.

## Current boundary

This model is designed for one bounded JIT execution. ASTER does not yet
provide long-lived ownership, individual deallocation, reference counting, or
a garbage collector. Pages are released when the `ExecutionContext` is
dropped. Future ownership work must preserve the explicit MIR region contract
and controlled runtime boundary.
