# ASTER documentation

Guides teach through working programs. Reference pages describe behavior accepted by the current
compiler. Compiler documents cover implementation. Specifications and research notes may discuss
work that is not implemented.

| Area | Use it for |
| --- | --- |
| 🚀 **Guides** | Install ASTER, create a project, and get to a working program quickly. |
| 📚 **Reference** | Check behavior supported by the current compiler, runtime, CLI, and standard library. |
| ⚙️ **Compiler internals** | Follow linking, specialization, HIR, MIR, runtime, and backend architecture. |
| 🧪 **Specification & research** | Explore design decisions and proposals without treating them as implemented behavior. |

## Start here

1. 🚀 [Install ASTER and create a project](getting-started.md).
2. 🧪 Follow the [runnable examples](../examples/README.md).
3. 🧭 Read the [language tour](language-tour.md).
4. 🛠️ Keep the [CLI reference](reference/cli.md) nearby.

## Language reference

- 🚪 [Application entry points](reference/application-entry.md)
- 🗂️ [Namespaces and `using`](reference/namespaces.md)
- 📦 [Packages and path/Git dependencies](reference/packages.md)
- 🔤 [Primitive types](reference/types.md), [strings](reference/strings.md), and
  [arrays](reference/arrays.md)
- 📦 [`List<T>` and `Dictionary<K,V>`](reference/collections.md)
- 🧱 [Classes](reference/classes.md), [structs](reference/structs.md), and
  [interfaces](reference/interfaces.md)
- 🧩 [Enums](reference/enums.md), [`Option<T>` and `Result<T, E>`](reference/option-result.md), and
  [postfix `?`](reference/result-propagation.md)
- 🧬 [Generic functions](reference/generics.md) and [generic types](reference/generic-types.md)
- 🧰 [Standard library](reference/standard-library.md), [filesystem and terminal I/O](reference/io.md),
  [`aster.math`](reference/math.md), [deterministic random](reference/random.md),
  [clock reads](reference/time.md), and [logging](reference/logging.md)
- ⚙️ [Tasks, parallel operations, and worker boundaries](reference/concurrency.md)
- 🔌 [Minimal native FFI and lexical unsafe boundary](reference/native-ffi.md)
- 🧪 [Package tests and assertions](reference/testing.md)
- 🧭 [Compatibility before 1.0](reference/compatibility.md)
- 📐 [Implemented grammar](compiler/grammar.md)

These pages use the present tense only for behavior accepted by the current compiler and JIT.

## Design and direction

- 🎯 [Design goals](specification/00-goals.md)
- 📋 [Language specification and decision records](specification/)
- 🧭 [Roadmap](roadmap.md)
- 🧪 [Research notes](research/)

Proposals and research notes are not proof of implementation.

## Compiler internals

- 🏗️ [Architecture and crate boundaries](compiler/architecture.md)
- 🛠️ [Compiler development](compiler/development.md)
- 🔗 [Project linking](compiler/module-resolution.md)
- 🧬 [Monomorphization](compiler/monomorphization.md)
- 🧠 [Memory management](compiler/memory-management.md)
- ⚙️ [Execution context](compiler/execution-context.md) and [runtime ABI](compiler/runtime-abi.md)
- 🔎 [HIR dump](compiler/dump-hir.md), [MIR dump](compiler/dump-mir.md), and
  [watch mode](compiler/watch.md)

## Contributing and releases

- 🤝 [Contributing](../CONTRIBUTING.md)
- ✍️ [Writing documentation](contributing/writing.md)
- 🚢 [Release process](releasing.md)
- 🧩 [VS Code extension development](../editors/vscode/DEVELOPMENT.md)
