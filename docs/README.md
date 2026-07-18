# Aster documentation

Choose a path based on what you are trying to do. Guides teach through working programs; reference
pages define current behavior; compiler documents explain implementation. Design specifications and
research notes may describe work that the compiler does not implement.

## Start using Aster

1. [Getting started](getting-started.md) — install the CLI and run a first application.
2. [Runnable examples](../examples/README.md) — follow a short sequence of complete programs.
3. [Language tour](language-tour.md) — understand the ideas that connect those programs.
4. [CLI reference](reference/cli.md) — check, run, watch, and inspect IR.

## Language guides and reference

- [Application entry points](reference/application-entry.md)
- [Namespaces and usings](reference/namespaces.md)
- [Primitive types](reference/types.md), [arrays](reference/arrays.md), and
  [strings](reference/strings.md)
- [Classes](reference/classes.md), [structs](reference/structs.md), and
  [interfaces](reference/interfaces.md)
- [Enums](reference/enums.md), [`Option<T>` and `Result<T, E>`](reference/option-result.md), and
  [postfix `?`](reference/result-propagation.md)
- [Generic functions](reference/generics.md) and [generic types](reference/generic-types.md)
- [Standard library](reference/standard-library.md), [`aster.math`](reference/math.md), and
  [logging](reference/logging.md)
- [Implemented grammar](compiler/grammar.md)

These pages use the present tense for behavior accepted by the current compiler.

## Design and direction

- [Design goals](specification/00-goals.md) — the principles behind language decisions.
- [Design specification](specification/) — accepted rules, proposals, and open questions. A proposal
  is not proof of implementation.
- [Roadmap](roadmap.md) — public direction and current boundaries.
- [Technical roadmap](technical-roadmap.md) — dependency order for compiler work and research.

## Compiler architecture

- [Compiler architecture](compiler/architecture.md)
- [Project linking](compiler/module-resolution.md) and
  [monomorphization](compiler/monomorphization.md)
- [Execution context](compiler/execution-context.md) and [runtime ABI](compiler/runtime-abi.md)
- [HIR dump](compiler/dump-hir.md), [MIR dump](compiler/dump-mir.md), and
  [watch mode](compiler/watch.md)
- [Hot-reload foundation](compiler/hot-reload-foundation.md) and
  [GPU/engine direction](compiler/gpu-engine-direction.md) — research, not implemented features.

## Contributing and releases

- [Contributing](../CONTRIBUTING.md)
- [Compiler development](compiler/development.md)
- [Writing Aster documentation](contributing/writing.md)
- [Release process](releasing.md)
- [VS Code extension development](../editors/vscode/DEVELOPMENT.md)

The official repository is [Natteens/Aster](https://github.com/Natteens/Aster). See the
[project identity policy](../TRADEMARKS.md) for clear attribution of forks and derived work.
