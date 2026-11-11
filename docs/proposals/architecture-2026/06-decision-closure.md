# F. Decision closure (revision 3)

> **Status.** The maintainer delegated every pending technical decision to the architecture lead on
> 2026-11-10. This document records those decisions. It supersedes the "PROPOSED" status and the
> review classes of [03-decision-register.md](03-decision-register.md) where they differ; the
> register keeps the evidence, alternatives, and full records. IDs are unchanged.
>
> A decision here is a **design** decision. It is not implemented until a task in
> [07-tasks.md](07-tasks.md) reaches its gate. Public documentation changes only with the
> implementing change.

## 1. Status vocabulary

| Status | Meaning |
| --- | --- |
| **MAINTAINER REQUIREMENT** | Accepted by the maintainer before this delegation. Not reopened |
| **ACCEPTED DESIGN** | Closed by this delegation; not necessarily implemented |
| **CONDITIONAL** | Direction chosen; adoption waits for a named experiment with thresholds fixed below and a named fallback |
| **DEFERRED** | Deliberately not done now; reason and reopening condition recorded |
| **IMPLEMENTED** | Behavior exists in the working tree and its tests pass |

## 2. Maintainer requirements (preserved, not reopened)

Native and independent of .NET; C#-like syntax; static, strong, nominal, non-null by default; no
traditional GC; safe against use after free, double free, and data races in safe code; long-running
programs with correct reclamation; efficient concurrency and general `async`/`await`; real hot
reload when safe; AOT executables; layered architecture.

Invariants kept verbatim: semantic analysis is the authority for types, names, calls, overloads;
typed backend-independent HIR; typed CFG MIR; Cranelift consumes validated MIR; runtime free of
compiler and Cranelift types; generics concrete before downstream contracts; AARM and regions kept
for provably local values; domain isolation without implicit shared mutation; no implicit finalizer;
controlled failure; safety first. Direction B and the five approved B1 decisions (descriptor pointer
at offset zero, immutable descriptors, B1 one constructor plus `base(…)` with B2 for overloads,
closed bases, identity-preserving upcast). No exceptions, no implicit null in semantics, concrete
generics without boxing (specification chapter 00).

## 3. The nineteen questions

Each entry gives: decision, status, reason, observable semantics and negative cases, safety and
performance, compatibility and migration, dependencies and tests, reversal criterion.

### Memory

**Q-MEM-0 — Third storage tier.** **CONDITIONAL.** Per-domain, non-atomic reference counting with
compile-time elision for values escape analysis does not prove local (master plan §5.3 to §5.6).
- Reason: the only option that keeps C# aliasing, needs no collector, and bounds long-running
  memory for acyclic data.
- Experiment: MEM-EXP-1 (§5.C below). Fallback if it fails: no counting; B1 with H8; long-running
  memory through an explicit `Arena` scope type (a library-visible region whose objects may not
  escape it, checked by escape analysis), recorded then as a new decision.
- Safety: obligations U1 to U8 (master plan §5.6) with an independent validator.
- Reversal: thresholds of §5.C not met for both H12 and H16.

**Q-MEM-1 — Release point of an owned local.** **ACCEPTED DESIGN.** Release at last use, not at the
end of the lexical scope.
- Reason: shortest lifetime gives lowest peak memory and enables in-place reuse; no user code runs
  at release (Q-MEM-2, MEM-005), so the difference is observable only through `weak`.
- Semantics: a `weak` read may return none for an object whose last strong local is still in scope
  but no longer used. `Memory.KeepAlive(x)` is a use of `x` at that point.
- Negative case: a test that reads a weak reference after the last use of the only strong local
  observes none; with `KeepAlive` placed after the read, it observes the object.
- Compatibility: none (counting is new).
- Reversal: if users report confusion in practice, switch to scope-end release; the ownership pass
  changes one placement rule.

**Q-MEM-2 — Runtime handles at reclamation.** **ACCEPTED DESIGN, changed from the revision-2
recommendation.** When a counted standard-library object that owns an operating-system handle is
reclaimed without having been disposed, the runtime closes the handle. It does not flush, does not
call `Dispose`, and runs no user code.
- Reason: a long-running main domain would otherwise exhaust operating-system handles. Closing a
  handle is runtime code, so invariant 14 (no user code at release) holds.
