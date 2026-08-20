# Strings

ASTER stores `string` values as immutable UTF-8 text. The public surface stays scalar-oriented: `Length`
counts Unicode scalar values, `foreach` walks those scalars in order, and equality compares text content.

| Capability | Current behavior |
| --- | --- |
| 🧬 **Representation** | Immutable UTF-8 |
| 📏 **Length** | Unicode scalar count |
| 🔁 **Scalar traversal** | `foreach (char scalar in text)` |
| 🔒 **Direct indexing** | Not supported |
| 🟰 **Equality** | Content comparison with `==` / `!=` |

```aster
string name = "Natte";
string message = "Olá, " + name + "!";
message += " Bem-vindo.";

WriteLine(message);
int length = message.Length;
```

Both operands of `+` must be strings. ASTER does not silently turn numbers, booleans, characters,
objects, arrays, or structs into text with `+`; use string interpolation instead.

## ✨ String interpolation

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

## 🔤 Length, Unicode, and iteration

`Length` counts Unicode scalar values, not UTF-8 bytes. For example, `"Olá, Natte!"` occupies more
than 11 UTF-8 bytes because `á` is multibyte, but its `Length` is 11. This is not a grapheme-cluster
count: a user-perceived character made from multiple Unicode scalars counts once per scalar.

`Length` is read-only. Use `foreach (char scalar in text)` to process the same scalar sequence in
Unicode-scalar order without allocating an iterator. Combining marks remain separate values because
ASTER does not segment grapheme clusters.

> [!IMPORTANT]
> Direct string indexing is not part of the current language. `text[index]` is rejected with a type
> diagnostic. ASTER currently exposes scalar traversal through `foreach`, not a random-access scalar
> API or raw UTF-8 byte indexing.

## `aster.text` helpers

The `aster.text` standard-library namespace provides ordinal, case-sensitive helpers. They do not
normalize Unicode or use a culture-sensitive comparison.

```aster
using aster.text;

string value = String.Trim("  alpha,beta  ");
string[] parts = String.Split(value, ",");
string changed = String.Replace(parts[0], "alpha", "ASTER");

if (String.StartsWith(changed, "ASTER"))
{
    WriteLine(changed);
}
```

| Function | Behavior |
| --- | --- |
| `IsEmpty(value)` | True exactly when `value.Length == 0`. |
| `Contains(value, pattern)` | Ordinal substring search. An empty pattern is true. |
| `StartsWith(value, pattern)` / `EndsWith(value, pattern)` | Ordinal prefix/suffix search. An empty pattern is true. |
| `Substring(value, start)` / `Substring(value, start, length)` | Copies complete Unicode scalars at checked scalar offsets. |
| `Trim(value)` | Removes ASTER's fixed Unicode White_Space scalars from both ends. |
| `Replace(value, oldValue, newValue)` | Replaces non-overlapping ordinal matches. |
| `Split(value, separator)` | Produces an array of ordinal segments, preserving empty leading, trailing, and adjacent segments. |

`Substring` rejects a negative or out-of-range scalar start/length with a controlled runtime error.
`Replace` and `Split` reject an empty pattern/separator with a controlled runtime error. The helpers
allocate ordinary immutable strings/arrays under the current execution context; callers observe the
same immutable reference and value semantics as any other string or array. Use `""` directly when
an empty value is needed; there is no `String.Empty`.

`Trim` recognizes exactly U+0009–U+000D, U+0020, U+0085, U+00A0, U+1680,
U+2000–U+200A, U+2028, U+2029, U+202F, U+205F, and U+3000. This set is independent
of operating-system locale and host Unicode-table updates. Zero-width space U+200B is not trimmed.
`Replace` scans left to right and replaces non-overlapping matches; replacement text is never
rescanned. `Split` treats the separator literally and preserves every empty segment.

## 🧠 Immutability and allocation

A string variable holds a reference to immutable bytes. Assignment copies that reference. Literals
are interned in the current JIT module. A dynamic concatenation allocates a new immutable UTF-8
string in the current `ExecutionContext`; all such allocations are released together when `run`
finishes. Watch creates a new context for every rebuild, so references never cross executions.

Literal-only constant concatenations are folded by the compiler. Concatenating with an empty string
reuses the other reference after evaluating both operands, so evaluation order is unchanged. Other
dynamic concatenations allocate exactly one result for each binary `+` operation.

`==` and `!=` compare string content, including dynamically concatenated strings.

## Incremental construction

Use `aster.core.StringBuilder` when text grows across a loop or another incremental process. It is
an explicitly mutable construction object; ordinary `string` values and the behavior of `+` remain
unchanged.

```aster
using aster.core;

StringBuilder builder = new StringBuilder();
for (int i = 0; i < 20000; i++)
{
    builder.Append("x");
}

string value = builder.ToString();
```

The initial API is deliberately small: `StringBuilder()`, `Append(string)`, and `ToString()`.
Backing storage grows geometrically, so repeated append has amortized linear copy work rather than
copying the accumulated prefix on every iteration. `ToString()` copies the active content into a
normal immutable string. A later append cannot mutate a snapshot returned earlier.

Builder headers and backing buffers belong to the current `ExecutionContext` and follow the same
temporary/persistent region decisions as other reference allocations. A builder is mutable local
state, not a shared concurrency primitive, and cannot cross `Task` or `Parallel` worker boundaries.
Static safe `+` chains can still use the compiler's one-allocation join path; effectful and ordinary
binary concatenation retain their existing evaluation and allocation behavior.

For one narrow, compiler-proven loop shape, ASTER may replace an unobservable
`value = value + part` chain with this same builder implementation. Ordinary strings remain
immutable, ordinary binary `+` keeps its language semantics, and the final observable value is an
ordinary immutable `string`. Removed intermediate allocations are not executed merely to preserve
their former resource-failure points. Any alias, intermediate read, effectful shape, or ambiguous
control flow retains normal pairwise concatenation; not every concat loop is rewritten.

## 🚧 Current limits

There is no general implicit `ToString`, regex, nullable string, or direct string indexing.
`StringBuilder` does not expose capacity, insertion, replacement, formatting, or implicit
conversion. ASTER exposes neither UTF-8 bytes nor raw string pointers. Interpolation has no format
specifiers, alignment, culture, or raw/verbatim form.
