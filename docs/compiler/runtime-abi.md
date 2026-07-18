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
| `aster_rt_string_concat` | `(ptr context, ptr left, ptr right) -> ptr` | Allocate immutable concatenated text |
| `aster_rt_string_length` | `(ptr context, ptr value) -> i32` | Count Unicode scalar values |
| `aster_rt_array_new` | `(ptr context, i32 length, i32 stride) -> ptr` | Allocate a zeroed fixed array |
| `aster_rt_array_element` | `(ptr context, ptr array, i32 index) -> ptr` | Checked element address |
| `aster_rt_array_length` | `(ptr context, ptr array) -> i32` | Read immutable array length |
| `aster_rt_object_new` | `(ptr context, i32 size) -> ptr` | Allocate zeroed object storage |
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

- String literals live in the data section of the JIT module that compiled them. Dynamically
  concatenated strings are aligned buffers owned by the current ExecutionContext.
- Inputs are borrowed only for one runtime call. Concatenation returns either a new context-owned
  reference or, for an empty operand, the unchanged other reference.
- **No pointer may outlive the JIT module or session that produced it.** Dropping a JIT module
  invalidates every string pointer created from its data section; callers (the CLI `run`
  command, the watcher) must copy any result they want to keep into host-owned memory before
  releasing the module.
- Array headers, array buffers, object storage, and dynamic strings belong to the host-created
  ExecutionContext and are released together after the invocation. Internal Aster calls forward
  the same hidden context pointer.
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
