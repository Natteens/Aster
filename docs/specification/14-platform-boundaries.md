# 14 — Language, standard library, runtime, and SDK

## Objective

Prevent features from being silently promoted into syntax or hidden runtime behavior by defining
the responsibilities of ASTER’s product layers.

## Accepted boundaries

### Language

The language is the source grammar, type system, semantic rules, memory-safety model, and required
compile-time diagnostics. `class`, `struct`, `interface`, variables, functions, and control flow
belong here. Compiler-known official types and operations have explicit typed HIR/MIR boundaries;
they are never recognized by short name or layout in the backend. No ECS keywords are accepted or
scheduled.

### Standard library

The standard library provides portable APIs implemented with ordinary language concepts where
possible. `Log`, `Log.Warning`, `Log.Error`, the scalar `aster.math` API, and the ordinary
`aster.core` definitions of `Option<T>` and `Result<T, E>` belong here. Official
library namespaces use the reserved `aster.*` prefix and are distributed read-only with the SDK.
Collections, text, I/O, and explicit shared-ownership types are expected candidates, not accepted
by this statement.

### Runtime

The runtime is the minimum target-side support required by compiled programs: allocation hooks,
panic/termination machinery, platform abstractions, scheduler services, or logging sinks when those
cannot be compiled away. A feature is not automatically runtime-backed merely because that is easy
to implement.

The initial `aster.math` implementation is ordinary ASTER source except for a typed runtime-error
bridge used by invalid `Abs` and `Clamp` inputs. That bridge records a controlled error in the
per-run ExecutionContext; it is metadata supplied by the trusted standard-library provider, not a
keyword or textual method-name check in the backend.

### SDK

The SDK is the developer distribution: compiler, CLI, formatter, documentation, standard-library
sources/artifacts, target support, package/build tools, and debugging integration. SDK tools do not
change program semantics without a corresponding language or library specification.

### Potential optional ECS package

ECS is only a research proposal for a possible future library and documented runtime hooks. It is
not present in the compiler or SDK, and it is neither an implicit game engine nor a requirement for
ordinary ASTER programs.

## Proposed syntax and ownership examples

```aster
int count = 1;                 // language
Log.Warning("Low count");      // standard library
```

The allocator or scheduler implementation behind an operation may involve the runtime. Building,
formatting, and packaging the file are SDK responsibilities.

## Valid design example

```aster
public class Greeter
{
    public void Greet(string name)
    {
        Log("Olá, " + name);
    }
}
```

This uses a normal standard-library API; `Log` is not parsed as a keyword.

## Invalid design examples

```aster
log "Hello";                  // no logging statement exists
```

No library import creates an implicit program entry point or engine lifecycle.

## OPEN QUESTIONS

### PROPOSED — Runtime size and linkage

1. **Statically link a minimal runtime** — self-contained executables and optimization; larger binaries.
2. **Dynamically link a shared runtime** — smaller updates/binaries; deployment and ABI coupling.
3. **Choose per target/profile** — flexible; makes deployment behavior less uniform.

**Recommendation:** PROPOSED — static linkage by default for self-contained native programs, with an
explicit dynamic option only after a stable runtime ABI exists.

### PROPOSED — Standard-library versioning

1. **Lock it to the compiler version** — coherent compatibility; library fixes require compiler releases.
2. **Version it independently** — flexible updates; needs a compatibility matrix.
3. **Lock core modules, version optional modules independently** — balanced; more release machinery.

**Recommendation:** PROPOSED — lock the core standard library to the language/SDK release initially;
allow optional packages such as higher-level libraries to version independently later.

### PROPOSED — ECS distribution

1. **Ship `aster.ecs` inside the standard library** — discoverable and version-aligned; expands the
   mandatory standard surface.
2. **Ship it as an official SDK package** — optional and independently evolvable; package tooling is required.
3. **Build it into the runtime** — easy scheduler integration; couples all programs to ECS machinery.

**Recommendation:** PROPOSED — if this research is resumed, prefer an official optional SDK package
with narrowly specified runtime hooks only where measurements prove them useful. No package or
implementation schedule is currently accepted.

### ACCEPTED — Compiler knowledge of official types

ASTER uses a small compiler-known boundary for official nominal types and host services whose
representation or validation cannot be expressed as ordinary source today. This includes
collections, I/O, tasks, and parallel operations. Semantic analysis resolves an official symbol to
typed HIR/MIR metadata; the backend never performs source-name or duck-typing lookup.

New privileged behavior still requires a concrete representation, diagnostics, and runtime
contract. Future ECS research does not gain compiler integration merely by being a library idea.

### ACCEPTED — Initial foreign-function boundary

No user-facing FFI syntax is accepted or implemented yet. When ASTER first
exposes foreign calls, they must be explicitly unsafe and target only
host-registered C-ABI functions. The initial source-level signature surface is
`void` plus fixed-width scalars: `bool`/`sbyte`/`byte` (8-bit),
`short`/`ushort` (16-bit), `char`/`int`/`uint` (32-bit, with `char` a validated
Unicode scalar), `long`/`ulong` (64-bit), `float` (binary32), and `double`
(binary64). `decimal` is excluded until it has an executable representation.

The C mapping is exact: `bool` is `uint8_t` (`0` or `1`); signed and unsigned
integer widths map to their matching fixed-width C integer; `char` maps to a
validated `uint32_t`; and floating values map to `float`/`double`. A registered
binding uses a separate C status result (`0` for success, non-zero for a
controlled ASTER runtime error); any scalar result is written only on success
by the host wrapper. This wrapper detail is not an ASTER pointer surface.

There is no initial ABI for strings, arrays or buffers, objects, interfaces,
collections, `Option`, `Result`, pointers, callbacks, retained ASTER
references, worker/context transfer, dynamic-library loading, or symbol lookup.
Nullability is therefore not applicable. Host wrappers own their storage; an
ASTER caller receives only scalar values or `void` and never frees foreign
memory. A non-zero native status must become a controlled ASTER runtime error;
native panics or unwinds must not cross `extern "C"`.

The exact `unsafe` and foreign-declaration syntax remains deliberately
undefined. It requires a separate language decision; no backend may infer this
boundary from textual type names.
