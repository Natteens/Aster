# Option and Result

[`aster.core`](standard-library.md) provides two generic enums for explicit absence and failure:

```aster
using aster.core;

Option<int> some = Option<int>.Some(42);
Option<int> none = Option<int>.None;

Result<int, string> ok = Result<int, string>.Ok(42);
Result<int, string> error = Result<int, string>.Error("invalid value");
```

`Option<T>` is either `Some(T value)` or `None`. It does not introduce `null`: a class stored in
`Some` is still a non-null class reference.

```aster
public int ValueOr(Option<int> option, int fallback)
{
    switch (option)
    {
        case Some(value):
            return value;
        case None:
            return fallback;
    }
}
```

`Result<T, E>` is either `Ok(T value)` or `Error(E error)`. The error is ordinary program data;
constructing it does not log, throw, or stop execution.

```aster
public int Read(Result<int, string> result)
{
    switch (result)
    {
        case Ok(value):
            return value;
        case Error(message):
            Log.Error(message);
            return 0;
    }
}
```

`Result` supports the postfix `?` propagation operator, which forwards an
`Error` and continues with the `Ok` payload — see
[result-propagation.md](result-propagation.md). `Option` does not support `?`
yet. There is otherwise no implicit unwrapping, exception conversion, default
`None`, or automatic logging. `Option` and `Result` are ordinary monomorphized
enum types and follow the same copy and reference rules as their payloads.
