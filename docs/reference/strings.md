# Strings

Use `string` for immutable UTF-8 text. Join two strings with `+`, append to a variable with `+=`,
and read `Length` when you need the number of Unicode scalar values:

```aster
string name = "Natte";
string message = "Olá, " + name + "!";
message += " Bem-vindo.";

Log(message);
int length = message.Length;
```

Both operands of `+` must be strings. Aster does not silently turn numbers, booleans, characters,
objects, arrays, or structs into text.

## Length and Unicode

`Length` counts Unicode scalar values, not UTF-8 bytes. For example, `"Olá, Natte!"` occupies more
than 11 UTF-8 bytes because `á` is multibyte, but its `Length` is 11. This is not a grapheme-cluster
count: a user-perceived character made from multiple Unicode scalars counts once per scalar.

`Length` is read-only, and text indexing is not implemented.

## Empty strings

The small `aster.text` standard-library namespace provides one helper:

```aster
using aster.text;

if (String.IsEmpty(message))
{
    Log("Mensagem vazia");
}
```

`String.IsEmpty(value)` is true only when `value.Length == 0`. Use `""` directly when an empty
value is needed; there is no `String.Empty`.

## Immutability and allocation

A string variable holds a reference to immutable bytes. Assignment copies that reference. Literals
are interned in the current JIT module. A dynamic concatenation allocates a new immutable UTF-8
string in the current `ExecutionContext`; all such allocations are released together when `run`
finishes. Watch creates a new context for every rebuild, so references never cross executions.

Literal-only constant concatenations are folded by the compiler. Concatenating with an empty string
reuses the other reference after evaluating both operands, so evaluation order is unchanged. Other
dynamic concatenations allocate exactly one result for each binary `+` operation.

`==` and `!=` compare string content, including dynamically concatenated strings.

## Current limits

There is no interpolation, implicit `ToString`, indexing, substring, split, replace, regex, mutable
buffer, or nullable string. Strings are not collections, and Aster exposes no raw string pointers.
