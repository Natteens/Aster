# Decision record: hexadecimal and binary integer literals

**Status: PROPOSED — awaiting maintainer approval.** Nothing in this record is implemented. Every
ASTER snippet below that uses a `0x` or `0b` literal is *proposed syntax* and is not currently
executable: today `0xFF` lexes as the integer `0` followed by the identifier `xFF` and is a syntax
error. Approving this record does not change the language; it authorizes the implementation in
section 11.

| | |
| --- | --- |
| Baseline | `da7944784a19533e77a38459ff19a86ef4a1a480`, release 0.68.0 |
| Origin | D9 of the [bitwise and shift decision](bitwise-shift-decision.md): masks are written in decimal today |
| Related | [lexical structure](../specification/01-lexical-structure.md), [types](../specification/02-types.md), [grammar](grammar.md), [operators](../reference/operators.md), [compatibility policy](../reference/compatibility.md) |
| C# references | [Integral numeric types, integer literals](https://learn.microsoft.com/en-us/dotnet/csharp/language-reference/builtin-types/integral-numeric-types), [C# language specification, lexical structure](https://learn.microsoft.com/en-us/dotnet/csharp/language-reference/language-specification/lexical-structure) |

## 1. Problem

With bitwise and shift operators in the language, masks and bit patterns are natural, but ASTER can
only write them in decimal: the SplitMix64 constants are `13787848793156543929UL` instead of
`0xBF58476D1CE4E5B9`, and a flag word such as `0b1010` has to be spelled `10`. Hexadecimal and binary
literals are the obvious notation for these values, they are familiar from C#, and they cost nothing
at run time.

## 2. Current behavior (authority: the repository)

- **Lexer** (`crates/aster-syntax/src/lexer.rs`, `number`): scans decimal digits, an optional
  fractional part, and one suffix, then emits one of `IntegerLiteral`, `LongLiteral` (`L`),
  `UIntLiteral` (`U`), `ULongLiteral` (`UL`/`LU`), `FloatLiteral`, `DoubleLiteral`, or
  `DecimalLiteral`. Each token carries only the **decimal digit text**; the suffix is encoded in the
  token kind, and the token span covers the whole source literal.
- **Typing is driven by that text.** `aster_types::classify_integer` returns `int` when the text
  parses as `i32`, `long` when it parses as `i64`, and nothing otherwise; `classify_unsigned` returns
  `uint` when it parses as `u32` and `ulong` when it parses as `u64`. `constexpr.rs`, HIR
  (`hir::Literal::Integer(String)`), MIR constants, and the backend all consume decimal text, so the
  rest of the pipeline never sees a radix.
- **Existing contract** (`docs/specification/02-types.md`, "Literals"): an unsuffixed integer is
  `int` when it fits and otherwise `long`; a value above `9223372036854775807` is rejected, with the
  one lexical exception `-9223372036854775808`; `L` is `long`; `U` is `uint` when it fits and
  otherwise `ulong`; `UL`/`LU` is `ulong`. This differs from C# for decimal literals (for example
  C# types `4294967295` as `uint`). That decimal contract is outside this record and does not change.
- **Not implemented:** `docs/specification/01-lexical-structure.md` states that
  "Hexadecimal/binary notation, exponents and digit separators are not implemented" and lists their
  syntax as an open question.
- **Conversions:** an integer constant converts implicitly to a narrower or differently signed
  integer type when its value fits (`byte b = 10;`); other conversions follow the exact table in
  `02-types.md`. `uint` does not convert implicitly to `int`.
- **Unary minus:** ASTER rejects `-` on an unsigned operand, and its only literal special case is
  the decimal `-9223372036854775808` (a `long`). `-2147483648` is a `long`, so the smallest `int` is
  written `-2147483647 - 1`.
- **Explicit casts of constants** wrap at the target width (`(int)3000000000L` is a valid constant),
  as the type specification states.

## 3. Proposal in one table

| Question | Proposal |
| --- | --- |
| Prefixes | `0x`/`0X` (hexadecimal) and `0b`/`0B` (binary) |
| Digits | hexadecimal `0-9 a-f A-F`; binary `0 1` |
| Suffixes | the existing `U`, `L`, `UL`, `LU` in any case |
| Unsuffixed type | the C# order: `int`, `uint`, `long`, `ulong` (the first that holds the value) |
| Limit | the value must fit `ulong` |
| Floating and `decimal` suffixes | not valid on prefixed literals |
| Digit separators `_` | a separate task |
| Unary minus | no new literal special cases; use a cast |
| Implementation | the lexer only: it validates the literal and emits the existing token kinds with **normalized decimal text** |

