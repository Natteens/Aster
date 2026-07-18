# Language tour

Aster uses familiar, type-first syntax, but its design is built around concrete representation and
explicit behavior. This tour explains those ideas through small excerpts. For complete programs
you can run directly, follow the [examples](../examples/README.md).

## Programs start at `Main`

An application begins at one public static `Main` method, selected by convention or by
`Aster.toml`:

```aster
public class Program
{
    public static int Main()
    {
        Log("Hello, Aster!");
        return 42;
    }
}
```

`Main` takes no parameters and returns `void` or `int`. The `int` result is useful for examples and
command-line programs because the CLI prints it after execution.

## Values say what they are

Types appear before names. `var` can infer a local type from its initializer, while `const` keeps a
compile-time value from changing:

```aster
const int MaxItems = 100;

public int LineTotal(int unitPrice, int quantity)
{
    int accepted = quantity > MaxItems ? MaxItems : quantity;
    var total = unitPrice * accepted;
    return total;
}
```

Variables must be initialized before use. Constant expressions are evaluated by the compiler, with
diagnostics for overflow and division by zero. Numeric conversions are implicit only when every
source value can be represented exactly; lossy conversions require an explicit cast.

The [types reference](reference/types.md) records the concrete primitive types and conversion rules.

## Control flow preserves evaluation order

Conditions require `bool`; zero is not treated as false. `&&` and `||` short-circuit, and the
conditional expression evaluates only its selected branch. Loops, calls, assignments, receivers,
and arguments keep their documented order of effects.

This complete example adds the even values from one through six:

```aster
public class Program
{
    public static int Main()
    {
        int total = 0;
        for (int value = 1; value <= 6; value++)
        {
            if (value % 2 == 0)
            {
                total += value;
            }
        }
        return total;
    }
}
```

Run it with `aster run examples/basics.aster`; it prints `12`.

## Values and references are different on purpose

Structs are copied by value. Classes, arrays, and strings are references owned by the current JIT
execution. Interfaces preserve the identity of the concrete class behind them.

```aster
public struct Position
{
    public int x;
    public int y;
}

public class Counter
{
    private int value;

    public Counter(int initialValue) { value = initialValue; }
    public void Add(int amount) { value += amount; }
    public int Value { get { return value; } }
}
```

Copying a `Position` produces independent data. Copying a `Counter` reference makes two variables
refer to the same object. There is no implicit `null`, boxing, class inheritance, or garbage
collector in the current runtime.

The [objects example](../examples/objects.aster) is a complete class program. The
[struct](reference/structs.md), [class](reference/classes.md), and
[interface](reference/interfaces.md) references describe initialization, copying, and dispatch.

## Collections stay concrete

Arrays have fixed length and checked indexing. Their element type is part of the array type:

```aster
public int SumScores()
{
    int[] scores = [10, 20, 30];
    scores[1] = 25;
    return scores[0] + scores[1] + scores[2];
}
```

Strings are immutable UTF-8 values. Concatenation creates a new string, `==` compares content, and
`Length` counts Unicode scalar values. Aster does not silently convert numbers or objects to text
with `+`; use `$"Total: {quantity * price}"` to build text from values instead.

See [arrays](reference/arrays.md) and [strings](reference/strings.md) for their runtime boundaries.

## Generics become concrete programs

Generic functions and types are monomorphized before HIR and MIR. Each closed use has a concrete
identity and layout:

```aster
public T Choose<T>(bool condition, T first, T second)
{
    return condition ? first : second;
}

public class Box<T>
{
    private T value;
    public Box(T value) { this.value = value; }
    public T Get() { return value; }
}
```

`Box<int>` and `Box<string>` are different nominal types. Repeated uses of `Box<int>` reuse one
specialization; they do not introduce runtime type erasure or hidden boxing.

The runnable [generics](../examples/generics.aster) and
[generic types](../examples/generic_types.aster) examples show both forms.

## Absence and errors are values

The embedded `aster.core` namespace defines `Option<T>` and `Result<T, E>` as ordinary generic
enums. Code handles their cases with an exhaustive `switch`:

```aster
using aster.core;

public Result<int, string> ValidateQuantity(int quantity)
{
    if (quantity <= 0)
    {
        return Result<int, string>.Error("quantity must be positive");
    }
    return Result<int, string>.Ok(quantity);
}
```

Postfix `?` returns an `Error` from the enclosing function or produces the `Ok` payload. It does not
throw an exception or match type names in the backend.

Run [Option and Result](../examples/option_result.aster) and
[result propagation](../examples/result_propagation.aster), then use the
[reference](reference/option-result.md) for the exact rules.

## Projects use namespaces

Folders establish default namespaces relative to the nearest `Aster.toml`. A `using` declaration
loads another namespace; it does not download a package or search outside the project. Official
`aster.*` namespaces come from the standard library embedded in the compiler.

```aster
using aster.math;

public int ClampScore(int score)
{
    return Math.Clamp(score, 0, 100);
}
```

The [namespace project](../examples/namespaces/app/main.aster) combines project code with
`aster.math`. See [Namespaces and usings](reference/namespaces.md) and the
[standard library](reference/standard-library.md) for the complete contracts.

## Current boundaries

Aster currently executes through a JIT. It does not produce standalone binaries, resolve external
packages, provide long-lived object ownership, or implement threads, async, automatic parallelism,
GPU execution, or HVM lowering. Those are design or research questions rather than syntax waiting
to be switched on.

The [roadmap](roadmap.md) separates implemented foundations, near-term work, and research. The
[documentation index](README.md) leads to detailed language reference and compiler architecture.
