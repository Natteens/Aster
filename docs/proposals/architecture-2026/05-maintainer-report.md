# E. Report to the maintainer (PROPOSED, revision 2)

> **Status: PROPOSED. Not an approved architecture.** Summary of the audit at `8b5466b` (0.71.0)
> after the second review. Details: [map](01-repository-map.md), [master plan](02-master-plan.md),
> [decision register](03-decision-register.md), [roadmap](04-roadmap.md).

## 0. State of the implementation, in brief

ASTER today is a well-layered compiler for a C#-flavored language that runs one bounded execution
through a JIT. It cannot yet run for a long time, pass a function as a value, call a native library
from the CLI, produce an executable, or use more than one shape of `async`. Evidence is in the map.

## 1. Existing decisions to preserve

Nothing in the proposal changes these.

| Decision | Source |
| --- | --- |
| No tracing GC; no moving objects; stable references | Maintainer; AARM frozen invariants |
| Direction B and the five approved B1 decisions, including the descriptor pointer at offset zero | `object-model-b1.md` §2 |
| Concrete generics; no erasure, no boxing, no universal `object` | `00-goals.md` |
| No exceptions; `Option`, `Result`, `?` | `00-goals.md` |
| No implicit null | `00-goals.md` |
| Isolated workers; explicit transfer; class references do not cross; no injected cancellation polling | `reference/concurrency.md` |
| Deterministic public outcomes (`WaitAll` order, lowest-index failure, first error) | Same |
| Semantic analysis as the single authority; typed backend-neutral HIR and MIR; runtime free of compiler and Cranelift types | `architecture.md` |
| Fail-closed backend validation; controlled runtime failure | Same |
| Regions and AARM as they are, for provably local values | `memory-management.md` |
| No finalizers | B1 §1 |
| ECS, engines, HTTP, UI outside the compiler; the product-layer boundaries | Specification chapters 10, 14 |
| Cranelift as the only backend | Current implementation |
| Evidence gates before production changes | AARM measurement gates; B1 stages |

Revision 1 conflicted with three of these and has been corrected: drop glue calling `Dispose`
(a finalizer in effect), moving class graphs between domains, and treating the 16-byte header as
settled.

## 2. Recommendations ready for analysis and approval

They need no experiment and change no accepted behavior. Register class READY.

**Foundations**

- BENCH-001: workload suite restricted to what today's syntax and harness can express, with named
  proxies for the rest.
- PERF-005: deterministic invariants gate CI; performance metrics advise only.
- ARCH-002: backend-neutral MIR validation in `aster-mir`, run after every pass.
- ARCH-001: structural semantic identity (the key types already exist, unused).
- ARCH-003, ARCH-005, ARCH-007, DOC-001.

**Contracts**

- MEM-005: reclamation, drop glue, and `Dispose` are separate; `Dispose` runs only through `using`
  or an explicit call.
- OBJ-001: B1 keeps its stages; stage 3 measures three header configurations in one report.
- OBJ-002 with U1, U2, U6, U8, U10; OBJ-004 with U4 as `is T x` and `as` yielding `Option<T>`;
  OBJ-006 for U7.
- CONC-005: transfer by copy of value-like data.
- CONC-003, CONC-006: general async and channels, once their prerequisites exist.
- HOT-001 to HOT-004: development-profile-only design with explicit safe points and restart as the
  default answer to doubt.
- PLAT-001 (FFI), PERF-001 (AOT direction), PERF-003.

**Additive language items**

- LANG-001 (closures), LANG-004, LANG-005, LANG-007, LANG-008, LANG-010 to LANG-015, LANG-017,
  LANG-018.

## 3. Decisions that depend on experiments

Thresholds are recorded by you before each run. No numeric target is proposed in these documents.

| Experiment | Decides | What is compared |
| --- | --- | --- |
| MEM-EXP-1, run as B1 stage 3 | MEM-001, MEM-002, MEM-003 | B1 header without counting against a count word at offset 8 (packed and unpacked) with counting: time, peak and final live bytes, count operations emitted, elided, executed |
| CONC-EXP-1 | CONC-002 | One finalized module shared by worker threads |
| PERF-EXP-1 | PERF-002, MEM-008 | MIR inlining on accessor-heavy code; compile-time cost |
| PERF-EXP-2 | PERF-001 | AOT size, startup, run time against JIT |
| CONC-EXP-2 | CONC-003, CONC-004 | Task cost; scaling; whether stealing or compensation threads are needed |
| MEM-EXP-2 | MEM-006 | Fast-path cost of the governor and purge policy when enabled |
| OBJ-U3 | OBJ-004 | Interface lookup by search or fixed index |

If MEM-EXP-1 fails, inheritance is unaffected: B1 proceeds on its 8-byte header as written, and
long-running memory needs a different, narrower answer.

## 4. Questions that require your choice

Full options are in register §3.2. Each has a recommendation; none is assumed elsewhere.

**Direction**

- Q-MEM-0: accept reference counting as the third storage tier, subject to MEM-EXP-1.
- Q-MEM-3: cycles are the programmer's responsibility, with `weak` and diagnostics, and no collector.
- Q-CONC-0: domains as the permanent concurrency model; no shared-memory threads.
- Q-LANG-5: confirm exceptions stay rejected.

**Amendments to B1's proposed text**

- Q-OBJ-1: a count word after the descriptor pointer (amends §4.1).
- Q-OBJ-2: virtual properties together with virtual methods (answers U9 differently from B1).
- Q-OBJ-3: constructor overloads (B2) before or after B1.
- Q-OBJ-4: `base` reserved (U5).

