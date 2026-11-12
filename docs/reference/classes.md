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
There is no virtual dispatch, downcast, static fields, auto-properties, finalizers,
independent freeing, or `null`. Single inheritance is described in the next section.

## Single inheritance

A class may name one base class after `:`, before any interfaces: `class Dog : Animal, IPet`. The
base must be a closed class: a non-generic class or a closed specialization such as `Box<int>`
(`class Box2<T> : Box<T>`, whose base mentions the derived class's own type parameters, is not
supported yet). A struct, enum, static class or `sealed` class cannot be a base, a class cannot have
two bases, and inheritance cycles are errors. `abstract class` cannot be instantiated; `sealed class`
cannot be derived from. `virtual`, `override` and `abstract` members are not supported yet and are
rejected.

```aster
public abstract class Shape
{
    protected int id;
    public Shape(int id) { this.id = id; }
}

public class Circle : Shape
{
    public int Radius;
    public Circle(int id, int radius) : base(id) { Radius = radius; }
}
```

- A derived class inherits its base's accessible fields, methods and properties. Private members stay
  usable by the base's own methods but are not members of the derived class. A derived member may
  not have the name of an inherited field, or the signature of an inherited method (no hiding).
- `protected` members are accessible in the declaring class and its derived classes, and only through
  a receiver whose static type is the accessing class or a class derived from it: inside `Circle`, a
  `Shape`-typed receiver does not grant access to `id`. A protected constructor is reached through
  `base(...)`.
- A `Derived` value converts implicitly to `Base` (and to an interface the base implements). The
  conversion keeps the same object: no copy, `==` compares identity, and field writes are visible
  through both references. There is no implicit or explicit `Base` to `Derived` conversion.
- Construction order: the arguments of `new`, then the object is allocated, then the derived class's
  field initializers, then the arguments of `base(...)` (which cannot use `this` or fields), then the
  base constructor (running the same sequence up the chain), then the derived constructor's body. A
  constructor without `: base(...)` calls a base constructor whose parameters are all defaulted;
  otherwise it must call `base(...)`.
- While a constructor runs, `this` may only read or write fields, be the receiver of a *this-confined*
  method, and be compared with `==`/`!=`. It cannot be passed, stored, returned or converted. A
  method is this-confined when its body only uses `this` the same way and calls only this-confined
  methods; a constructor may call such a method only after every reference-typed field the method may
  read is assigned in the constructor body itself (assignments inside helpers do not count).
- A base's interface table is shared with its derived classes, which is correct because no member is
  virtual yet. A derived class cannot list an interface its base already implements.

Classes may declare type parameters, such as `Box<T>`. Every construction names a closed type,
`new Box<int>(42)` (or `Box<int> box = new(42);`), and each closed class has its own fields and methods while retaining the same
ExecutionContext-owned reference semantics. See [generic types](generic-types.md).
