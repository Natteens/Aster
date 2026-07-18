# 14 — Language, standard library, runtime, and SDK

## Objective

Prevent features from being silently promoted into syntax or hidden runtime behavior by defining
the responsibilities of Aster’s product layers.

## Accepted boundaries

### Language

The language is the source grammar, type system, semantic rules, memory-safety model, and required
compile-time diagnostics. `class`, `struct`, `interface`, variables, functions, and control flow
belong here. Proposed ECS keywords belong here only if ultimately accepted as syntax.

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

The initial `aster.math` implementation is ordinary Aster source except for a typed runtime-error
bridge used by invalid `Abs` and `Clamp` inputs. That bridge records a controlled error in the
per-run ExecutionContext; it is metadata supplied by the trusted standard-library provider, not a
keyword or textual method-name check in the backend.

### SDK

The SDK is the developer distribution: compiler, CLI, formatter, documentation, standard-library
sources/artifacts, target support, package/build tools, and debugging integration. SDK tools do not
change program semantics without a corresponding language or library specification.

### Optional `aster.ecs`

ECS is an optional module/library plus any documented runtime scheduler support. It is neither an
implicit game engine nor a requirement for ordinary Aster programs.

## Proposed syntax and ownership examples

```aster
int count = 1;                 // language
Log.Warning("Low count");      // standard library
using aster.ecs;               // optional library namespace
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

Importing or using ECS does not create an implicit program entry point or engine lifecycle.

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

**Recommendation:** PROPOSED — distribute `aster.ecs` as an official optional SDK package, with narrowly
specified runtime hooks only where measurements prove them useful.

### PROPOSED — Compiler knowledge of library features

1. **No library-specific compiler behavior** — clean layering; may prevent desired static ECS analysis.
2. **Recognized attributes/intrinsics** — controlled optimization and validation; creates privileged APIs.
3. **Dedicated syntax lowering to library/runtime contracts** — best diagnostics; strongest coupling.

**Recommendation:** PROPOSED — permit a small, documented intrinsic/metadata boundary. Accept dedicated
ECS syntax only if it materially improves safety or clarity beyond a library API.
