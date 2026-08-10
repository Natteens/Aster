# Generics

Generics let one declaration work with several concrete types without converting values to
`object` or boxing them. ASTER currently supports generic namespace functions and generic classes,
structs, and interfaces. See [generic types](generic-types.md) for the type forms.

```aster
public T Choose<T>(bool condition, T first, T second)
{
    return condition ? first : second;
}

int answer = Choose(true, 42, 0);       // T is inferred as int
int same = Choose<int>(true, 42, 0);    // T is explicit
```

Inference comes from ordinary arguments, not from the expected return type. Every occurrence of a
parameter must infer the same concrete type. ASTER rejects `Choose(true, 1, 2L)` rather than silently
guessing a conversion.

The compiler creates and caches a concrete native version for each used type combination. This is
monomorphization: the JIT sees ordinary typed functions and types, never an unresolved `T`.

## Interface constraints

A type parameter can require one or more interfaces with a trailing `where` clause:

```aster
public interface IScored
{
    int Score();
}

public T PickHigher<T>(T left, T right)
    where T : IScored
{
    if (left.Score() >= right.Score()) { return left; }
    return right;
}
```

Use commas for several interfaces (`where T : IFirst, ISecond`) and one clause per parameter
(`where T : IFirst where U : ISecond`). Generic classes, structs, interfaces, and enums accept the
same clauses; on a type declaration they come after any interface list and before the body, as in
`class Box<T> : IBox where T : IScored`.

An argument satisfies a constraint when it is the interface itself, or a class declared to
implement it. The check runs when the specialization is requested, so a bad argument is reported at
the call or type use rather than deep inside someone else's template body:

```text
type argument `Plain` does not satisfy constraint `T: IScored`
```

Constraints are erased once the specialization is created; the JIT still sees ordinary concrete
calls, with no boxing and no interface dispatch added. Only non-generic interfaces are accepted for
now: `where T : IBox<int>` is rejected with its own diagnostic. Constraints are also not required
to call a member on `T`; an unconstrained template keeps being checked against each concrete
specialization.

There is no variance, no generic methods with their own type parameters, no generic constructors,
no generic inheritance, no `class`/`struct`/`new()`/numeric constraints, no reflection, and no
runtime type erasure yet.
