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

For a positive or runtime-known length, `new` requires an element type with a valid all-zero value.
In particular, `new string[1]`, `new string[length]`, and zeroed structs containing references are
rejected because ASTER does not have `null`; use an array literal that initializes every element
instead.

A length proven to be constant zero is different: `new T[0]` is valid for every otherwise-valid
element type because it creates no element slots and therefore no invalid default value. The result
is still an ordinary non-null array object with `Length == 0`, and indexing it fails through normal
bounds checking. An explicit array variable also supplies the exact type for an empty literal:

```aster
string[] first = [];
string[] second = new string[0];
```

The same expected-type rule applies to explicit initialization, assignment, return values,
unambiguous call/constructor arguments, conditional and enum-switch arms, and nested arrays:

```aster
string[] Empty() => [];
string[][] groups = [[], ["a", "b"]];
groups[0] = [];
```

A standalone or `var`-initialized `[]` still has no element type and is rejected. Overloaded calls
remain ambiguous when more than one candidate can supply an array element type, and non-empty
literals retain their existing inference and promotion rules.

`values.Length` returns an `int` and cannot be assigned. Indices must be non-negative `int` values.
Every runtime access is bounds checked; an invalid index makes the CLI report a controlled runtime
error.

Arrays may contain executable scalar types or finite structs and may be passed to or returned from
ASTER functions. Arrays returned to the selected CLI entry are not printable, so a public scalar
entry function should consume the result.

`foreach (T value in values)` or `foreach (var value in values)` captures the array reference and length once, then reads each element
from index zero upward. The iteration variable is a read-only copy. Array elements can still be
changed through an explicit index, and later iterations observe those changes.

There is no `null`, resizing, slicing, rectangular multidimensional array, or independent freeing.
Nested jagged arrays such as `T[][]` are ordinary arrays whose elements are array references. `==` and
`!=` compare array reference identity; elements are not compared implicitly. All storage belongs
to the current JIT execution context.

See [Arrays and collections](collections.md) for `foreach`, `List<T>`, and `Dictionary<K, V>`.
