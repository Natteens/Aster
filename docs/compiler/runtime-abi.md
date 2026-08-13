# Runtime ABI

`aster-runtime` defines the execution boundary between JIT-compiled ASTER code and functions
provided by the host process. It depends on no other ASTER crate and exposes no Cranelift
types; backends translate the backend-neutral `RuntimeType` themselves.

## Registry

`aster_runtime::runtime_functions()` returns every exported function with its stable symbol
name, host address, and `extern "C"` signature. Backends bind these symbols centrally (the
Cranelift backend passes them to `JITBuilder::symbol`). Strings, arrays, official collections,
terminal I/O, filesystem I/O, and memory scopes all cross this checked registry boundary rather
than relying on backend name lookup.

Representative export groups:

| Symbols | Purpose |
| --- | --- |
| `aster_rt_string_*` | Immutable UTF-8 comparison, scalar traversal, formatting, parsing, slicing, and allocation |
| `aster_rt_array_*` | Fixed-array allocation, checked element access, and length |
| `aster_rt_list_*` | `List<T>` allocation, length, mutation, indexed access, and structural version checks |
| `aster_rt_dictionary_*` | `Dictionary<K,V>` allocation, key operations, length, and entry snapshots |
| `aster_rt_string_builder_*` | Explicit mutable UTF-8 construction and immutable snapshots |
| `aster_rt_object_*` | Class-object storage |
| `aster_rt_io_*` | Terminal and host-managed filesystem operations |
| `aster_rt_temporary_scope_*` | Temporary-arena checkpoints |
| `aster_rt_temporary_subregion_*` | Private experimental compiler-authorized fine Temporary checkpoints |
| arithmetic/domain-error exports | Controlled runtime failures without exceptions or unwind |

Log levels: `0` = normal, `1` = warning, `2` = error. Any other value is reported as a
controlled `[error]` line, never a panic.

## String layout

An ASTER `string` value crosses the ABI as a single pointer to an 8-byte-aligned allocation:

```text
offset 0:              usize len      // payload length in bytes (native endianness)
offset size_of<usize>: u8 * len       // UTF-8 payload, no NUL terminator
```

Rules:

- The byte length is stored separately from the payload; code must never assume NUL
  termination.
- The payload must be valid UTF-8. The runtime validates UTF-8 at the boundary and treats
  invalid payloads as controlled failures (`view` returns `None`; comparisons return `0`;
  logging reports `[error] ... not a valid ABI string`).
- The representation is immutable. Concatenation creates context-owned storage; no API can mutate
  the payload. There is no garbage collector or reference counting.
- The header stores bytes for ABI traversal. Source-level `Length` separately validates UTF-8 and
  counts Unicode scalar values.

`StringBuilder` uses a separate private native header containing its active buffer pointer, byte
length, capacity, allocation region, and temporary-scope birth depth. Capacity starts at zero and
grows to a checked power of two only when an append does not fit. Input strings are borrowed for one
append call. `ToString()` allocates and copies an exact-size ordinary string, so immutable strings
never alias builder storage.

## Ownership and lifetime

- String literals live in the data section of the JIT module that compiled them. Dynamic
  strings are aligned buffers owned by either the persistent or temporary arena of the current
  ExecutionContext.
- Inputs are borrowed only for one runtime call. Concatenation and interpolation always copy into
  a new allocation, including empty operands, so the result lifetime depends only on its selected
  region and never aliases a shorter-lived input.
- **No pointer may outlive the JIT module or session that produced it.** Dropping a JIT module
  invalidates every string pointer created from its data section; callers (the CLI `run`
  command, the watcher) must copy any result they want to keep into host-owned memory before
  releasing the module.
- Persistent collection headers, backing buffers, object storage, and dynamic strings belong to
  the host-created `ExecutionContext` and are released together after the invocation. Internal
  ASTER calls forward the same hidden context pointer.
- An array header and its data buffer always use the same arena, so neither can outlive the other.
- Generated functions that contain temporary objects, arrays, lists, dictionaries, string builders, or strings
  enter a nested temporary scope on function entry and leave it on every normal `Return` or `End`.
  Leaving rewinds only the innermost checkpoint; allocations made by callers before a nested call
  remain valid.
- The explicit AARM research path can additionally place a private
  `aster_rt_temporary_subregion_enter`/`exit` pair inside that outer function scope. The fine pair
  checkpoints and rewinds the same Temporary arena, but is tracked separately and does not change
  semantic function-scope depth. At most one fine subregion may be active per execution context in
  the initial straight-line implementation. Generated failure cleanup exits an active fine
  subregion before it leaves the outer function scope. These symbols are compiler/runtime
  machinery, not ASTER source methods or a public FFI feature.
- Temporary allocation is valid only while such a scope is active. The runtime reports a
  controlled error for unmatched scope exits or temporary allocation without a scope.
- The backend identifies functions that can participate in direct, mutual, or interface-dispatched
  call cycles. Those functions enter and leave a call-depth guard owned by their
  `ExecutionContext`; acyclic calls pay no guard ABI cost. A failed guarded entry records a
  controlled error and returns a neutral ABI value so native frames unwind normally; it does not
  unwind through `extern "C"`. Worker contexts use the same guard independently.
- Object storage follows the same ownership rule. Generated constructors and methods receive the
  object pointer directly; the runtime only allocates and owns the bytes.
- Interface values do not allocate runtime memory. They copy a pair of pointers: the object owned
  by the ExecutionContext and a read-only method table owned by the live JIT module. Consequently,
  an interface value has exactly the same per-run lifetime boundary as its object and compiled code.

## `unsafe` boundary

All `unsafe` in `aster-runtime` is confined to ABI pointer views, context-owned allocation, and
aligned buffer projection across string, collection, I/O, logging, math, and object entry points.
The crate lints with `unsafe_code = "deny"` and each use carries an explicit `SAFETY` comment.
Runtime entry points are panic-free: malformed input yields controlled diagnostics because a
panic across the `extern "C"` boundary would abort the process. The JIT may directly load the
runtime-owned `repr(C)` array header's data pointer and length on the proven in-bounds path; the
runtime exports target-local offsets for that private ABI, while out-of-bounds access still uses
the same controlled runtime diagnostic path. This is not a public FFI layout contract.

This private JIT/runtime ABI is not a user FFI surface. The constrained future
foreign-call contract, including scalar-only values and the requirement for an
explicit unsafe context, is specified in
[platform boundaries](../specification/14-platform-boundaries.md).

`aster.math` itself is ordinary ASTER source. Its private domain-error declarations carry trusted
provider metadata and lower to a typed MIR intrinsic, which the backend binds through this registry. The intrinsic provides a
panic-free host boundary without introducing exception or unwind behavior; backend code never
identifies public math methods by their textual names.

String concatenation and length are typed MIR intrinsics selected from checked string expressions.
Cranelift maps those enum variants to registry entries; it never recognizes `String`, `Length`, or
`aster.text` by source spelling.

## Memory statistics

`ExecutionContext::with_stats` enables cumulative allocation counts together with current and peak
arena usage. The CLI exposes the same snapshot through `aster run <FILE> --memory-stats`.

`used_bytes` can decrease when a function Temporary scope or compiler-authorized experimental fine
subregion rewinds. `reserved_bytes` does not decrease during
the invocation because pages remain owned by the context and are reused. Allocation counts,
`requested_bytes`, and peak values are cumulative. Array accounting includes its element buffer in
`requested_bytes`; the separate header and any alignment padding appear only in used/reserved
metrics.

The region model, conservative escape rules, and benchmark procedure are documented in
[memory management](memory-management.md).
