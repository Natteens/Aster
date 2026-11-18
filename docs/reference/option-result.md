# Option and Result

[`aster.core`](standard-library.md) provides two generic enums for explicit absence and failure:

```aster
using aster.core;

Option<int> some = Option<int>.Some(42);
Option<int> none = Option<int>.None;

Result<int, string> ok = Result<int, string>.Ok(42);
Result<int, string> error = Result<int, string>.Error("invalid value");
```

Enum payload constructors use the ordinary named-argument rule, so a payload may also be written as
`Option<int>.Some(value: 42)`. The names are compile-time call metadata and do not change enum layout.

| Container | Continue case | Early-return case | Enclosing function |
| --- | --- | --- | --- |
| `Option<T>` | `Some(T value)` | `None` | `Option<U>` |
| `Result<T, E>` | `Ok(T value)` | `Error(E error)` | `Result<U, E>` |

## 🧩 `Option<T>` and `T?`

`Option<T>` is either `Some(T value)` or `None`. `T?` is shorthand for `aster.core.Option<T>` and can
be used without `using aster.core;` (`void?` is rejected; `T??` is `Option<Option<T>>`). Non-optional
classes, strings, arrays, collections, and interfaces remain strictly non-null; `null` only inhabits
explicit optional types `T?` / `Option<T>`.

- **Target-typed `null`**: `null` produces `Option<T>.None` when an optional target type is known
  (initialization, assignment, `return`, call arguments, ternary/array branches, `(T?)null`, or
  `x == null` / `x != null`). `var x = null;` and `null == null` are rejected.
- **Implicit wrapping**: any `T` implicitly converts to `Option<T>.Some(value)` (`T?`), while exact
  `T` matches retain overload priority over `T?`. `Option<T>` does not implicitly unwrap to `T`.
- **Null checks without flow narrowing**: `x == null` and `x != null` test the `None` discriminant.
  They do **not** narrow `x`'s static type in `if (x != null)` — unwrap explicitly with `??`, `?.`,
  `?`, or `switch`.
- **Null-coalescing `??`**: short-circuiting and right-associative (`a ?? b ?? c`). Evaluates `a`
  once; `T? ?? T` yields `T`, while `T? ?? T?` yields `T?`.
- **Null-conditional `?.`**: short-circuiting (`a?.Member`, `a?.Call()`, chaining `a?.B?.C`). Evaluates
  `a` once; if `Some`, accesses the member and lifts `V` to `V?` (flattening `Option<V>` to `V?`, so
  `a?.Maybe()?.Next` stays one `Option` layer). Accessing a `void` method via `?.` (`a?.Reset();`)
  is allowed only as a statement. `?.` results cannot be assigned to or incremented.
- **Null-pointer niche layout**: `Option<Class>`, `Option<string>`, `Option<T[]>`, `Option<List<T>>`,
  and `Option<Dictionary<K, V>>` use a single pointer-width word (8 bytes on 64-bit, `0` = `None`,
  non-null pointer = `Some`). Nested `Option<Option<Class>>` (`Class??`) and `Option<Interface>` keep
  tagged enum layouts so `None` and `Some(None)` never collide and fat interface pointers stay intact.

```aster
string? name = null;
int? length = name?.Length;        // None (flattens nested Option<T> automatically)
int finalLen = length ?? 0;        // 0; RHS only runs on None
int? wrapped = 42;                 // implicit T -> T?
```

Use an exhaustive `switch` when both cases need different behavior:

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

Postfix `?` is available when absence should be forwarded to another `Option`-returning function:

```aster
public Option<int> ParsePositive(string text)
{
    int value = text.TryParseInt()?;

    return value > 0
        ? Option<int>.Some(value)
        : Option<int>.None;
}
```

`Some(value)` continues with `value`. `None` returns the enclosing function's own
`Option<U>.None`; the enclosing payload type `U` does not need to equal the operand payload type
`T`.

## ⚠️ `Result<T, E>`

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

Postfix `?` forwards an `Error` and continues with the `Ok` payload. The enclosing function may use
a different success type, but its error type must match exactly.

```aster
public Result<string, ParseError> Format(string text)
{
    int number = Parse(text)?;
    return Result<string, ParseError>.Ok("valid");
}
```

## ↪️ Propagation with `?`

`?` works with the official `aster.core.Option` and `aster.core.Result` types. Recognition is
nominal, the operand is evaluated exactly once, and the enclosing function must return the matching
container family.

There is no automatic conversion between `Option` and `Result`, no implicit error conversion,
implicit unwrap, exception conversion, default `None`, or automatic logging. `?` is ordinary typed
control flow, not exception handling.

See [Propagation with `?`](result-propagation.md) for the exact rules and diagnostics.

## 🧬 Representation

`Option` and `Result` are ordinary monomorphized enum types. Their concrete payload types are
specialized before HIR, MIR, layout, and backend execution, and they follow the same copy/reference
rules as those payloads.
