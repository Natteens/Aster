# Classes, fields, methods and properties

Classes are reference types with identity. An instance method receives the current object as
`this`; a `static` method belongs to the class and receives no object.

```aster
public class Player
{
    private int health = 100;

    public Player(int damage) { Health -= damage; }

    public int Health
    {
        get { return health; }
        private set { health = value; }
    }

    public static int MaximumHealth() { return 100; }
}
```

Field initializers run in declaration order before the constructor body. They cannot use `this`;
the constructor may replace their values. A field stores data directly. A property is an API whose
read and write execute explicit accessor bodies. A getter is required for reads, a setter for writes,
and `value` exists only in a setter. Compound assignment such as `Health -= 10` calls the getter once
and then the setter. Accessor visibility is enforced independently.

Methods and namespace-level functions may be overloaded by parameter count and types. Exact matches win,
then documented safe implicit conversions. Equally good candidates are an error; return type never
selects an overload. Constructor overloads remain unsupported.

Executable methods may use `=> expression;`. Calls and the single supported constructor accept
positional arguments followed by named arguments, and trailing parameters may have compile-time
constant defaults:

```aster
public static int Scale(int value, int factor = 2) => value * factor;
Player player = new(health: 100);
```

Named argument expressions evaluate left to right in source order. Target-typed `new()` is accepted
when an initializer, assignment, return, or selected call candidate supplies one exact class or
collection type.

A `static class` is only a container for static methods. It has no instances, fields,
constructors, properties, interfaces, or instance methods:

```aster
public static class NumberTools
{
    public static int Twice(int value) { return value * 2; }
}
```

The standard library uses this form for `aster.math.Math`.

Assigning a class variable copies its reference. `==` and `!=` therefore compare object identity,
not fields. Objects are non-null in the current language and live in the per-run ExecutionContext.
There is no inheritance, `base`, virtual dispatch, static fields, auto-properties, finalizers,
independent freeing, or `null`.

Classes may declare type parameters, such as `Box<T>`. Every construction names a closed type,
`new Box<int>(42)` (or `Box<int> box = new(42);`), and each closed class has its own fields and methods while retaining the same
ExecutionContext-owned reference semantics. See [generic types](generic-types.md).
