# 07 — Classes, structs, interfaces, and enums

## Objective

Define Aster’s accepted user-defined type categories while keeping inheritance,
memory management, layout, and dispatch choices explicit.

## Accepted syntax

```aster
public class Player
{
    private string name;

    public void Rename(string nextName)
    {
        name = nextName;
    }
}

public struct Point
{
    public float x;
    public float y;
}

public interface IDrawable
{
    void Draw();
}

public enum Outcome
{
    Success(int value),
    Failure(string message),
}
```

## Accepted rules

- `class` declares objects with identity, state, and behavior.
- `struct` declares compact values. Copy and move details remain part of the memory model.
- `interface` declares behavior contracts without instance state.
- `enum` declares a value with one active named case and optional typed payloads.
- Fields and parameters use type-before-name order.
- Field names must be unique within a type.
- Required interface members are public by contract and normally omit the redundant `public` modifier.
- Interfaces may declare callable members but cannot declare instance fields.
- Visibility follows `15-visibility.md`: namespace-level declarations default to `internal`, while class
  and struct members default to `private`.
- Class inheritance is not adopted. No syntax such as `class Child : Parent` is currently valid.
- A class implements interfaces nominally with `class Counter : IScore, IResettable`. Names after
  `:` must be interfaces; this syntax does not permit a base class.
- These type declarations have no automatic ECS, lifecycle, serialization, or engine behavior.
- `static class` declares a non-instantiable container whose members must all be static methods.
  It cannot declare fields, constructors, properties, interfaces, or instance methods.

```aster
public static class NumberTools
{
    public static int Twice(int value)
    {
        return value * 2;
    }
}
```

## Executable structs by value

Struct values use a named-field literal. Every field must be supplied exactly once; there is
no silent zero/default initialization.

```aster
Position position = Position { x: 10, y: 20 };
```

- Fields use declaration order and natural alignment; final size is rounded to the largest
  field alignment. Empty structs have size 1 and alignment 1.
- Nested structs are allowed when their layouts are finite. Recursive value layouts are rejected.
- Assignment, parameters and returns copy the complete value.
- The JIT uses stack slots, callee-owned parameter copies and hidden return destinations.
- Structs do not use heap allocation by default. Comparable structs use structural equality.
- Only public fields can be named by a literal or accessed from namespace-level functions.

The aggregate ABI is internal to Aster, not a promised C ABI. Struct methods remain
frontend-only until HIR/MIR represent an explicit receiver correctly. A struct may be returned
between Aster functions, but not directly as the CLI-selected entry result.

## Executable classes in the JIT

The current executable class subset uses one constructor with no written return type:

```aster
public class Counter
{
    private int value;

    public Counter(int initial)
    {
        this.value = initial;
    }
}
```

`new Counter(10)` allocates a non-null object in the current ExecutionContext and invokes that
constructor immediately. Assignment, parameters and internal returns copy the reference, not the
fields. Objects have identity and live until the execution ends. Reference fields must be assigned
by the constructor on every continuing path; numbers, booleans and entirely zero-safe structs may
use their zero default. Struct inheritance, finalizers and `null` are not defined.

## Valid design examples

```aster
public struct Size
{
    public float width;
    public float height;
}

public interface INamed
{
    string GetName();
}

public class User
{
    private string name;
    public string GetName() { return name; }
}
```

## Invalid design examples

```aster
public interface IInvalid
{
    int state;                 // interfaces have no instance state
}

public class Admin : User { }  // class inheritance is not adopted

public struct Pair
{
    public int value;
    public int value;          // duplicate field
}
```

## Decisões de construção e comportamento

### ACCEPTED — Nominal class implementation

Classes use `class User : INamed`. Every contract method must exist as a public instance method
with an exact signature. Conformance is never inferred structurally. Class inheritance remains
unsupported, so a class name after `:` is an error.

### ACCEPTED — Runtime interface references

An interface reference stores the same non-null object identity plus its concrete method table.
Calls dispatch to the runtime implementation. Class-to-declared-interface conversion is implicit;
unrelated conversions are rejected. Interface equality compares underlying object identity;
downcasts are not defined yet.

Struct implementation, interface inheritance, default methods and generic interface constraints
remain future work.

### ACCEPTED — Struct initialization

1. **Named literal: `Point { x: 1.0, y: 2.0 }`** — readable and order-independent; more verbose.
2. **Constructor calls: `Point(1.0, 2.0)`** — compact; fragile when fields change.
3. **Only user-declared constructors** — explicit invariants; boilerplate for data-only values.

Named literals are implemented. Positional `Point(1.0, 2.0)` is not an implicit constructor.

### ACCEPTED — Class construction

`new Player(...)` explicitly allocates a non-null class object in the current ExecutionContext and
invokes its constructor. A plain type call never hides class allocation. Individual destruction,
`null`, finalizers and long-lived ownership remain outside the current per-execution model.

### ACCEPTED — Value equality and copying for structs

1. **Always generated** — convenient; can make large copies or inappropriate equality silent.
2. **Explicit opt-in** — predictable; adds declarations for common value types.
3. **Generated under size/type constraints** — efficient defaults; rules may surprise users.

Assignment, arguments and returns copy by value. Equality is structural when every field is
comparable: nested structs recurse, scalar/string fields compare values, and reference fields use
their documented identity rule. Padding bytes are never compared.
