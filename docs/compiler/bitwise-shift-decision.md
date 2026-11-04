# Decision record: bitwise and shift operators

**Status: PROPOSED — awaiting maintainer approval.** Nothing in this record is implemented. Every
ASTER snippet below that uses `&`, `|`, `^`, `~`, `<<`, `>>`, or `>>>` is *proposed syntax* and does
not compile today. Approving this record does not change the language; it authorizes the
implementation work in section 9.

| | |
| --- | --- |
| Baseline | `28d284363a9658e972a00c3bc06500b3c89201ae`, workspace version 0.67.3 |
| Origin | P1-2 and capability C05 of the [core readiness audit](core-readiness-audit.md) |
| Related | [standard library extension contract](standard-library-extension.md), [types](../specification/02-types.md), [expressions](../specification/04-expressions.md), [compatibility policy](../reference/compatibility.md) |
| C# references | [Bitwise and shift operators](https://learn.microsoft.com/en-us/dotnet/csharp/language-reference/operators/bitwise-and-shift-operators), [C# language specification, expressions](https://learn.microsoft.com/en-us/dotnet/csharp/language-reference/language-specification/expressions) |

## 1. Problem

ASTER has no bitwise or shift operators. Programs cannot test or combine flags, mask or pack
integers, hash, or implement bit-level algorithms. The standard library already pays for this gap:
`aster.random.Random` calls the compiler-known intrinsic `__RandomMix` because the SplitMix64
output function cannot be written in ASTER (see section 8).

## 2. Current behavior (authority: the repository)

- **Lexer** (`crates/aster-syntax/src/lexer.rs`): `&&` and `||` are tokens. A single `&` or `|` is a
  lexical error with the help "use `&&` for a logical operator". `^`, `~`, `<<`, and `>>` do not
  exist. `<` and `>` are single-character tokens (`>=` and `<=` are two-character tokens); `>` is
  never merged with a following `>`, which is what lets nested generics such as
  `List<List<int>>` close.
- **Parser** (`crates/aster-syntax/src/parser.rs`): a precedence ladder built from `binary_level`.
  From lowest: assignment, `?:`, `||`, `&&`, equality, comparison, additive, multiplicative, unary,
  postfix. This matches the "Implemented precedence" list in the expressions specification.
- **Operators**: `ast::BinaryOperator`, `hir::BinaryOperator`, and `mir::BinaryOperator` each have
  the same thirteen members (arithmetic, comparison, `LogicalAnd`, `LogicalOr`). Unary operators
  are only `Not` and `Negate`. Assignment operators are `=`, `+=`, `-=`, `*=`, `/=`.
- **Semantic analysis** (`crates/aster-compiler/src/semantic/general/expressions.rs`, `binary`):
  numeric operands go through `promoted_numeric`, which calls `aster_types::promote`. Anything
  else, including `char`, fails with "binary operator is not valid for …". Unary `-` keeps the
  operand type for signed integers and floats and rejects unsigned operands.
- **Promotion** (`crates/aster-types/src/lib.rs`, `promote`): two small integers (`sbyte`, `byte`,
  `short`, `ushort`) become `int`; otherwise the first of `int`, `uint`, `long`, `ulong` to which
  both operands convert implicitly; `ulong` with a signed type has no common type. Verified by the
  numeric conformance tests added in the P1-1 work.
- **Compound assignment** (same file, `assignment`): allowed only when the promoted operation type
  equals the target type; otherwise "compound assignment would narrow `X` to `Y`" or "requires
  compatible operands".
- **Constants** (`crates/aster-compiler/src/constexpr.rs`): constant integer arithmetic is checked
  and overflow is a compile-time error (`docs/specification/02-types.md`, "Overflow and division").
- **Backend** (`crates/aster-codegen-cranelift/src/values.rs`, `translate_binary`): lowers integer
  `+ - * /` and comparisons from a single operand type; division and remainder go through a guard
  that produces controlled runtime errors.
- **`char`** is not numeric: `char < char` is already rejected. `(int)c` is a supported cast.
- Hexadecimal and binary literals do not exist (open question in the lexical specification), so
  all literals below are decimal.

