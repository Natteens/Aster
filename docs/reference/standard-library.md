# Standard library

Aster ships a small standard library embedded in the compiler. Its APIs are written in ordinary
Aster source and pass through the same type checking, monomorphization, HIR, MIR, and JIT pipeline
as project code.

Use a standard-library namespace explicitly:

```aster
using aster.math;
using aster.text;
using aster.core;
```

- [`aster.math`](math.md) contains scalar `Abs`, `Min`, `Max`, and `Clamp` overloads.
- [`aster.text`](strings.md) contains focused text helpers such as `String.IsEmpty`.
- [`aster.core`](option-result.md) defines the generic `Option<T>` and `Result<T, E>` enums.
- [Logging](logging.md) exposes `Log`, `Log.Warning`, and `Log.Error` through the runtime boundary.

The `aster.*` prefix is reserved for this embedded library. A project cannot shadow an official
namespace with local source. If an expected embedded source is missing, the compiler reports an
installation error rather than searching the project.

The library is deliberately narrow. It has no general collection package, task API, engine API, or
GPU surface today. Those areas require language and runtime decisions before a library can give them
a stable contract.
