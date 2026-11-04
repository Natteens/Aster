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

### Hexadecimal and binary integer literals

An integer literal may be written with a `0x`/`0X` (hexadecimal) or `0b`/`0B` (binary) prefix and the
same integer suffixes (`U`, `L`, `UL`, `LU`, any case). The literal is a non-negative value; a sign is
the separate unary operator. The type follows the C# order for unsuffixed literals, so a bit pattern
keeps its width:

| Written | Type |
| --- | --- |
| no suffix | the first of `int`, `uint`, `long`, `ulong` that holds the value |
| `U` or `u` | `uint` when it fits, otherwise `ulong` (as for decimal literals) |
| `L` or `l` | `long`; a value above `9223372036854775807` is an error (there is no fallback to `ulong`, unlike C#) |
| `UL`, `LU` and mixed-case equivalents | `ulong` |

| Literal | Type | Value |
| --- | --- | --- |
| `0x7FFFFFFF` | `int` | 2147483647 |
| `0x80000000` | `uint` | 2147483648 |
| `0xFFFFFFFF` | `uint` | 4294967295 |
| `0x100000000` | `long` | 4294967296 |
| `0x7FFFFFFFFFFFFFFF` | `long` | 9223372036854775807 |
| `0x8000000000000000` | `ulong` | 9223372036854775808 |
| `0xFFFFFFFFFFFFFFFF` | `ulong` | 18446744073709551615 |

A value above `ulong` is rejected. Binary literals follow the same table (`0b` followed by 32 ones is
`uint`). Unsuffixed decimal literals keep the stricter rule above, so `4294967295` is a `long` while
`0xFFFFFFFF` is a `uint`.

These literals add no conversion rule: the integer-constant conversion and the implicit-conversion
table apply as for any literal (`byte b = 0xFF;` is valid, `int i = 0x80000000;` is not because the
constant does not fit `int`). Unary minus does not negate an unsigned literal, so `-0x80000000` is
rejected; write `(int)0x80000000` (an explicit constant cast wraps) for `-2147483648` and
`(long)0x8000000000000000` for the smallest `long`. `uint` combined with `int` promotes to `long`, so
`0xFFFFFFFF | 1` is a `long`; write `0xFFFFFFFF | 1U` to stay unsigned. Digit separators are not
implemented.

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

## Bitwise and shift operators

Integers support `&`, `|`, `^`, unary `~`, `<<`, `>>`, and `>>>`; `bool` supports eager `&`, `|`, and
`^`. See the [operators reference](../reference/operators.md) for examples.

- **`&`, `|`, `^`** on integers use the binary promotion table above: both operands convert to the
  common type, which is also the result type. `byte & byte` is `int`, `int | uint` is `long`, and
  `ulong` with a signed integer has no common type and is rejected.
- **`~`** applies unary promotion: `sbyte`, `byte`, `short`, and `ushort` become `int`; `int`, `uint`,
  `long`, and `ulong` keep their type.
- **Shifts** apply unary promotion to the left operand only; the result has that type. The count must
  implicitly convert to `int` (`sbyte`, `byte`, `short`, `ushort`, `int`); `uint`, `long`, `ulong`,
  and non-integer counts are rejected. The effective count is `count & 31` when the promoted left
  operand is 32-bit and `count & 63` when it is 64-bit, so negative counts and counts at or above the
  width wrap. `<<` fills with zeros, `>>` is arithmetic for signed and logical for unsigned operands,
  and `>>>` is always logical (on a signed operand it reinterprets the bits and keeps the signed type).
- **`bool`**: `&`, `|`, and `^` evaluate both operands, left to right; `^` equals `!=`. They are
  distinct from the short-circuiting `&&` and `||`.
- `float`, `double`, `decimal`, `string`, and `char` operands are rejected. Mixed `bool` and integer
  operands are rejected.
- These operations never overflow and never fail at runtime. Constant expressions evaluate them with
  the same truncating semantics and report no overflow diagnostic.

Compound forms `&=`, `|=`, `^=`, `<<=`, `>>=`, and `>>>=` follow the compound-assignment rule above:
the operation type (the promoted left type for shifts) must equal the target type, so
`byte b |= 1;` is rejected and `b = (byte)(b | 1);` is written instead. This is an intentional
difference from C#, as is the rejection of `char` operands.

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
  operation or target width in both debug and release builds. Bitwise and shift operations cannot
  overflow.
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
