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

### Auto-properties

An accessor written without a body, `get;`, `set;` or `init;`, declares an auto-property. The
compiler synthesizes a private backing field, which no source can name, and trivial accessors over it:

```aster
public class Account
{
    public int Id { get; }                       // assignable only in a constructor
    public string Owner { get; init; }           // assignable only while constructing
    public int Balance { get; private set; } = 100;
    public int Limit { get; set; } = 500;
    public Account(int id, string owner) { Id = id; Owner = owner; }
}
```

- Every accessor must be bodiless; an auto-property cannot mix them with explicit bodies. Accessor
  visibility follows the explicit-property rules (`private set`, `protected set`, ...).
- `= expression;` after the closing brace initializes the backing field. It runs with the field
  initializers, in declaration order, before the base constructor and the constructor body. Only an
  auto-property can have one (an `abstract` property cannot).
- A `get`-only auto-property is assignable only in a constructor of its declaring class, through
  `Name = value` or `this.Name = value`. An `init` accessor is assignable only while the object is
  under construction: in a constructor of the declaring class or of a derived class, or in an
  [object initializer](#object-initializers). Anywhere else the assignment is a diagnostic.
- A non-virtual auto-property is lowered directly to its backing field: there is no accessor call
  and no inliner is involved. For this-confinement a read or write of such a property on `this` is a
  field access (a read before the constructor assigned a reference-typed backing field is an error).
  A `virtual`, `override` or `abstract` auto-property keeps dispatching through its accessors, so a
  use on `this` inside a constructor stays rejected like any virtual member. An `override`
  auto-property gets its own backing field. `init` accessors are not accepted on virtual members yet.
- A `string` or class-typed auto-property has no zero default, so a constructor must assign it
  (or the property needs an initializer), exactly like a field.
- `static` auto-properties are not supported yet (they need static fields). Interfaces cannot
  declare properties. `++`/`--` on a property is not supported; write `P += 1`.

Methods and namespace-level functions may be overloaded by parameter count and types. Exact matches win,
then documented safe implicit conversions. Equally good candidates are an error; return type never
selects an overload. A class may declare several constructors, selected by the same rules
([constructor overloads and `this(...)`](../compiler/object-model-b2.md)).

Executable methods may use `=> expression;`. Calls and constructors accept
positional arguments followed by named arguments, and trailing parameters may have compile-time
constant defaults:

```aster
public static int Scale(int value, int factor = 2) => value * factor;
Player player = new(health: 100);
```

Named argument expressions evaluate left to right in source order. Target-typed `new()` is accepted
when an initializer, assignment, return, or selected call candidate supplies one exact class or
collection type.

### Object initializers

`new T(args) { A = e1, B = e2 }` (or `new T { A = e1 }` when a parameterless constructor is
accessible, and target-typed `new() { ... }`) constructs the object and then assigns members:

- Evaluation order: the constructor arguments, then the constructor (and its base chain), then each
  `Member = value` from left to right in source order. The object is not observable (it cannot be
  named) until every assignment has completed; the whole expression then yields it.
- A member is a field or a settable property (including `init` properties, which are assignable in
  an object initializer and in constructors only). It is checked exactly like `obj.Member = value`:
  it must exist, be accessible from the creation site and accept the value's type. A get-only
  property is an error. Each member may appear at most once.
- A `virtual` property assignment dispatches to the most-derived setter: the object is fully
  constructed when the assignments run. A non-virtual auto-property is written directly to its
  backing field.
- Not supported yet: nested initializers (`P = { X = 1 }`), collection initializers
  (`new List<int> { 1, 2 }`) and struct creation (structs keep the named struct literal).

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
There is no `(Derived)value` cast, static fields, finalizers, independent freeing, or `null`. A checked downcast
is the pattern `value is Derived d` or `value as Derived` (see [downcast](#downcast-and-interface-tests)).
Single inheritance and virtual dispatch are described in the next sections.

## Single inheritance

A class may name one base class after `:`, before any interfaces: `class Dog : Animal, IPet`. The
base must be a closed class: a non-generic class or a closed specialization such as `Box<int>`
(`class Box2<T> : Box<T>`, whose base mentions the derived class's own type parameters, is not
supported yet). A struct, enum, static class or `sealed` class cannot be a base, a class cannot have
two bases, and inheritance cycles are errors. `abstract class` cannot be instantiated; `sealed class`
cannot be derived from. Virtual members are described in the next section.

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
  through both references. There is no implicit or explicit `Base` to `Derived` conversion; a
  `Base` reference is narrowed with a type pattern, `if (value is Derived derived)`, which tests the
  object's run-time class and binds the same object as `Derived` (see
  [Control flow](../specification/06-control-flow.md#type-patterns-and-when-guards)).
- Construction order: the arguments of `new`, then the object is allocated, then the derived class's
  field initializers, then the arguments of `base(...)` (which cannot use `this` or fields), then the
  base constructor (running the same sequence up the chain), then the derived constructor's body. A
  constructor without `: base(...)` calls a base constructor whose parameters are all defaulted;
  otherwise it must call `base(...)`. A constructor may instead delegate with `: this(...)` to another
  constructor of its own class: the target runs the initializers and its own `base(...)` call, then
  the delegating body runs. `this(...)` and `base(...)` cannot appear on the same constructor, a
  delegation cycle is an error, and a class with any explicit constructor has no implicit one.
- While a constructor runs, `this` may only read or write fields, be the receiver of a *this-confined*
  method, and be compared with `==`/`!=`. It cannot be passed, stored, returned or converted. A
  method is this-confined when its body only uses `this` the same way and calls only this-confined
  methods; a constructor may call such a method only after every reference-typed field the method may
  read is assigned in the constructor body itself (assignments inside helpers do not count).
- A derived class cannot list an interface its base already implements; it inherits the
  implementation (see the interface rules under virtual dispatch below).

## Virtual dispatch

Methods and property accessors are statically bound by default. `virtual` introduces a member that a
derived class may replace with `override`; `abstract` declares a member without a body, and only an
`abstract class` may declare one. A call through a base-typed value runs the override of the object's
real class:

```aster
public abstract class Shape
{
    protected int id;
    public Shape(int id) { this.id = id; }
    public abstract int Area();
    public virtual int Perimeter() { return 0; }
    public virtual int Label { get { return id; } set { id = value; } }
    public int Describe() { return Area() + Perimeter(); }   // dispatches dynamically
}

public class Square : Shape
{
    private int side;
    public Square(int id, int side) : base(id) { this.side = side; }
    public override int Area() { return side * side; }
    public override int Perimeter() { return base.Perimeter() + 4 * side; }
    public override int Label { get { return base.Label + 1000; } }   // setter stays inherited
}

Shape shape = new Square(7, 5);
int total = shape.Describe();   // 25 + 20
```

- An `override` must match the member it replaces exactly: the same parameters, return type and
  visibility, and the member must be `virtual`, `abstract` or itself an `override`. Covariant returns
  and `sealed override` are not supported. A same-signature member without `override` is an error;
  there is no hiding and no `new`.
- A class that is not `abstract` must override every inherited `abstract` member. An abstract class
  cannot be instantiated. `virtual`, `override` and `abstract` are not allowed on static, private or
  constructor members, on fields, or on interface members.
- A property is virtual through its accessors. An override may declare only one accessor (the other
  stays inherited) but cannot add one the overridden property does not have, and must keep the
  property's type. `abstract int P { get; set; }` declares abstract accessors without bodies.
- `base.Method(...)`, `base.Property` and `base.Property = value` call the base implementation
  directly, with no further dispatch. `base` is only valid inside an instance member of a class that
  has a base class, and `base.Member` of an abstract member is an error. `base` is not a value.
- One virtual slot exists per introduced virtual member. An override reuses the slot of the member it
  replaces. The slot index is fixed at compile time and a virtual call loads the function from the
  object's class descriptor.
- A virtual, abstract or overriding member cannot be called on `this` while the object is under
  construction, because a derived override would run before the derived constructor has finished.
  That covers `Init()`, `this.Init()` and a virtual property read or written on `this` inside a
  constructor, and any helper that does so (it is not this-confined). `base.Member(...)` is a static
  call and follows the ordinary this-confinement rule. Non-virtual properties keep working in
  constructors.
- An interface implemented through a virtual method dispatches dynamically: a `Base` reference whose
  object is a `Derived` converts to the interface and a call through it runs `Derived`'s override.
  The implicit conversion from a base-typed value to an interface still needs the static class to
  implement it (`Base` to `IFoo` is an error when only `Derived` implements `IFoo`); the checked forms
  below look the interface up from the dynamic type.
- A virtual call is an escape of its receiver for the memory analyses (the callee is unknown until
  devirtualized), so an object that receives one is never eliminated or kept in a temporary region.

### Downcast and interface tests

A base-typed value is narrowed with a checked operation, never an implicit conversion:

```aster
Shape shape = new Square(7, 5);

if (shape is Square square)          // primary form: test and bind the same object
{
    Use(square.Side);
}

Option<Square> maybe = shape as Square;   // Some(square) or None (needs `using aster.core;`)
```

- `value is Derived d` and `value is Derived` test the run-time class of a class reference; `d` is an
  identity alias of the same object, as in [type patterns](../specification/06-control-flow.md).
- `value as T` yields the official `Option<T>`: `Some` holding the same object when the run-time type
  matches, `None` otherwise. It requires `aster.core`; the `T?` spelling is not available.
- The target may be a class or an interface. For an interface, `shape is IShape s` and `shape as IShape`
  search the interface list in the object's class descriptor, bounded by the descriptor's interface
  count, and the binding is an interface value (the object and the table found). The search is linear;
  an indexed table is only worth adding if it measures at least 5 percent faster on `interface_dispatch`
  through a base type (decision OBJ-004), and that experiment has not been run.
- The subject may itself be an interface value: `area is Square sq` and `area as Polygon` test the
  descriptor of the object behind it, and `area is IOther other` searches for another interface.
- A test that can never succeed is a compile error: unrelated classes, and a `sealed` class that does
  not implement the interface (or an interface subject tested against such a sealed class). A test that
  always succeeds (an upcast, or an interface the static class implements) is allowed, as in C#.
- Unwrapping an `Option<T>` with `is` or `as` is not supported; use a `switch`.

Classes may declare type parameters, such as `Box<T>`. Every construction names a closed type,
`new Box<int>(42)` (or `Box<int> box = new(42);`), and each closed class has its own fields and methods while retaining the same
ExecutionContext-owned reference semantics. See [generic types](generic-types.md).
