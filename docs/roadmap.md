# Roadmap

The dependency order and definition of done for the next major phases live in
[`technical-roadmap.md`](technical-roadmap.md). The short roadmap below records the currently
implemented compiler surface and immediate gaps.

## Bootstrap (`0.0.0`)

Implemented:

- Stable Rust Cargo workspace, CLI, source diagnostics, CI, and automatic releases.
- Hand-written lexer with positioned keywords, literals, punctuation, comments, and operators.
- Modular AST for general declarations, statements, and expressions.
- Recursive-descent parser for classes, structs, interfaces, fields, namespace-level functions,
  methods, parameters, local/namespace variables, constants, blocks, returns, calls, member access, unary/binary
  expressions, and right-associative assignment.
- Control-flow parsing and validation for `if`/`else if`/`else`, `while`, C-style `for`, `break`,
  and `continue`, with boolean conditions, lexical block scopes, and a loop-local `for` scope.
- All-reachable-path return analysis and warning diagnostics for code after `return`, `break`, or `continue`.
- Initial semantic validation for symbols, basic/user types, calls, argument types/counts, returns,
  mutability, constants, visibility, interface restrictions, and the standard logging surface.
- A separate typed HIR with resolved symbol references, normalized visibility, and lowering for the
  implemented general frontend.
- Development-only `aster dump-hir FILE` inspection without execution.
- A minimal typed MIR for functions, parameters, source locals, temporaries, constants, arithmetic,
  comparisons, assignments, resolved calls, basic blocks, branches, jumps, returns, and `void` endings.
- HIR-to-MIR lowering that turns `if`/`else`, `while`, and `for` into explicit control-flow graphs.
- Development-only `aster dump-mir FILE` inspection without execution.
- Isolated Cranelift `0.121.2` JIT backend consuming only validated MIR.
- Conventional application execution through one public static parameterless `Main` returning
  `void` or `int`, with an optional `Aster.toml` entry and preserved explicit
  `aster run FILE --function NAME` development mode.
- Initial native execution for `int`, `bool`, `void`, direct calls, locals, assignments, arithmetic,
  comparisons, boolean operations, branches, loops, `break`, `continue`, and returns.

## Productivity, runtime, and standard-library foundation (current)

Implemented on top of the bootstrap:

- Prefix/postfix `++`/`--` on mutable numeric variables and the right-associative conditional
  expression `?:`, end to end through lexer, parser, AST, semantics, HIR, MIR, and Cranelift.
- Short-circuiting `&&`/`||` lowered as control flow, so right operands evaluate only when needed.
- Parser error recovery at statement, member, and declaration boundaries with recovery tests.
- The `aster-runtime` crate: the execution boundary between JIT code and host services, a
  documented immutable UTF-8 string ABI (`docs/compiler/runtime-abi.md`), and a central registry of
  runtime functions consumed generically by the backend.
- Executable immutable strings with literals, locals, parameters, returns, content equality,
  dynamic `+`/`+=`, Unicode-scalar `Length`, context-owned allocation, `String.IsEmpty`, and the
  standard `Log`/`Log.Warning`/`Log.Error` surface through the runtime ABI.
- Primitive types executable end to end: `sbyte`, `byte`, `short`, `ushort`, `int`, `uint`,
  `long`, `ulong`, `float`, `double`, `bool`, `char`, `string`, and `void`; range-based
  integer literals, `L`/`U`/`UL`/`f`/`d` suffixes, exact implicit conversions, explicit
  `(type)value` casts, and documented promotion/overflow behavior (see
  `docs/specification/02-types.md`).
- `decimal` syntax, type checking, HIR and MIR preservation with `m`/`M` literals. It is not
  represented as `double`: the JIT rejects it until a dedicated runtime layout and ABI exist.
- Backend-neutral primitive metadata and conversion/promotion rules in `aster-types`, shared
  by semantic analysis and the backend instead of duplicated tables.
- Executable structs with named fields, natural layout, nesting, field reads/writes, value
  copies, parameters and internal aggregate returns. Struct methods remain frontend-only.
