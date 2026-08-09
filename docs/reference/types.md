# Primitive types

ASTER uses fixed-width primitive types and keeps their identities through HIR and MIR. Most of the
surface below is executable today; `decimal` is deliberately different and remains outside the
current backend-supported subset.

| Type | Size | Sign | Typical use | Execution |
| --- | ---: | --- | --- | --- |
| `sbyte` | 8 bits | signed | compact signed binary data | JIT |
| `byte` | 8 bits | unsigned | bytes, small counters and protocols | JIT |
| `short` | 16 bits | signed | compact signed values | JIT |
| `ushort` | 16 bits | unsigned | ports and compact unsigned values | JIT |
| `int` | 32 bits | signed | default whole-number type | JIT |
| `uint` | 32 bits | unsigned | bit fields and non-negative 32-bit data | JIT |
| `long` | 64 bits | signed | large counters, timestamps and file sizes | JIT |
| `ulong` | 64 bits | unsigned | full-width unsigned identifiers and masks | JIT |
| `float` | 32 bits | n/a | graphics and approximate values | JIT |
| `double` | 64 bits | n/a | higher-precision approximate calculations | JIT |
| `decimal` | runtime layout pending | n/a | exact base-10 values such as money | recognized; public CLI rejects it |
| `bool` | logical value | n/a | conditions | JIT |
| `char` | Unicode scalar | n/a | one Unicode scalar value | JIT |
| `string` | UTF-8 text | n/a | immutable text | JIT |
| `void` | no value | n/a | function with no result | JIT |

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
> `decimal` is recognized by the lexer, parser, type system, HIR, and MIR, but it is not executable
> yet. Current public compiler commands validate backend support before succeeding, so `aster check`,
> `aster dump-hir`, `aster dump-mir`, and `aster run` reject compilation units that require
> `decimal` with a controlled diagnostic.

The frontend representation exists so ASTER can preserve the intended type without pretending that
its runtime layout, arithmetic, conversions, and ABI are finished. Treat decimal source as a
negative compiler fixture for now, not as a partially usable runtime feature.

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
