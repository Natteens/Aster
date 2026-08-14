# 02 — Primitive types

## Objective

Define the primitive values that the compiler can type-check and preserve through HIR and
MIR. This chapter also fixes literal typing, exact implicit conversions, numeric promotion,
casts and overflow behavior. User-defined types and memory management are separate topics.

## Numeric types

| Type | Width | Sign | Typical use | Current execution state |
| --- | ---: | --- | --- | --- |
| `sbyte` | 8 | signed | compact signed binary data | Cranelift JIT |
| `byte` | 8 | unsigned | bytes and protocols | Cranelift JIT |
| `short` | 16 | signed | compact signed values | Cranelift JIT |
| `ushort` | 16 | unsigned | compact unsigned values | Cranelift JIT |
| `int` | 32 | signed | ordinary whole numbers | Cranelift JIT |
| `uint` | 32 | unsigned | masks and 32-bit unsigned data | Cranelift JIT |
| `long` | 64 | signed | large counters and sizes | Cranelift JIT |
| `ulong` | 64 | unsigned | 64-bit masks and identifiers | Cranelift JIT |
| `float` | 32 | n/a | IEEE 754 binary32 approximation | Cranelift JIT |
| `double` | 64 | n/a | IEEE 754 binary64 approximation | Cranelift JIT |
| `decimal` | OPEN QUESTION | n/a | exact base-10 values | syntax reserved; source use rejected before IR |

> [!IMPORTANT]
> `decimal` has reserved keyword and literal syntax but is not part of the current typed or
> executable source subset. After linking, every decimal type reference, literal, or conversion is
> rejected before specialization, HIR, and MIR with one controlled language diagnostic.

`bool`, `char`, `string` and `void` remain primitive language types but are not numeric.
`char` is one Unicode scalar value. `string` is an immutable UTF-8 reference: `+` and `+=`
concatenate only two strings, `Length` counts Unicode scalar values, and equality compares content.
No other type converts to string implicitly. `void` is valid only as a function result.

## Literals

An integer without a suffix is `int` when it fits and otherwise `long`. A value greater than
`9223372036854775807` is rejected, with one lexical-expression exception:
`-9223372036854775808` is the valid minimum `long` value.

| Suffix | Result |
| --- | --- |
| none | smallest of `int` or `long` that fits |
| `L` or `l` | `long` |
| `U` or `u` | `uint` when it fits, otherwise `ulong` |
| `UL`, `ul`, `LU`, `lu` and mixed-case equivalents | `ulong` |
| `F` or `f` | `float` |
| `D` or `d` | `double` |
| `M` or `m` | reserved `decimal` syntax; rejected by the language-surface gate |

A decimal-point literal without a suffix is currently `float`. This differs from C# and is
kept for compatibility with existing ASTER source. Floating literals that parse as infinity
because they exceed their type's finite range are rejected rather than rounded silently to
infinity. Normal IEEE 754 rounding still applies to representable source literals.

```aster
int count = 42;
long population = 4000000000L;
uint mask = 4294967295u;
ulong distance = 18446744073709551615ul;
float speed = 2.5f;
double ratio = 0.1d;
// Reserved syntax; current compilers diagnose this before typed IR.
decimal price = 19.95m;
```

Invalid examples:

```aster
long tooLarge = 9223372036854775808;    // no signed integer type can hold it
ulong alsoTooLarge = 18446744073709551616ul;
float wrongSuffix = 1.5L;               // L is integer-only
```

## Implicit conversions

An implicit conversion must represent every possible source value exactly. The accepted
families are:

- integer-to-integer when the target contains the complete source range;
- `sbyte`, `byte`, `short` and `ushort` to `float`;
- integer types up to 32 bits to `double`;
- `float` to `double`;
- no implicit conversion to `decimal` is currently accepted; the syntax is reserved while its
  exact representation remains unspecified.

Therefore `byte → short`, `ushort → uint`, `uint → long`, `int → double` and
`float → double` are valid. Sign changes or conversions such as `int → float`,
`long → double` and `ulong → long` require an explicit cast.

