# 06 — Control flow

## Accepted syntax

ASTER supports `if`/`else`, `while`, `do`-`while`, C-style `for`, compiler-known `foreach`, enum `switch`,
`break`, `continue`, and `return`.

```aster
if (temperature > limit)
{
    Warn();
}

do
{
    attempts++;
}
while (attempts < limit);

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
- `do { body } while (condition);` runs the body before the first test of the `bool` condition, and
  the terminating `;` is required. `continue` inside it jumps to the condition check. Locals declared in
  the body are not visible in the condition (as in C#).
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

## `switch` on integers, `char` and `string`

`switch` also accepts a subject of any integer type, `char` or `string`, in both the statement and the
`=>` expression form. Labels are compile-time constants (literals, named constants, constant
expressions) that convert implicitly to the subject type; an integer constant must fit the subject
range, so `case 300:` on a `byte` is an error, and a label of another type is an error. A duplicate
constant label is a compile error. `string` labels compare ordinally (byte-wise UTF-8, no
normalization or case folding). Only enums can be exhaustive, so these switches require `default`.
The subject is evaluated once, cases do not fall through, and `default` must be the last case.
`break` and `continue` inside a case apply to the enclosing loop.

```aster
switch (command)
{
    case "start":
        Start();
    case "stop":
        Stop();
    default:
        Unknown();
}
```

Patterns such as `is T x` and `when` guards are not part of this form.

## Not implemented

`if` is not an expression. There is no unconditional `loop`, iterator protocol, loop label,
general `match`, nested pattern, guard, or exception control flow.
