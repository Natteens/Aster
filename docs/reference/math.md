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
| `Math.Pi()` / `Math.Tau()` / `Math.E()` | `double` | Stable scalar constants (methods in the current language) |
| `Math.Abs(value)` | `int`, `long`, `float`, `double` | Non-negative magnitude |
| `Math.Min(left, right)` | `int`, `long`, `float`, `double` | Smaller operand |
| `Math.Max(left, right)` | `int`, `long`, `float`, `double` | Larger operand |
| `Math.Clamp(value, min, max)` | `int`, `long`, `float`, `double` | `value` limited to the inclusive range |
| `Math.Sqrt(value)` | `float`, `double` | IEEE square root |
| `Math.Pow(value, exponent)` | `float`, `double` | IEEE exponentiation |
| `Math.Floor(value)` / `Math.Ceil(value)` | `float`, `double` | IEEE rounding toward negative/positive infinity |
| `Math.Round(value)` | `float`, `double` | IEEE ties-to-even rounding |
| `Math.Sin(value)` / `Math.Cos(value)` / `Math.Tan(value)` | `float`, `double` | Trigonometry with radians |
| `Math.Asin` / `Math.Acos` / `Math.Atan` / `Math.Atan2` | `float`, `double` | Inverse trigonometry |
| `Math.Exp` / `Math.Log` / `Math.Log2` / `Math.Log10` | `float`, `double` | Exponential and logarithmic operations |
| `Math.Sinh` / `Math.Cosh` / `Math.Tanh` | `float`, `double` | Hyperbolic operations |
| `Math.Truncate(value)` | `float`, `double` | Rounding toward zero |
| `Math.Sign(value)` | signed integers, `float`, `double` | `-1`, `0`, or `1`; NaN is a controlled failure |
| `Math.IsNaN` / `Math.IsInfinity` / `Math.IsFinite` | `float`, `double` | IEEE classification |
| `Math.Lerp(start, end, amount)` | `float`, `double` | `start + (end - start) * amount` |
| degree/radian conversion | `float`, `double` | Fixed-factor scalar conversion |

Overloads use ASTER's normal deterministic overload rules. Smaller types may use an existing safe
widening conversion; the return type is the type of the selected overload. The library does not add
new implicit numeric conversions or dedicated unsigned, `char`, or `decimal` overloads.

## Boundary behavior

`Abs` cannot represent the magnitude of the minimum `int` or `long`, because the positive value is
outside the same signed type. The runtime records a controlled error, generated code transfers to
its current runtime-failure path before consuming the result or executing later ASTER statements,
and the CLI reports the invocation as failed. The operation does not wrap, saturate, panic Rust, or
corrupt memory.

`Clamp` requires `min <= max`. An invalid range produces a controlled runtime error. This first
version checks the rule at runtime, including when all arguments happen to be constants; general
compile-time evaluation of library calls is not implemented.

Floating-point methods follow IEEE 754 values. Infinities participate in normal ordering. If a
`Min` or `Max` operand is `NaN`, that `NaN` is returned (the left one is considered first). `Clamp`
returns the first `NaN` found in `value`, `min`, then `max`; otherwise it validates the range and
clamps normally. For numerically equal operands, `Min` and `Max` return the right operand; this also
determines the sign returned for signed zero.

The root, power, rounding, exponential, logarithmic, trigonometric, and hyperbolic overloads use the IEEE operation selected by their
concrete `float` or `double` overload. They accept and produce `NaN`, signed zero, infinities, and
subnormal values according to that operation; they do not add a domain-error exception contract.
Angles for `Sin`, `Cos`, and `Tan` are radians. `Round` uses ties-to-even. Classification, domain,
ties-to-even, and signed-zero behavior are consistent across supported hosts. The last bits of finite
`Pow`, `Sin`, `Cos`, and `Tan` results may vary slightly with the platform math implementation, so
portable code should compare approximate transcendental results with an appropriate tolerance.

`Abs` maps both signed-zero inputs to positive zero. `Sign` returns zero for either signed zero and
reports a controlled runtime error for NaN. Classification is exact and allocation-free. Constants
are exposed as methods because ASTER does not yet have a public static compile-time constant field
surface.

## Not included

Random numbers live in the separate `aster.random` namespace. `Math` has no `decimal` overloads;
executable decimal semantics remain intentionally undefined.
`float2`, `float3`, other vectors, matrices, and quaternions are not primitive types. They may
become value types in a future `aster.math`, after the scalar foundation and ABI are stable.
