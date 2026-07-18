# Primitive types

Aster uses fixed-width numeric types. The frontend preserves each type through HIR and MIR;
the JIT executes every type in this table except `decimal`.

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
| `decimal` | runtime layout pending | n/a | exact base-10 values such as money | frontend only; JIT rejects it |
| `bool` | logical value | n/a | conditions | JIT |
| `char` | Unicode scalar | n/a | one Unicode character | JIT |
| `string` | UTF-8 text | n/a | immutable text | JIT |
| `void` | no value | n/a | function with no result | JIT |

Use `int` for ordinary whole numbers and `long` when the 32-bit range is insufficient. Use
`uint` when the unsigned range or bit-level meaning is important, not merely to forbid
negative input. Use `float` for compact approximate calculations and `double` when its extra
precision matters. Reserve `decimal` for exact base-10 work after its runtime is available;
today such code can be checked and dumped, but `aster run` rejects it explicitly.

```aster
int attempts = 3;
long fileSize = 8000000000L;
uint mask = 255u;
float opacity = 0.5f;
double ratio = 1.0d / 3.0d;
decimal price = 19.95m; // frontend only for now
```

Only conversions that preserve every possible source value are implicit. Narrowing, sign
changes and precision-losing conversions require an explicit cast such as `(byte)value`.
See the exact tables and overflow policy in the
[type specification](../specification/02-types.md).

## What is not primitive

Vectors such as `float2`, `float3`, `float4` and `int2`, as well as matrices and quaternions,
will be ordinary value types in the future `aster.math` library. They do not belong in the
lexer, type core or Cranelift type mapping. Arrays, generics, `object`, `dynamic` and `null`
are also outside the primitive numeric system and are not implemented here.

Classes, structs, interfaces, and [enums](enums.md) are user-defined types rather than primitives.

`nint` and `nuint` are reserved for a future platform-dependent interop/low-level design.
`half` may be added later for graphics and FFI. None of these names is accepted today.