## 3. Recommendation

Add the C# set, with C# semantics, adapted only where ASTER's existing guarantees require it.

| Operator | Meaning | Proposed |
| --- | --- | --- |
| `&`, `\|`, `^` | bitwise AND / OR / XOR on integers; eager logical on `bool` | include |
| `~` | bitwise complement | include |
| `<<`, `>>` | left shift, arithmetic (signed) / logical (unsigned) right shift | include |
| `>>>` | unsigned (zero-fill) right shift | include (decision D1) |
| `&=`, `\|=`, `^=`, `<<=`, `>>=`, `>>>=` | compound forms | include, with ASTER's strict rule (section 6) |

`>>>` is recommended because it is the only way to write a logical shift of a signed value without
casting to the unsigned type and back, and the C# reference now recommends it for that purpose. It
costs one operator and one backend instruction selection. It is not needed by `Random`, so D1 can
be answered "defer" without affecting any other part of this record.

## 4. Typing rules

All integer rules reuse `aster_types::promote` and the existing implicit-conversion table. No
second promotion table is introduced.

### 4.1 Binary `&`, `|`, `^`

Both operands must be integers or both must be `bool`.

- **Integers:** both operands convert to the common type `aster_types::promote` selects, and that is
  the result type. `byte & byte` is `int`; `int | uint` is `long`; `uint ^ ulong` is `ulong`.
  A pair with no common type (for example `long & ulong`, or `int & ulong`) is rejected with the
  existing "`X` and `Y` have no implicit common type" diagnostic.
- **`bool`:** `&`, `|`, and `^` are *eager*: both operands are always evaluated, left then right,
  and `^` equals `!=`. They are distinct from the short-circuiting `&&` and `||`. Recommended
  (decision D2): the semantics are small, match C#, and make flag predicates side-effect-safe to
  reason about. If rejected, bool operands report the generic "not valid" diagnostic.
- **Everything else** (`float`, `double`, `decimal`, `string`, `char`, classes, mixed `bool` and
  integer) is rejected with "binary operator is not valid for `X` and `Y`".

### 4.2 Unary `~`

