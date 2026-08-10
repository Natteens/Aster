# Enums and safe selection

Enums represent exactly one case from a closed set. A case may carry data, which supports states,
absence, and expected failures without `null`, exceptions, or sentinel values.

## Accepted syntax

```aster
public enum Message
{
    Quit,
    Move(int x, int y),
}

Message message = Message.Move(20, 22);
```

Generic enums are specialized with concrete types before code generation:

```aster
public enum Option<T>
{
    Some(T value),
    None,
}
```

## `switch`

```aster
switch (message)
{
    case Move(x, y):
        return x + y;
    case Quit:
        return 0;
}
```

The selected value is evaluated once. Cases do not fall through, and `break` remains a loop-only
statement. Each arm has its own scope. Without `default`, every case must appear; duplicate or
unknown cases and incorrect payload binding counts are errors.

The restricted expression form selects one value:

```aster
return message switch
{
    Quit => 0,
    Move(x, y) => x + y,
};
```

It accepts the same enum cases and payload bindings, evaluates the selected value once, evaluates
only the chosen arm, and requires compatible arm result types. Arms are comma-separated and
`default`, when present, must be last.

## Representation

A concrete enum is a value containing an internal tag and aligned storage for its largest payload.
The tag is not public and cannot be converted to an integer. Copy and equality inspect only the
active payload. Enums do not require boxing or a heap allocation by default.

## Invalid examples

```aster
switch (message)
{
    case Quit:
        return 0;
}
```

This switch omits `Move` and has no `default`.

```aster
Message message = Message.Move(10);
```

`Move` requires two `int` arguments.

## Current limits

Nested patterns, guards, general pattern matching, numeric discriminants, flags, enum casts, enum methods,
and implicit default values are not implemented. Nested structural patterns may be reconsidered
after exhaustiveness diagnostics can remain equally precise.
