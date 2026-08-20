# Standard library

ASTER ships its standard library with the toolchain. An installed CLI resolves it relative to the
executable; development binaries can use the embedded copy, and `ASTER_STDLIB` can select an
explicit valid tree. An invalid higher-priority source fails instead of silently falling back.

Standard-library source passes through the same parser, linking, semantic analysis,
monomorphization, HIR, MIR, and JIT pipeline as project code. Compiler-known nominal types and host
intrinsics still resolve through official `aster.*` symbols rather than text or layout matching.

## Namespaces

- `aster.core` defines [`Option<T>`, `Result<T, E>`](option-result.md), and the explicit
  incremental [`StringBuilder`](strings.md#incremental-construction).
- `aster.io` provides [terminal and host-managed filesystem operations](io.md).
- `aster.math` provides scalar [`Abs`, `Min`, `Max`, `Clamp`, rounding, power, root, and trigonometric](math.md) overloads.
- `aster.text` provides ordinal immutable-text helpers documented with [strings](strings.md).
- `aster.collections` defines the official
  [`DictionaryEntry<K, V>` snapshot value](collections.md#dictionaryk-v).

`List<T>`, `Dictionary<K, V>`, `Task<T>`, and `Parallel` are official nominal surfaces recognized
by the compiler. Their executable operations are described in
[Collections](collections.md) and [Concurrency](concurrency.md).

The `aster.*` prefix is reserved. Project source cannot shadow an official namespace or replace an
official type with a class that has the same short name or layout.

The library does not contain a package registry, dependency resolver, engine API, GPU API, or
general shared-memory threading surface.
