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

## Type patterns and `when` guards

`value is Type` tests, at run time, whether a class reference is an instance of `Type` or of a class
derived from it, and yields a `bool`. `value is Type name` also declares `name`, the same object
(an identity alias, no copy) with the static type `Type`. A pattern is accepted anywhere a `bool`
expression is: `if`, `while`, `do`, `for` conditions, `&&`, `||`, `!`, a stored or returned `bool`.
`is` and `when` are contextual keywords and remain ordinary identifiers elsewhere.

```aster
if (shape is Circle circle && circle.Radius > 10)
{
    Grow(circle);
}

if (!(shape is Square square))
{
    return;
}
Use(square);   // definitely assigned: the other path returned
```

- The value must be a class reference and `Type` a class related to the value's class: a derived class
  (a real test), the same class or a base class (always true). Unrelated classes can never match and
  are a compile error, as in C#. A struct, primitive or enum target is an error.
- The test follows the single-inheritance descriptor check: it compares the object's ancestor
  at `Type`'s depth, never reading past the descriptor's ancestor array.
- `name` is definitely assigned only where the test is known to have succeeded: the true branch of
  `if`/`while` and the body of a `for`, the right operand of `&&` (and of `||` after a negated test),
  through `!`, `&&` and `||` with the C# rules. Reading it elsewhere is the error
  "variable `name` is used before initialization". A variable declared in an `if` condition belongs to
  the enclosing block (so a guard clause leaves it in scope); one declared in a loop condition is
  scoped to the loop. Declaring the same name twice in one scope is an error.
- Not supported yet: interface targets (`value is IShape s`, which needs the dynamic interface
  lookup of object model B1 part 3), interface-typed subjects, unwrapping `Option<T>` with `is`
  (use a `switch`), nested patterns, `as`, and generic-type inference from a pattern binding.

A `when` guard follows the label of a `switch` case or arm: `case 5 when limit > 3:`,
`Circle(r) when r > 10 => ...`. The guard is a `bool` evaluated only after the label matched, in the
scope of the case's payload bindings; if it is false, matching continues with the following cases and
finally `default`. The subject is still evaluated once, and guards run in case order. A guarded case
never counts as covering its label for exhaustiveness (an enum switch still needs the unguarded case
or `default`), and a guarded label may be repeated by a later case, while a label that an earlier
unguarded case already covers is a duplicate error. Pattern variables of a guard are assigned in the
body of that case.

## Not implemented

`if` is not an expression. There is no unconditional `loop`, iterator protocol, loop label,
general `match`, nested pattern, interface or `Option` type pattern, or exception control flow.
