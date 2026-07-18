# 06 — Control flow

## Objective

Describe explicit branching, looping, and early exit. This chapter does not define
concurrency, task scheduling, ECS iteration, or lifecycle behavior.

## Proposed syntax

```aster
if (temperature > limit)
{
    warn();
}
else
{
    continueWork();
}

while (remaining > 0)
{
    remaining -= 1;
}

for (int value in values)
{
    Log("Valor processado");
}
```

## Proposed rules

- `if` and `while` conditions must have type `bool`.
- Braces are required around control-flow bodies.
- `else if` is proposed as an `else` followed by another `if`.
- `while` repeats while its condition is true.
- `for (Type name in expression)` is proposed for iteration; the iteration protocol is
  not yet specified.
- `break` exits the nearest loop and `continue` starts its next iteration.
- `return` exits the current function.
- Code proven unreachable should produce at least a diagnostic; whether it is an error
  remains open.

## Valid design examples

```aster
if (ready)
{
    PerformWork();
}

while (running)
{
    if (shouldStop())
    {
        break;
    }
}
```

## Invalid design examples

```aster
if (1) { PerformWork(); } // condition is not bool
break;                    // not inside a loop
continue;                 // not inside a loop
```

## Accepted enum switch

`switch` over an enum is exhaustive unless it has `default`. Cases do not fall
through, and payload bindings are scoped to their direct arm.

## OPEN QUESTIONS

- **OPEN QUESTION:** Is `if` an expression, a statement, or both?
- **OPEN QUESTION:** Is there a `loop` construct for unconditional loops?
- **OPEN QUESTION:** What iteration protocol powers `for`?
- **OPEN QUESTION:** Are general `match`, nested patterns, guards, labels, and labeled breaks supported?
- **OPEN QUESTION:** Are exceptions supported, or does error flow use explicit result values?
- **OPEN QUESTION:** Is unreachable code an error or warning?
