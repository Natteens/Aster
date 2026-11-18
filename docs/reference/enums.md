# Enums and switch

An enum is a value type with a fixed set of named cases. A case may carry values:

```aster
public enum Message
{
    Quit,
    Move(int x, int y),
    Write(string text),
}
```

Cases are separated by commas. Construct a value through its enum type:

```aster
Message quit = Message.Quit;
Message move = Message.Move(10, 20);
Message named = Message.Move(y: 20, x: 10);
```

Enum values copy by value. Their numeric tag and memory layout are compiler details: ASTER does
not implicitly convert an enum to or from an integer, and cases do not accept explicit numeric
values in this version.

## Reading a value with `switch`

`switch` checks cases without fallthrough. The selected arm runs directly; it does not need
`break` or an extra block:

```aster
public int Distance(Message message)
{
    switch (message)
    {
        case Move(x, y):
            return x + y;
        case Quit:
            return 0;
        case Write(text):
            return text.Length;
    }
}
```

The switched expression is evaluated once. Payload names such as `x` and `text` exist only in
their arm. A switch must list every case or provide one `default` arm. Duplicate cases, the wrong
number of payload names, and cases from another enum are errors.

Use the restricted expression form when every arm produces a value:

```aster
public int Distance(Message message)
{
    return message switch
    {
        Move(x, y) => x + y,
        Quit => 0,
        Write(text) => text.Length,
    };
}
```

The input is evaluated once and only the selected arm is evaluated. Arms must produce compatible
types and are comma-separated. The same exhaustiveness and payload rules apply; `default`, when
used, must be the final arm.

When the surrounding expression supplies one exact type, switch arms receive it as context. This
allows values such as `string[] names = mode switch { Empty => [], One => ["ASTER"], };` without
changing exhaustiveness or evaluation rules.

```aster
switch (message)
{
    case Quit:
        return 0;
    default:
        return 1;
}
```

Enum cases are matched by name. Integer, `char` and `string` values can also be switched on with
constant labels and a mandatory `default`; see
[Control flow](../specification/06-control-flow.md), which also describes `when` guards. General
pattern matching, combined cases, explicit discriminants, enum methods, and recursive value layouts are not implemented.
