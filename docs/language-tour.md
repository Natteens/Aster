# Language tour

A walk through Aster as it exists today, by example. Every snippet in this page compiles,
and unless marked otherwise it also runs with `aster run`. If you haven't run anything yet,
start with [getting started](getting-started.md).

## Variables

A variable declares its type first, like C#:

```aster
public int Sum()
{
    int count = 3;
    var doubled = count * 2;    // type inferred from the initializer: int
    doubled = doubled + 1;
    return doubled;             // 7
}
```

`var` needs an initializer — that's where the type comes from. Variables are mutable;
reading one before it has a value is a compile error.

## Constants

`const` values are computed at compile time and can never be reassigned:

```aster
const int MaxScore = 100;

public int Cap()
{
    const int Bonus = MaxScore / 10 + 2;
    return MaxScore + Bonus;    // 112
}
```

A constant's initializer must itself be constant: literals, other constants, operators,
`?:`, and casts. A function call there is a compile error, and so are overflow and
division by zero — the compiler catches `2147483647 + 1` for you.

## Functions

Functions declare a return type, a name, and typed parameters:

```aster
internal int Add(int left, int right)
{
    return left + right;
}

public int CallIt()
{
    return Add(20, 22);         // 42
}
```

A `void` function returns nothing. Any other return type must be returned on every path.
Recursion works, including two functions calling each other:

```aster
public int Factorial(int n)
{
    return n <= 1 ? 1 : n * Factorial(n - 1);
}
```

## Conditionals and loops

```aster
public int Classify(int value)
{
    if (value > 100)
    {
        return 2;
    }
    else if (value > 10)
    {
        return 1;
    }
    return 0;
}

public int SumBelow(int limit)
{
    int total = 0;
    for (int i = 0; i < limit; i++)
    {
        if (i == 3)
        {
            continue;           // skip one iteration
        }
        total += i;
    }
    while (total > 100)
    {
        total -= 10;
    }
    return total;
}
```

Conditions must be `bool` — there is no "0 is false". Braces are required. `break` leaves
the nearest loop, `continue` jumps to its next iteration.

`&&` and `||` stop early: in `a && b`, `b` is only evaluated when `a` is true.

## The ternary expression

`condition ? whenTrue : whenFalse` picks one of two values, and only the chosen side runs:

```aster
public int Choose(bool enabled)
{
    return enabled ? 10 : 20;
}
```

## Increment and decrement

`++` and `--` change a variable by one. Prefix gives you the new value, postfix the old:

```aster
public int Order()
{
    int i = 5;
    int old = i++;              // old = 5, i = 6
    int fresh = ++i;            // fresh = 7, i = 7
    return old * 100 + fresh;   // 507
}
```

They only apply to mutable numeric variables — not constants, not literals, not
expression results like `(a + b)++`.

## Strings

Strings are immutable UTF-8 text. You can store them, pass them, return them, join them, and compare
them by content:

```aster
public int Greet()
{
    string name = "Natte";
    string message = "Olá, " + name + "!";
    Log(message);
    return message.Length;       // 11 Unicode scalar values
}
```

`+=` creates and assigns a new string. `==` compares text rather than reference identity. There is
no automatic conversion from numbers or objects to text. See the
[strings reference](reference/strings.md).

## Logging

`Log(message)`, `Log.Warning(message)`, and `Log.Error(message)` each print one line. See
the [logging reference](reference/logging.md) for the exact behavior.

## Standard math

`aster.math` is the first official standard-library namespace. It provides overloaded scalar
`Abs`, `Min`, `Max`, and `Clamp` methods:

```aster
using aster.math;

public int LimitedScore(int score)
{
    return Math.Clamp(score, 0, 100);
}
```

The namespace is compiled from normal Aster code; it is not a parser special case. See the
[`aster.math` reference](reference/math.md) for supported types and edge cases.

## Enums, Option, and Result

Enums may carry values. `switch` selects one case without fallthrough or a trailing `break`:

```aster
using aster.core;

public int ValueOr(Option<int> option)
{
    switch (option)
    {
        case Some(value):
            return value;
        case None:
            return 0;
    }
}
```

Cases in an enum declaration are separated by commas. See [enums](reference/enums.md) and
[`Option` and `Result`](reference/option-result.md).

