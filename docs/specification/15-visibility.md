# 15 — Visibility

## Objective

Define the accepted accessibility modifiers and defaults for namespaces, projects, user-defined types,
and type members without silently introducing class inheritance.

## Accepted syntax

```aster
public
internal
protected
private
```

Exactly one simple visibility modifier may appear on a declaration. Compound forms such as
`protected internal` and `private protected` are not part of Aster at this stage.

## Accepted rules

### `public`

Accessible from any project that can access the declaration. It may be applied to classes,
structs, interfaces, methods, fields, properties, constructors, and exported namespace declarations.

```aster
public class Player
{
    public void Move()
    {
    }
}
```

### `internal`

Accessible within the same Aster project, including across different namespaces. A file must still
use the other namespace to bring its names into scope. The project root is the nearest `Aster.toml`
directory, or the root source directory when no manifest exists. It may be used for namespace-level
declarations and type members.

```aster
internal class CompilerCache
{
    internal void Clear()
    {
    }
}
```

### `private`

Accessible only within the type that declares the member. It is valid for fields, methods,
properties, constructors, and nested types. It is not valid on ordinary namespace-level declarations.

```aster
public class Player
{
    private int health = 100;

    private void ResetHealth()
    {
        health = 100;
    }
}
```

### `protected`

Accessible from the declaring class and related derived types. It is valid only for class members
and related nested types, never for ordinary namespace-level declarations. `protected` is accepted as
part of the visibility system, but its practical use depends on a future accepted inheritance or
extension model. Class inheritance itself remains unaccepted.

```aster
public class Entity
{
    protected int id;
}
```

### Default visibility

- A namespace-level declaration is `internal` when no modifier is written.
- A class or struct member is `private` when no modifier is written.
- Required interface members are public by contract.
- Explicit modifiers are recommended for public API declarations.
- Only one simple visibility modifier may be applied to one declaration.
- Aster does not currently have compound accessibility, `friend`, file-only visibility, or other
  visibility modifiers.

## Valid design example

```aster
public class Player
{
    private int health;

    public int GetHealth()
    {
        return health;
    }

    internal void RestoreForTesting()
    {
        health = 100;
    }
}
```

## Invalid design examples

```aster
public private class Player
{
}
```

Invalid because a declaration may have only one simple visibility modifier.

```aster
protected class GlobalType
{
}
```

Invalid because `protected` applies only to class members or related nested types, not ordinary
namespace-level declarations.

```aster
private void GlobalFunction()
{
}
```

Invalid because `private` is type-scoped and cannot be applied to an ordinary namespace-level function.
Use the default `internal` visibility or an explicit namespace-level `internal`/`public` modifier.

## OPEN QUESTIONS

### PROPOSED — Extension model required by `protected`

1. **Single class inheritance** — gives `protected` conventional meaning; introduces object hierarchy,
   layout, dispatch, construction, and fragile-base-class concerns.
2. **Explicit extension types without inherited state** — permits privileged extensions while avoiding
   class inheritance; creates a less familiar access model.
3. **Keep `protected` reserved but unusable** — preserves the accepted vocabulary without choosing an
   extension model; offers no immediate value and requires diagnostics for every practical use.

**Recommendation:** PROPOSED — keep `protected` specified and diagnosed as unavailable until an extension
model is accepted. Do not adopt inheritance solely to make the modifier usable.
