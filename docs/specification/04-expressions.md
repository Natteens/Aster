# 04 — Expressions

## Objective

Define implemented expression forms, evaluation order, and operator categories.

## Proposed syntax

```aster
int total = left + right * 2;
bool allowed = active && attempts > 0;
player.position.x = origin.x;
int result = calculate(10, 20);
```

## Accepted additions (implemented)

- `++` and `--` are accepted as prefix and postfix operators on mutable numeric variables.
  Prefix returns the new value; postfix returns the old value. They are rejected on constants,
  literals, temporary results, and non-numeric types.
- The conditional expression `condition ? whenTrue : whenFalse` is accepted. The condition must
  be `bool`, exactly one branch is evaluated, both branches need a compatible value type, and
  the operator is right-associative. Its precedence sits between assignment (lower) and `||`
  (higher).
- `&&` and `||` short-circuit.
- `value is Type [name]` is a `bool` class type test at relational precedence (see
  [control flow](06-control-flow.md#type-patterns-and-when-guards)); `is` is contextual.
- The bitwise operators `&`, `|`, `^`, unary `~`, and the shifts `<<`, `>>`, `>>>` are implemented
  with C# precedence and semantics (see [types](02-types.md#bitwise-and-shift-operators)). `&`, `|`,
  and `^` on `bool` operands are eager: both operands are evaluated, left to right. `a & b == 0`
  parses as `a & (b == 0)`, as in C#.
- `>>` and `>>>` are recognized from directly adjacent `>` characters in operator position only, so
  nested generic arguments such as `List<List<int>>` are unaffected; `a > > b` is not a shift.
- `==` and `!=` compare scalar and string values, comparable structs field by field, arrays and
  classes by reference identity, and interfaces by the identity of their underlying object.
- Implemented precedence, lowest to highest: assignment, `?:`, `||`, `&&`, `|`, `^`, `&`, equality,
  comparison, shift (`<<`, `>>`, `>>>`), additive, multiplicative, unary (`!`, unary `-`, `~`,
  prefix `++`/`--`), postfix (member access, call, postfix `++`/`--`).
- Calls evaluate the receiver and then argument expressions left to right in source order. Named
  arguments map those already-evaluated values into parameter positions.
- Empty `[]` and target-typed `new()` use one exact expected type from an explicit initializer,
  assignment, return, selected call candidate, or contextual conditional/switch arm. They remain
  errors without that context.
- `List<T>` indexing uses the existing checked `Get`/`Set` semantics for reads, writes, compound
  assignments, and prefix/postfix `++`/`--`. Receiver and index expressions are evaluated once.

## General rules

- Literals, names, member access, indexing, calls, unary operations, binary operations,
  assignments, and parenthesized expressions are proposed expression forms.
- Function arguments evaluate from left to right.
- Short-circuiting applies to `&&` and `||`.
- Assignment requires a writable place expression on the left.
- Proposed arithmetic operators are `+`, `-`, `*`, `/`, and `%`.
- Proposed comparison operators are `==`, `!=`, `<`, `<=`, `>`, and `>=`.
- Implemented compound assignments are `+=`, `-=`, `*=`, `/=`, `%=`, `&=`, `|=`, `^=`, `<<=`, `>>=`, and
  `>>>=`. `%=` follows the typing rules of `%` and of the other arithmetic compound assignments
  (the operation type must equal the target type, so it never narrows silently), and evaluates its
  target place once.
- No implicit truthiness is proposed; conditions require `bool`.

## Valid design examples

```aster
int sum = (left + right) * 2;
bool ready = enabled && (count > 0);
position.x += velocity.x;
```

## Invalid design examples

```aster
int value = true + 1;       // incompatible operands
42 = value;                 // literal is not writable
if (count) { }              // int has no proposed truthiness conversion
```

## OPEN QUESTIONS

- **OPEN QUESTION:** Are operator overloading, ranges, and lambdas supported? (The conditional expression `?:` is accepted above.)
