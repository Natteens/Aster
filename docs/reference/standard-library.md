# Standard library

The standard library is the small set of official `aster.*` namespaces shipped with the compiler.
It is ordinary Aster source code, so its public API follows the same type checking, overload
resolution, HIR, MIR, and JIT pipeline as project code.

Bring an official namespace into scope by name:

```aster
using aster.math;
using aster.text;
using aster.core;
```

Official namespaces are loaded from the compiler distribution, not from the project directory. A
project therefore cannot replace `aster.math`, `aster.text`, or `aster.core` with local files or declare its own `aster.*`
namespace. If an official source is missing, the compiler reports an incomplete installation instead
of silently falling back to project code.

The initial namespaces are [`aster.math`](math.md) for scalar math and
[`aster.text`](strings.md) for `String.IsEmpty`. [`aster.core`](option-result.md) provides the
generic `Option<T>` and `Result<T, E>` enums. Logging is also part of Aster's standard surface, but
continues to use its existing runtime-backed `Log`, `Log.Warning`, and `Log.Error` API.

This is deliberately a small foundation. `Option` and `Result` are value types, not collections.
There are no collections, tasks, engine APIs, GPU APIs,
or vector types in the standard library yet.
