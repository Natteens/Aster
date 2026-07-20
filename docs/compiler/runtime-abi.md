# Runtime ABI

`aster-runtime` defines the execution boundary between JIT-compiled Aster code and functions
provided by the host process. It depends on no other Aster crate and exposes no Cranelift
types; backends translate the backend-neutral `RuntimeType` themselves.

## Registry

`aster_runtime::runtime_functions()` returns every exported function with its stable symbol
name, host address, and `extern "C"` signature. Backends bind these symbols centrally (the
Cranelift backend passes them to `JITBuilder::symbol`). Future runtime modules — files, time,
windowing, audio, networking, ECS — add entries to this registry instead of adding special
cases to a backend.

Current exports:

| Symbol               | Signature                              | Purpose                          |
| -------------------- | -------------------------------------- | -------------------------------- |
| `aster_rt_log`       | `(i32 level, ptr message) -> ()`       | `Log` / `Log.Warning` / `Log.Error` |
| `aster_rt_string_eq` | `(ptr left, ptr right) -> i8`          | `string == string` by content    |
| `aster_rt_string_concat` | `(ptr context, ptr left, ptr right) -> ptr` | Allocate persistent immutable concatenated text |
| `aster_rt_string_concat_temporary` | `(ptr context, ptr left, ptr right) -> ptr` | Allocate concatenated text in the active temporary scope |
| `aster_rt_string_from_*` | `(ptr context, scalar value) -> ptr` | Format a scalar into a persistent string |
| `aster_rt_string_from_*_temporary` | `(ptr context, scalar value) -> ptr` | Format a scalar into the active temporary scope |
| `aster_rt_string_join` | `(ptr context, ptr parts, i32 count) -> ptr` | Join interpolation parts into persistent text |
| `aster_rt_string_join_temporary` | `(ptr context, ptr parts, i32 count) -> ptr` | Join interpolation parts into temporary text |
| `aster_rt_string_length` | `(ptr context, ptr value) -> i32` | Count Unicode scalar values |
| `aster_rt_array_new` | `(ptr context, i32 length, i32 stride) -> ptr` | Allocate a persistent zeroed fixed array |
| `aster_rt_array_new_temporary` | `(ptr context, i32 length, i32 stride) -> ptr` | Allocate an array header and buffer in the active temporary scope |
| `aster_rt_array_element` | `(ptr context, ptr array, i32 index) -> ptr` | Checked element address |
| `aster_rt_array_length` | `(ptr context, ptr array) -> i32` | Read immutable array length |
| `aster_rt_object_new` | `(ptr context, i32 size) -> ptr` | Allocate zeroed object storage |
| `aster_rt_object_new_temporary` | `(ptr context, i32 size) -> ptr` | Allocate zeroed object storage in the active temporary scope |
| `aster_rt_temporary_scope_enter` | `(ptr context) -> ()` | Push a temporary-arena checkpoint for one generated function |
| `aster_rt_temporary_scope_leave` | `(ptr context) -> ()` | Rewind the innermost temporary-arena checkpoint |
| `aster_rt_math_domain_error` | `(ptr context, i32 code) -> ()` | Record a controlled `aster.math` domain failure |

Log levels: `0` = normal, `1` = warning, `2` = error. Any other value is reported as a
controlled `[error]` line, never a panic.

## String layout

An Aster `string` value crosses the ABI as a single pointer to an 8-byte-aligned allocation:

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
- Persistent array headers, array buffers, object storage, and dynamic strings belong to the
  host-created ExecutionContext and are released together after the invocation. Internal Aster
  calls forward the same hidden context pointer.
- An array header and its data buffer always use the same arena, so neither can outlive the other.
- Generated functions that contain temporary objects, arrays, or strings enter a nested temporary
  scope on function entry and leave it on every normal `Return` or `End`. Leaving rewinds only the
  innermost checkpoint; allocations made by callers before a nested call remain valid.
- Temporary allocation is valid only while such a scope is active. The runtime reports a
  controlled error for unmatched scope exits or temporary allocation without a scope.
- Object storage follows the same ownership rule. Generated constructors and methods receive the
  object pointer directly; the runtime only allocates and owns the bytes.
- Interface values do not allocate runtime memory. They copy a pair of pointers: the object owned
  by the ExecutionContext and a read-only method table owned by the live JIT module. Consequently,
  an interface value has exactly the same per-run lifetime boundary as its object and compiled code.

## `unsafe` boundary

All `unsafe` in `aster-runtime` is confined to ABI pointer views, context-owned allocation, and
aligned buffer projection across the string, logging, math, array, and object entry points.
The crate lints with `unsafe_code = "deny"` and each use carries an explicit `SAFETY` comment.
Runtime entry points are panic-free: malformed input yields controlled diagnostics because a
panic across the `extern "C"` boundary would abort the process.

`aster.math` itself is ordinary Aster source. Its private domain-error declarations carry trusted
provider metadata and lower to a typed MIR intrinsic, which the backend binds through this registry. The intrinsic provides a
panic-free host boundary without introducing exception or unwind behavior; backend code never
identifies public math methods by their textual names.

String concatenation and length are typed MIR intrinsics selected from checked string expressions.
Cranelift maps those enum variants to registry entries; it never recognizes `String`, `Length`, or
`aster.text` by source spelling.

## Memory statistics

`ExecutionContext::with_stats` enables cumulative allocation counts together with current and peak
arena usage. The CLI exposes the same snapshot through `aster run <FILE> --memory-stats`.

`used_bytes` can decrease when a temporary scope rewinds. `reserved_bytes` does not decrease during
the invocation because pages remain owned by the context and are reused. Allocation counts,
`requested_bytes`, and peak values are cumulative. Array accounting includes its element buffer in
`requested_bytes`; the separate header and any alignment padding appear only in used/reserved
metrics.

The region model, conservative escape rules, and benchmark procedure are documented in
[memory management](memory-management.md).
