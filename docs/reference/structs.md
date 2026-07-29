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

Structs may contain other finite structs and executable scalar types. A recursive value such as
`struct Node { public Node next; }` has no finite size and is rejected. `==` and `!=` compare fields
recursively when every field is comparable. This is value equality; it does not compare padding.

Struct methods are checked by the frontend but are not executable yet. A struct may be returned
between ASTER functions; the function selected directly by `aster run` must still return a scalar,
`string`, or `void`.

Generic structs such as `Pair<T, U>` are specialized before layout calculation. `Pair<int, string>`
is an ordinary concrete value type and still copies by value. Generic struct methods remain subject
to the same current limitation as other struct methods. See [generic types](generic-types.md).