- Rules: OS handle types in the standard library are unbuffered; buffering lives in separate writer
  objects whose `Dispose` flushes. A buffered writer reclaimed with unflushed data discards it, and
  the development profile reports "handle reclaimed without Dispose" with the creation site. Region
  objects never own handles (heap invariant H3), so the rule applies to counted objects only.
- Negative case: no `Dispose` method is ever invoked by reclamation; a test type whose `Dispose`
  increments a counter shows zero calls after its last reference is dropped.
- Reversal: none expected.

**Q-MEM-3 — Cycles.** **ACCEPTED DESIGN** (effective together with Q-MEM-0). Strong cycles are the
programmer's responsibility; `weak` fields; leak and cycle reports in the development profile; no
automatic collector (MEM-009 stays DEFERRED).
- Semantics: master plan §5.8. A cycle lives until its domain ends; never a dangling reference.
- Reversal: MEM-009 is reopened if, after counting ships, more than one of the repository's own
  representative programs needs a cycle that `weak` cannot express.

### Object model

**Q-OBJ-1 — Count word in the B1 header.** **CONDITIONAL.** Descriptor pointer stays at offset 0
(approved). The count word follows at offset 8: H12 (29-bit count plus 3 flag bits, first field at
`align_up(12, a)`) is the primary candidate, H16 the fallback, H8 (B1 as written) if counting is
rejected. The choice is made by MEM-EXP-1 with the thresholds of §5.C.
- Count overflow: a count that would exceed its width saturates to the immortal sentinel: the object
  leaks, never frees early.
- B1 stage 1 proceeds now with the first-field offset written as the named constant `H`.