**Semantics**

- Q-MEM-1: an owned local is released at its last use, not at scope end. Observable only through
  `weak`.
- Q-MEM-2: whether the runtime may close a standard-library handle when its wrapper is reclaimed.
  Recommended: no.
- Q-CONC-3: a runtime failure terminates its whole domain, not only its task.
- Q-CONC-1: worker domains still running when `Main` returns are abandoned.
- Q-CONC-2: one runtime-internal thread-local for FFI callbacks, a narrow exception to the current
  "no thread-local task state".
- Q-CONC-5: a small I/O polling dependency in the runtime.

**Breaking changes — all pending, none assumed**

| Item | Breaks |
| --- | --- |
| LANG-003 `T?`, `null`, `??`, `?.` as sugar for `Option` | Nothing existing; reverses "no `null`" in spelling |
| LANG-009 struct receiver by reference; `ref` | Code relying on a method mutating a copy |
| LANG-016 real literal defaults to `double` | `float x = 1.5;` needs a suffix |
| LANG-006 static fields, one copy per domain | Nothing existing; differs from C# |
| CONC-009 `Reduce` chunking independent of core count | Grouping for non-associative combines |
| MEM-002 header growth | Tests and benchmarks that pin sizes |
| U5 `base` reserved | An identifier named `base` |

## 5. Residual risks

These remain even if every recommendation is accepted and every experiment passes.

1. **The ownership validator is new code as subtle as the pass it checks.** Independence reduces the
   chance of a shared mistake; it does not remove it. A wrong release is a use after free in safe
   code. Mitigations (differential runs, poisoning allocator, sanitizer) find defects; they do not
   prove absence.
2. **Soundness of the heap invariants rests on escape analysis.** H1 and H2 turn its classification
   into a safety property for a second mechanism. Today a wrong "local" verdict is caught by region
   validation; after this plan it must also never let a counted object point into a region.
3. **Cycles leak.** By policy. Tools report them in development; release builds only count.
4. **Release cost is local but unbounded.** Freeing a large graph happens at one point. There is no
   global pause, and there can be a spike.
5. **Reentrancy across `await`.** Atomic between suspensions, not across them. This is the C#
   single-threaded model and it is a known source of defects.
6. **A task that never awaits freezes its domain.** No preemption. A blocked pool with no
   compensation threads can stall.
7. **A failure costs a whole domain.** Coarser than a caught exception. Finer isolation is the
   programmer's job.
8. **Forgotten `Dispose` is a resource leak** until the domain ends. A lint and a development
   report, not a guarantee.
9. **FFI is outside every guarantee.** A callback on a foreign thread is refused by the thunk, but a
   native library that corrupts memory cannot be contained.
10. **Hot reload retains old code for the session** in its first version, and falls back to restart
    often (any type change with live instances). It may feel less capable than the name suggests.
11. **Incremental compilation against whole-program passes** (specialization, escape summaries) may
    not reach the development loop the vision implies.
12. **Complexity budget.** `context.rs` is 10,436 lines and backend validation 8,671. The plan adds
    an allocator, an ownership pass with its validator, a scheduler, a reactor, and a second emission
    path.
13. **Existing fragile invariants** ("string operations always copy"; builder birth depth) stay
    load-bearing until MEM-003 replaces them.
14. **`unsafe` in the runtime**: 234 allowed sites against 129 `SAFETY` comments in one file.
15. **Proxies are not the feature.** Until virtual dispatch, closures, and general async exist,
    their cost is unknown, and the first measurement has no earlier baseline to compare with.
16. **Goals may not be met.** No numeric target is set. "Extremely fast" and "extremely light" stay
    hypotheses until baselines exist.

## 6. Dependencies and order

```text
workloads ─► B1 stage 2/3 with the counting experiment ─► ownership ─► closures ─► general async ─► I/O
validator beside MIR ─► ownership, inliner
structural identity ─► incremental ─► hot reload, language server
shared module ─► execution crate ─► AOT ─► more targets, debug information
```

- Inheritance needs only B1's header. Counting needs B1's header plus one word.
- Closures and async frames outlive a call, so they need the memory model.
- Hot reload keeps state only when the heap stops belonging to a JIT session.
- No ungated item depends on a pending breaking change.

## 7. First implementation task (not started)

**Roadmap 0.2 to 0.4: the workload suite (BENCH-001), limited to what compiles today.**

It changes no behavior, it is the evidence every experiment above needs, and the readiness audit
already asks for it. Writing the workloads in today's ASTER also produces an exact list of language
gaps. In parallel, ungated: 0.1 (document corrections), 0.6, 0.7 (validator move).

## 8. Limits of this audit

- Static only. No build, test, or benchmark was run. No performance figure here is a measurement of
  mine. Object sizes in the header comparison are computed from B1's rules.
- Direct reading covered the canonical documents, MIR, and targeted source checks; the rest came
  from three auditing agents with file and line references, labeled [A] in the map.
- Not verified: whether backend validation re-checks builder provenance; snapshot paths for
  `foreach` and dictionary views; native stack headroom at call depth 1024; serial pumping of
  several async tasks; shareability of finalized JIT code across threads; the growing-recursion
  diagnostic described in `monomorphization.md`; whether a recursive `Option<Node>` class field
  compiles.
- The contracts in revision 2 are designs on paper. None has been prototyped.
