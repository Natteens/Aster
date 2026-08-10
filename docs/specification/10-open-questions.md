# 10 — Decision register

## Objective

Track accepted decisions and index unresolved proposals without allowing a recommendation to
silently become language behavior. Detailed open proposals must include two or three alternatives,
advantages, disadvantages, and a recommendation marked `PROPOSED`.

## Decision status syntax

```text
Status: ACCEPTED | PROPOSED | REJECTED | SUPERSEDED
Scope: language | standard-library | runtime | SDK | optional-library
```

`PROPOSED` is not permission to implement. Compiler support requires a separate implementation
decision and an update to the implemented grammar.

## Accepted rules

### Variables and constants — ACCEPTED

- `Type name = value;` declares a mutable explicitly typed variable.
- `var name = value;` declares a mutable inferred variable.
- `const Type name = value;` declares a compile-time constant.
- `var Type name` is rejected, and variables are not immutable by default.
- `readonly` remains under consideration for write-once fields.

### User-defined types — ACCEPTED

- Classes are objects with identity and behavior.
- Structs are compact values.
- Interfaces are stateless contracts.
- Class inheritance is not adopted.
- Class memory management remains unresolved.
- Finite structs use named-field literals, copy by value, and compare structurally when all fields
  are comparable.

### Logging — ACCEPTED

- The standard library exposes `Log(message)`, `Log.Warning(message)`, and
  `Log.Error(message)`.
- `Log.Info` and `Log.Debug` are not part of the initial API.

Build-profile filtering and its manifest schema remain `PROPOSED`; see `12-logging.md`.

### Visibility — ACCEPTED

- `public`, `internal`, `protected`, and `private` are the four simple visibility modifiers.
- Module-level declarations default to `internal`.
- Class and struct members default to `private`.
- Required interface members are public by contract.
- Only one simple visibility modifier is permitted; compound accessibility is not accepted.
- Practical use of `protected` depends on a future inheritance or extension decision. Class
  inheritance is not accepted merely to support `protected`.

### Callables — ACCEPTED

- Module functions, class methods, struct methods, and interface callable members coexist.
- Applications use one public static parameterless `Main` returning `void` or `int`; an optional
  `Aster.toml` may select `namespace.Class.Main`, and `--function` is an explicit tooling override.
- A `static class` is a non-instantiable container containing only static methods.

### Initial standard library — ACCEPTED

- The `aster.*` namespace is reserved for official read-only library sources distributed with the SDK.
- `aster.math` exposes executable scalar overloads of `Abs`, `Min`, `Max`, and `Clamp`.
- `Abs` reports a controlled runtime error for the minimum `int`/`long`; `Clamp` reports one when
  `min > max`.
- A small typed runtime-error intrinsic may bridge a library precondition until the language has a
  general error-propagation mechanism. Public math methods are not parser or backend special cases.
- `aster.core` exposes ordinary generic enums `Option<T>` and `Result<T, E>` without implicit
  logging, exceptions, unwrapping, or `null`.

### Enums and switch â€” ACCEPTED

- An enum is a value type whose comma-separated cases may carry typed payloads.
- Enum construction names the enum and case; there is no implicit integer representation in source.
- `switch` matches enum cases, evaluates its input once, has no fallthrough, and must be exhaustive
  or contain `default`.
- Restricted enum switch expressions are accepted. General pattern matching, guards, and explicit
  discriminants are not accepted.

### ECS direction — ACCEPTED

- ECS is not part of the ASTER language or compiler. It is not required to use ASTER.
- A future ECS, if built, is a library/framework/engine package written using ASTER —
  not core-language syntax, semantics, or backend support.
- No ECS keyword or compiler-known construct is promised. See `../research/ecs.md`.

## Valid decision record example

```text
Status: PROPOSED
Question: How are class instances reclaimed?
Alternatives: ownership/borrowing; tracing GC; reference counting.
Current JIT subset: a host-owned per-execution arena releases all instances together.
Long-lived/AOT recommendation: investigate ownership/borrowing before accepting a final model.
Implementation authorized beyond the JIT arena: no.
```

## Invalid decision record example

```text
Classes probably use garbage collection because that is easy for users.
```

This silently selects behavior without comparing costs and is not a specification decision.

## OPEN QUESTIONS

The detailed active proposals are:

- Variables, `readonly`, shadowing, globals, and constant evaluation:
  `03-variables-and-constants.md`.
- Interface inheritance/default methods, construction, and struct value behavior: `07-structs.md`.
- Class ownership, allocation, and lifetimes: `09-memory-model.md`.
- Generic syntax, constraints, code generation, and variance: `11-generics.md`.
- Logging formatting, sinks, and failures: `12-logging.md`.
- Runtime linkage and boundaries between product layers: `14-platform-boundaries.md`.
- Visibility package boundaries and the extension model behind `protected`: `15-visibility.md`.
- Program entry-point selection: `05-functions.md`.

Each indexed proposal is `PROPOSED`, not `ACCEPTED`, and contains alternatives, tradeoffs,
and a recommendation. Earlier chapter-level open-question lists remain discovery backlogs; they
must be promoted to the same structured proposal format before any decision is accepted or implemented.
