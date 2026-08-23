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
- `aster.math` provides [practical scalar math](math.md), including constants, classification,
  logarithms, trigonometry, and interpolation.
- `aster.random` provides the explicitly seeded, cross-platform deterministic
  [`Random`](random.md) generator.
- `aster.time` provides operation-scoped [monotonic and Unix clock reads](time.md).
- `aster.text` provides ordinal immutable-text helpers documented with [strings](strings.md).
- `aster.collections` defines the official
  [`DictionaryEntry<K, V>` snapshot value](collections.md#dictionaryk-v).
- `aster.testing` provides the compact [`Assert`](testing.md#assertions) surface used by
  root-package tests.

`List<T>`, `Dictionary<K, V>`, `Task<T>`, and `Parallel` are official nominal surfaces recognized
by the compiler. Their executable operations are described in
[Collections](collections.md) and [Concurrency](concurrency.md).

Primitive text conversion is locale-independent. Every scalar has `ToString()`; `string` provides
strict `TryParseBool`, `TryParseChar`, `TryParseSByte`, `TryParseByte`, `TryParseShort`,
`TryParseUShort`, `TryParseInt`, `TryParseUInt`, `TryParseLong`, `TryParseULong`, `TryParseFloat`,
and `TryParseDouble`, each returning `Option<T>` rather than failing execution for invalid text.

The `aster.*` prefix is reserved. Project source cannot shadow an official namespace or replace an
official type with a class that has the same short name or layout.

The library does not contain a package registry, dependency resolver, engine API, GPU API, or
general shared-memory threading surface.
