# 11 — Generics

## Objective

Allow reusable functions and nominal types without boxing, reflection, `object`, or erased runtime
types. Generic declarations are templates; executable programs contain only concrete
specializations.

## ACCEPTED — Syntax and implemented subset

Namespace functions, classes, structs, interfaces, and enums may declare one or more type parameters:

```aster
public T Identity<T>(T value) { return value; }

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

public interface IValue<T>
{
    T Get();
}

public enum Option<T>
{
    None,
    Some(T value),
}
```

Every type annotation, struct literal, and `new` expression supplies all type arguments explicitly:

```aster
Box<int> box = new Box<int>(42);
Pair<int, string> pair = Pair<int, string> { First: 42, Second: "Aster" };
```

Function calls retain exact argument inference and may also provide explicit arguments. Expected
return types do not participate in inference.

## ACCEPTED — Type rules

- A parameter stands for one type and must resolve consistently.
- Closed types are nominal and invariant: `Box<int>` and `Box<long>` are distinct.
- Parameters may appear in fields, properties, constructors, instance methods, parameters,
  returns, and arrays.
- Generic interfaces are distinct contracts per closed type and use normal class interface dispatch.
- Generic enums are distinct value types per closed type; their cases substitute payload types.
- Struct specializations retain copy-by-value semantics. Class specializations retain non-null
  ExecutionContext-owned reference semantics. Arrays remain references.
- Generic types may be linked through `using`; visibility is checked on the template declaration.
- Missing, extra, unknown, or conflicting type arguments are errors.
- A specialization that recursively requests ever-growing specializations is an error.
- No open type parameter may reach HIR, MIR, layout calculation, or Cranelift.

## ACCEPTED — Monomorphization

The compiler specializes templates after namespace linking and before normal semantic lowering. A
structural cache keyed by linked declaration and ordered concrete arguments reuses repeated
specializations. Layout and interface tables are created only after substitution. There is no
boxing, dictionary dispatch, reflection metadata, or generic backend ABI.

## Valid examples

```aster
public class Score : IValue<int>
{
    private int value;
    public Score(int value) { this.value = value; }
    public int Get() { return value; }
}

IValue<int> score = new Score(42);
```

## Invalid examples

```aster
Box value;              // Box requires one type argument
Box<int, long> value;   // Box accepts one type argument
Box<long> other = new Box<int>(42); // closed types are incompatible
```

## PROPOSED — Constraints

Alternatives:

1. trailing `where T : IComparable<T>` clauses — explicit and scalable, but adds syntax and
   constraint resolution;
2. inline constraints — local, but noisy for nested signatures;
3. structural operation inference — concise, but weakens contracts and diagnostics.

Recommendation: trailing interface-based `where` clauses. Constraints are not implemented.

## PROPOSED — Variance

Alternatives:

1. remain invariant — simplest and safest, but less flexible for interface APIs;
2. declaration-site `in`/`out` — familiar and expressive, but requires sound conversion rules;
3. use-site variance — flexible, but substantially more complex.

Recommendation: keep invariance until real library APIs demonstrate a need.

## Not implemented

Generic methods with their own type parameters, generic constructors, constraints, variance,
default arguments, partially applied types, generic class or interface inheritance, static members
on generic types, reflection, boxing, runtime type erasure, and executable struct methods remain
unimplemented. Official `List<T>`, `Dictionary<K,V>`, `Option<T>`, and `Result<T,E>`
specializations use the existing monomorphization pipeline.
