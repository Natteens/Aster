# Roadmap

Aster is still at `0.0.0`. The current work is about making a coherent language and toolchain,
not promising a production release date. The [technical roadmap](technical-roadmap.md) records
dependency order and deeper compiler milestones; this page describes the public direction.

## Available now

Aster has an end-to-end native JIT pipeline: source is parsed and linked, generic uses are
monomorphized, semantic analysis produces typed HIR, HIR lowers to control-flow MIR, and Cranelift
executes the result.

Programs can use concrete primitive and user-defined types, functions and methods, deterministic
overloads, arrays, classes, structs, interfaces, enums, properties, namespaces, multifile projects,
generic functions and types, `Option<T>`, `Result<T, E>`, and postfix `?`. The CLI checks, runs,
watches, and exposes HIR/MIR dumps for compiler development.

This is an experimental JIT environment. Classes, arrays, and strings live for one execution;
structs and enums are values. The standard library is embedded and intentionally small.

## Near-term language work

The next language work should close gaps rather than add disconnected syntax:

- settle long-lived ownership and the boundary for unsafe or foreign code;
- make unsupported type features explicit, including constraints and richer generic relationships;
- improve pattern-oriented enum handling without weakening exhaustive checking;
- define string and collection growth beyond the current immutable strings and fixed arrays;
- keep diagnostics, examples, and reference documentation aligned with executable behavior.

No item is considered complete merely because it parses. It must preserve concrete types through
semantics, HIR, MIR, execution, diagnostics, and tests.

## Tooling and distribution

The CLI is installable locally, but Aster has no package manager, dependency registry, standalone
binary output, or stable ABI. Future distribution work includes deciding how projects identify
their root source, how the standard library is packaged, and whether AOT/object generation belongs
in the first stable toolchain.

## Research, not current features

Automatic parallelism, explicit task APIs, GPU execution, HVM integration, hot reload, and optional
engine-oriented libraries are research areas. None is implemented today.

Any future concurrency or parallel execution model must prioritize safety, determinism, and
understandable scheduling and synchronization costs. Aster may study systems such as Bend and HVM,
but it has no commitment to copy their architecture. ECS and engine lifecycle conventions remain
outside the core language unless a future proposal demonstrates a clear, optional boundary.

## Toward `0.1.0`

The first planned release will be scoped from language and JIT feedback. It should represent a
documented, testable baseline rather than a claim of production readiness. Source compatibility,
supported platforms, memory guarantees, and distribution expectations must be stated explicitly
before that release is considered stable.
