# `aster.math`

Use `aster.math` for small scalar operations that should read the same way across applications:

```aster
namespace app;

using aster.math;

public class Program
{
    public static int Main()
    {
        int health = Math.Clamp(150, 0, 100);
        return Math.Max(health, Math.Abs(-42));
    }
}
```

`Math` is a static class: it is a named container for methods and cannot be instantiated or used
as a value type.

## Functions

| Function | Declared overloads | Result |
| --- | --- | --- |
| `Math.Abs(value)` | `int`, `long` | Non-negative magnitude |
| `Math.Min(left, right)` | `int`, `long`, `float`, `double` | Smaller operand |
| `Math.Max(left, right)` | `int`, `long`, `float`, `double` | Larger operand |
| `Math.Clamp(value, min, max)` | `int`, `long`, `float`, `double` | `value` limited to the inclusive range |

Overloads use ASTER's normal deterministic overload rules. Smaller types may use an existing safe
widening conversion; the return type is the type of the selected overload. The library does not add
new implicit numeric conversions or dedicated unsigned, `char`, or `decimal` overloads.

## Boundary behavior

`Abs` cannot represent the magnitude of the minimum `int` or `long`, because the positive value is
outside the same signed type. The runtime records a controlled error and the CLI reports the
invocation as failed after the entry function returns; the value is discarded. ASTER has no
exception/unwind mechanism yet, so later ASTER instructions may run internally before the host sees
the error. The operation does not wrap, saturate, panic Rust, or corrupt memory.

`Clamp` requires `min <= max`. An invalid range produces a controlled runtime error. This first
version checks the rule at runtime, including when all arguments happen to be constants; general
compile-time evaluation of library calls is not implemented.

Floating-point methods follow IEEE 754 values. Infinities participate in normal ordering. If a
`Min` or `Max` operand is `NaN`, that `NaN` is returned (the left one is considered first). `Clamp`
returns the first `NaN` found in `value`, `min`, then `max`; otherwise it validates the range and
clamps normally. For numerically equal operands, `Min` and `Max` return the right operand; this also
determines the sign returned for signed zero.

## Not included yet

Trigonometry, `Pow`, `Sqrt`, interpolation, and random numbers are not part of this first API.
`float2`, `float3`, other vectors, matrices, and quaternions are not primitive types. They may
become value types in a future `aster.math`, after the scalar foundation and ABI are stable.
