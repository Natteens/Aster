# Bitwise and shift operators

ASTER has the C# bitwise and shift operators for integers, plus eager `&`, `|`, and `^` on `bool`.
They compile to single native instructions: no runtime call, no allocation, and the same behavior in
debug and release. The design record is
[bitwise and shift decision](../compiler/bitwise-shift-decision.md); the exact typing tables are in the
[type specification](../specification/02-types.md#bitwise-and-shift-operators).

| Operator | Meaning | Operands |
| --- | --- | --- |
| `a & b`, `a \| b`, `a ^ b` | bitwise AND, OR, XOR | two integers, or two `bool` values (eager) |
| `~a` | bitwise complement | one integer |
| `a << n` | left shift | integer `a`, count `n` convertible to `int` |
| `a >> n` | right shift: arithmetic for signed, logical for unsigned | same |
| `a >>> n` | logical right shift (zero fill) for any integer | same |
| `&=`, `\|=`, `^=`, `<<=`, `>>=`, `>>>=` | compound forms | as above |

```aster
int a = 5;
int b = 3;

int common    = a & b;    // 1
int combined  = a | b;    // 7
int different = a ^ b;    // 6
int doubled   = a << 1;   // 10
int halved    = a >> 1;   // 2
```

## Flags

```aster
const int CAN_JUMP = 1;
const int CAN_RUN = 2;
const int CAN_ATTACK = 4;

int abilities = CAN_JUMP | CAN_ATTACK;          // set two flags
bool canAttack = (abilities & CAN_ATTACK) != 0; // test one; the parentheses are required
abilities &= ~CAN_JUMP;                         // clear a flag
abilities ^= CAN_RUN;                           // toggle a flag
```

`&` binds more loosely than `==` (as in C#), so `abilities & CAN_ATTACK != 0` means
`abilities & (CAN_ATTACK != 0)` and is rejected because it mixes `int` and `bool`. A complete runnable
program is [`examples/bitwise_flags.aster`](../../examples/bitwise_flags.aster).

## Types

- **`& | ^`** use the common type of the two operands, exactly as `+` does: two operands narrower than
  32 bits give `int`, `int` with `uint` gives `long`, `uint` with `ulong` gives `ulong`, and `ulong`
  with a signed integer is rejected ("no implicit common type").
- **`~`** gives `int` for `sbyte`, `byte`, `short`, and `ushort`; other integers keep their type.
- **Shifts** have the type of the left operand after the same small-integer promotion. The count is
  not part of that promotion. It must implicitly convert to `int` (`sbyte`, `byte`, `short`, `ushort`,
  or `int`); a `uint`, `long`, or `ulong` count needs an explicit `(int)` cast.
- **`bool`** supports `&`, `|`, and `^`. Unlike `&&` and `||`, both operands are always evaluated.
- `float`, `double`, `decimal`, `string`, and `char` operands are errors. Use `(int)c` for a `char`.

## Shift counts

The count is masked to the operand width: `count & 31` for 32-bit operands and `count & 63` for
64-bit operands, so a negative count or a count of at least the width wraps around instead of
failing:

| Expression | Result |
| --- | --- |
| `1 << 31` | `-2147483648` |
| `1 << 32` | `1` |
| `1 << 33` | `2` |
| `1 << -1` | `-2147483648` |
| `1L << 65` | `2` |
| `-8 >> 1` | `-4` |
| `-8 >>> 1` | `2147483644` |
| `4294967295U >> 31` | `1` |

Shifts and the other bitwise operators never overflow and never produce a runtime error. In constant
expressions they truncate in the same way and report no overflow diagnostic:
`const int HighBit = 1 << 31;` is valid.

## Compound assignment differs from C#

ASTER never narrows implicitly in a compound assignment, and that includes the new operators:

```aster
int flags = 0;
flags |= 4;                // fine: the operation type is int

byte small = 1;
// small |= 4;             // error: compound assignment would narrow `int` to `byte`
small = (byte)(small | 4); // write the cast explicitly
```

C# accepts the commented line. This keeps one rule for every compound operator.

## Intentional differences from C#

1. `char` operands are rejected; write `(int)c`.
2. Compound assignment never narrows implicitly (above).

There are no hexadecimal or binary literals yet, so masks are written in decimal.
