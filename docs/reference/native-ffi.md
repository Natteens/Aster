# Minimal native FFI

ASTER's first native boundary calls explicitly registered C-ABI wrappers. It is intended for a
small embedding host, not for loading arbitrary libraries or exposing ASTER memory.

## Source form

A foreign declaration is top-level, bodyless, and visibly marked:

```aster
public unsafe foreign int NativeAdd(int left, int right);
```

The call site must be inside a lexical `unsafe` block:

```aster
public int Add(int left, int right)
{
    unsafe
    {
        return NativeAdd(left, right);
    }
}
```

Callers of `Add` do not need `unsafe`; the unsafe operation is contained and auditable in the
wrapper. Nested unsafe blocks and safe statements inside them are allowed. A foreign call outside
one is a semantic error before HIR. Foreign declarations cannot be generic, async, instance-bound,
or use an unsupported type.

## Scalar ABI

The wrapper returns a fixed `int32_t` status. Zero means success; every non-zero value becomes a
controlled ASTER runtime error. A non-void result is written through one final hidden out pointer
only on success. `void` wrappers have no out pointer.

| ASTER | C wrapper ABI |
| --- | --- |
| `bool` | `uint8_t`, exactly `0` or `1` |
| `sbyte` / `byte` | `int8_t` / `uint8_t` |
| `short` / `ushort` | `int16_t` / `uint16_t` |
| `char` | validated Unicode-scalar `uint32_t` |
| `int` / `uint` | `int32_t` / `uint32_t` |
| `long` / `ulong` | `int64_t` / `uint64_t` |
| `float` / `double` | C `float` / `double` (IEEE binary32/binary64) |
| `void` | no out value |

For example, the registered wrapper for `int NativeAdd(int, int)` is equivalent to:

```c
int32_t host_add(int32_t left, int32_t right, int32_t *out_result);
```

ASTER initializes private result storage before the call, ignores it on non-zero status, validates
incoming bool and char values, then publishes the result. Integer widths and signedness are exact;
floating NaN, infinities, subnormals, and signed zero cross without normalization.

## Host registration

An embedding host builds `ForeignRegistry`, then registers the declaration's fully linked name,
exact `ForeignSignature`, and a long-lived `extern "C"` wrapper address. Registration is an explicit
Rust `unsafe` operation because the host asserts that the address really implements that ABI.
ASTER validates the declaration's resolved scalar signature against the registered descriptor,
including distinctions with the same machine width such as `bool`/`byte` and `char`/`uint`. It
cannot inspect an arbitrary code address to prove that the embedding host described its actual C
function type truthfully: that remains the host's unsafe registration contract. Duplicate
name/signature registrations are rejected, including attempts to replace the address. Overloads may
share a name only with different exact signatures.

The registry belongs to the execution host. It is neither process-global nor thread-local, and two
hosts may bind the same ASTER declaration differently. A binding is immutable once registered. The
public execution APIs prepare, bind, and execute one program invocation as one operation, so later
registry changes can affect only a later invocation; they cannot retarget code already prepared for
the active invocation. JIT setup resolves every declaration in the linked module once, in
deterministic declaration order. Consequently, even an unused declaration requires a binding for
that execution. Missing or mismatched bindings fail before ASTER user code executes. `aster check`,
`dump-hir`, and `dump-mir` need no registry; plain `aster run` has an empty registry and therefore
reports a missing binding. There is no `--dll`, symbol-search, library-path, or environment lookup.

The host wrapper must:

- use the exact declared C ABI and keep its code address valid for the prepared JIT program;
- return zero only after writing a successful non-void result;
- never retain the hidden result pointer;
- never unwind or throw across the C ABI boundary; Rust wrappers must catch a panic internally and
  convert it to non-zero status;
- avoid assuming ASTER can catch C++, SEH, or other foreign exceptions.

No ASTER data pointer is passed, so the host cannot retain ASTER arena storage through this API.
Host allocations remain host-owned and are outside `MemoryGovernor` accounting.

The repository's compact [Rust embedding example](../../crates/aster-codegen-cranelift/examples/foreign_ffi.rs)
registers a successful scalar wrapper, demonstrates status failure, and prints informational direct
versus foreign call medians:

```console
cargo run --release -p aster-codegen-cranelift --example foreign_ffi
```

## Deliberate limits

The boundary accepts no `decimal`, strings, arrays, buffers, collections, classes, interfaces,
structs, enums, `Option`, `Result`, pointers, references, callbacks, variadics, foreign allocation,
dynamic libraries, or stable standalone ASTER ABI. Foreign calls are prohibited directly and
transitively in Task and Parallel workers because v1 has no worker-safety annotation. They are
opaque, observable, fallible optimizer/lifetime barriers and are never constant-folded, duplicated,
reordered, or removed as dead work.
