# Propagation with `?`

The postfix `?` operator propagates absence or failure through the official
[`aster.core.Option<T>` and `aster.core.Result<T, E>`](option-result.md) types.

| Operand | Continue path | Early-return path | Required enclosing return |
| --- | --- | --- | --- |
| `Option<T>` | `Some(value)` → `value` | `None` | `Option<U>` |
| `Result<T, E>` | `Ok(value)` → `value` | `Error(error)` | `Result<U, E>` |

> [!IMPORTANT]
> `?` is nominal and container-specific. It does not convert between `Option` and `Result`, and a
> user-defined enum with the same case names does not participate automatically.

## 🧩 Propagating `Option<T>`

```aster
public Option<int> ParsePort(string text)
{
    int value = text.TryParseInt()?;
    return Option<int>.Some(value);
}
```

If `TryParseInt()` produces `Some(value)`, execution continues and the `?` expression evaluates to
that payload. If it produces `None`, the enclosing function returns `Option<int>.None` immediately.

The enclosing payload type may differ from the operand payload type:

```aster
public Option<string> Classify(string text)
{
    int value = text.TryParseInt()?;

    return value > 0
        ? Option<string>.Some("positive")
        : Option<string>.Some("non-positive");
}
```

`None` from `TryParseInt()` still propagates as `Option<string>.None`.

## ⚠️ Propagating `Result<T, E>`

```aster
public Result<string, ParseError> Format(string text)
{
    int value = Parse(text)?;
    return Result<string, ParseError>.Ok("valid");
}
```

If `Parse(text)` produces `Ok(value)`, execution continues with `value`. If it produces
`Error(error)`, the enclosing function returns its own `Result<string, ParseError>.Error(error)`.

For `Result`, the success payload type may change, but the error type must match exactly. There is
no automatic `From`/`Into`, widening, wrapping, or coercion to `string`.

## 🔒 Shared rules

- **The operand is evaluated exactly once.** A call such as `Next()?` runs `Next` once and reuses the
  resulting enum value for the tag test and payload projection.
- **The container family must match.** `Option<T>?` requires an enclosing `Option<U>` function;
  `Result<T, E>?` requires an enclosing `Result<U, E>` function.
- **Recognition is nominal.** Only the official `aster.core.Option` and `aster.core.Result` types
  participate.
- **`?` needs an enclosing function.** There is no return target at namespace scope.
- **There are no exceptions.** Propagation is typed control flow; there is no `throw`, `catch`, or
  unwinding behind the operator.
- **No implicit unwrap exists.** Without `?`, `Option` and `Result` remain ordinary enum values and
  must be handled explicitly.

The compiler lowers propagation directly to typed control flow. It does not rewrite the source into
a `switch` before compilation.

## 📍 Where `?` can appear

`?` joins the postfix expression chain and binds tighter than arithmetic. It can appear anywhere the
extracted payload is valid:

```aster
int value = Read()?;
Use(Read()?);
return Result<int, string>.Ok(Read()? + 1);
if (Validate()? == true)
{
    return Result<int, string>.Ok(42);
}
```

The same rule applies to `Option` expressions inside functions that return `Option`.

## 🩺 Diagnostics

Invalid uses fail with controlled compiler diagnostics rather than panics. The compiler rejects, in
particular:

- operands that are neither the official `Option` nor `Result`;
- `Option` propagation from a function that does not return `Option`;
- `Result` propagation from a function that does not return `Result`;
- mismatched `Result` error types;
- user-defined lookalike enum types;
- propagation outside a function.

Diagnostics should describe the container actually used and the enclosing return contract instead
of suggesting an implicit conversion.

## 🚧 Current boundaries

There is no automatic `Option` ↔ `Result` conversion, customizable propagation trait/interface,
automatic error conversion, or implicit fallback value.

The ternary `?:` is a separate expression. `?.` and `??` are not propagation forms and are not part
of the current language.

Generic functions may propagate official `Option` or `Result` values built from type parameters.
Specialization substitutes those parameters before concrete HIR, MIR, layout, and backend execution,
so unresolved generic parameters do not reach Cranelift.

Propagation composes with expression-bodied functions because those bodies normalize to the same
typed return flow as a block-bodied function; no separate propagation path is involved.
