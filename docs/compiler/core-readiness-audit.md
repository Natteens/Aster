# Core readiness audit

This is an engineering snapshot, not a language reference and not a replacement for the
[roadmap](../roadmap.md). It records what the existing language core was observed to do at one
commit, which gaps are demonstrated, and which stabilization work should come before standard
library and framework development. Behavior described elsewhere in the canonical documentation is
not restated here.

## 1. Baseline

| Item | Value |
| --- | --- |
| Repository | `Natteens/Aster` |
| Baseline commit | `e580b4f5effb3a8a15b22b99c7e69a63cfcee55c` (`main`, after PR #69) |
| Workspace version | 0.67.0. The baseline is a later commit than the published release: tag `v0.67.0` points to `0f3a65b4ad1fb8e77150ca878b54b3a7204c8026` (`chore(release): 0.67.0 [skip ci]`), and `main` contains subsequent commits that did not change the version |
| Audit date | 2026-11-03 |
| Open pull requests at start | none |
| Method | Read the canonical documentation, read implementation and test sources, ran the validation gates, and ran temporary reproduction programs kept outside the repository |

Evidence hierarchy used: implementation and configuration first, then tests, CI and examples, then
the canonical reference and compiler documentation, then GitHub state, then proposals. A proposal
or parser acceptance was never counted as implemented behavior.

The audit was a sample, not an exhaustive proof. The repository has 2,735 `#[test]` functions
across `crates/`; they were run, not individually re-derived.

## 2. Milestones

**Milestone A — Stable JIT language core.** Defined syntax and semantics; predictable type and
overload behavior; correct linking and generic specialization; validated HIR/MIR contracts; safe
rejection of unsupported operations; correct JIT execution; defined runtime errors and lifetimes;
tested and documented behavior; and enough primitives to grow the standard library. It does not
require AOT, incremental compilation, hot reload, or unrestricted memory ownership.

**Milestone B — Broader 1.0 readiness.** Long-running execution, compatibility guarantees,
platform guarantees, a memory-management story beyond one bounded execution, and distribution.
Items that belong only to B are labeled so and are not folded into A.

## 3. Capability matrix

Status values: **Verified** (implemented, with tests and/or a reproduction in this audit),
**Partial** (implemented, evidence is thin or untested at the edges), **Restricted** (restricted by
documented design), **Defect** (defect confirmed), **Decision** (unresolved decision),
**Out of scope** (unsupported and deliberately excluded).

Evidence paths are relative to the repository root. "Probe" means a temporary program run with
`aster run` in this audit; probes are summarized in section 4 and are not committed.

| ID | Capability | Status | Evidence | Risk | Milestone | Action |
| --- | --- | --- | --- | --- | --- | --- |
| C01 | Lexing, parsing, deferred-surface rejection (`decimal`, unsupported syntax) | Verified | `crates/aster-syntax` (101 tests); probe: `decimal` rejected identically by `check` and `run` with a reserved-feature diagnostic; `examples/README.md` states `decimal_frontend.aster` is a negative fixture | Low | A | None |
| C02 | Diagnostics for common negative paths | Verified | Probes: missing return, private field access, use before initialization, duplicate overload, recursive struct layout, constant overflow, `null`, `string` across `Task.Run`, infinitely expanding generic all produce a located error before execution | Low | A | None |
| C03 | Numeric literals and promotion table | Defect | See D1 and section 4; `docs/specification/02-types.md` "Binary promotion" versus `crates/aster-compiler/src/hir_lowering/types.rs:214` | High | A | P0-1 |
| C04 | Numeric literal forms (hex, binary, exponent, separators) | Decision | `docs/specification/01-lexical-structure.md` lists these as not implemented and as an open question; probe: `1.0e20` rejected | Low | A | D2 |
| C05 | Integer bitwise and shift operators | Decision | No mention in `docs/` (search for "bitwise" is empty); probe: `a & b` rejected by the parser; `stdlib/aster/random/random.aster` needs `__RandomMix` | Medium | A | P1-2 |
| C06 | Runtime arithmetic failures (divide/remainder by zero, signed overflow of `MIN / -1`) | Verified | Probes: controlled `Aster runtime error:` messages; `crates/aster-codegen-cranelift/tests/jit.rs` `small_integer_operations_are_promoted_and_guarded` | Low | A | None |
| C07 | Float-to-int and unsigned-to-signed casts | Verified | Probe: NaN casts to 0, out-of-range saturates, `uint` to `int` wraps; consistent with the documented explicit-cast section | Low | A | None |
| C08 | Evaluation order and compound assignment | Verified | Probe: `a[i++] += 5 + i` evaluates left-to-right; probe: arguments evaluated left-to-right; compound assignment rejects hidden narrowing | Low | A | None |
| C09 | Overload resolution | Verified | Probe: `Wide(3)` with `long` and `double` overloads is reported ambiguous, matching `docs/reference/classes.md` ("Equally good candidates are an error") | Low | A | None |
| C10 | Structs: value copy, nested copy, array fields | Verified | Probe: nested struct copy is independent while an array field is shared; `docs/reference/structs.md` | Low | A | None |
| C11 | Classes, properties, constructors, identity equality | Verified | `crates/aster-compiler/tests/class_pipeline.rs`; examples under `examples/` | Low | A | None |
| C12 | Interfaces and class implementation | Restricted | Probe: struct implementing an interface rejected with "only classes can declare implemented interfaces"; `docs/reference/interfaces.md` states no struct implementations, no downcasts | Medium | A | D3 |
| C13 | Enums with payloads, exhaustive `switch` statement and expression | Verified | `crates/aster-compiler/tests/enum_pipeline.rs`, `enum_switch_expression.rs`; probe: generic `Option<T>` helper via `switch` expression | Low | A | None |
| C14 | Static fields and static members of generic types | Restricted | Probe: `static int Count` rejected with "static fields are not implemented"; `docs/reference/classes.md` | Medium | A | D4 |
| C15 | Namespaces, `using`, multi-file projects, visibility | Verified | `crates/aster-compiler/tests/package_dependencies.rs`, `crates/aster-cli/tests/manifest_contract.rs`; `examples/multifile*`, `examples/namespaces` | Low | A | None |
| C16 | Package manifests and path/Git dependencies | Verified | `crates/aster-cli/tests/git_dependencies.rs`, `manifest_contract.rs`; `docs/reference/packages.md` | Low | A | None |
| C17 | Generic functions: explicit and inferred type arguments | Verified | `crates/aster-compiler/tests/generic_pipeline.rs`; probe: `Max<T>` over `int` and `long` | Low | A | None |
| C18 | Generic methods | Verified | `crates/aster-compiler/tests/generic_methods.rs` | Low | A | None |
| C19 | Generic classes, structs, interfaces, enums | Verified | `crates/aster-compiler/tests/generic_type_pipeline.rs`, `crates/aster-codegen-cranelift/tests/generic_construction_jit.rs`; probes: user-defined growing `Stack<T>`, `Cache<K, V>` over `List<T>` | Low | A | None |
| C20 | Generic constraints (interface-only, nominal) | Restricted | `crates/aster-compiler/tests/generic_constraints.rs`; `docs/reference/generics.md`; probe: constraint satisfied only by interfaces and classes | Medium | A | D3 |
| C21 | Unconstrained template body errors | Partial | Probe: `Max<T>` with `>` on a class argument is reported inside the template body, not at the call; `docs/compiler/monomorphization.md` documents the per-specialization check | Low | A | Accept as documented |
| C22 | Recursive and expanding generic specialization | Verified | Probe: infinitely expanding generic rejected with a diagnostic; `docs/compiler/monomorphization.md` | Low | A | None |
| C23 | Specialization identity and caching | Partial | `docs/reference/generics.md` documents the identity; `crates/aster-compiler/src/generics/specialization.rs`; semantic `CallableKey`/`ModelNodeKey` remain span-positional (see `docs/compiler/architecture.md`) | Low for A | A/B | P2-2 |
| C24 | HIR lowering and typed contracts | Defect | D1: HIR recomputes a binary result type that semantic analysis computes differently | High | A | P0-1, P1-1 |
| C25 | MIR lowering and general optimizer | Partial | `crates/aster-compiler/tests/mir_lowering.rs`, `mir_optimizer.rs`, `crates/aster-codegen-cranelift/tests/mir_optimizer.rs`; `docs/reference/compatibility.md` fixes purity/failure rules for DCE and folding | Medium | A | P1-1 |
| C26 | Backend validation (fail-closed) | Verified | `crates/aster-codegen-cranelift/src/validation.rs` (`validate_module`; in-file tests); `crates/aster-cli/tests/fail_closed.rs`; no program was found that `check` accepts and the JIT rejects at validation, apart from file-mode entry shape (see section 11) | Low | A | P2-4 |
| C27 | Cranelift code generation, call ABI, aggregates | Verified | `crates/aster-codegen-cranelift/tests/jit.rs`, `ownership_contract.rs`, `foreach*.rs`; probes: nested `List<List<int>>`, `List<Option<int>>`, `List<W>` with array fields, interface returning an array, jagged arrays | Low | A | None |
| C28 | Interface dispatch | Verified | `crates/aster-compiler/tests/interface_pipeline.rs`; probe: interface method returning an array | Low | A | None |
| C29 | Execution context, per-run arenas, 1 GiB cap, allocation failure | Verified | `docs/compiler/execution-context.md`; `crates/aster-codegen-cranelift/tests/memory_stress.rs`, `crates/aster-cli/tests/fail_closed.rs` `impossible_positive_allocation_is_a_controlled_cli_failure`; probe: 400,000 retained 4 KiB arrays fail with a controlled error | Low (A), High (B) | A/B | P2-1 |
| C30 | Escape analysis, AARM, owned regions | Verified | `crates/aster-compiler/src/escape_analysis.rs`, `lifetime_analysis.rs`, `temporary_subregions*`, `owned_regions.rs`; `crates/aster-codegen-cranelift/tests/aarm_*.rs`, `long_lived_ownership.rs`, `memory_matrix.rs`; `docs/compiler/memory-management.md` states every incomplete proof retains Persistent storage | Low (safety) | A | None |
| C31 | Long-running reclamation of unreachable shared or escaping values | Decision | `docs/specification/10-open-questions.md` ("Class memory management remains unresolved"); probe in section 4 | High for B | B | P2-1 |
| C32 | Strings, arrays, `List<T>`, `Dictionary<K, V>`, `StringBuilder` | Verified | `crates/aster-codegen-cranelift/tests/string_methods.rs`, `list*.rs`, `dictionary*.rs`, `string_builder.rs`; probes: nested lists and jagged arrays (see C27) | Low | A | None |
| C33 | Dictionary key types | Restricted | Probe: `Dictionary<P, int>` for a struct `P` rejected with "not supported as a Dictionary key in ASTER 1.0" | Low | A | Documented limit |
| C34 | `Option<T>`, `Result<T, E>`, `?` propagation | Verified | `crates/aster-codegen-cranelift/tests/option_try_propagation.rs`, `result_propagation_jit.rs`; `docs/reference/result-propagation.md` | Low | A | None |
| C35 | Controlled runtime failures | Verified | Probes: array bounds, divide by zero, recursion depth (1024) all produce `Aster runtime error:` with exit 1; `crates/aster-codegen-cranelift/tests/recursion_guard.rs` | Low | A | None |
| C36 | Task, Parallel, WaitAll, cancellation | Verified | `crates/aster-codegen-cranelift/tests/task_run.rs`, `parallel.rs`, `concurrency_stress.rs`, `task_ergonomics.rs`; `docs/reference/concurrency.md` fixes deterministic failure selection | Low | A | None |
| C37 | Async functions | Restricted | `docs/reference/concurrency.md`: only the documented single direct `await Task.Run` shape; `crates/aster-compiler/tests/async_await.rs`, `crates/aster-codegen-cranelift/tests/async_runtime.rs` | Medium | A | Document as restricted (already is) |
| C38 | Host I/O, filesystem, clocks, worker restrictions | Verified | `crates/aster-codegen-cranelift/tests/filesystem.rs`, `console_io.rs`, `io_error.rs`; worker rejection in `crates/aster-compiler/src/semantic/general/nested_concurrency.rs` | Low | A | None |
| C39 | Native FFI (`unsafe foreign`) | Verified | `crates/aster-compiler/tests/foreign_ffi.rs`, `crates/aster-codegen-cranelift/tests/foreign_ffi.rs`; `docs/reference/native-ffi.md` | Low | A | None |
| C40 | Standard library written in ASTER | Partial | `stdlib/aster/**` is 622 lines; `Array.Copy/CopyRange/Fill/Reverse`, `String.*` wrappers and `Random` are ASTER code | Medium | A | P1-3 |
| C41 | Compiler-known stdlib intrinsics | Partial | about 44 `Intrinsic::` references in `crates/aster-compiler/src/standard_library.rs`; classification in section 7 | Medium | A | P1-3 |
| C42 | Test runner (`aster test`, `Assert`) | Verified | `docs/reference/testing.md`, `stdlib/aster/testing/testing.aster`, `crates/aster-cli/tests/cli.rs` | Low | A | None |
| C43 | Performance measurement infrastructure | Partial | `crates/aster-codegen-cranelift/examples/perf_baseline.rs`, `benchmarks/performance/`, `benchmarks/memory/`; no shipped baseline | Medium | A | P2-5 |
| C44 | CI and release automation | Verified | `.github/workflows/ci.yml`, `windows-ci.yml`, `release.yml`, `release-recovery.yml`; section 9 | Low | A | None |

44 capabilities were assessed.

## 4. Confirmed problems and evidence

### D1 — Small-integer binary arithmetic wraps at the operand width (P0)

`docs/specification/02-types.md` ("Binary promotion") states: two integers narrower than 32 bits
promote to `int`, and "Two equal small integer operands still promote to `int`: `byte + byte` has
type `int`." The central table `crates/aster-types/src/lib.rs` (`promote`) implements this, and
semantic analysis uses it (compound assignment `byte x = 200; x += x;` is correctly rejected with
"compound assignment would narrow `int` to `byte`").

HIR lowering involves two cooperating points, and both contribute:

1. `crates/aster-compiler/src/hir_lowering/expressions.rs` (`Work::Combine` in the binary
   expression lowering): operands are converted to the promoted type only when
   `left.type_ != right.type_`. Two operands of the same small type are therefore never converted.
2. `crates/aster-compiler/src/hir_lowering/types.rs` (`binary_type`, line 214): when both operand
   types are equal it returns that type before consulting `promoted`.

For `byte + byte`, the operands stay `Byte`, the expression is typed `Byte`, and MIR/Cranelift
perform the arithmetic at 8 bits. The result is then widened to `int` only after it has wrapped.
Changing only the result type recorded on the HIR expression would not repair this: the operands
would still be narrow, so the operation would still execute at 8 bits.

Reproduction (temporary file, run with `aster run`; results observed):

| Source | Spec result | Observed |
| --- | --- | --- |
| `byte b = 200; int c = b + b;` | 400 | 144 |
| `short s = 30000; int e = s + s;` | 60000 | -5536 |
| `sbyte sb = 100; int f = sb + sb;` | 200 | -56 |
| `byte b = 200; long d = b * b * b;` | 8000000 | 0 |
| `byte x = 200; return (x + x) > 300 ? 1 : 0;` | 1 | 0 |
| `byte x = 200; int y = (x + x) / 2;` | 200 | 72 |
| `ushort u = 60000; int z = u + u;` | 120000 | 54464 |

Impact: silent wrong results for every arithmetic expression whose operands are two equal small
integer types, with no diagnostic. This is in the Milestone A scope ("predictable type behavior",
"correct JIT execution"). The existing test `small_integer_operations_are_promoted_and_guarded`
(`crates/aster-codegen-cranelift/tests/jit.rs`) only checks division and remainder results that fit
the narrow type, so it cannot see the defect. Mixed small-integer pairs such as `byte + short` fall
through to `promoted` and are correct.

This is also an instance of the principle "one authoritative layer per decision": the result type
of a binary expression is decided twice.

### Observations that are not defects

These were examined and recorded so they are not rediscovered as problems.

- Large retained allocations fail in a controlled way. A loop that allocates a 1,024-element `int`
  array and a wrapper object per iteration and stores the latest one in a field retains every
  allocation: at 200,000 iterations `--memory-stats` reported 824,000,032 bytes used; at 400,000
  iterations the run stopped with `allocation exceeds the execution memory limit of 1073741824
  bytes`. This matches `docs/compiler/memory-management.md` ("intentional conservative retention")
  and is a Milestone B limit, not a safety bug.
- `aster check` on a file whose `Main` returns a struct, array, or interface succeeds, while `aster run`
  rejects the entry. `check` on a file has no entry requirement, so this is consistent with its
  documented role; it is noted under uncertainty only.
- `Wide(3)` with `long` and `double` overloads is reported ambiguous, which is the documented
  behavior for equally good candidates (C09).
- Probes for method-level generics, constrained sorting with an interface comparer, nested
  collections, and enum payloads containing arrays all behaved correctly.

## 5. Open decisions

| ID | Decision | Why it matters now | Recommendation |
| --- | --- | --- | --- |
| D2 | Hexadecimal, binary, exponent literals and digit separators | Already an open question; low urgency | Defer until D3/P1-2 settle numeric syntax together |
| D3 | Whether structs (and primitives) may implement interfaces or satisfy constraints | Constrained generic algorithms cannot name a struct or primitive; unconstrained templates currently cover the use case | Do not expand yet; revisit when a stdlib algorithm needs it |
| D4 | Static fields / module-level state | Library caches and counters have no home; static methods exist | Decide with the first concrete stdlib need |
| D5 | Where MIR verification lives (backend today) | Optimizer output is only validated at JIT preparation | Keep for A; revisit if a second backend appears |
| D6 | Class and shared-object reclamation model | Required for B; already listed as open | Collect representative long-running programs first |

## 6. Prioritized backlog

Complexity: S (hours to a day), M (a few days), L (a week or more). "Decision first" means a
decision record must exist before implementation starts.

### P0 — Core blocker

**P0-1. Make small-integer binary expressions follow the promotion table end to end.**

- Problem and evidence: D1.
- Layer: HIR lowering (`binary_expression` in `hir_lowering/expressions.rs` and `binary_type` in
  `hir_lowering/types.rs`); verify that MIR lowering, constant folding, and Cranelift agree.
- User impact: silently wrong integer results for `byte`, `sbyte`, `short`, `ushort` pairs.
- Smallest acceptable correction: when both operands are small integers (equal or different
  types), convert both operands to the promoted type (`promoted` / `aster_types::promote`) before
  the operation is built, and derive the expression type from the converted operands. The
  semantic, HIR, MIR, and Cranelift layers must then agree that the operation executes at the
  promoted width. Changing only `Expression.type_` to `int` is insufficient, because the operands
  would remain narrow and the arithmetic would still wrap at the operand width. No new feature,
  no refactor of unrelated paths.
- Acceptance criteria: every row of the D1 table matches the specification; `byte + byte`,
  `sbyte + sbyte`, `short + short`, `ushort + ushort` have HIR type `int` in `aster dump-hir`;
  compound assignment diagnostics are unchanged; `cargo test --workspace --all-targets` passes.
- Regression tests: JIT tests for each of the four types and for `+`, `-`, `*`, comparison, and
  nesting inside a larger expression; a HIR-level type assertion in
  `crates/aster-compiler/tests/`; negative test that compound assignment still rejects narrowing.
- Dependencies: none.
- Complexity: S.
- Decision first or implementation first: implementation first (specification is explicit).

### P1 — Core stabilization

**P1-1. Add a numeric conformance suite and a semantic/HIR type-agreement check.**

- Problem: D1 shows a type decision duplicated across layers and a test that could not detect it.
- Layer: tests (JIT and compiler); no production change unless a mismatch is found.
- Smallest correction: one table-driven test covering every row of the specification's promotion
  table and each operator at overflow boundaries for every integer width, plus a test that walks
  lowered HIR expressions of a representative corpus and asserts the HIR type equals the semantic
  type where both exist. Cover unary operators and conditional expressions explicitly.
- Acceptance criteria: tests fail on the baseline commit for D1 and pass after P0-1; any further
  mismatch becomes a new backlog item rather than being fixed in this item.
- Dependencies: P0-1.
- Complexity: M.
- Decision or implementation first: implementation.

**P1-2. Decide integer bitwise and shift operators.**

- Problem and evidence: C05. The language has no `&`, `|`, `^`, `~`, `<<`, `>>`; `Random` relies on
  the `__RandomMix` intrinsic because the generator cannot be written in ASTER; hashing, flags and
  binary protocols have no expression.
- Layer: syntax, semantic, HIR/MIR, backend if accepted.
- User impact: the standard library cannot express bit-level algorithms without new intrinsics.
- Smallest correction: write the decision record (operator set, precedence, operand types,
  shift-count behavior, interaction with the promotion table fixed by P0-1). Implement only after it
  is ACCEPTED.
- Acceptance criteria: decision record exists; if accepted, a feature meets the roadmap definition
  (syntax, semantics, HIR, MIR, JIT, diagnostics, tests, example agree) and `Random` can drop
  `__RandomMix` without changing its documented sequence.
- Dependencies: P0-1, P1-1.
- Complexity: decision S; implementation L.
- Decision first.

**P1-3. Document the standard library extension contract.**

- Problem: C40/C41. The mechanism by which `stdlib/aster` declarations with placeholder bodies are
  bound by qualified name to `Intrinsic` values (`crates/aster-compiler/src/standard_library.rs`) is
  not described in `docs/`, so contributors cannot tell when a new API needs a compiler change.
- Layer: documentation only.
- Smallest correction: a short compiler-development note stating (a) APIs should be ordinary
  ASTER, (b) an intrinsic is justified only for host effects, runtime-owned layouts, or measured
  performance, and (c) the section 7 classification as the starting inventory.
- Acceptance criteria: the note exists and the intrinsic inventory matches the binding table.
- Dependencies: none.
- Complexity: S.
- Implementation first.

### P2 — Deferred

**P2-1. Long-running memory behavior (Milestone B).** Decision first, using representative
long-running programs. Evidence: C29, C31. The model is safe and bounded by a 1 GiB cap; it cannot
reclaim unreachable escaping values within one execution.

**P2-2. Stable semantic identity (prerequisite for incremental compilation).** Evidence: C23.
Semantic callable and model-node keys remain span-positional. Excluded now by the task scope.

**P2-3. Struct and primitive interface implementation (D3).** Decision first; no demonstrated
blocker today.

**P2-4. MIR verifier location (D5).** Backend-only validation is fail-closed; revisit with a second
consumer.

**P2-5. Performance regression gating.** The harness exists; no baseline or CI gate is shipped.

**P2-6. Static state (D4), numeric literal forms (D2).** Decide when a concrete library need exists.

Confirmed counts: **P0 = 1, P1 = 3, P2 = 6**. Of the P1 items, one is a test suite, one a decision
followed by a feature, and one documentation; none requires a refactor.

## 7. Compiler-known intrinsics

`crates/aster-compiler/src/standard_library.rs` binds qualified stdlib names to `Intrinsic`
values (about 44 references). A first classification:

| Group | Examples | Assessment |
| --- | --- | --- |
| Host effects | `aster.io::*`, `aster.time::*` | Justified: effects cannot be written in ASTER |
| Controlled failure | `ReportRuntimeError(...)` bindings, assertion intrinsics | Justified: runtime error channel |
| Math | `aster.math::__MathPowDouble`, trigonometry | Justified: libm semantics |
| Runtime layout | `StringBuilder`, `List`, `Dictionary`, `Task`, `Parallel` | Justified: allocation and region rules (see memory model) |
| Text algorithms | `aster.text::__Trim`, `__Split`, `__Replace`, `__Repeat`, `__JoinArray`, `__ConcatArray`, `__ToChars`, `__FromChars`, `__LastIndexOf` | Not shown to be language-forced; likely performance and representation motivated. Strings cannot be indexed (`docs/reference/strings.md`), so a pure-ASTER implementation depends on `ToChars`/`FromChars` and would need measurement before replacing any of these |
| Bit mixing | `aster.random::__RandomMix` | Language-forced today (C05) |

`Array.Copy`, `CopyRange`, `Fill`, `Reverse`, and the `String` wrappers show that ordinary
library code, including generic code, composes existing capabilities. No stdlib addition tried in
this audit required a compiler change except where noted above.

## 8. Dependency-ordered sequence

1. P0-1 (correct the small-integer typing).
2. P1-1 (conformance suite and type-agreement check) — validates step 1 and guards later work.
3. P1-3 (stdlib extension contract) — independent; may run in parallel with 1–2.
4. P1-2 decision record, then implementation if accepted.
5. Re-run the section 9 gates; evaluate the section 10 criteria.
6. Only then begin standard library growth; P2 items on demand.

## 9. Validation performed

Run locally at the baseline commit on Windows (two Cargo jobs):

| Gate | Result |
| --- | --- |
| `cargo fmt --all --check` | pass |
| `git diff --check` | pass |
| `cargo check --workspace --all-targets --all-features --locked` | pass |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | pass |
| `cargo test --workspace --all-targets` pass (2,617 passed, 0 failed, 4 ignored) |

GitHub CI: the Ubuntu `CI` workflow passed on the exact baseline commit (run 41741612333). The
Windows workflow runs on pull requests, a weekly schedule, and manual dispatch, not on pushes to
`main`; it passed on the head of PR #69 (`13e9dd2b12bdd306f2b607f2be69575d3cbbfe9f`), whose tree
(`947e4852a50cf3a9c610af7a15773d86d95ae00a`) is identical to the baseline tree.

Not run: Rust 1.88 MSRV check (no Rust change), release-binary and Node-based release tests
(Node is not installed in this environment), benchmarks (see section 10).

## 10. Performance baseline and measurement gaps

Existing infrastructure: `perf_baseline` (phase-separated frontend, validation, codegen, finalize,
cold and warm execution) over six workloads, and the memory-region matrix. See
`benchmarks/performance/README.md` and `benchmarks/memory/README.md`.

This audit did not run the benchmarks and makes no performance claims. Gaps:

- no committed baseline and no CI gate, so regressions are noticed only when someone runs the
  harness on the same machine;
- all workloads are small single-file programs; there is no large-program frontend or
  specialization-count benchmark;
- no workload for text/IO standard library paths, `Dictionary`-heavy programs, or `Task`/`Parallel`
  throughput;
- results are same-machine only by design.

## 11. Remaining uncertainty

- The sample is not exhaustive. Only the paths listed in section 3 were exercised.
- Other places where HIR re-derives a type (unary, conditional, compound assignment, casts) were
  not individually audited; P1-1 is designed to find them.
- `aster check` in project mode was not compared against `run` for entry validation.
- Concurrency determinism rests on existing stress tests; no new scheduling experiments were run.
- Backend validation was not fuzzed; "no counterexample found" is not a proof.

## 12. Intentional exclusions

Incremental compilation, hot reload, AOT, ECS, engine and framework work, new language features,
and any memory-model change are outside this audit and are not requested by any P0/P1 item.

## 13. Declaring Milestone A complete

Milestone A can be declared when all of the following hold on one commit:

1. P0-1 is merged and the D1 reproduction table passes as automated tests.
2. The P1-1 conformance suite passes, and any mismatch it found is fixed or accepted in writing.
3. P1-2 has an ACCEPTED or REJECTED decision record, and `Random` no longer depends on an
   undocumented limitation if accepted.
4. P1-3 is merged.
5. Every status in section 3 marked Defect is Verified or Restricted with documentation.
6. The five gates in section 9 pass locally and Ubuntu and Windows CI pass on the same head.
7. No open decision (D2–D6) is silently assumed by documentation or examples.

**Readiness judgment (qualified).** At the baseline, Milestone A is not yet met because of one
confirmed correctness defect (D1). Outside it, the sampled core behaved consistently with its
documentation, with fail-closed rejection and controlled runtime errors. This is a judgment from a
sampled audit, not a proof of absence of defects.
