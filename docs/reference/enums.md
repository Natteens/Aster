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
```

Enum values copy by value. Their numeric tag and memory layout are compiler details: Aster does
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

```aster
switch (message)
{
    case Quit:
        return 0;
    default:
        return 1;
}
```

This first `switch` works only with enums. It is a statement, not an expression. General pattern
matching, guards, combined cases, numeric/string switches, explicit discriminants, enum methods,
and recursive value layouts are not implemented.