## 4. Lexical rules

```text
prefixed-integer = ( "0x" | "0X" ) , hex-digit , { hex-digit } , [ suffix ]
                 | ( "0b" | "0B" ) , bin-digit , { bin-digit } , [ suffix ] ;
hex-digit        = "0".."9" | "a".."f" | "A".."F" ;
bin-digit        = "0" | "1" ;
suffix           = "U" | "u" | "L" | "l" | "UL" | "Ul" | "uL" | "ul"
                 | "LU" | "Lu" | "lU" | "lu" ;
```

- The prefix is recognized only when the literal starts with exactly `0` immediately followed by
  `x`, `X`, `b`, or `B`. `10x5` and `00x1` remain a decimal literal followed by an identifier, as
  today.
- After the prefix the lexer consumes the maximal run of ASCII letters and digits. Characters that
  are valid digits for the radix form the value; the remainder must be empty or one valid suffix.
  Anything else is a diagnostic (section 9), reported once for the literal, so a typo does not
  cascade into identifier errors.
- `f`, `F`, `d`, `D` are hexadecimal digits, so floating-point suffixes cannot appear on a
  hexadecimal literal; on a binary literal they are invalid digits. `m`/`M` (decimal) is never valid
  on a prefixed literal.
- A `.` after a prefixed literal is not part of it. `0xFF.ToString()` is a member call on the
  literal, exactly like `255.ToString()`; `0x1.5` is a syntax error because hexadecimal
  floating-point literals do not exist.
- Exponents and octal notation do not exist (a leading `0` stays an ordinary decimal digit).

## 5. Typing

An unsuffixed prefixed literal has the first of `int`, `uint`, `long`, `ulong` that represents its
value. The suffix rules reuse the existing ASTER contract:

