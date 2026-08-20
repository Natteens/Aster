# Roadmap

ASTER develops in vertical slices. A language feature is complete only when syntax, semantic
validation, HIR, MIR, the Cranelift JIT, diagnostics, tests, and a usable example agree on its
behavior.

This page describes public direction rather than a release schedule.

## Available now

The current toolchain includes:

- static nominal types, classes, structs, interfaces, enums, and concrete generic specialization;
- arrays, `List<T>`, `Dictionary<K, V>`, `Option<T>`, `Result<T, E>`, and compiler-known `foreach`;
- immutable UTF-8 strings with Unicode-scalar `char`, scalar-counting `Length`, and `foreach` iteration;
- practical ordinal string helpers, floating-point math helpers, and collection mutation/snapshot APIs;
- region-based temporary and persistent allocation with conservative escape analysis, AARM fine
  reclaim, and compiler-proven long-lived owned-region reclamation for fresh return-only values;
- terminal and host-managed filesystem operations;
- multifile projects with `Aster.toml` and conventional `Main`;
- deterministic path dependencies and public HTTPS Git dependencies pinned by `Aster.lock`;
- restricted `Task<T>`, `await`, and `Parallel` operations with checked worker boundaries;
- deterministic typed-MIR optimization for CFG cleanup, primitive constants, scalar copies, and dead
  pure assignments, before escape and ownership analysis;
- checking, JIT execution, watch mode, installation diagnostics, and HIR/MIR inspection;
- official Windows x64 and Linux x64 release archives, installers, repair/update/rollback, and
  uninstallers.

## Toward 1.0

The work before 1.0 is about closing explicit boundaries:

- extend ownership evidence only where useful shared/CFG-spanning shapes can remain deterministic,
  and define any safe foreign-function or unsafe-code boundary;
- improve diagnostics and reference coverage as the executable subset grows;
- finish type-system decisions such as generic constraints and richer pattern handling;
- apply the documented [pre-1.0 compatibility policy](reference/compatibility.md) as source,
  project, standard-library, CLI, and runtime contracts evolve;
- extend compiler optimization only where representative structural and runtime evidence justifies it;
- keep the worker model deterministic and explicit as concurrency support develops;
- keep package resolution explicit and reproducible as future sources are evaluated.

No item is complete merely because it parses. Unsupported layouts and operations must continue to
fail before unsafe execution.

## Not available yet

ASTER does not currently provide:

- a package manager or dependency registry;
- standalone AOT executables or a stable native ABI;
- official macOS or ARM distributions;
- general shared-memory threads or implicit automatic parallelization;
- a language server, semantic editor features, or a formatter;
- GPU compilation, HVM lowering, or an engine lifecycle.

## Research

Hot reload, GPU targets, HVM-related execution models, and an optional ECS/engine layer remain
research. Research notes live in [`docs/research`](research/) so they cannot be mistaken for
compiler documentation or scheduled features.

Any future work in these areas must preserve concrete types, deterministic failure, explicit
transfer and ownership rules, and understandable runtime costs.
