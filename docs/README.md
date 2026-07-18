# Aster documentation

This index separates guides to the implemented language from API reference, design specifications,
compiler internals, and future research. Documents under `reference/` describe current behavior.
Documents under `specification/` may also record proposals and open questions; they are not proof
that a feature is implemented.

## Getting Started

- [Getting started](getting-started.md) — install Rust, run an application, use namespaces, and try
  the standard library.
- [Language tour](language-tour.md) — a compact walkthrough of the implemented language.
- [CLI reference](reference/cli.md) — `check`, `run`, `watch`, `dump-hir`, and `dump-mir`.
- [VS Code extension](../editors/vscode/README.md) — syntax highlighting, snippets, and local
  installation.

## Language Guide

- [Namespaces and usings](reference/namespaces.md)
- [Application entry points](reference/application-entry.md)
- [Classes](reference/classes.md), [structs](reference/structs.md), and
  [interfaces](reference/interfaces.md)
- [Enums and switch](reference/enums.md)
- [Generic functions](reference/generics.md) and [generic types](reference/generic-types.md)
- [Arrays](reference/arrays.md), [strings](reference/strings.md), and
  [primitive types](reference/types.md)
- [Internal modules and public namespaces](reference/modules.md) — terminology and migration from
  the removed `module`/`import` syntax.

## Language Reference

The [implemented grammar](compiler/grammar.md) is the concise reference for syntax currently
accepted by the compiler. The detailed design specification is organized as follows:

- [Goals and status](specification/00-goals.md),
  [lexical structure](specification/01-lexical-structure.md), and
  [types](specification/02-types.md)
- [Variables and constants](specification/03-variables-and-constants.md),
  [expressions](specification/04-expressions.md), and
  [functions](specification/05-functions.md)
- [Control flow](specification/06-control-flow.md),
  [structs and type categories](specification/07-structs.md), and
  [namespaces](specification/08-modules.md)
- [Memory model](specification/09-memory-model.md),
  [open questions](specification/10-open-questions.md), and
  [generics](specification/11-generics.md)
- [Logging](specification/12-logging.md),
  [platform boundaries](specification/14-platform-boundaries.md),
  [visibility](specification/15-visibility.md), and
  [enums](specification/16-enums.md)

## Standard Library

- [Standard-library overview](reference/standard-library.md)
- [`Option<T>` and `Result<T, E>`](reference/option-result.md)
- [Postfix `?` and result propagation](reference/result-propagation.md)
- [Strings and `aster.text`](reference/strings.md)
- [`aster.math`](reference/math.md)
- [Logging](reference/logging.md)

## Compiler Architecture

- [Compiler architecture](compiler/architecture.md) — workspace boundaries and the complete
  frontend-to-JIT pipeline.
- [Compiler development](compiler/development.md) and [runtime ABI](compiler/runtime-abi.md)
- [Module resolution](compiler/module-resolution.md) and
  [monomorphization](compiler/monomorphization.md)
- [Execution context](compiler/execution-context.md) and [watch mode](compiler/watch.md)
- [Inspecting HIR](compiler/dump-hir.md) and [inspecting MIR](compiler/dump-mir.md)
- [Hot-reload foundation](compiler/hot-reload-foundation.md) and
  [GPU/engine direction](compiler/gpu-engine-direction.md) — future architecture research, not
  implemented functionality.

## Development and Releases

- [Contributing](../CONTRIBUTING.md)
- [Compiler development](compiler/development.md)
- [Release process](releasing.md)
- [Developing the VS Code extension](../editors/vscode/DEVELOPMENT.md)

## Roadmap

- [Current roadmap](roadmap.md) — implemented surface and immediate gaps.
- [Technical roadmap](technical-roadmap.md) — dependency order and research milestones.
- [Future ECS package direction](future/ecs-package.md) — records the removed experiment and the
  constraints on any future proposal.
