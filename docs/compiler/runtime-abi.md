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
| `aster_rt_list_*` | `List<T>` allocation, capacity/range mutation, indexed access, snapshots, and version checks |
| `aster_rt_dictionary_*` | `Dictionary<K,V>` allocation, capacity, key/fallback operations, length, and snapshots |
| `aster_rt_string_builder_*` | Explicit mutable UTF-8 construction and immutable snapshots |
| `aster_rt_object_*` | Class-object storage |
| `aster_rt_io_*` | Terminal and host-managed filesystem operations |
| `aster_rt_time_*` | Allocation-free monotonic and Unix millisecond clock reads |
| `aster_rt_temporary_scope_*` | Temporary-arena checkpoints |
| `aster_rt_temporary_subregion_*` | Private compiler-authorized fine Temporary checkpoints |
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
length, Unicode-scalar length, capacity, allocation region, and temporary-scope birth depth. The
scalar count is maintained during append, so public `Length` is O(1) and never exposes UTF-8 byte
length. Capacity starts at zero and
grows to a checked power of two only when an append does not fit. Input strings are borrowed for one
append call. `ToString()` allocates and copies an exact-size ordinary string, so immutable strings
never alias builder storage. Its private append ABI takes context, builder, and string pointers and
returns `I8`: exactly `1` means the append completed with no runtime error; `0` means an error was
already present or the runtime recorded one. Generated code branches on that result directly; the
runtime remains the sole authority for validation, growth, allocation, and diagnostics.
Scalar append entry points share the immutable conversion formatter but write into a bounded stack
buffer and pass its bytes directly to that same builder authority, avoiding both an intermediate
host `String` and an intermediate ASTER string allocation.

Fallible context-taking intrinsics transfer to the current generated runtime-failure block before
generated code loads or otherwise consumes an out destination. Ordinary semantic outcomes such as
`TryParse` returning `Option.None` or filesystem APIs returning `Result.Error` are fully initialized
values, not runtime failures. The current failure block retains AARM/owned-region cleanup authority.
This applies equally to checked array access, integer arithmetic reporters, string decoding and
formatting, Task/async/Parallel operations, and collection calls: an error edge never rejoins the
success path through a dummy value or an unwritten out slot.

Fallible private collection mutations and out-producing `List<T>` and `Dictionary<K,V>` calls return
an `I8` success status. Generated code branches on that status before loading an out destination such
as `List.Get` or `TryGet`'s `Option<T>`. Dictionary operations with a source-level boolean result
use a private tri-state result instead: `0` is runtime failure, `1` is successful `false`, and `2` is
successful `true`. These statuses are compiler/runtime control flow, not public ASTER values or a
second diagnostic authority; `ExecutionContext` still owns first-error state.

Bulk string intrinsics validate immutable UTF-8/array inputs, use checked size arithmetic, select
the current allocation region, and publish only fully initialized strings or `char[]` values.
Filesystem list/text/result intrinsics similarly write a complete typed `Result` destination only
after the operation succeeds or a complete portable `IOError` is available. New list/dictionary
capacity and range entry points keep the same private status-first rule and MemoryGovernor/region
authority as their existing collection operations.
Runtime entry points validate every tag, success payload, error payload, `IOError.kind`, and
`IOError.osCode` offset against the complete destination size with checked arithmetic before host
effects or publication. `ReadLine` also derives a byte ceiling from the current execution's
configured per-allocation string materialization limit and caps standard-input reading before
allocating the ASTER string; it does not claim that this ceiling is a snapshot of remaining arena
capacity. LF, CRLF, EOF, embedded NUL, and UTF-8 validation retain their existing semantics.

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
- Normal compilation may additionally place a private
  `aster_rt_temporary_subregion_enter`/`exit` pair inside that outer function scope. The fine pair
  checkpoints and rewinds the same Temporary arena, but is tracked separately and does not change
  semantic function-scope depth. At most one fine subregion may be active per execution context.
  One logical fine region may have multiple static,
  compiler-authorized exit sites, but a dynamic path executes exactly one; the same balanced pair
  may execute repeatedly for compiler-proven loop iterations. Generated failure cleanup exits an active fine
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

This private JIT/runtime ABI is not the user FFI surface. The implemented minimal foreign-call
boundary is separate: it accepts only fixed-width scalar C-ABI wrappers supplied through an
execution-scoped host registry, with a lexical unsafe context and no ASTER runtime layout or
reference crossing. See [native FFI](../reference/native-ffi.md) and
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

`used_bytes` can decrease when a function Temporary scope or compiler-authorized fine
subregion rewinds. `reserved_bytes` does not decrease during
the invocation because pages remain owned by the context and are reused. Allocation counts,
`requested_bytes`, and peak values are cumulative. Array accounting includes its element buffer in
`requested_bytes`; the separate header and any alignment padding appear only in used/reserved
metrics.

The region model, conservative escape rules, and benchmark procedure are documented in
[memory management](memory-management.md).
