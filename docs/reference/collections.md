# Arrays and collections

ASTER has three compiler-known iterable types: arrays, `List<T>`, and `string`. Iteration lowers to
an indexed control-flow graph; it does not allocate an iterator or use a public iterator protocol.

## `foreach`

The element type may be explicit or inferred from the compiler-known collection:

```aster
foreach (int value in values)
{
    total += value;
}

foreach (var value in values)
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
List<int> values = new();
values.Add(10);
values.Add(20);
values[1] = 25;

int first = values[0];
values[0]++;
values.RemoveAt(0);
int[] snapshot = values.ToArray();
values.Clear();
int count = values.Length;
```

`Length` is read-only. `Get`, `Set`, and `RemoveAt` validate the index. `Set` replaces one element
without changing structural iteration version. `Clear` removes every element. `ToArray()` copies
the active elements into a new fixed-length array; later list mutations cannot change that snapshot.
Assignment copies the list reference, not its elements. `Clear` preserves the list identity and may
retain reusable backing capacity; every alias observes the empty list and subsequent `Add` calls
reuse the same collection normally.

`values[index]` is ergonomic syntax over the same checked `Get`/`Set` runtime operations. Reads,
writes, numeric compound assignments, and numeric prefix/postfix increment/decrement are supported.
The receiver and index are evaluated once, and a controlled failure branches before an unwritten
`Get` result can be consumed. Dictionary and string indexing remain unsupported.

`foreach` captures the list and checks its structural version as it advances. `Add` or `RemoveAt`
through any alias during iteration fails with a controlled runtime error. The element binding is a
copy obtained at the start of that iteration.

## `Dictionary<K, V>`

`Dictionary<K, V>` is a nominal hash table with deterministic insertion-order snapshots:

```aster
using aster.collections;
using aster.core;

Dictionary<string, int> counts = new();
counts.Add("aster", 1);
counts.Set("aster", 2);

Option<int> count = counts.TryGet("aster");
bool known = counts.ContainsKey("aster");
DictionaryEntry<string, int>[] entries = counts.Entries();
string[] keys = counts.Keys();
int[] values = counts.Values();
counts.Clear();
```

`Add` returns `false` for an existing key. `Set` inserts or replaces and reports whether it replaced
an existing value. `Remove` reports whether a key was present. `TryGet` returns `Option<V>`, and
`Length` is the number of live entries.

Supported key types are `bool`, `char`, the signed and unsigned integer types, and `string`. String
keys use ordinal UTF-8 equality without normalization or case folding. Values may be any concrete
type with an executable layout.

`Dictionary` is not directly iterable. `Entries()`, `Keys()`, and `Values()` create independent
insertion-order snapshot arrays, which can be used with ordinary array `foreach`. `Clear()` removes
all live entries. Neither a snapshot nor a copied collection reference becomes transferable across
worker boundaries.

Snapshots copy values with ordinary ASTER assignment semantics: primitives and structs are value
copies, while reference-bearing elements preserve reference identity rather than being deep-cloned.
The snapshot storage is independent, so clearing or mutating the source collection cannot change its
length, order, or element slots. `Dictionary.Clear()` preserves dictionary identity and reusable
backing; newly added entries establish a new insertion order after the clear.

Snapshot arrays work with inferred iteration variables, for example
`foreach (var entry in counts.Entries())`.

## Strings

`foreach (char scalar in text)` and `foreach (var scalar in text)` decode immutable UTF-8 in Unicode-scalar order. Combining marks
remain separate scalar values, and no grapheme-cluster allocation occurs. Direct string indexing is
not supported. See [Strings](strings.md) for scalar iteration, interpolation, equality, and allocation
behavior.

Arrays, lists, dictionaries, and strings are not transferable across worker boundaries. They may
still be created and used inside a worker when every operation in that body is otherwise allowed.