A `Result` can be propagated with the postfix `?` operator: `int value =
Parse(text)?;` continues with the `Ok` payload or returns the `Error` from the
enclosing function. See [result propagation](reference/result-propagation.md).

## Arrays

Arrays have fixed length and shared reference identity. A literal supplies every initial element;
`new T[length]` creates zero-initialized storage:

```aster
public int Sum()
{
    int[] values = [10, 20, 30];
    int[] same = values;
    same[1] = 25;
    return values[0] + values[1] + values[2] + values.Length;
}
```

This returns `68`. Every index is checked at runtime. See the [arrays reference](reference/arrays.md)
for ownership and current limits.

## Classes

Classes are reference types allocated for one execution. Constructors have the class name and no
written return type:

```aster
public class Counter
{
    private int value;

    public Counter(int initial) { value = initial; }
    public void Add(int amount) { value += amount; }
    public int Get() { return this.value; }
}
```

`Counter second = first` makes both variables refer to the same object. See the
[classes reference](reference/classes.md) for initialization and lifetime rules.

## Types, literals, and casts

The executable primitive types include `sbyte`, `byte`, `short`, `ushort`, `int`, `uint`,
`long`, `ulong`, `float`, `double`, `bool`, `char`, `string`, and `void`. `decimal` is
understood by the frontend but deliberately rejected by the JIT until it has an exact runtime
representation. The [types reference](reference/types.md) has the full table.

```aster
public bool Numbers()
{
    uint mask = 4000000000u;    // u chooses uint or ulong by range
    long wide = 4000000000L;    // L makes a long literal
    float speed = 1.5f;         // plain 4.5 is also float
    double precise = 2.5d;      // d makes a double literal
    char comma = ',';
    return wide > 0 && speed < precise && comma == ',';
}
```

Only exact widening conversions are automatic. For example, `byte` fits in `int`, `int` fits
exactly in `double`, and `float` fits exactly in `double`. `int` to `float` and `long` to
`double` require casts because some values lose precision.

### Casts

Going the other way — or converting between integers and floating point — is explicit,
written `(type)value`:

```aster
public int Shrink()
{
    double measured = 9.7d;
    return (int)measured;       // 9 — truncates toward zero
}
```

Casts work between the numeric types. Integer-to-`char` currently requires a constant valid
Unicode scalar; `char` otherwise casts only with integer types. `bool` and `string` never take
part in casts. Decimal casts can be represented by the frontend but cannot execute yet.

## Structs, classes, and interfaces

Aster already understands declarations in the C# style:

```aster
public interface IDamageable
{
    void Damage(int amount);
}

public struct Position
{
    public float x;
    public float y;
}

public class DamageCounter : IDamageable
{
    private int total;

    public DamageCounter(int initial)
    {
        total = initial;
    }

    public void Damage(int amount)
    {
        total -= amount;
    }
}
```

Data structs are executable and use named fields:

```aster
Position position = Position { x: 10.0f, y: 20.0f };
Position copy = position;
copy.x = 30.0f; // position.x remains 10.0f
```

Structs can be nested, passed and returned by value between Aster functions. Comparable structs
use field-by-field equality; struct methods remain frontend-only. Classes are non-null references
owned by the current execution context. A class lists its interfaces after `:`; this is nominal
contract implementation, not class inheritance. Interface locals, parameters, fields, arrays and
internal returns execute with dynamic dispatch. See the [struct reference](reference/structs.md),
[class reference](reference/classes.md), and [interface reference](reference/interfaces.md).

Classes support instance and `static` methods, declaration-order field initializers, explicit
properties, and deterministic overloads. Arrays and classes compare reference identity; interfaces
compare the identity of their underlying object. Strings and scalar values compare by value.

## Generic types

Use generic classes when the same reference-shaped container should hold different concrete types,
and generic structs for reusable value-shaped data:

```aster
public class Box<T>
{
    private T value;
    public Box(T value) { this.value = value; }
    public T Get() { return value; }
}

public struct Pair<T, U>
{
    public T First;
    public U Second;
}

Box<int> answer = new Box<int>(42);
Pair<int, string> named = Pair<int, string> { First: 42, Second: "Aster" };
```

Every use supplies concrete arguments. The compiler generates concrete layouts and native code;
there is no hidden boxing or runtime `object`. Generic interfaces use the same dynamic dispatch as
ordinary interfaces. See [generic types](reference/generic-types.md).
