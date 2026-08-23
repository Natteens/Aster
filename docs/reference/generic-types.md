# Generic classes, structs, and interfaces

A generic type declares placeholders between `<` and `>`. Uses must provide every concrete type.

```aster
public class Box<T>
{
    private T value;

    public Box(T initialValue)
    {
        value = initialValue;
    }

    public T Get()
    {
        return value;
    }
}

Box<int> score = new Box<int>(42);
Box<string> name = new Box<string>("Aster");
```

`Box<int>` and `Box<string>` are different nominal types. They cannot be assigned to each other.
Type arguments are required in annotations. Construction may repeat the closed type or use an exact
target: `Box<int> value = new Box<int>(42);` and `Box<int> value = new(42);` are equivalent.

Generic structs keep value semantics:

```aster
public struct Pair<T, U>
{
    public T First;
    public U Second;
}

Pair<int, string> pair = Pair<int, string>
{
    First: 42,
    Second: "Aster"
};
```

ASTER struct literals use `:` between a field and its value. Copying `Pair<int, string>` copies its
fields, just like any other struct. Arrays and classes used as fields retain their reference
semantics.

Generic interfaces are concrete contracts after specialization:

```aster
public interface IValue<T>
{
    T Get();
}

public class Score : IValue<int>
{
    private int value;

    public Score(int value) { this.value = value; }
    public int Get() { return value; }
}
```

`IValue<int>` uses the normal safe interface dispatch. It is distinct from `IValue<string>`.

Generic enums specialize in the same way. The standard library's
[`Option<T>` and `Result<T, E>`](option-result.md) are ordinary examples; they do not use boxing or
runtime type erasure.

The compiler specializes only combinations that the program uses and reuses repeated
specializations. Struct layout, class fields, constructor signatures, properties, and interface
tables are calculated from concrete types before HIR and MIR. Expanding recursive definitions that
can never reach a finite specialization are rejected.

Methods may declare type parameters in addition to those on their owner. The owner is specialized
first, then the method's arguments are inferred or supplied explicitly using the same rules as a
generic namespace function:

```aster
public class Box<T>
{
    public Box(T ignored) {}
    public U Choose<U>(U value) { return value; }
}

Box<string> box = new Box<string>("Aster");
int answer = box.Choose<int>(42);
```

Generic functions and methods may use expression bodies and named arguments. Concrete-typed
trailing parameters may have compile-time defaults. A default whose parameter type depends on an
open type parameter is rejected until ASTER can prove it valid for every permitted specialization;
this does not impose a blanket ban on defaults in generic callables.

Generic classes, structs, interfaces, and enums accept nominal interface-only `where` constraints,
written after any interface list and before the body: `class Box<T> : IBox<int> where T :
IComparable<T>`. Closed and self-referential generic interface constraints are supported when every
argument can be structurally substituted. The constraint is proven when the closed type is
requested and erased before HIR. See [generics](generics.md).

Current limits: no open generic constraint target such as bare `IBox`, no
`class`/`struct`/`new()`/numeric constraints, no variance, generic interface methods, constructors
with their own type parameters, generic inheritance, static members on generic owner types, partial
application, default type arguments, reflection, boxing, or `object`.
