# Primitive types

ASTER uses fixed-width primitive types and keeps their identities through HIR and MIR. `decimal`
syntax is reserved, but source that uses it is deliberately rejected before specialization and IR
lowering until its exact contract exists.

**Status:** ✅ executable in the JIT · 🚧 recognized by the frontend but rejected by current public compiler commands

| Type | Size | Sign | Typical use | Execution |
| --- | ---: | --- | --- | --- |
| `sbyte` | 8 bits | signed | compact signed binary data | ✅ JIT |
| `byte` | 8 bits | unsigned | bytes, small counters and protocols | ✅ JIT |
| `short` | 16 bits | signed | compact signed values | ✅ JIT |
| `ushort` | 16 bits | unsigned | ports and compact unsigned values | ✅ JIT |
| `int` | 32 bits | signed | default whole-number type | ✅ JIT |
| `uint` | 32 bits | unsigned | bit fields and non-negative 32-bit data | ✅ JIT |
| `long` | 64 bits | signed | large counters, timestamps and file sizes | ✅ JIT |
| `ulong` | 64 bits | unsigned | full-width unsigned identifiers and masks | ✅ JIT |
| `float` | 32 bits | n/a | graphics and approximate values | ✅ JIT |
| `double` | 64 bits | n/a | higher-precision approximate calculations | ✅ JIT |
| `decimal` | runtime layout pending | n/a | exact base-10 values such as money | 🚧 CLI rejects |
| `bool` | logical value | n/a | conditions | ✅ JIT |
| `char` | Unicode scalar | n/a | one Unicode scalar value | ✅ JIT |
| `string` | UTF-8 text | n/a | immutable text | ✅ JIT |
| `void` | no value | n/a | function with no result | ✅ JIT |

## 🧭 Choosing a numeric type

Use `int` for ordinary whole numbers and `long` when the 32-bit range is insufficient. Use `uint`
when the unsigned range or bit-level meaning matters, not merely to forbid negative input. Use
`float` for compact approximate calculations and `double` when its extra precision matters.

```aster
int attempts = 3;
long fileSize = 8000000000L;
uint mask = 255u;
float opacity = 0.5f;
double ratio = 1.0d / 3.0d;
```

> [!IMPORTANT]
> `decimal` is recognized lexically and syntactically, then rejected by the linked-language surface
> gate before generic specialization, semantic lowering, HIR, or MIR. Consequently `aster check`,
> `aster dump-hir`, `aster dump-mir`, and `aster run` all report the same controlled diagnostic.

HIR/MIR keep fail-closed decimal variants for validation compatibility, but successful source
compilation cannot produce them. Precision, scale, literal rounding, overflow, arithmetic,
comparison, conversion, formatting, layout, and ABI remain unspecified; ASTER therefore does not
silently map decimal to binary floating point.

```aster
// Recognized syntax, but rejected by current public compilation commands.
decimal price = 19.95m;
```

## 🔁 Conversions

Only conversions that preserve every possible source value are implicit. Narrowing, sign changes,
and precision-losing conversions require an explicit cast such as `(byte)value`. See the exact
tables and overflow policy in the [type specification](../specification/02-types.md).

## 🧩 What is not primitive

Arrays and generic specializations are implemented types, but they are not primitives. Classes,
structs, interfaces, and [enums](enums.md) are user-defined types rather than primitives.

`object`, `dynamic`, `null`, vectors, matrices, and quaternions are not supported.

`nint` and `nuint` are reserved for a future platform-dependent interop/low-level design. `half` is
also unsupported. None of these names is accepted today.