## Binary promotion

Promotion chooses one common type only when both operands reach it through exact implicit
conversions. It never changes sign or loses precision silently.

| Operands | Common type |
| --- | --- |
| two integers narrower than 32 bits | `int` |
| `int` and `uint` | `long` |
| `uint` and `ulong` | `ulong` |
| `int` and `float` | `double` |
| `short` and `float` | `float` |
| `float` and `double` | `double` |
| `decimal` and an integer | reserved; rejected before promotion is applied |
| `long` and `float` | none; cast required |
| `ulong` and any signed integer | none; cast required |
| `decimal` and `float`/`double` | reserved; rejected before promotion is applied |

Two equal small integer operands still promote to `int`: `byte + byte` has type `int`.
Compound assignment is strict. It is accepted only when the promoted operation type is the
target type; ASTER does not insert a hidden narrowing cast. Write the cast explicitly:

```aster
byte value = 255;
value = (byte)(value + 1); // explicit wrapping conversion; result is 0
```

```aster
byte value = 1;
long amount = 1000L;
value += amount; // invalid: the operation produces long and would narrow to byte
```

## Explicit casts

Numeric casts use `(type)expression`. Integer narrowing keeps the low two's-complement bits.
Integer widening extends according to the source sign. Float-to-integer casts truncate toward
zero, saturate at the target range and convert NaN to zero. These choices are implemented by
the current JIT and are subject to compatibility review before 1.0.

Integer-to-`char` casts currently require a compile-time constant that is a valid Unicode
scalar. This restriction avoids creating an invalid `char` until runtime validation exists.
Direct casts between `char` and floating-point or decimal types are rejected.

Casts involving `decimal` are rejected by the same early language-surface gate. Backend validation
continues to reject hand-built or adulterated decimal MIR as a fail-closed invariant.

## ⚠️ Overflow and division

- Runtime integer addition, subtraction, multiplication and explicit narrowing wrap at the
  operation or target width in both debug and release builds.
- Constant integer arithmetic is checked; overflow is a compile-time error. This intentional
  difference prevents an overflowing value from becoming part of a constant declaration.
- Unsigned comparisons, division and remainder use unsigned semantics.
- Integer division or remainder by zero reports a controlled ASTER runtime error. It does not rely
  on a raw machine or Cranelift trap.
- The signed overflow case `MIN / -1` also reports a controlled ASTER runtime error rather than
  terminating the process unexpectedly.
- Floating arithmetic follows IEEE 754. `%` for `float` and `double` is not implemented and is
  rejected by the backend.

## 🧪 Decimal status

`decimal` is not a disguised `double`. The keyword and `m` literal stay reserved so their intended
meaning cannot be captured by another feature, but source use is deliberately frozen before typed
IR. The compiler diagnostic names the missing precision, scale, rounding, overflow, formatting,
layout, and ABI contract. There is no constant or runtime decimal arithmetic in the accepted
language subset.

The next executable step is to choose a fixed decimal layout (precision, scale and overflow
rules), implement arithmetic and conversions in `aster-runtime`, define its ABI, then lower
MIR decimal operations to those runtime calls.

## What is not primitive

`float2`, `float3`, `float4`, `int2`, matrices and quaternions will be structs/value types in
the future `aster.math` library. They are not compiler primitives. `nint` and `nuint` are
future platform-dependent interop/low-level types. `half` is a possible future graphics/FFI
type. Arrays and monomorphized generics are current language features. Root `object`,
`dynamic`, and `null` types are not part of ASTER.

## OPEN QUESTIONS

- **OPEN QUESTION:** What exact coefficient width, scale range and memory layout will
  executable `decimal` use?
- **OPEN QUESTION:** Should a future checked-arithmetic mode replace or supplement runtime
  wrapping?
- **OPEN QUESTION:** What runtime validation model should permit dynamic integer-to-`char`
  conversions while guaranteeing a valid Unicode scalar?
- **OPEN QUESTION:** Will unsuffixed decimal-point literals remain `float` before ASTER 1.0?
