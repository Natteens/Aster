# Standard library extension contract

This page describes how the official `aster.*` standard library is organized and loaded, how a
placeholder declaration becomes a compiler-known intrinsic, and the rule for deciding whether a new
API belongs in ordinary ASTER or needs compiler and runtime support. It documents the current
implementation as of release 0.67.3; it proposes no redesign and no plugin interface.

Related pages: [architecture](architecture.md), [runtime ABI](runtime-abi.md),
[memory management](memory-management.md), [module resolution](module-resolution.md), and the
user-facing [standard library](../reference/standard-library.md) reference.

## 1. Standard library architecture

### Namespaces and sources

The standard library is ordinary ASTER source under `stdlib/aster/`. It is compiled by the same
lexer, parser, linker, monomorphizer, semantic analysis, HIR, MIR, optimizer, and Cranelift backend
as project code. There is no separate stdlib compiler.

Exactly eight modules are registered, each one source file:

| Namespace | File |
| --- | --- |
| `aster.math` | `stdlib/aster/math.aster` |
| `aster.text` | `stdlib/aster/text/text.aster` |
| `aster.core` | `stdlib/aster/core/core.aster` |
| `aster.io` | `stdlib/aster/io/io.aster` |
| `aster.collections` | `stdlib/aster/collections/collections.aster` |
| `aster.testing` | `stdlib/aster/testing/testing.aster` |
| `aster.random` | `stdlib/aster/random/random.aster` |
| `aster.time` | `stdlib/aster/time/time.aster` |

The registry is the `STDLIB_MODULES` table and the matching `include_str!` constants in
`crates/aster-compiler/src/standard_library.rs`.

### `StandardLibrary`

`StandardLibrary` (same file) is a map from namespace name to source text. It has two constructors:

- `StandardLibrary::embedded()` uses the copies compiled into the binary with `include_str!`.
- `StandardLibrary::from_path(root)` reads every registered file from a directory laid out as
  `<root>/aster/...`. It fails if the path is missing, is a file, or lacks any of the eight files. It
  validates presence only, not content.

The CLI selects a source in this order (`crates/aster-cli/src/stdlib_discovery.rs`): the
`ASTER_STDLIB` environment variable, then `<exe-dir>/../stdlib/`, then the embedded copy. An invalid
higher-priority source is an error; it never silently falls back. `aster doctor` validates a managed
installation's `stdlib` directory with `from_path`, and `scripts/bundle.mjs` has its own
`REQUIRED_STDLIB_MODULES` list that decides which files a release archive must contain.

### Loading and reserved names

A standard-library namespace is loaded when source names it with `using` (`load_namespace` in
`crates/aster-compiler/src/project.rs`). Any name equal to `aster` or beginning with `aster.` is
treated as official (`is_official_name`). The loader enforces:

- Project source cannot declare a namespace in the `aster.*` space; it is rejected as a reserved
  namespace (covered by `project_cannot_declare_or_shadow_aster_namespaces` in
  `crates/aster-compiler/tests/project_modules.rs`).
- Standard-library files may use only other `aster.*` namespaces.
- A name that is official but not registered fails with "official standard library namespace
  `<name>` is unavailable".
- `internal` declarations in a standard-library file are not exported to other files, so a helper
  declared `internal` stays private to its own module.

The set of modules is therefore fixed by the registry. Dropping a new `.aster` file into the
stdlib directory does not make a new `aster.*` namespace available.

### Ordinary APIs versus compiler/runtime-backed operations

Most of what a program calls in the library is plain ASTER: classes, static methods, generic
functions, and `Option`/`Result` enums compiled like any other code. A second category is
implemented by the compiler and runtime and only *declared* in ASTER. There are two forms of that
second category:

1. **Bound intrinsics** — a declaration in `stdlib/aster` that `StandardLibrary::intrinsic_bindings`
   maps to an `Intrinsic` (section 3).
2. **Compiler-recognized surface** — operations the semantic analyzer recognizes by nominal type or
   member rather than through the binding table: `string` members such as `Length`, `Contains`,
   `IndexOf`, `Substring`, and `TryParse*`; scalar `ToString()`; `Log`; `StringBuilder` operations
   (identified by the constant `STRING_BUILDER_NAME`, `aster.core::StringBuilder`);
   `List<T>`, `Dictionary<K, V>`, `Task<T>`, `Parallel`; and postfix `?` on the official
   `Option`/`Result`. These are listed in the MIR `Intrinsic` enum but are not registered in
   `intrinsic_bindings`. See [Collections](../reference/collections.md) and
   [Concurrency](../reference/concurrency.md).

## 2. Extension decision rule

**Write a standard-library API in ASTER whenever its behavior can be expressed correctly, safely,
and adequately using existing language capabilities.**