- A host-owned ExecutionContext created fresh for every JIT invocation and discarded afterwards.
  Fixed-length arrays are executable for scalars and structs, with shared reference identity,
  zeroed allocation, checked indexing, immutable `Length`, parameters and internal returns.
- Executable classes with one constructor, context-owned allocation, non-null reference identity,
  fields, instance methods, implicit receivers, explicit `this`, parameters and internal returns.
  Constructor analysis requires every non-zero-safe reference field on all continuing paths.
- Executable nominal interfaces for classes using `class Type : IContract`, exact public method
  conformance, safe class-to-interface conversion and real JIT dispatch. Interface references work
  in locals, parameters, fields, arrays and internal returns without adding class inheritance.
- Executable generic namespace-level functions specialized by concrete type before HIR. Calls support
  argument inference or explicit `<Type>` lists, reuse identical specializations, cross local
  namespace boundaries, and work with executable scalar, struct, array, class and interface types.
- Executable generic classes, structs and interfaces specialized before semantic HIR. Closed types
  have concrete layouts and symbols, reuse a structural specialization cache, work across
  namespaces, and preserve value, reference and interface-dispatch semantics.
- Executable static methods, declaration-order class field initializers, explicit getter/setter
  properties, deterministic function/method overloads, and equality by documented value or
  reference-identity rules.
- Compile-time constant expressions with a single shared evaluator: folding, overflow and
  division-by-zero diagnostics, constant references, and executable namespace-level constants.
- Consolidated namespace-level functions including direct and mutual recursion.
- Directory-inferred namespaces with optional `namespace`, explicit `using`, deterministic
  multi-file discovery, project-wide `internal` visibility, ambiguity/cycle diagnostics, migration
  errors for `module`/`import`, multifile JIT execution, and dependency-aware watch rebuilds.
- The first official read-only standard-library namespace, `aster.math`, loaded from the compiler
  distribution and executed through the normal frontend and JIT. It provides scalar `Abs`, `Min`,
  `Max`, and `Clamp` overloads with controlled domain failures.
- Value enums with optional payloads, exhaustive non-fallthrough `switch`, and the ordinary
  standard-library enums `aster.core.Option<T>` and `Result<T, E>`.
- Postfix `?` propagation for `aster.core.Result<T, E>`: forwards the `Error`
  case and continues with the `Ok` payload, with exact error-type matching and no
  automatic conversion (`docs/reference/result-propagation.md`).
- `aster watch FILE [--function NAME]`: debounced recompile-and-restart with per-rebuild JIT
  session cleanup (`docs/compiler/watch.md`), plus the future hot reload design in
  `docs/compiler/hot-reload-foundation.md`.
- An initial declarative VS Code extension in `editors/vscode`.

## Next frontend phases

Not yet implemented:

- General pattern matching, switch expressions/guards, exceptions, `goto`, slices/resizable or nested arrays, auto-properties,
  static fields, constructor overloads, named/default arguments, and operator overloads.
- Generic methods with their own parameters, constraints, variance, generic inheritance, generic
  static members, class inheritance, interface inheritance, and default interface methods.
- Long-lived/AOT class ownership, `null`, finalization and independent object destruction remain
  open; the JIT arena intentionally defines only per-execution lifetime.
- Package/dependency manifests, external dependencies, aliases, selective usings, or reexports.
- Ownership, borrowing, class memory management, unsafe/FFI, and concurrency semantics.
- Final ECS/query/schedule/resource/event syntax or any ECS runtime behavior.
- MIR analyses and optimization passes, AOT/object generation, executable linking, and
  general program execution.
- String interpolation, indexing, conversion APIs, and mutable string buffers.
- Executable decimal arithmetic and conversions (requires decimal runtime layout, operations,
  overflow policy, and ABI).
- Named/default arguments and constructor/operator overloads.

## First planned release (`0.1.0`)

Scope will be stabilized from frontend and JIT feedback. MIR optimization, AOT generation, linking,
stable platform ABI support, ECS runtime, scheduling, and game-engine lifecycle remain future work and
must not be inferred from this explicit-function JIT experiment.
