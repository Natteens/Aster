# Strings

Use `string` for immutable UTF-8 text. Join two strings with `+`, append to a variable with `+=`,
and read `Length` when you need the number of Unicode scalar values:

```aster
string name = "Natte";
string message = "Olá, " + name + "!";
message += " Bem-vindo.";

WriteLine(message);
int length = message.Length;
```

Both operands of `+` must be strings. ASTER does not silently turn numbers, booleans, characters,
objects, arrays, or structs into text with `+`; use string interpolation instead.

## String interpolation

`$"..."` builds a `string` from literal text and embedded `{expression}` slots, using the ordinary
expression grammar — names, fields, properties, calls, operators, and more:

```aster
int quantity = 4;
int price = 15;

WriteLine($"Total: {quantity * price}");
```

Each `{expression}` is evaluated exactly once, left to right, then converted to text and joined
with the surrounding literal text into one new `string`. `string`, `bool`, `char`, every integer
width, and `float`/`double` have a defined textual conversion; `true`/`false` for `bool`, and a
locale-independent decimal representation for numbers (the separator is always `.`, never a
regional variant). A `void` expression or any other type — classes, interfaces, structs, arrays —
is rejected with a diagnostic naming the type.

Write a literal `{` or `}` as `{{` or `}}`. Format specifiers and alignment (`{value:00}`,
`{value,10}`) are not implemented in this version and are rejected with a specific diagnostic
rather than silently ignored.

Normal strings are unaffected: `"{like this}"` has no `$` prefix, so `{` and `}` stay literal text.

## Length and Unicode

`Length` counts Unicode scalar values, not UTF-8 bytes. For example, `"Olá, Natte!"` occupies more
than 11 UTF-8 bytes because `á` is multibyte, but its `Length` is 11. This is not a grapheme-cluster
count: a user-perceived character made from multiple Unicode scalars counts once per scalar.

`Length` is read-only. `text[index]` accepts an `int` Unicode-scalar index and returns `char`; it
does not expose UTF-8 bytes or split a multibyte scalar.

`foreach (char scalar in text)` decodes the same scalar sequence without allocating an iterator.
Combining marks remain separate values because ASTER does not segment grapheme clusters.

## Empty strings

The small `aster.text` standard-library namespace provides one helper:

```aster
using aster.text;

if (String.IsEmpty(message))
{
    WriteLine("Mensagem vazia");
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

There is no general implicit `ToString`, split, replace, regex, mutable buffer, or nullable string.
ASTER exposes neither UTF-8 bytes nor raw string pointers. Interpolation has no format specifiers,
alignment, culture, or raw/verbatim form.
