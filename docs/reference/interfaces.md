# Interfaces

An interface is a stateless contract that a class chooses to implement. It lets a function depend
on required behavior without knowing the concrete class.

```aster
public interface IScore
{
    int Value();
}

public class Counter : IScore
{
    private int score;

    public Counter(int initialScore)
    {
        score = initialScore;
    }

    public int Value()
    {
        return score;
    }
}

public int Read(IScore item)
{
    return item.Value();
}
```

The names after `:` are an optional base class (first, see [classes](classes.md)) followed by interfaces. A class may list more than one interface,
separated by commas. Every required method must be public and match the parameter and return types
exactly. Interfaces cannot contain fields or method bodies.

Converting a class reference to one of its declared interfaces is implicit and safe. The resulting
value still refers to the same object, so mutation through an interface is visible through the
original class reference. Calls use the object's concrete implementation at runtime.

An interface value can be recovered from a base-typed reference with `value is IShape s` or
`value as IShape` when a derived class implements the interface, and an interface value can be tested
against a class (`area is Square sq`) or another interface; see
[downcast and interface tests](classes.md#downcast-and-interface-tests).

Interface values can be used in locals, parameters, class fields, arrays and returns between ASTER
functions. They are non-null in the current language because `null` does not exist. Equality uses
the identity of the underlying object. Interface inheritance, default methods and struct
implementations are not implemented yet.

Interfaces may declare type parameters. A closed contract such as `IValue<int>` receives its own
resolved method table and is distinct from `IValue<string>`. Classes implement a concrete contract
with `class Score : IValue<int>`. See [generic types](generic-types.md).

A non-generic interface can also constrain a generic type parameter with `where T : IScored`. The
constraint is satisfied by the interface itself or by a class declared to implement it, and it is
checked when the specialization is requested. Because the specialization is concrete, a constrained
call does not create an interface value or use interface dispatch. Generic interfaces cannot be
used as constraints yet. See [generics](generics.md).

The CLI-selected entry function cannot return an interface because the host output format currently
supports scalar and string results only.
