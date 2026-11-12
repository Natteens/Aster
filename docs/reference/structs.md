# Struct values

Structs group a fixed set of values without creating a heap object.

```aster
public struct Position
{
    public int x;
    public int y;
}

Position position = Position { x: 10, y: 20 };
```

All fields must be named exactly once. Their order inside the literal does not matter. Read and
write fields with dot notation. Assignment and calls copy the whole value, so changing a copy
does not change the original.

A struct may declare auto-properties (`public int X { get; set; }`, `{ get; }`, `{ get; private set; }`).
Each is a private backing field plus trivial accessors, lowered directly to the field, so it copies
by value like any field. A struct literal names the property, not the backing field:
`Position { X: 1, Y: 2 }`; every property must be named exactly once (a get-only property is
initialized there, because a struct has no constructor; object initializers will widen this later).
Struct auto-properties cannot have `= value` initializers yet, and a struct cannot declare explicit
properties. `p.X = 5` writes the variable's field; a `get`-only or `init` property cannot be assigned
after construction.

Structs may contain other finite structs and executable scalar types. A recursive value such as
`struct Node { public Node next; }` has no finite size and is rejected. `==` and `!=` compare fields
recursively when every field is comparable. This is value equality; it does not compare padding.

Struct instance methods execute as ordinary functions with a hidden by-value `this` parameter. They
may read fields, call other methods, and return scalars or aggregate values. Assigning to a field
inside a method changes only that receiver copy; it does not write back into the caller's variable.
Return the changed struct explicitly when the caller needs the updated value. This is the same copy
contract used by struct assignment and parameter passing; ASTER has no reference-receiver syntax.

Copying a struct copies each field value. For a `string`, array, class, or interface field, that
means the copied struct still refers to the same underlying value or object. Mutating an object or
array through that reference is visible through its other aliases. Replacing the struct field
inside an instance method changes only the callee's receiver copy. Returning a referenced field
keeps the referenced value alive under the same conservative escape rules as ordinary function
calls; copying the containing struct does not shorten its lifetime.

A struct may be returned between ASTER functions; the function selected directly by `aster run`
must still return a scalar, `string`, or `void`.

Generic structs such as `Pair<T, U>` are specialized before layout calculation. `Pair<int, string>`
is an ordinary concrete value type and still copies by value. Its instance methods execute after
the owner specialization is concrete, and a method may additionally declare and specialize its own
type parameters. See [generic types](generic-types.md).
