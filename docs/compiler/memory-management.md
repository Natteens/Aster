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

The temporary arena holds objects, arrays, lists, dictionaries, string builders, and dynamic
strings that the compiler proves do not escape their containing function.

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

`StringBuilder` follows the same rule. Its header records the selected region, and its UTF-8 backing
capacity grows geometrically. Growth inside a deeper helper scope is promoted when necessary so a
live caller-owned header never points into storage that the helper is about to rewind. `ToString()`
allocates an exact-size immutable snapshot in the result's independently selected region; it never
exposes or aliases mutable builder capacity.

String literals are not arena allocations; they live in the JIT module data
section. Concatenation, interpolation, and scalar formatting create dynamic
strings in either the persistent or temporary arena. These operations always
copy their result, including empty-operand concatenation, so the destination
region determines the lifetime.

## Statistics

`aster run <FILE> --memory-stats` prints one snapshot after execution.

- `allocations`, `objects`, `arrays`, and `strings` are cumulative logical allocation counts.
  Collection and `StringBuilder` headers/backing buffers use the existing object category;
  immutable snapshots use the string category.
- `requested` is cumulative logical requested storage. It excludes arena
  alignment padding and excludes the separate array header.
- `used` is the current consumed storage across both arenas.
- `reserved` is the total page capacity retained by both arenas.
- `peak used` and `peak reserved` are maximum simultaneous values.

Regular arena pages start at 4 KiB and grow geometrically to the 64 KiB
steady-state page size. Larger requests use dedicated non-moving pages. This
keeps small executions dense without trading away sustained-allocation
throughput or pointer stability.

The compiler lowers a statically visible string-concatenation tree whose leaves are literals or
stable string values through the existing multi-part join intrinsic. This keeps left-to-right value
order while allocating and sizing the final immutable string once. Chains containing calls,
interpolation, or other effectful expressions retain pairwise concatenation so allocation failure
and side-effect ordering do not change.

Loop-carried `value = value + part` still allocates one immutable result per iteration and copies its
accumulated prefix repeatedly. ASTER does not rewrite that expression because aliases, intermediate
reads, effects, exits, and allocation-failure ordering are observable. Programs can opt into
amortized O(n) construction with `aster.core.StringBuilder`; the release-only
`string_construction_matrix` example records both curves without machine-dependent thresholds.

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

An escaping or uncertain allocation can therefore remain reserved after it is
no longer reachable by source code. This is intentional conservative retention
for the current execution, not an automatic leak classification. Cycles are
safe for the same reason: they remain context-owned until teardown.

Reference counting and non-moving tracing are both deferred. RC would need
correct accounting for every copied aggregate, collection entry, interface,
call, and return; tracing would need allocation descriptors plus reliable JIT
root maps and safe points. Worker executions keep separate contexts and never
share arena references, so either future model must preserve that isolation.

| Candidate | Missing evidence before a prototype can change production ownership |
| --- | --- |
| Non-atomic RC | Retain/release placement for aggregate copies, collection mutation, interface aliases, calls, returns, and a cycle policy. |
| Non-moving tracing | Allocation descriptors, reference-field maps, JIT stack/root discovery, safe points, and traversal of collections and interface pairs. |

These are runtime/JIT contracts, not backend heuristics. No production RC or
tracing abstraction exists until the required evidence is demonstrated.
