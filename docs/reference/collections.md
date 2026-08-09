# Arrays and collections

ASTER has three compiler-known iterable types: arrays, `List<T>`, and `string`. Iteration lowers to
an indexed control-flow graph; it does not allocate an iterator or use a public iterator protocol.

## `foreach`

The element type is explicit:

```aster
foreach (int value in values)
{
    total += value;
}
```

The collection expression is evaluated once. The iteration variable exists only in the body and
cannot be reassigned. Primitive and struct elements are copied by value; class, string, array, and
collection references are copied as references. Mutating an object reached through a copied class
reference is allowed, but assigning a field through a copied struct iteration variable is not.

`break`, `continue`, `return`, and postfix `?` use the same control-flow rules as other loops.

## Arrays

Arrays are the fixed-length collection. They support checked indexing, reference identity, and
compiler-known `foreach`:

```aster
int[] values = [10, 20, 30];
values[1] = 25;
```

The complete allocation, initialization, bounds, identity, and iteration rules are in
[Arrays](arrays.md).

## `List<T>`

`List<T>` is a nominal, growable reference collection:

```aster
List<int> values = new List<int>();
values.Add(10);
values.Add(20);

int first = values.Get(0);
values.RemoveAt(0);
int count = values.Length;
```

`Length` is read-only. `Get` and `RemoveAt` validate the index. Assignment copies the list
reference, not its elements.

`foreach` captures the list and checks its structural version as it advances. `Add` or `RemoveAt`
through any alias during iteration fails with a controlled runtime error. The element binding is a
copy obtained at the start of that iteration.

## `Dictionary<K, V>`

`Dictionary<K, V>` is a nominal hash table with deterministic insertion-order snapshots:

```aster
using aster.collections;
using aster.core;

Dictionary<string, int> counts = new Dictionary<string, int>();
counts.Add("aster", 1);
counts.Set("aster", 2);

Option<int> count = counts.TryGet("aster");
bool known = counts.ContainsKey("aster");
DictionaryEntry<string, int>[] entries = counts.Entries();
```

`Add` returns `false` for an existing key. `Set` inserts or replaces and reports whether it replaced
an existing value. `Remove` reports whether a key was present. `TryGet` returns `Option<V>`, and
`Length` is the number of live entries.

Supported key types are `bool`, `char`, the signed and unsigned integer types, and `string`. String
keys use ordinal UTF-8 equality without normalization or case folding. Values may be any concrete
type with an executable layout.

`Dictionary` is not directly iterable. `Entries()` creates an insertion-order snapshot array, which
can be used with ordinary array `foreach`.

## Strings

`foreach (char scalar in text)` decodes immutable UTF-8 in Unicode-scalar order. Combining marks
remain separate scalar values, and no grapheme-cluster allocation occurs. Direct string indexing is
not supported. See [Strings](strings.md) for scalar iteration, interpolation, equality, and allocation
behavior.

Arrays, lists, dictionaries, and strings are not transferable across worker boundaries. They may
still be created and used inside a worker when every operation in that body is otherwise allowed.