An intrinsic or other compiler-known operation is justified only when the implementation needs
something ordinary ASTER cannot express:

1. Host operating-system effects or privileged runtime services (terminal, filesystem, clocks).
2. Runtime-owned representation, layout, allocation, or ownership contracts (strings, collections,
   `StringBuilder`, task state).
3. Controlled failure or execution-context integration (the runtime error channel).
4. Low-level behavior required by an established public contract (for example, the deterministic
   `Random` sequence).
5. A demonstrable performance need supported by representative benchmarks.

Performance alone is not a justification until it has been measured. The existing
[benchmarks](../../benchmarks/performance/README.md) and the `perf_baseline` harness are the
starting point. Nothing here implies that existing intrinsics should be removed; several have
legitimate runtime reasons, and replacing one needs the same evidence as adding one, plus proof
that the public behavior is unchanged.

## 3. Intrinsic binding mechanism

### From declaration to execution

1. **Declaration.** The library declares an ordinary function with a placeholder body that type
   checks, for example in `stdlib/aster/text/text.aster`:

   ```aster
   internal string __Trim(string value)
   {
       return "";
   }
   ```

   Semantic analysis validates it like any other function. Calls to the function are lowered to
   the intrinsic (step 4) rather than to the placeholder body, which only has to be valid ASTER of
   the declared type.

2. **Binding.** `StandardLibrary::intrinsic_bindings()` returns a `HashMap<String, Intrinsic>` keyed
   by the *linked, namespace-qualified name*, for example `"aster.text::__Trim"`. Entries are added
   only when the corresponding module is present in the library. The map is written out in
   `standard_library.rs`; nothing is read from `.aster` files, comments, attributes, or the
   function's visibility or name prefix.

3. **HIR.** `crates/aster-compiler/src/hir_lowering/declarations.rs` looks up each function's
   qualified name in that map and stores the result in `hir::Function::intrinsic`. The filesystem
   intrinsics carry a payload of concrete enum-case and field symbols (`FileIoResultLayout`) that
   only HIR lowering can resolve from the function's declared `Result<T, IOError>` return type
   (`resolve_intrinsic_payload`).

4. **MIR.** Calls to a function with an intrinsic are lowered by symbol, not by name
   (`lower_intrinsic_call` in `crates/aster-compiler/src/mir_lowering/calls.rs`), into
   `mir::Instruction::CallIntrinsic` with a MIR `Intrinsic`. The lowering for each intrinsic assumes
   the shape of the official declaration.

