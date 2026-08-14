# 11 — Generics

## Objective

Allow reusable functions and nominal types without boxing, reflection, `object`, or erased runtime
types. Generic declarations are templates; executable programs contain only concrete
specializations.

## ACCEPTED — Syntax and implemented subset

Namespace functions, class/struct methods, classes, structs, interfaces, and enums may declare one
or more type parameters:

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

Methods reuse those rules. A method specialization is identified structurally by its closed owner,
declaration and parameter signature, plus ordered method type arguments. The compiler rewrites the
call to that concrete declaration before semantic analysis; repeated identical requests reuse the
same declaration. Generic interface methods and constructors with their own type parameters are not
accepted.

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

## ACCEPTED — Interface constraints

A type parameter may require one or more interfaces through a trailing `where` clause. This is an
interface-only subset, not a general constraint or trait system.

```aster
public T PickHigher<T>(T left, T right)
    where T : IScored
{
    if (left.Score() >= right.Score()) { return left; }
    return right;
}
```

- One clause per type parameter; list several interfaces with commas: `where T : IFirst, ISecond`.
- Constrain several parameters with several clauses: `where T : IFirst where U : ISecond`.
- Every generic declaration kind accepts clauses: namespace functions, classes, structs,
  interfaces, and enums. On a type declaration the clauses follow any interface list and precede
  the body: `class Box<T> : IBox where T : IScored`.
- `where` is contextual. It opens a clause only between a declaration header and its body, so it
  remains usable as an ordinary identifier everywhere else.

A concrete type argument satisfies a required interface when it *is* that interface, or when it is
a class whose declaration nominally lists it. This mirrors the existing nominal compatibility rule
for interface values. Whether such a class really implements the interface's members stays with
ordinary semantic analysis, which rejects a class that lists an interface it does not implement.

Constraints are proven when a specialization is requested, before the specialization cache is
consulted and before any concrete declaration is generated. Every request path is covered: explicit
arguments, inferred arguments, nested requests, and repeated request sites. An unsatisfied
constraint is reported at the request span and names the parameter, the argument, and the
interface.

Constraints are a template contract only. They are erased with the rest of the type-parameter list
during monomorphization, so no constraint reaches semantic analysis, HIR, MIR, backend validation,
Cranelift, or the runtime, and adding one changes no generated instruction. There is no boxing, no
erasure, and no new dispatch mechanism.

Closed generic nominal constraints are accepted when their arity is exact and substitution closes
the identity. This includes `where T : IBox<int>` and `where T : IComparable<T>`. A class satisfies
only the exact interface specialization it nominally lists; `IBox<string>` never proves
`IBox<int>`. The same proof runs for generic methods and every other supported generic declaration
kind.

Rejected in this subset, each with its own diagnostic: bare open generic targets, wrong generic
arity, primitives, classes, structs, enums, unknown types, duplicate constraints, a clause naming
an unknown type parameter, and a duplicate clause for one parameter.

Constraints are not required to call members on a type parameter. An unconstrained template that
uses a member stays legal and is checked against each concrete specialization, exactly as before.
Requiring constraints for member access needs open-template semantic validation, which ASTER does
not have.

## PROPOSED — Variance

Alternatives:

1. remain invariant — simplest and safest, but less flexible for interface APIs;
2. declaration-site `in`/`out` — familiar and expressive, but requires sound conversion rules;
3. use-site variance — flexible, but substantially more complex.

Recommendation: keep invariance until real library APIs demonstrate a need.

## Not implemented

Generic interface methods, generic constructors, non-interface constraints (`class`, `struct`,
`new()`, numeric), variance, default arguments, partially applied types, generic class or interface
inheritance, static members on generic owner types, reflection, boxing, and runtime type erasure
remain unimplemented. Official `List<T>`, `Dictionary<K,V>`, `Option<T>`, and `Result<T,E>`
specializations use the existing monomorphization pipeline.
