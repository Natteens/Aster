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

There are no constraints, variance, generic methods with their own type parameters, generic
constructors, generic inheritance, reflection, or runtime type erasure yet.