5. **Backend.** Cranelift validation checks the shape of every intrinsic call
   (`validate_intrinsic_shape` in `crates/aster-codegen-cranelift/src/validation.rs`: destination,
   return type, argument count and types) and fails closed on a mismatch. Code generation maps each
   intrinsic to a runtime export (`crates/aster-codegen-cranelift/src/calls.rs`, for example
   `StringTrim` to `aster_rt_string_trim`), and those exports are registered in
   `crates/aster-runtime/src/registry.rs` (see [runtime ABI](runtime-abi.md#registry)).

### What this means for contributors

- **Adding an ordinary function needs no intrinsic.** A new method or function written in ASTER is
  compiled and linked like project code. Nothing in the compiler enumerates stdlib functions.
- **A wrapper over an existing intrinsic is ordinary code.** `Clock.MonotonicMilliseconds()` in
  `aster.time` is a normal public method that calls the bound internal helper
  `__MonotonicMilliseconds()`; the public API can be reshaped without touching the compiler.
- **A new compiler-known operation is a cross-layer change.** It needs a new `Intrinsic` in the HIR
  and MIR enums, an entry in `intrinsic_bindings`, the HIR-to-MIR mapping and lowering arm, backend
  shape validation, a code-generation mapping, a runtime function and its registry entry, and tests
  at each layer. That is a compiler change, reviewed as one, not a library change.
- **There is no public mechanism to define intrinsics.** The binding table is compiled into the
  compiler. An external stdlib directory loaded with `ASTER_STDLIB` can supply different ASTER
  source for the registered modules, but it cannot add bindings, and placeholder signatures must
  stay identical to the official ones because lowering and validation depend on them.
- **The `__` prefix and `internal` are conventions.** Most placeholders are `internal` helpers named
  with a leading `__` and wrapped by a public API. `aster.io` is the exception: functions such as
  `Write`, `ReadLine`, `ReadAllText` are public and bound directly under their public names. Only
  the qualified-name lookup decides whether a function is an intrinsic.

## 4. Existing examples

### Ordinary ASTER implementations

`aster.collections.Array` (`stdlib/aster/collections/collections.aster`) is generic library code
with no binding:

- `Array.Copy<T>` builds a `List<T>`, calls `AddRange`, and returns `ToArray()`.
- `Array.Fill<T>` is a `for` loop assigning each element.
- `Array.Reverse<T>` swaps elements from both ends in a `while` loop.
- `Array.CopyRange<T>` validates the range, reports a failure through the bound
  `__CollectionRangeError()`, and copies forward or backward with ordinary loops.

Other ordinary code:

- `aster.text.String.IsEmpty`, `IndexOf`, and `LastIndexOf` wrap string members or a single bound
  helper and convert the result into `Option<int>`.
- `aster.io.CombinePath`, `ReadAllLines`, and `WriteAllLines` are written in ASTER over bound file
  primitives.
- `aster.testing.Assert.True`, `False`, and the `Equal` overloads are ASTER that call bound
  helpers only on failure.
- `aster.math.Math.Abs`, `Pi`, `Tau`, and `E` are plain expressions.
- `aster.random.Random` implements the SplitMix64 sequence in ASTER except for one step (below).

### Runtime- or compiler-backed operations

| Area | Example | Why it is not ordinary ASTER |
| --- | --- | --- |
| Terminal and filesystem | `aster.io.Write`, `ReadLine`, `ReadAllText`, `DeleteFile` | Host effects; filesystem results use the `Result<T, IOError>` layout resolved in HIR |
| Time | `aster.time.__MonotonicMilliseconds`, `__UnixMilliseconds` | Host clock reads |
| Strings, `StringBuilder`, collections | `StringBuilder`, `List<T>`, `Dictionary<K, V>` | Runtime-owned layout and arena allocation; see [memory management](memory-management.md) |
| Controlled runtime errors | `__CollectionRangeError`, `__AbsIntOverflow`, `__AssertionTrue` | Entry into the runtime error channel; there is no source-level `throw` |
| Math | `aster.math.__MathUnaryFloat`, `__MathPowDouble` | Floating-point library routines provided by the runtime |
| Random mixing | `aster.random.__RandomMix` | See below |
| Entropy seed | `aster.random.__RandomEntropySeed` | A fallible host read of the operating system's random source, used once per `Random.FromEntropy()` |

`aster.random.__RandomMix` implements the SplitMix64 finalizer. It became an intrinsic because the
language originally had no bitwise or shift operators. The language now has them (see the
[bitwise and shift decision record](bitwise-shift-decision.md)) and the function is expressible in
ASTER and verified against the intrinsic, but a controlled benchmark measured the ASTER version about
1.8 times slower on the `NextULong` and `NextUInt` hot paths, so the intrinsic is kept on
performance grounds (condition 5 of the extension rule, backed by a measurement).

## 5. Intrinsic inventory

### Registered bindings

`StandardLibrary::intrinsic_bindings` registers **45** entries at this commit, grouped by module.
Verify the count by listing the `"aster.<module>::<name>"` keys in
`crates/aster-compiler/src/standard_library.rs`; the older readiness audit's "about 44" predates the entropy seed.

| Group | Bindings | Count | Classification |
| --- | --- | --- | --- |
| Host effects: terminal | `aster.io::Write`, `WriteLine`, `ReadLine` | 3 | Inherently runtime-backed |
| Host effects: filesystem | `aster.io::ReadAllText`, `WriteAllText`, `AppendAllText`, `ListFiles`, `ListDirectories`, `FileExists`, `DirectoryExists`, `CreateDirectory`, `DeleteFile`, `DeleteDirectory` | 10 | Inherently runtime-backed |
| Host effects: clock | `aster.time::__MonotonicMilliseconds`, `__UnixMilliseconds` | 2 | Inherently runtime-backed |
| Host effects: entropy | `aster.random::__RandomEntropySeed` | 1 | Inherently runtime-backed (fallible OS random source) |
| Controlled runtime errors | `aster.math::__AbsIntOverflow`, `__AbsLongOverflow`, `__ClampInvalidRange`, `__MathSignNaN`; `aster.collections::__CollectionRangeError`; `aster.random::__RandomInvalidRange`; `aster.testing::__AssertionTrue`, `__AssertionFalse` | 8 | Inherently runtime-backed (error channel) |
| Assertion with message | `aster.testing::__AssertionEqual` | 1 | Inherently runtime-backed (error channel with expected and actual text) |
| Math operations | `aster.math::__MathUnaryFloat`, `__MathUnaryDouble`, `__MathBinaryFloat`, `__MathBinaryDouble`, `__MathPredicateFloat`, `__MathPredicateDouble`, `__MathPowFloat`, `__MathPowDouble` | 8 | Runtime-backed (floating-point library routines) |
| Random | `aster.random::__RandomMix` | 1 | Expressible in ASTER with the bitwise operators, but measured about 1.8 times slower on the `NextULong`/`NextUInt` hot paths, so it is kept |
| Text operations | `aster.text::__Trim`, `__TrimStart`, `__TrimEnd`, `__LastIndexOf`, `__Replace`, `__Split`, `__Repeat`, `__JoinArray`, `__ConcatArray`, `__ToChars`, `__FromChars` | 11 | Mixed; see below |

(3 + 10 + 2 + 1 + 8 + 1 + 8 + 1 + 11 = 45.)

**Text operations (uncertain).** Strings are immutable runtime-owned UTF-8 values. The operations
`__ToChars` and `__FromChars` cross the representation boundary and are runtime-backed. The others
(`__Trim*`, `__Replace`, `__Split`, `__Repeat`, `__JoinArray`, `__ConcatArray`, `__LastIndexOf`)
look expressible with `StringBuilder`, `foreach` over characters, `Substring`, and the other
existing string members, so they are *potentially expressible in ASTER*. That classification is
unverified: direct string indexing does not exist, the public contract (for example which
characters `Trim` removes) must be reproduced exactly, and the runtime variants include
`Temporary`-region forms chosen by escape analysis. Replacing any of them needs equivalence tests
and measurements first.

### Compiler/runtime operations outside the binding table

These are implemented without an `intrinsic_bindings` entry and should not be counted with it:

- `string` members (`Length`, `Contains`, `StartsWith`, `EndsWith`, `IndexOf`, `Substring`, the
  `TryParse*` family), scalar `ToString()`, string concatenation and interpolation.
- `StringBuilder` operations, including the `Length` and `Clear` forms in the HIR `Intrinsic` enum.
- `List<T>` and `Dictionary<K, V>` members.
- `Task<T>`, `Task.Run`, `Task.WaitAll`, cancellation, `Parallel.For/ForEach/Reduce`, and the async
  lowering primitives.
- `Log` and the foreign-call machinery for `unsafe foreign`.

The MIR `Intrinsic` enum in `crates/aster-mir/src/lib.rs` is the superset (about 120 variants,
including `Temporary` variants chosen by escape analysis); the HIR enum in
`crates/aster-hir/src/lib.rs` lists the 39 kinds that bound declarations can carry. These are
inherently runtime-backed because they own allocation, scheduling, or host effects.

## 6. Contributor workflow

1. **Decide whether ASTER can express it.** Try to write it with the existing language, the
   official types, and `Option`/`Result`. If you can, stop there; no compiler change is involved.
2. **Pick the module.** Put the API in the matching official namespace from the table in section 1.
   A genuinely new `aster.*` namespace is a compiler change (step 8).
3. **Implement it with existing mechanisms.** Use static classes, generics, and overloads like the
   current `Array` and `String` helpers. Keep helpers `internal`. Avoid hidden global state; the
   language has no static fields.
4. **Add tests.** Add functional tests and negative-path tests (invalid ranges, empty input,
   failure results). JIT-level library tests live under `crates/aster-codegen-cranelift/tests/`
   (for example `standard_library_usability.rs`, `practical_stdlib.rs`, `string_methods.rs`), and
   language-level tests under `crates/aster-compiler/tests/`.
5. **Check behavior.** Failures should use `Option`/`Result` when recoverable, and the existing
   bound error reporters when a controlled runtime error is the documented contract. Results must
   be deterministic. Consider which allocations the API makes and how escape analysis will place
   them ([memory management](memory-management.md)).
6. **Confirm the public contract.** Update the matching reference page under `docs/reference/`
   and respect the [compatibility policy](../reference/compatibility.md): before 1.0, additive APIs
   are allowed in patch releases, but documented behavior may only change for correctness or safety.
7. **Request an intrinsic only if a concrete limitation remains.** State which condition in
   section 2 applies and which language capability is missing. If the limitation is a missing
   language feature, the right change is to the language, decided through the usual specification
   process, not to the library.
8. **Benchmark before native performance work.** Compare the ASTER implementation against the
   proposed native one with the existing harnesses and report the same-machine numbers.

Changes that touch the registry:

- Adding or renaming a function inside an existing module changes only that `.aster` file (the
  embedded copy is rebuilt from it with `include_str!`).
- Adding a new module requires adding it to `STDLIB_MODULES`, the `include_str!` constants, and
  `StandardLibrary::embedded()` in `standard_library.rs`, and to `REQUIRED_STDLIB_MODULES` in
  `scripts/bundle.mjs`. Because `from_path` demands every registered file, installed copies and
  `ASTER_STDLIB` trees must include it.
- Adding an intrinsic additionally changes the cross-layer sites listed in section 3.

## 7. Architectural constraints this contract preserves

- Semantic analysis owns language-level decisions; the binding table only labels functions.
- HIR and MIR stay typed and backend-neutral; Cranelift consumes validated MIR and never inspects
  library source.
- The runtime owns the execution context, memory, and host I/O; library code reaches them only
  through registered runtime functions.
- Library APIs compose existing language capabilities where practical, so the compiler does not
  grow with the library.
