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
Type arguments are required in annotations and after `new`; target-typed `new Box(...)` is not
implemented.

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

Aster struct literals use `:` between a field and its value. Copying `Pair<int, string>` copies its
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

Current limits: no constraints (`where`), variance, generic methods with additional parameters,
constructors with their own type parameters, generic inheritance, static members on generic types, partial application,
default type arguments, reflection, boxing, `object`, or generic standard collections. Struct
methods are not executable yet; generic structs are currently data types with fields and literals.
