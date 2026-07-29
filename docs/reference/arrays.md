# Arrays

Arrays are mutable references to fixed-length homogeneous storage:

```aster
int[] values = [10, 20, 30];
int[] sameValues = values;
sameValues[0] += 5;
```

Both variables refer to the same array, so `values[0]` is now `15`. Assigning a struct copies its
fields; assigning an array copies its identity-bearing reference.

Use `new T[length]` for zero-initialized storage:

```aster
int[] values = new int[3];
values[1] = 20;
```

`new` currently requires an element type with a valid all-zero value. In particular,
`new string[length]` and zeroed structs containing references are rejected because ASTER does not
have `null`; use an array literal that initializes every element instead.

`values.Length` returns an `int` and cannot be assigned. Indices must be non-negative `int` values.
Every runtime access is bounds checked; an invalid index makes the CLI report a controlled runtime
error.

Arrays may contain executable scalar types or finite structs and may be passed to or returned from
ASTER functions. Arrays returned to the selected CLI entry are not printable, so a public scalar
entry function should consume the result.

`foreach (T value in values)` captures the array reference and length once, then reads each element
from index zero upward. The iteration variable is a read-only copy. Array elements can still be
changed through an explicit index, and later iterations observe those changes.

There is no `null`, resizing, slicing, multidimensional array, or independent freeing. `==` and
`!=` compare array reference identity; elements are not compared implicitly. All storage belongs
to the current JIT execution context.

See [Arrays and collections](collections.md) for `foreach`, `List<T>`, and `Dictionary<K, V>`.