| Written | Type | Rule |
| --- | --- | --- |
| no suffix | `int`, else `uint`, else `long`, else `ulong` | C# order; error above `ulong` |
| `U` | `uint`, else `ulong` | the existing `U` rule (same as C#) |
| `L` | `long` | the existing `L` rule: error when the value exceeds `long` (**differs from C#**, which falls back to `ulong`) |
| `UL`, `LU` | `ulong` | the existing rule (same as C#) |

The unsuffixed order is deliberately the C# order and not the decimal rule. The purpose of a
prefixed literal is a bit pattern, and a bit pattern's width is part of its meaning: `0xFFFFFFFF` is a
32-bit all-ones value, so it is `uint` (as in C#), not a `long` that happens to hold 4294967295. A
hexadecimal literal is never typed `int` merely because its value fits in 32 bits when C# selects an
unsigned type: `0x80000000` is `uint`.

Decimal literals keep their stricter rule. Consequently, `4294967295` is a `long` while
`0xFFFFFFFF` is a `uint`; this is an intentional, documented asymmetry.

### Boundary values (all proposed)

| Hexadecimal | Binary | Type | Value |
| --- | --- | --- | --- |
| `0x0` | `0b0` | `int` | `0` |
| `0x7FFFFFFF` | `0b` + 31 ones | `int` | `2147483647` |
| `0x80000000` | `0b1` + 31 zeros | `uint` | `2147483648` |
| `0xFFFFFFFF` | `0b` + 32 ones | `uint` | `4294967295` |
| `0x100000000` | `0b1` + 32 zeros | `long` | `4294967296` |
| `0x7FFFFFFFFFFFFFFF` | `0b` + 63 ones | `long` | `9223372036854775807` |
| `0x8000000000000000` | `0b1` + 63 zeros | `ulong` | `9223372036854775808` |
| `0xFFFFFFFFFFFFFFFF` | `0b` + 64 ones | `ulong` | `18446744073709551615` |
| `0x10000000000000000` | `0b1` + 64 zeros | error | exceeds `ulong` |

Suffixed examples: `0xFFU` is `uint`; `0xFFFFFFFFU` is `uint`; `0x100000000U` is `ulong`;
`0xFFL` is `long`; `0x7FFFFFFFFFFFFFFFL` is `long`; `0xFFFFFFFFFFFFFFFFL` is an error (write `UL`, or
cast the unsuffixed `ulong` literal); `0xFFUL` and `0xFFLU` are `ulong`.

## 6. Interaction with conversions and decimal typing

Because the literal already has its final type, no new conversion rule is needed. The existing
integer-constant conversion (a constant that fits converts to a narrower or differently signed
target) and the exact implicit-conversion table apply unchanged.

```aster
// All of the following are PROPOSED and not currently executable.
int mask = 0xFF;                  // int 255
int flags = 0b1010;               // int 10
byte low = 0xFF;                  // int constant 255 fits byte
uint high = 0x80000000;           // uint literal, no conversion
long wide = 0xFFFFFFFF;           // uint widens to long
ulong all = 0xFFFFFFFFFFFFFFFFUL; // ulong
ulong sameAll = 0xFFFFFFFFFFFFFFFF; // unsuffixed prefixed literals may be ulong
int mixed = (0x0F | 0b1010);      // int 15: int | int
// int bad = 0x80000000;          // error: expected `int`, found `uint`
// sbyte s = 0xFF;                // error: constant 255 does not fit sbyte
```

Two consequences of ASTER's stricter numeric rules, both existing behavior, deserve a note:

1. `0xFFFFFFFF | 1` is a `long` (`uint` with `int` promotes to `long`), whereas C# keeps `uint`
   because it converts the constant `1`. Write `0xFFFFFFFF | 1U` to stay unsigned.
2. `uint flags; flags |= 0xFF;` is rejected ("compound assignment would narrow `long` to `uint`")
   because `0xFF` is an `int`. Write `flags |= 0xFFU;`. Teaching the compiler to adapt constants to
   the other operand is a separate language decision that would also change decimal literals, so it is
   not part of this record.

## 7. Unary minus, minimum values, and bit patterns

The literal is always a non-negative value; a sign is a separate unary operator, as in C#.

- `-0x7FFFFFFF` is an `int` (`-2147483647`) and `-0x1` is `-1`.
- `-0x80000000` is **rejected**: the operand is a `uint` and ASTER does not negate unsigned values.
  C# accepts it because of a rule that gives the literals 2147483648 and 9223372036854775808 their
  negative `int`/`long` meaning in any radix. ASTER has no such rule for decimal either
  (`-2147483648` is a `long`), so adding one only for prefixed literals would be inconsistent. This
  record adds **no** new unary-minus special case (decision D5).
- The smallest values are written with an explicit cast, which wraps for constants:
  `(int)0x80000000` is `-2147483648` and `(long)0x8000000000000000` is
  `-9223372036854775808`. A bit pattern with the high bit set is reinterpreted the same way:
  `(int)0xFFFFFFFF` is `-1`, `(sbyte)0xFF` is `-1`, `(short)0xFFFF` is `-1`.
- The unsigned types need no cast: `uint high = 0x80000000;` and `ulong top = 0x8000000000000000;`.

## 8. Digit separators

`_` separators (`0xFF_FF`, `0b_0010_1010`, and also `1_000`, `3.141_592`) are **not** part of this
change. They apply to decimal and floating-point literals as well, they need placement rules (no
leading, trailing, or doubled separator; behavior next to the prefix and the decimal point), and they
interact with identifier scanning. None of that is needed to express a mask. The lexer routine added
for prefixed literals should consume only the digits defined above so that separators can be added
later without changing the token representation. Decision D6 records the recommendation to treat
them as their own task.

## 9. Diagnostics

All are lexical errors with a source span; the lexer then continues after the literal.

| Situation | Example | Message (proposed) | Span |
| --- | --- | --- | --- |
| No digits after the prefix | `0x`, `0b`, `0x;` | `hexadecimal literal has no digits` / `binary literal has no digits` | the prefix |
| Invalid digit | `0b102`, `0xG1`, `0b1F` | ``invalid digit `2` in binary literal`` | the offending character |
| Floating or decimal suffix | `0x1m`, `0b1d` | ``invalid digit `m` in hexadecimal literal`` with help that `f`, `d`, and `m` apply only to decimal literals | the character |
| Malformed integer suffix | `0xFFLL`, `0xFFUU`, `0xFFULL` | ``invalid integer literal suffix `LL` `` with help listing `U`, `L`, `UL`, `LU` | the suffix |
| Value too large | `0x10000000000000000` | ``hexadecimal literal exceeds the range of `ulong` `` | the whole literal |
| `L` value above `long` | `0xFFFFFFFFFFFFFFFFL` | the existing "integer literal `…L` is out of range for `long`" | the whole literal |

The existing out-of-range messages print the normalized decimal text, so the last row would show
`18446744073709551615L` for the source `0xFFFFFFFFFFFFFFFFL`. That is accurate but not the source
spelling; it is an accepted limitation of the minimal design.

## 10. Spans, compatibility, and differences from C#

- **Spans.** The token span still runs from the `0` of the prefix to the last suffix character, so
  every later diagnostic points at the source text the user wrote.
- **Compatibility.** `0x` and `0b` followed by a letter or digit were always syntax errors, so no
  valid program changes meaning. Decimal and floating-point lexing, typing, and diagnostics are
  untouched, as is every existing decimal-literal test.
- **Runtime cost.** None. The values are ordinary typed integer constants before HIR.

Differences from C#, all deliberate:

1. `L` never falls back to `ulong` (the existing ASTER `L` rule).
2. No negative special case for 2³¹ and 2⁶³ (section 7).
3. `0xFFFFFFFF | 1` is `long` and `uint |= 0xFF` is rejected, because ASTER does not adapt an `int`
   constant to the other operand (section 6).
4. Digit separators are deferred (section 8).
5. Decimal unsuffixed literals keep the ASTER rule, so decimal and prefixed typing differ.

## 11. Future implementation path

The smallest complete change is confined to the lexer; no type-promotion table is added or changed.

1. **Lexer** (`number`): when the literal is `0` followed immediately by `x`/`X`/`b`/`B`, call a new
   prefixed scanner. It validates digits and the suffix, accumulates the value in a `u128` with an
   overflow guard against `u64::MAX`, emits the section 9 diagnostics, and then produces a token:
   - unsuffixed: `IntegerLiteral` if the value is at most `i32::MAX`, otherwise `UIntLiteral` if at
     most `u32::MAX`, otherwise `LongLiteral` if at most `i64::MAX`, otherwise `ULongLiteral`;
   - `U`: `UIntLiteral`; `L`: `LongLiteral`; `UL`/`LU`: `ULongLiteral`;
   - the token text is the **decimal rendering of the value**, and the span covers the source literal.
2. **Token representation, parser, semantic analysis, `constexpr`, HIR, MIR, backend:** no change.
   They already classify and parse the decimal text, so the existing `aster_types` classification
   (`classify_integer`, `classify_unsigned`, `fits_long`, `fits_ulong`) remains the single authority,
   and no run-time conversion appears.
3. **Documentation:** replace the "not implemented" sentence and open question in the lexical
   specification, extend the literal tables in `02-types.md`, the grammar, and the types and
   operators references, and add the SplitMix64 constants to an example.

Focused tests for that change:

- **Syntax:** token kind, normalized text, and exact span for every boundary row (hex and binary,
  upper and lower case prefixes, every suffix spelling); invalid digits, empty prefixes, malformed
  suffixes, `m`/`f`/`d` suffixes, and values above `ulong`; `0xFF.ToString()` and `0x1.5`;
  `10x5` unchanged; literals inside interpolated-string expressions; decimal and floating-point
  regression cases.
- **Compiler:** the typing table and the conversion examples (accepted and rejected), `uint | int`
  promotion to `long`, `(int)0x80000000` as a constant, rejection of `-0x80000000`, constants in
  `const` declarations, and HIR/MIR literal types for each boundary.
- **JIT (debug and release):** values at every boundary, hex and binary equal to the same decimal
  value, bit-pattern casts, interaction with `& | ^ ~ << >>`, constant versus runtime parity, and the
  SplitMix64 constants written in hexadecimal producing the same `Random` sequence.

## 12. Decisions requested

| ID | Question | Recommendation |
| --- | --- | --- |
| D1 | Accept `0x`/`0X` and `0b`/`0B` with the digit sets above | Yes |
| D2 | Unsuffixed prefixed literals follow the C# order `int`, `uint`, `long`, `ulong` | Yes |
| D3 | Keep ASTER's `L` rule (`long` only) for prefixed literals | Yes |
| D4 | Implement entirely in the lexer, emitting the existing token kinds with normalized decimal text | Yes |
| D5 | Add no new unary-minus special case for 2³¹ and 2⁶³ | Yes; revisit with the unary-minus decision (D10 of the bitwise record) |
| D6 | Treat `_` digit separators as a separate task | Yes |
| D7 | Leave constant adaptation (`uint \|= 0xFF`) as a separate decision | Yes |

## 13. Non-goals

This record does not add digit separators, octal literals, hexadecimal floating-point literals,
exponents, an `unchecked` operator, or constant adaptation, and it does not change decimal literal
typing, numeric promotion, the implicit-conversion table, or any existing diagnostic.