The operand must be an integer. Small integers promote to `int` (C# unary numeric promotion);
`int`, `uint`, `long`, and `ulong` keep their type. So `~(byte)5` is `int -6`, and `~0U` is
`4294967295U`. `bool`, floating-point, `char`, and `string` operands are rejected.

Note: ASTER's existing unary `-` keeps a small operand's own type (`-(sbyte)1` is `sbyte`), which
C# does not (C# promotes to `int`). That inconsistency is outside this record. D3 asks whether `~`
should follow C# (recommended) or the current unary `-` behavior.

### 4.3 Shifts

`x << n`, `x >> n`, `x >>> n`:

- The left operand uses unary promotion: small integers become `int`, other integers keep their
  type. The result has that type. The count does **not** participate in promotion.
- The count must implicitly convert to `int`, which admits `sbyte`, `byte`, `short`, `ushort`, and
  `int`. `uint`, `long`, and `ulong` counts are rejected (as in C#); the diagnostic should suggest an
  explicit `(int)` cast.
- The effective count is the count's low bits: `count & 31` when the promoted left type is 32-bit,
  `count & 63` when it is 64-bit. This covers negative counts and counts greater than the width.
  It is identical to C# and to Cranelift's `ishl`/`sshr`/`ushr`, which mask the amount to the
  operand's width (verified in the `cranelift-codegen-meta` 0.123.20 instruction definitions).
- `<<` discards high bits and fills with zero. `>>` is arithmetic for `int` and `long` and logical
  for `uint` and `ulong`. `>>>` is always logical; on a signed left operand it reinterprets the
  bits and returns the signed promoted type.
- Shifts never overflow and never fail at runtime. Debug and release behave identically.

### 4.4 Examples of typing outcomes (proposed)

| Expression | Operand types | Result |
| --- | --- | --- |
| `b1 & b2` | `byte`, `byte` | `int` |
| `s \| t` | `sbyte`, `ushort` | `int` |
| `i ^ u` | `int`, `uint` | `long` |
| `u & ul` | `uint`, `ulong` | `ulong` |
| `l & ul` | `long`, `ulong` | error: no implicit common type |
| `i & ul` | `int`, `ulong` | error: no implicit common type |
| `a & b` | `bool`, `bool` | `bool` |
| `c & d` | `char`, `char` | error: not valid for `char` and `char` |
| `f & 1` | `float`, `int` | error: not valid for `float` and `int` |
| `1 << 3L` | `int`, `long` | error: shift count must convert to `int` |
| `b << 1` | `byte`, `int` | `int` |
| `1UL << 3` | `ulong`, `int` | `ulong` |
| `~b` | `byte` | `int` |

## 5. Precedence, grammar, and evaluation

Adopt the C# order, lowest to highest:

assignment and compound assignment → `?:` → `||` → `&&` → `|` → `^` → `&` → `==` `!=` → `<` `<=`
`>` `>=` → `<<` `>>` `>>>` → `+` `-` → `*` `/` `%` → unary (`!`, `-`, `~`, prefix `++`/`--`, casts) →
postfix.

Consequences:

- The new levels slot into the existing `binary_level` ladder: `logical_and` descends to a new
  `|` level, then `^`, then `&`, then `equality`; `comparison` descends to a new shift level, then
  `term`.
- As in C#, `a & b == 0` parses as `a & (b == 0)` and is rejected for mixing `int` and `bool`
  rather than silently misbehaving. A help message suggesting parentheses is recommended.
- Operands evaluate left to right, each exactly once. For shifts the left operand is evaluated
  before the count. `&`, `|`, `^` on `bool` evaluate both operands; `&&` and `||` remain
  short-circuiting.
- **Tokenization.** `<<`, `<<=`, `&=`, `|=`, `^=`, `~`, `&`, `|`, `^` can be ordinary lexer tokens.
  The lexer must keep emitting single `>` tokens (and `>=`) because generic argument lists close
  with consecutive `>`. The expression parser should recognize `>>`, `>>>`, `>>=`, and `>>>=` as
  *adjacent* `>`/`>=` tokens (no whitespace between their spans) only in an operator position;
  type-argument parsing is unchanged (decision D5).
- **Existing programs.** `&`, `|`, `^`, `~`, `<<`, and `>>` are syntax errors today, so no valid
  program changes meaning. The follow-set in the parser's postfix-`?` lookahead
  (`at_try_propagation`) lists the tokens that may follow `expr?`; the new binary operators must be
  added there.

### Constants

Bitwise and shift operations cannot overflow, so constant evaluation (`constexpr.rs`) computes
them with the same truncating semantics as runtime and reports no overflow diagnostic:
`const int HighBit = 1 << 31;` is `-2147483648`. This is an intentional distinction from the checked
`+ - *` constant rule and matches C# ("never cause overflow", same result in checked and unchecked
contexts). Constant shift counts use the same masking (decision D4).

## 6. Compound assignment

Add `&=`, `|=`, `^=`, `<<=`, `>>=`, and (with D1) `>>>=`.

**Recommendation:** apply ASTER's existing strict rule unchanged. `x op= y` is accepted only when
the operation type equals the type of `x`; ASTER never inserts a hidden narrowing cast.

- `int flags |= 4;`, `uint mask &= other;`, `ulong state ^= other;`, `long value <<= 3;` are valid.
- For shifts the operation type is the promoted *left* type, so `int x <<= (byte)2` is valid.
- `byte b |= 1;` and `ushort w <<= 1;` are rejected with the existing "compound assignment would
  narrow `int` to `byte`" diagnostic. The fix is explicit, as for `+=`: `b = (byte)(b | 1);`.
- `bool ok &= other;` (and `|=`, `^=`) is valid if D2 is accepted.

**Intentional difference from C#.** C# evaluates `x op= y` as `x = (T)(x op y)` when the result is
explicitly convertible to `T`, so `byte b |= 1;` compiles there. ASTER does not allow that for any
operator today (`byte += byte` is rejected), and a bitwise-only exception would give two rules.
The cost is real: code that manipulates `byte`, `ushort`, or `sbyte` flag fields must spell out the
cast. If that proves painful, an exception for operations that cannot widen beyond the operand
range could be proposed separately with its own evidence (decision D6); it is not part of this
record.

## 7. Comparison with C#

| Operation | C# behavior | Proposed ASTER | Relationship |
| --- | --- | --- | --- |
| `&` `\|` `^` on integers | Defined for `int`, `uint`, `long`, `ulong`; smaller types convert to `int`; mixed types go to the closest containing type | Same, using `aster_types::promote` | Compatible |
| `int` with `uint` | `long` | `long` | Compatible |
| `ulong` with a signed integer | Error | Error ("no implicit common type") | Compatible |
| `&` `\|` `^` on `bool` | Eager logical operators | Eager logical operators | Compatible |
| `char` operands | Allowed, converts to `int` | Rejected; use `(int)c` | **Different:** ASTER's `char` is a non-numeric Unicode scalar and `char` comparisons are already rejected |
| `~` | Small types promote to `int` | Same | Compatible |
| `<<` `>>` | Left operand `int`/`uint`/`long`/`ulong`; count is `int`; result is the promoted left type | Same | Compatible |
| Shift count masking | `& 0x1F` for 32-bit, `& 0x3F` for 64-bit, negative counts included | Same | Compatible |
| `>>` | Arithmetic for signed, logical for unsigned | Same | Compatible |
| `>>>` | Always logical (C# 11) | Same (D1) | Compatible |
| Overflow | Never; same in checked and unchecked | Never; constants also truncate | Compatible |
| Precedence | shift above relational; `&` above `^` above `\|` | Same | Compatible |
| Compound assignment | `x = (T)(x op y)` may narrow | Strict: no implicit narrowing | **Different:** ASTER's established policy |
| Evaluation order | Left to right | Left to right | Compatible |
| Enum, pointer, `nint`/`nuint`, operator overloading | Supported | Not applicable (ASTER has none of these for operators) | Out of scope |

## 8. `Random` and `SplitMix64`

`stdlib/aster/random/random.aster` advances a state with `state = state + 11400714819323198485UL`
and calls `__RandomMix(state)`. The runtime implements it in `aster_rt_random_mix`
(`crates/aster-runtime/src/math.rs`):

```text
z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9
z = (z ^ (z >> 27)) * 0x94D049BB133111EB
return z ^ (z >> 31)        // all arithmetic wraps modulo 2^64
```

**The proposed operator set is sufficient.** The function needs only `ulong ^`, `ulong >>` by
constants 30, 27, and 31 (logical, because the operand is unsigned), and `ulong *`, which already
wraps. The constants fit as decimal `ulong` literals: `0xBF58476D1CE4E5B9` is
`13787848793156543929` and `0x94D049BB133111EB` is `10723151780598845931`. A future replacement
would look like this (**proposed; not executable today**):

```aster
internal ulong Mix(ulong value)
{
    value = (value ^ (value >> 30)) * 13787848793156543929UL;
    value = (value ^ (value >> 27)) * 10723151780598845931UL;
    return value ^ (value >> 31);
}
```

This formula was checked against the repository rather than assumed: an independent Python
implementation of the above reproduces the actual `Random(seed).NextULong()` output of the current
build for the first three values of seeds `0`, `1`, `123`, and `18446744073709551615`. `>>>` and
`>>` on signed types are not needed.

Requirements for the future migration (a separate PR; `__RandomMix` is not replaced here):

- keep the documented sequence bit-for-bit, with fixed-seed equivalence tests (including the
  seeds above and edge states) that compare the ASTER implementation with the retained
  `__RandomMix` before it is removed;
- rely on unsigned wrapping `*` and logical `>>` only, so results are platform independent;
- remove the intrinsic binding, runtime export, and enum variants together, per the
  [extension contract](standard-library-extension.md);
- measure `NextULong` before and after; no performance claim is made here.

## 9. Implementation plan (future work)

The roadmap rule applies: a feature is complete only when syntax, semantics, HIR, MIR, the JIT,
diagnostics, tests, and a usable example agree. The smallest vertical path, in dependency order:

1. **Syntax.** New lexer tokens as in section 5 (keep `>` single); `ast::BinaryOperator`,
   `ast::UnaryOperator`, and `ast::AssignmentOperator` members; the parser precedence ladder;
   adjacent-`>` composition in operator position; extend `at_try_propagation`; replace the
   "use `&&`" lexer help with a message that matches the new tokens.
2. **Semantic analysis.** New arms in `binary` (integer promotion through `promoted_numeric`,
   eager `bool`, shifts with their own count rule), unary `~`, the compound-assignment check
   (including a `bool` case), and `constexpr.rs` evaluation with truncating semantics.
3. **HIR.** New operator members and `convert_operator!` entries; operand conversion to the
   promoted type as for other binary operators. Convert a shift count to the promoted *left* type:
   because only the low 5 or 6 bits matter and widening preserves them, this keeps the MIR
   invariant that both operands of a binary operation have one type and preserves C# masking.
4. **MIR.** New `mir::BinaryOperator` and `mir::UnaryOperator` members and lowering. Review every
   exhaustive match (optimizer folding, escape and lifetime analysis, validation); leaving an
   operation unfolded is always safe, so folding can follow later.
5. **Cranelift.** `band`, `bor`, `bxor`, `bnot` on integers and booleans; `ishl`, `sshr`, `ushr` by
   operator and signedness. Shifts and `~` must only ever see 32- and 64-bit values because
   promotion removes the 8- and 16-bit cases; add backend validation that rejects any narrower
   shift or complement fail-closed. This matters: the earlier float-to-narrow-integer cast crash
   showed that Cranelift's x64 backend panics on some 8- and 16-bit shapes.
6. **Tests.** Extend the numeric conformance matrix (all pairs: HIR operands and result types, MIR
   operand widths, rejected pairs, diagnostics), add JIT boundary tables (counts `0`, `1`, `31`,
   `32`, `33`, `63`, `64`, `65`, `-1`, `-32` for every shiftable type; complement and mixed
   promotions; eager `bool`), compare constant folding against runtime evaluation, run debug and
   release, and add parser regressions for `List<List<int>>`, `Dictionary<string, List<int>>`, and
   generic calls next to shift expressions.
7. **Docs and examples.** Update the specification chapters, grammar, reference, compatibility
   notes, and add an example, only in the PR that makes the operators work.
8. **Migrate `Random`.** Separate PR after the feature lands, per section 8.

**Sequencing.** Steps 1–7 should land together (or behind a build that rejects the new tokens until
the backend exists), because a half-implemented operator would violate the fail-closed rule.
Step 8 depends on them.

**Risks.**

- *`>` tokenization.* Merging `>>` in the lexer would break nested generics; the adjacency rule
  must be tested thoroughly, including `generic_call_ahead` lookahead.
- *Precedence regressions.* New ladder levels touch every expression; the existing parser and
  conformance tests should pass unchanged.
- *MIR consumers.* Passes that match operators exhaustively must not treat a new operator as an
  arithmetic one.
- *Backend narrow types.* Guard against 8- and 16-bit shifts reaching Cranelift (item 5).
- *Constants.* Truncating constant shifts differ from checked arithmetic and must be documented.
- *Ergonomics.* No hexadecimal or binary literals means masks are written in decimal; hex literals
  are a separate, already-open decision that this feature makes more valuable.

## 10. Proposed examples (not executable today)

```aster
int a = 5;
int b = 3;

int common    = a & b;   // 1
int combined  = a | b;   // 7
int different = a ^ b;   // 6
int doubled   = a << 1;  // 10
int halved    = a >> 1;  // 2
```

```aster
const int CAN_JUMP = 1;
const int CAN_RUN = 2;
const int CAN_ATTACK = 4;

int abilities = CAN_JUMP | CAN_ATTACK;                // 5
bool canAttack = (abilities & CAN_ATTACK) != 0;       // true; parentheses are required
```

Expected results, computed independently of any ASTER build:

| Expression | Result type | Value |
| --- | --- | --- |
| `(byte)200 & (byte)15` | `int` | `8` |
| `(byte)200 \| (byte)15` | `int` | `207` |
| `-8 & 5` | `int` | `0` |
| `-1 & 255` | `int` | `255` |
| `-8 \| 3` | `int` | `-5` |
| `-8 ^ -1` | `int` | `7` |
| `-1 & 4294967295U` | `long` | `4294967295` |
| `-1 \| 1U` | `long` | `-1` |
| `5 ^ 3U` | `long` | `6` |
| `~5` | `int` | `-6` |
| `~0U` | `uint` | `4294967295` |
| `~(ushort)65535` | `int` | `-65536` |
| `~(sbyte)127` | `int` | `-128` |
| `~0UL` | `ulong` | `18446744073709551615` |
| `1 << 31` | `int` | `-2147483648` |
| `1 << 32` | `int` | `1` (count masked to 0) |
| `1 << 33` | `int` | `2` |
| `1 << -1` | `int` | `-2147483648` (count masked to 31) |
| `1U << 31` | `uint` | `2147483648` |
| `1L << 63` | `long` | `-9223372036854775808` |
| `1L << 64` | `long` | `1` |
| `1L << 65` | `long` | `2` |
| `1L << -1` | `long` | `-9223372036854775808` |
| `-8 >> 1` | `int` | `-4` |
| `-8 >> 33` | `int` | `-4` |
| `(-2147483647 - 1) >> 31` | `int` | `-1` |
| `4294967295U >> 31` | `uint` | `1` |
| `-8 >>> 1` | `int` | `2147483644` |
| `-1 >>> 31` | `int` | `1` |
| `-1L >>> 63` | `long` | `1` |
| `(byte)255 << 4` | `int` | `4080` |
| `(sbyte)-1 << 1` | `int` | `-2` |
| `(sbyte)-128 >> 1` | `int` | `-64` |
| `(sbyte)-1 >>> 1` | `int` | `2147483647` (promoted to `int` first) |
| `(short)-1 << 16` | `int` | `-65536` |
| `true & false`, `true \| false`, `true ^ true` | `bool` | `false`, `true`, `false` |

Rejected (proposed diagnostics): `1.5 & 2`, `"a" | "b"`, `'a' & 'b'`, `1 << 3L`, `1 << 3U`,
`long & ulong`, `int & ulong`, `~true`, `~1.5`, `true << 1`, `byte b; b |= 1;` (would narrow
`int` to `byte`).

## 11. Decisions requested

| ID | Question | Recommendation |
| --- | --- | --- |
| D1 | Include `>>>` and `>>>=`? | Include; it is separable and not needed by `Random` |
| D2 | Include eager `&`, `\|`, `^` on `bool`? | Include |
| D3 | Does `~` promote small operands to `int` (C#) or keep their type like ASTER's unary `-`? | Promote to `int` |
| D4 | Constant bitwise and shift evaluation truncates and never reports overflow? | Yes |
| D5 | Compose `>>`, `>>>`, `>>=`, `>>>=` in the parser from adjacent `>` tokens (lexer unchanged) | Yes |
| D6 | Keep the strict compound-assignment rule for the new operators? | Yes; revisit separately with evidence |
| D7 | Reject `char` operands and require `(int)c`? | Yes |
| D8 | Land the feature as one vertical PR? | Yes |
| D9 | Hexadecimal and binary literals | Separate record, recommended soon after |
| D10 | ASTER's unary `-` keeps small operand types while C# promotes; align it? | Out of scope here; track separately |

## 12. Non-goals

This record does not add operator overloading, enum flag operators, `nint`/`nuint`, checked
contexts, rotate operations, or bit-counting helpers, and it does not change numeric promotion, the
implicit-conversion table, or existing compound-assignment behavior.
