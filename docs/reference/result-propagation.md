# Result propagation (`?`)

The postfix `?` operator propagates the `Error` case of an
[`aster.core.Result<T, E>`](option-result.md) and continues with the `Ok`
payload:

```aster
int value = Parse(text)?;
```

If `Parse(text)` produced `Ok(value)`, execution continues and `Parse(text)?`
evaluates to that `value`. If it produced `Error(error)`, the enclosing function
returns immediately with its own `Error(error)`.

That single line is equivalent to writing the `switch` by hand:

```aster
Result<int, string> temporary = Parse(text);

switch (temporary)
{
    case Ok(value):
        // continue with value
    case Error(error):
        return Result<int, string>.Error(error);
}
```

The equivalence is only explanatory: `?` lowers directly to typed control flow,
it does not expand into source-level `switch`.

## Rules

- **`Ok` continues, `Error` returns early.** The success payload becomes the
  value of the expression; the error path is a normal early `return`.
- **The operand is evaluated exactly once.** `NextResult()?` calls `NextResult`
  a single time and reuses that value to read the tag, extract the payload, and
  continue or return.
- **The error types must match exactly.** If the expression is
  `Result<T, E>`, the enclosing function must return `Result<U, E>` with the
  same `E`. There are no automatic error conversions — no `From`/`Into`, no
  widening, no wrapping, no coercion to `string`. Convert with a `switch` when
  the error types differ.
- **The success type may differ.** The expression's success type `T` need not
  equal the function's success type `U`:

  ```aster
  public Result<string, ParseError> Format(string text)
  {
      int number = Parse(text)?;          // Parse returns Result<int, ParseError>
      return Result<string, ParseError>.Ok("valid");
  }
  ```

- **Only the official `aster.core.Result` supports `?`.** Recognition is
  nominal: a user-defined enum named `Result` in another namespace is not the
  official type and is rejected.
- **`?` needs an enclosing function.** Used at namespace scope there is nowhere to
  return, so it is rejected.
- **There are no exceptions.** `?` is ordinary control flow; there is no
  `throw`, `catch`, or unwinding.

## Where `?` can appear

`?` joins the existing postfix chain (calls, member access, indexing) and binds
tighter than arithmetic, so `Parse(text)? + 1` means `(Parse(text)?) + 1`. It is
valid anywhere the extracted value is valid: local initializers, call arguments,
returned expressions, arithmetic, and `bool` conditions.

```aster
int value = Read()?;
Use(Read()?);
return Result<int, string>.Ok(Read()? + 1);
if (Validate()? == true) { return Result<int, string>.Ok(42); }
```

## Diagnostics

`?` reports, never panics:

- operand is not a `Result` → *`?` requires an `aster.core.Result<T, E>` value*;
- enclosing function does not return `Result` → *requires the enclosing function
  to return `aster.core.Result<..., E>`*;
- error types differ → *cannot propagate error type `A`; the enclosing function
  returns `Result<..., B>`*;
- operand is a user-defined `Result` → *works only with `aster.core.Result`*;
- operand is an `Option` → *`?` does not support `aster.core.Option<T>` yet*;
- used outside a function → *`?` cannot be used outside a function*.

## Current limitations

Current boundaries:

- **`Option<T>` is not propagated yet.** Only `Result` participates in `?`.
- **No automatic error conversion.** Mismatched error types must be converted
  explicitly with `switch`.
- **No operator customization.** `?` is not backed by a trait or interface.
- **The ternary `?:`, `?.`, and `??` are separate constructs.** A lone trailing
  `?` after an expression is always `Result` propagation; a `?` followed by
  `?.`/`??` is reported as a syntax error for now.

Generic functions may both propagate and construct a `Result` built from their
own type parameters — `Result<T, E>.Ok(..)` and `input?` inside
`Forward<T, E>(Result<T, E> input)` monomorphize and run like any other generic
code. Every type parameter is substituted before concrete HIR, so no unresolved
`T`/`U`/`E` reaches MIR or the backend.