**Q-OBJ-2 — Virtual properties.** **ACCEPTED DESIGN.** `virtual`, `override`, `abstract` apply to
methods and to property accessors (B1 U9 answered differently from B1's own proposal).
- Negative cases: a virtual property read or written on `this` inside a constructor is rejected like
  B1 N11; overriding only one accessor of a property is allowed; an override must keep the accessor
  set (adding a setter is an error).

**Q-OBJ-3 — B2 before B1.** **ACCEPTED DESIGN.** Constructor overloads and `this(…)` land before
inheritance.
- Contract added to B2: field initializers run exactly once, in the constructor that does not
  delegate with `this(…)`; a constructor that delegates runs its body after the target returns; a
  delegation cycle is a compile error; B1's `this` rule applies across the delegation chain.
- Negative cases: `this(…)` and `base(…)` on one constructor; delegation cycle; ambiguous overload.

**Q-OBJ-4 — `base` reserved.** **ACCEPTED DESIGN.** Reserved keyword from the inheritance change.
- Migration: an identifier named `base` gets a diagnostic suggesting a rename. None exists in the
  repository's sources.

### Language

**Q-LANG-1 — `T?` and `null`.** **ACCEPTED DESIGN.**
- `T?` denotes exactly `Option<T>` (same type identity; they are interchangeable everywhere).
- `null` is a target-typed literal valid only where the expected type is `Option<U>`; elsewhere it is
  an error ("`null` requires an optional type").
- `a ?? b`: `a : Option<T>`; `b : T` gives `T`, `b : Option<T>` gives `Option<T>`; `b` is evaluated
  only when `a` is `None`.
- `a?.M`: `a : Option<T>`; result `Option<U>` where `U` is the member type; flattened when `U` is
  already `Option<V>`; `void` members allowed as statements only.
- `a == null` and `a != null` test the case. No flow narrowing: `if (a != null) a.Length` is an error;
  use `if (a is T value)` (LANG-004) or `??`.
- `Option<reference>` uses the null-pointer niche: one word.
- No implicit null anywhere else; non-optional types stay non-null. Specification chapter 00's
  "no implicit null" holds; spelling changes only.
- Compatibility: additive.

**Q-LANG-2 — Struct receivers and `ref`.** **ACCEPTED DESIGN (breaking).**
- An instance method on a struct receives `this` by reference when the receiver is a variable: a
  local, a parameter, an array element, or a field reachable through variables. Otherwise (a call
  result, a `List<T>` indexer result, a property result) it receives a temporary copy, as in C#.
- Diagnostic: assigning to a field of a non-variable struct receiver (`list[i].x = 1`) is an error,
  as in C# CS1612. Calling a method on a non-variable receiver is allowed (copy semantics).
- `ref` parameters: call site writes `ref`; the argument must be a variable; a `ref` cannot be
  stored, captured, returned, or used in `async` functions; a `ref` to a field of a counted object
  is guaranteed by the guarantor rule G1 for the duration of the call.
- Migration: one release note; a diagnostic is not possible for the semantic change itself, so the
  change ships with the repository's examples and tests audited for reliance on copy semantics.

**Q-LANG-3 — Real literal default.** **ACCEPTED DESIGN (breaking).** An unsuffixed real literal is
`double`; `f`/`F` gives `float`. `1e9` is `double`. `float x = 1.5;` becomes an error with the
fix-it "use `1.5f`". Repository examples and standard library migrate in the same change.

**Q-LANG-4 — Static fields.** **ACCEPTED DESIGN.** One copy per domain.
- Constant initializers are part of a per-module static image copied at domain creation.
- A non-constant initializer runs lazily, per type, at the first access in that domain, guarded by an
  initialization flag; a re-entrant access during initialization is a controlled runtime failure.
- Statics are owned storage of the domain and released with it. They are hot-reload state roots.
- Negative cases: static field of a non-transferable type is still allowed (it never crosses);
  reading a static from another domain sees that domain's copy.

**Q-LANG-5 — Exceptions.** **MAINTAINER REQUIREMENT confirmed.** No exceptions; `Result`, `Option`,
`?`; runtime failures are defects handled by the failure contract (§4.A).

### Concurrency

**Q-CONC-0 — Domains as the permanent model.** **ACCEPTED DESIGN.** Master plan §8; no shared-memory
threads or locks in safe code.

**Q-CONC-1 — Domains alive when `Main` returns.** **ACCEPTED DESIGN, changed from the revision-2
recommendation.** Structured exit: the root scope requests cancellation of every task, then waits
for every domain to finish. No domain is abandoned.
- Reason: deterministic cleanup; matches structured concurrency (CONC-007); `using` scopes in
  workers complete.
- Consequence: a task that never observes cancellation keeps the process alive; this is a defect in
  that program. The process can still be terminated externally.
- Exit status: `Main`'s status unless a domain failed, in which case 1.
- Reversal: if hangs at exit are common in practice, add a configurable grace period in `Aster.toml`.

**Q-CONC-2 — FFI callback thread-local.** **ACCEPTED DESIGN.** One runtime-internal thread-local slot
holds the current domain only while a foreign call is in progress on that thread. It is not visible
to ASTER code and is not task state; the concurrency reference is amended in the implementing change.

**Q-CONC-3 — Unit of failure.** **ACCEPTED DESIGN.** A runtime failure terminates its domain. Full
contract in §4.A. This contract is now fixed, which authorizes the ownership pass's exemption of
failure edges (MEM-003).

**Q-CONC-4 — Reproducible `Parallel.Reduce`.** **ACCEPTED DESIGN (observable change).** Chunk
boundaries depend only on the input length: `chunks = min(length, 64)`, chunk `i` covers
`[i * length / chunks, (i + 1) * length / chunks)`. Partial results combine left to right in chunk
order. Worker count affects only scheduling. Same result on every machine for any combine function.

**Q-CONC-5 — I/O polling dependency.** **ACCEPTED DESIGN.** The runtime may depend on `mio`, pinned
to an exact version, for readiness and completion polling. Reason: an in-tree IOCP and epoll layer is
a large amount of `unsafe` code that the runtime would then own. Files use a blocking pool where the
operating system has no readiness model. Reversal: a licensing or maintenance problem with the crate.

## 4. The four contract corrections

### 4.A Failure contract (MEM-003 and Q-CONC-3)

Domain states: `Running` → `Failed` or `Completed` → `Released`.

1. **Detection.** A runtime failure (bounds, arithmetic, failed assertion, allocation limit,
   reported runtime error, foreign status non-zero) records the domain's first error, as the
   `ExecutionContext` does today. Generated code leaves the current turn through its existing failure
   edges. No unwinding through native frames.
2. **No user code after failure.** In a `Failed` domain no further turn starts. Suspended tasks are
   never resumed. `using` bodies and `Dispose` methods do not run. Drop glue does not run.
3. **Memory.** The domain heap (counted allocator pages, regions, arenas, weak cells, frames,
   statics) is released in bulk. This is safe because no reference crosses a domain (H4) and no code
   of the domain runs again (rule 2). Region marks and owned-region checkpoints left open by the
   failing turn are discarded with the pages.
4. **Ownership.** Because of rules 2 and 3, failure edges carry no `Release` instructions, and a
   store sequence interrupted by a failure leaves no observable state.
5. **Resources.** The runtime closes every operating-system handle in the domain's handle table
   without flushing. No user code runs.
6. **Propagation.** Every task of the failed domain completes as `Failed(TaskError)` with the
   domain's first error. In another domain, `await` or `Wait()` on such a task fails the awaiting
   domain; `TryWait()` and `await task.TryAwait()` return `Result<T, TaskError>` and do not.
   A `TaskScope` that owns the failed domain cancels its other children and reports the failure of
   the lowest-index failed child (with cancellation, which children fail can depend on timing).
   Channel endpoints owned by the domain are closed; peers observe `Closed`.
7. **Main domain.** A failure in the main domain requests cancellation of every worker domain,
   waits for them (Q-CONC-1), prints the first error, and exits with status 1.
8. **Callbacks.** A failure inside a callback invoked by native code records the error; the callback
   returns its declared default; native code continues until the foreign call returns, at which
   point generated code observes the failure and the domain fails. Native work done meanwhile is
   outside ASTER's guarantees. A foreign function that never returns is unrecoverable.
9. **Runtime defects.** A Rust panic inside the runtime is not a runtime failure: entry points stay
   panic-free, and a panic that escapes in a worker thread is caught at the thread boundary and turns
   the domain into `Failed` with an internal-error message; in the main thread it aborts the process.
10. **Abnormal process termination.** The operating system reclaims everything. No guarantee about
    unflushed buffers.

Tests: failure in each of a sync call, an `async` step, a callback, and a `using` body; supervisor
survives a child failure through `TryWait`; no `Dispose` call after failure; handle closed after
failure; poisoning allocator shows no access to the failed domain's pages after release.

### 4.B Store sequence

For every store of a reference-bearing value `v` into a slot `s` owned by a base `b`:

| Step | Operation | Rule |
| --- | --- | --- |
| 1 | Evaluate and guarantee the base | `b` must have a guarantor (G1) that lives through step 6. A base obtained by a load from a mutable location is retained first |
| 2 | Checks | Bounds, version, and capacity checks run here, before any count changes |
| 3 | Obtain the new value | If `v` is owned (allocation, call result, consumed local), it is transferred. If borrowed, `Retain(v)` **here**, before step 4 |
| 4 | Read the old value | `old = load s` (a bit copy; no count change) |
| 5 | Write | `s = v` |
| 6 | Release the old value | `Release(old)` (or drop glue for an aggregate) |

Consequences: self-assignment (`x.f = x.f`) retains before it releases; aliases (`a.f = b.f` with
`a == b`) are the same case; no path releases `v` before it is stored. `Release` is an invalidating
instruction for every borrowed value obtained from a slot (it may free anything reachable), so a
borrowed value may not be used after a `Release` in the same window.

Per kind:
- **Fields and statics:** as above.
- **Array elements:** step 2 is the bounds check.
- **`List<T>.Set`, `Dictionary.Set`:** the caller performs steps 1 to 3 and passes an owned value;
  the runtime performs 4 to 6 using the element's drop glue. `Add` and `Insert` transfer an owned
  value; growth moves bits. `RemoveAt` and `Remove` first restore the collection's own invariants
  (shift, length), then release the removed element. `Clear` sets the length to zero first, then
  releases elements.
- **Interface values:** steps apply to the object word; the table word is copied.
- **Structs and enums with references:** step 3 is copy glue on `v` when borrowed; step 4 copies the
  old aggregate to a temporary; step 6 is drop glue on that temporary, selected by its tag for enums.
- **Closures:** the environment word is the reference.
- **Owned locals:** the same sequence; a borrowed local needs no count operation.

MIR tests:
- Positive: self-assignment of a field, array element, list element, interface field, struct field
  with a reference, closure field; each run under the poisoning allocator with final live bytes zero.
- Negative (validator rejects): `Release(old)` before `Retain(v)` when `v` was loaded from the same
  slot; use of a slot-borrowed value after any `Release`; store of a borrowed value without `Retain`;
  `Release` of a value not owned on that path; double `Release` on one path; store into a counted
  object of a region-classified value (H1).

### 4.C MEM-EXP-1 design and thresholds

**Configurations** (all built from one branch; same source programs; every comparison changes one
factor):

| Config | Header | Escaping allocations | Counting |
| --- | --- | --- | --- |
| C0 | none (today) | Persistent bump arena | none |
| C1 | H8 written, unused | Persistent bump arena | none |
| C2 | H12 written, unused | Persistent bump arena | none |
| C3 | H16 written, unused | Persistent bump arena | none |
| C4 | best of C2/C3 | size-class allocator, never freed | none |
| C5 | same | size-class allocator | full rules, elision disabled |
| C6 | same | size-class allocator | full rules, elision enabled |

Factors: C1 versus C0 is the B1 header alone; C2 and C3 versus C1 is the extra word; C4 versus the
same header in C2 or C3 is the allocator; C5 versus C4 is naive counting; C6 versus C5 is elision.

**Measured:** class A (checksums, allocation counts, final and peak live bytes, count operations
emitted and executed, MIR instruction counts, validator findings) and class B (wall time per phase,
resident memory) on the BENCH-001 suite and on `benchmarks/memory`; front-end and validation time on
`frontend_large`.

**Thresholds, fixed now, before any run.** Class B values are medians on one reference machine, A and
B interleaved, at least seven samples.

| Gate | Threshold | Kind |
| --- | --- | --- |
| Correctness | Identical checksums in all configurations; zero validator findings; whole test suite passes under the poisoning allocator in C6 | hard |
| Long-running memory | In C6, final live bytes at 10N are at most final live bytes at N plus 64 KiB, for `longrun_frame`, `longrun_request`, `cache_churn` | hard |
| Tier-1 preservation | `benchmarks/memory` class A values identical between C0 and C6 except allocation sizes changed by the header | hard |
| Header choice | H12 is chosen over H16 unless C2's time exceeds C3's by more than 2% median on the object workloads | selection |
| Header cost | C2 (or C3) time at most 1.05× C1 on every workload | hard |
| Execution time | C6 at most 1.15× C0 on each object-heavy workload (`longrun_frame`, `longrun_request`, `object_graph`, `interface_dispatch`, `accessor_heavy`); geometric mean over the suite at most 1.10× | hard |
| Elision effectiveness | Executed count operations in C6 at most 50% of C5 | soft: failing it blocks merging until elision is improved, not the direction |
| Compile time | Front end plus MIR passes plus validation in C6 at most 1.20× C0 on `frontend_large` | hard |
| Peak memory | Peak live bytes of C6 at most 1.6× C0 on `object_graph` (header growth on small objects) | hard |

If a hard gate fails for H12 and H16, apply the Q-MEM-0 fallback. The report is a document in
`docs/proposals/` with the raw class A values and the class B medians and spreads.

### 4.D ARCH-005 and CONC-002 are independent

Revision 2 listed ARCH-005 as needing CONC-002. That is wrong. Today a worker calls
`PreparedProgram::prepare_with_foreign_registry` itself (`worker_pool.rs:959-965`), so the scheduler
calls into code generation. Moving the scheduler to `aster-exec` (which may not depend on Cranelift)
requires an injected **code provider**: an interface the backend implements that yields entry
addresses for a domain. With per-worker compilation the provider compiles per worker; with a shared
module (CONC-002) it returns addresses of one module; with AOT it returns linked addresses. The
extraction is valid under all three, so ARCH-005 does not depend on CONC-002. CONC-EXP-1 is still
scheduled first because it is cheap and its answer shapes the provider's lifetime rules. The
roadmap and tasks are corrected accordingly.

## 5. Status of every ID

| ID | Status | Contract additions in this closure |
| --- | --- | --- |
| BENCH-001 | ACCEPTED DESIGN; task T-BENCH-1 **IMPLEMENTED** (see 07-tasks.md) | — |
| ARCH-001 | **IMPLEMENTED** (T-ARCH-1, 2026-11-10) | Contexts by owner, name, and parameter types; nodes by pre-order ordinal; spans kept only for diagnostics |
| ARCH-002 | **IMPLEMENTED** (T-ARCH-2a to 2d, 2026-11-10) | All backend-neutral checks in `aster-mir`; phased per-pass checks in debug builds; backend keeps lowering-capability checks only |
| ARCH-003 | ACCEPTED DESIGN | — |
| ARCH-004 | ACCEPTED DESIGN | — |
| ARCH-005 | ACCEPTED DESIGN | Code-provider interface (§4.D) |
| ARCH-006 | ACCEPTED DESIGN | Fingerprint = hash of declaration text after normalization plus the fingerprints of declarations it references |
| ARCH-007 | ACCEPTED DESIGN | Code format `A` + four digits, grouped by layer (`A1xxx` syntax, `A2xxx` semantic, `A3xxx` lowering, `A4xxx` backend); codes are never reused |
| MEM-001 | CONDITIONAL (MEM-EXP-1) | §4.C |
| MEM-002 | CONDITIONAL (MEM-EXP-1) | Q-OBJ-1; saturation to immortal |
| MEM-003 | CONDITIONAL (MEM-EXP-1); failure-edge exemption authorized by §4.A | §4.B |
| MEM-004 | CONDITIONAL (with MEM-001) | Weak count saturates to an immortal cell |
| MEM-005 | ACCEPTED DESIGN | Q-MEM-2 safety-net close for runtime handles |
| MEM-006 | CONDITIONAL (MEM-EXP-2: allocation fast path at most 1.02× with the governor enabled) | Default aggregate budget = the existing AARM-2C2 Auto policy |
| MEM-007 | CONDITIONAL (with MEM-001) | §4.B collection rules |
| MEM-008 | CONDITIONAL (PERF-EXP-1) | — |
| MEM-009 | DEFERRED | Reopening: Q-MEM-3 reversal criterion |
| OBJ-001 | ACCEPTED DESIGN | §4.C configurations |
| OBJ-002 | ACCEPTED DESIGN | U1, U2, U5, U6, U8, U10 as in master plan §6.4 |
| OBJ-003 | ACCEPTED DESIGN | Q-OBJ-2 |
| OBJ-004 | ACCEPTED DESIGN; U3 lookup CONDITIONAL (search unless an indexed table is measured at least 5% faster on `interface_dispatch` through a base type) | `as T` yields `Option<T>` (spelled `T?` under Q-LANG-1) |
| OBJ-005 | ACCEPTED DESIGN | Q-OBJ-3 |
| OBJ-006 | ACCEPTED DESIGN | — |
| LANG-001 | ACCEPTED DESIGN; escaping environments CONDITIONAL on MEM-001 (fallback: Persistent until domain end, as today) | Capture by reference; one closure type per signature; recursion guard treats indirect calls as cyclic |
| LANG-002 | MAINTAINER REQUIREMENT (Q-LANG-5) | Failure carries source position in the development profile |
| LANG-003 | ACCEPTED DESIGN | Q-LANG-1 |
| LANG-004 | ACCEPTED DESIGN | Integer, char, string, enum subjects; duplicate constant case is an error; string cases compare ordinal UTF-8; `default` required unless exhaustive (enums only) |
| LANG-005 | ACCEPTED DESIGN | `init` accessors assignable only in a constructor or an object initializer |
| LANG-006 | ACCEPTED DESIGN | Q-LANG-4 |
| LANG-007 | ACCEPTED DESIGN | — |
| LANG-008 | ACCEPTED DESIGN | Initializer assignments run after the constructor, in source order |
| LANG-009 | ACCEPTED DESIGN | Q-LANG-2 |
| LANG-010 | ACCEPTED DESIGN | Equality is component-wise when every component is comparable; no arity limit |
| LANG-011 | ACCEPTED DESIGN | Overloadable: unary `- ! ~`, binary `+ - * / % & | ^ << >> == != < > <= >=`; `==` with `!=` and `<` with `>` and `<=` with `>=` declared in pairs; `public static`, one operand of the declaring type; not usable through generic constraints |
| LANG-012 | ACCEPTED DESIGN | — |
| LANG-013 | ACCEPTED DESIGN | — |
| LANG-014 | ACCEPTED DESIGN; `yield` DEFERRED until CONC-003 | — |
| LANG-015 | ACCEPTED DESIGN | — |
| LANG-016 | ACCEPTED DESIGN | Q-LANG-3 |
| LANG-017 | ACCEPTED DESIGN | Digit separators follow C# (`_` between digits, allowed after `0x`/`0b`); an exponent makes the literal real |
| LANG-018 | ACCEPTED DESIGN | `args` excludes the program name; invalid Unicode in an argument is replaced with U+FFFD; the `int` result is passed to the operating system unchanged (POSIX observes the low 8 bits); `void` exits 0; failure exits 1 |
| CONC-001 | ACCEPTED DESIGN | §4.A; Q-CONC-1 |
| CONC-002 | **IMPLEMENTED** (CONC-EXP-1 PASS; adopted by default in T-CONC-2, 2026-11-10) | Fallback no longer needed; prototype behind the `shared-jit-experiment` feature |
| CONC-003 | ACCEPTED DESIGN; depends on MEM-003. Fallback if MEM-001 fails: frames and their references live in a per-task arena released when the task completes | — |
| CONC-004 | ACCEPTED DESIGN; stealing and compensation threads CONDITIONAL (CONC-EXP-2) | Turn budget: a domain yields its thread after 64 consecutive turns when another domain is ready; helping `Wait()` nests at most 4 deep |
| CONC-005 | ACCEPTED DESIGN | — |
| CONC-005b | DEFERRED | Reopen when a workload shows copy cost above 20% of its run time |
| CONC-006 | ACCEPTED DESIGN | Capacity at least 1, explicit; `Send` on a closed channel returns `Result.Error`; `Receive` on a closed empty channel returns `None` |
| CONC-007 | ACCEPTED DESIGN | §4.A rule 6 |
| CONC-008 | ACCEPTED DESIGN | Q-CONC-5 |
| CONC-009 | ACCEPTED DESIGN | Q-CONC-4 |
| CONC-010 | DEFERRED | Reopen if a workload needs shared read-only data larger than its transfer budget |
| PERF-001 | ACCEPTED DESIGN; size and startup reported, not gated | — |
| PERF-002 | CONDITIONAL (PERF-EXP-1: at least 10% median gain on `accessor_heavy` with at most 10% front-end time increase on `frontend_large`) | — |
| PERF-003 | ACCEPTED DESIGN | — |
| PERF-004 | DEFERRED | Reopen after AOT if a workload's JIT time exceeds AOT time by more than 20% |
| PERF-005 | ACCEPTED DESIGN | — |
| HOT-001 to HOT-004 | ACCEPTED DESIGN | Global safe-point timeout: 2 seconds, configurable in the development profile |
| PLAT-001 | ACCEPTED DESIGN | `CString` construction fails with `Result.Error` on an interior NUL; foreign signatures accept only types listed in the FFI reference |
| PLAT-002 | ACCEPTED DESIGN | — |
| PLAT-003 | DEFERRED | Reopen after a stable manifest and compatibility policy for 1.0 |
| PLAT-004 | ACCEPTED DESIGN for macOS and arm64; WebAssembly and mobile DEFERRED until a second backend is justified | — |
| TOOL-001 to TOOL-004 | ACCEPTED DESIGN | — |
| DOC-001 | ACCEPTED DESIGN; task T-DOC-1 **IMPLEMENTED** | — |

No contradiction remains between accepted entries: inheritance does not depend on counting; every
breaking item is accepted explicitly and lands as its own change; failure semantics are fixed before
ownership; the scheduler extraction no longer waits for the shared-module experiment.
