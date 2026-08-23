# 06 — Control flow

## Accepted syntax

ASTER supports `if`/`else`, `while`, C-style `for`, compiler-known `foreach`, enum `switch`,
`break`, `continue`, and `return`.

```aster
if (temperature > limit)
{
    Warn();
}

for (int index = 0; index < values.Length; index++)
{
    total += values[index];
}

foreach (int value in values)
{
    total += value;
}

foreach (var value in values)
{
    total += value;
}
```

## Rules

- Conditions have type `bool`.
- Control-flow bodies use blocks.
- C-style `for` has initializer, condition, and update clauses.
- `foreach (T name in expression)` checks an explicit element type; `foreach (var name in
  expression)` infers the exact element type.
- Arrays, `List<T>`, and strings are the only `foreach` collections.
- The collection expression is evaluated once; no public iterator protocol is involved.
- The iteration variable is read-only and scoped to the body.
- `break` exits the nearest loop and `continue` advances that loop.
- `return` exits the current function.
- Statements proven unreachable receive a warning.

List iteration fails if the list is structurally modified while the loop is active. String
iteration yields Unicode scalars as `char`.

## Enum `switch`

`switch` over an enum is exhaustive unless it has `default`. Cases do not fall through, and payload
bindings are scoped to their direct arm. A restricted expression form uses `=>` arms and produces
one value. See [Enums](16-enums.md).

## Not implemented

`if` is not an expression. There is no unconditional `loop`, iterator protocol, loop label,
general `match`, nested pattern, guard, or exception control flow.
