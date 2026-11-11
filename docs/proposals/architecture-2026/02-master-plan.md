# B. Architecture master plan (PROPOSED, revision 2)

> **Status: PROPOSED. Not an approved architecture.** Decision IDs refer to the
> [decision register](03-decision-register.md). The current state is in the
> [repository map](01-repository-map.md). Revision 2 closes contract gaps found in review; it does
> not widen scope. Items that change accepted behavior are marked **PENDING MAINTAINER** and are not
> assumed anywhere else in this document.

## 1. The end state

ASTER is a native, statically typed, nominal, non-null-by-default language with C#-like syntax, its
own compiler and runtime, no tracing garbage collector, deterministic reclamation of acyclic data,
isolated-by-default concurrency with general `async`/`await`, an ahead-of-time compiler for
distribution, and a JIT for the development loop with state-preserving hot reload.

It keeps the C# surface a programmer uses daily and rejects the parts that require a managed
runtime: universal `object`, boxing, runtime reflection, exceptions as control flow, finalizers,
implicit null.

## 2. Goals as measurements, not promises

Revision 1 listed numeric targets. They were not derived from any measurement and are withdrawn.
This section separates three kinds of statement.

### 2.1 Properties that hold by construction (testable deterministically)

| Property | Why it holds | Test |
| --- | --- | --- |
| No stop-the-world heap scan | No tracing collector exists | Absence, by design review |
| Acyclic garbage on the counted heap is freed at a point fixed by the compiled program | MEM-003 | Final live bytes of a steady-state workload are equal at N and 10N iterations |
| No data race in safe code | One running thread per domain; nothing mutable shared (CONC-001) | Validator and semantic rules; stress suites |
| No thread per task | Tasks are frames, not threads (CONC-003) | Thread count independent of task count |
| Production builds contain no reload indirection | Profile-specific lowering (HOT-001) | Deterministic code-shape test |
| Structs and enums have no header | Layout rule | Layout tests |

Limits of these properties, stated here so they are not read as more than they are:

- **Release cost is proportional to what is freed.** Dropping the last reference to a large graph
  frees the whole graph at that point. There is no global pause, but there can be a local latency
  spike. A budgeted deferred-free queue is possible and is DEFERRED, because it weakens the fixed
  release point.
- **Cyclic garbage is not freed before its domain ends** (MEM-004).
- **Fragmentation** of a non-moving heap is bounded by size classes, not eliminated.

### 2.2 Quantities to measure before any target exists

No value is proposed. For each quantity the first step is a baseline on the reference machine
(BENCH-001). A threshold exists only when the maintainer records one before the experiment that
depends on it.

| Goal in the brief | Quantity | Workload | Status |
| --- | --- | --- | --- |
| Fast execution | Wall time by phase; executed instructions where available | whole suite | No baseline yet |
| Light memory | Peak and final live bytes; reserved capacity; process resident memory | whole suite | No baseline yet |
| Light objects | Bytes per instance for the B1 worked examples under each header variant | layout tests | Computable now (§6.2) |
| Fast startup | Process start to `Main` | AOT smoke test | Not measurable until PERF-001 |
| Small binaries | Linked size of minimal programs | AOT smoke test | Not measurable until PERF-001 |
| Fast development loop | Edit to running code | `frontend_large` | Baseline measurable now (whole-program) |
| Cheap tasks | Bytes per suspended task; spawn time | `task_swarm` | Only the restricted form is measurable now |
| Multicore scaling | Speedup by worker count | `parallel_scaling` | Measurable now through the research worker-count entry points |

### 2.3 External comparison

Comparing against the same program in C# or Rust is informational. It is never a gate and never a
guarantee: different runtimes, different warm-up, different machines.

"Any application" is bounded explicitly in §11.

## 3. Invariants

Kept from the current architecture (verified as real today):

1. Semantic analysis is the only authority for names, types, overloads, and call targets.
2. HIR is typed and backend-independent. MIR is typed, control-flow explicit, backend-independent.
3. The backend receives validated MIR and rejects what it does not support, before generating code.
4. The runtime depends on no compiler or backend type.
5. Generics are concrete. No erasure, no implicit boxing, no universal root object.
6. No tracing GC, no moving of live objects, stable references.
7. Domains (execution contexts) are isolated; nothing mutable is shared implicitly.
8. No hidden global mutable state. One authority per decision.
9. Failure is controlled. Safe code has no undefined behavior.

Added by this plan:

10. **Every lifetime operation is explicit MIR**, inserted by a compiler pass and checked by a
    validator that does not share code with that pass.
11. **MIR validation is backend-neutral and lives beside MIR** (ARCH-002).
12. **Semantic identity is structural, not positional** (ARCH-001).
13. **Production pays nothing for development features** (HOT-001).
14. **No user code runs implicitly at a release point.** Reclaiming memory never calls into the
    program (MEM-005). This keeps reclamation unobservable except through `weak`.
15. **Fail closed in classification.** When an analysis cannot prove the cheaper storage class, the
    value takes the safer one. A wrong proof must be a rejected program or a slower program, never a
    dangling reference.

## 4. Current versus proposed

| Area | Today | Proposed |
| --- | --- | --- |
| Memory | Two arenas per execution; escaping values live until the run ends | Scalar replacement, regions for proven-local values, a per-domain counted heap for the rest (MEM-001) |
| Objects | Headerless | Descriptor pointer at offset 0 (B1, approved); a count word as a **proposed amendment** (MEM-002) |
| Classes | No inheritance, one constructor | B1 as proposed, with resolved open points (OBJ-002 to OBJ-004) |
| Functions as values | None | Closures as `{code, environment}` (LANG-001) |
| Resources | None | Explicit `Dispose` through `using`; no implicit disposal (MEM-005) |
| Concurrency | FIFO thread pool, scalar transfer, one async shape | Domains as actors, general stackless `async`, structured scopes, channels, I/O reactor (CONC-001 to CONC-008) |
| Backend | JIT only, per-worker recompilation | One shared code module; AOT (PERF-001, CONC-002) |
| Optimization | Scalar and CFG cleanup | MIR inliner and devirtualizer (PERF-002) |
| Compiler identity | Span-keyed side tables, whole-program | Stable IDs, incremental reuse (ARCH-001, ARCH-006) |
| Dev loop | Recompile and restart | Hot reload through development-only indirection (HOT-001 to HOT-004) |
| FFI | Scalar, host-registered only | C ABI declarations with libraries, pointers in `unsafe`, structs, callbacks (PLAT-001) |

## 5. Memory architecture (MEM)

### 5.1 Problem

Safe and fast for one bounded execution; unusable for a long-running one. Any value that escapes its
function lives until the run ends; collection growth leaks the old buffer; a loop containing a call
gets no fine reclaim (map §3.6).

### 5.2 Alternatives

| Strategy | Fit to C# aliasing | Run-time cost | Cycles | Verdict |
| --- | --- | --- | --- | --- |
| Ownership and borrowing in the surface language | Poor | None | N/A | Rejected as the default; used inside the compiler |
| Regions only | Good surface, wrong lifetime for long-lived mutable state | Very low | Trivial | Kept as tier 1, insufficient alone |
| Tracing GC | Excellent | Pauses or barriers, root maps | Solved | Excluded by the maintainer's decision |
| Naive reference counting | Good | High | Leak | Rejected |
| **Counting with compile-time elision, non-atomic per domain, over regions** | Good | To be measured | Explicit `weak` | **Recommended, subject to MEM-EXP-1** |
| Runtime-checked generational handles | Good | A check per dereference | Explicit free | Rejected: turns leaks into run-time failures |

Two existing properties make counting cheaper here than usual: domains are isolated, so counts are
non-atomic; escape analysis already exempts proven-local values. Whether that is cheap **enough** is
unknown until measured.

### 5.3 Storage classes

Every heap object has exactly one storage class, fixed when it is allocated. **Nothing is promoted or
moved at run time.** "Promotion" in this design is always a compile-time classification of the
allocation site.

| Class | Lifetime authority | Reclaimed | Count operations |
| --- | --- | --- | --- |
| Eliminated (tier 0) | Local object elimination | Never allocated | None |
| Region (tier 1) | Escape analysis and the existing region proofs | Bulk rewind of the region; no per-object work | Execute as no-ops |
| Counted (tier 2) | The ownership pass | When the count reaches zero | Real |
| Immortal | The module or the session | Never during the domain | Execute as no-ops |

The count word of a region or immortal object holds a reserved sentinel. Retain and release test for
it, so an operation that reaches such an object is harmless. Correctness does not depend on that
test (the rules below make it unnecessary); it is defense in depth.

### 5.4 Heap invariants

- **H1. A counted or immortal object never holds a strong or weak reference to a region object.**
  Any store that could create one makes the stored object's allocation site Counted. This is
  today's `Stored` and `Contained` escape rule, kept as is.
- **H2. A region object holds only unowned references.** Region rewind runs no per-object code, so a
  region object cannot own a count. Each reference stored in a region object must be backed by a
  **pin** that outlives the region: a borrowed parameter of the function that owns the region, an
  immortal, an object of the same or an enclosing region, or an owned local whose release the
  ownership pass places after the region exit. If the compiler cannot place a pin (for example, an
  unbounded number of stores in a loop of values loaded from mutable fields), the containing
  allocation site is classified Counted.
- **H3. A region object owns no resource that needs per-object release.** Its collection buffers
  live in the same region. A collection header whose backing may grow outside the frame that owns
  its region is classified Counted. This replaces the run-time `birth_scope_depth` promotion for new
  code and removes the fragile invariant recorded in map §3.6.
- **H4. No reference crosses a domain.** Transfers copy (CONC-005).
- **H5. Only the allocator, retain, and release write the count word. Only the allocation sequence
  writes the descriptor word** (B1 rule, extended).

### 5.5 Ownership rules in MIR (MEM-003)

Every reference-typed MIR local is, at each program point, **owned** (it accounts for one count) or
**borrowed** (something else guarantees the object).

**Sources of an owned value**

- An allocation.
- The result of a call. Every function returns owned.
- A load that the pass upgrades with `Retain`.
- A successful weak read.

**Sources of a borrowed value**

- A parameter. The caller guarantees it for the whole call.
- A load from a field, element, or static, valid only inside its **borrow window**: the straight-line
  span up to the next instruction that may write that location or run unknown code. A use outside
  the window forces a `Retain` at the load.

**Guarantor rule (G1).** An argument passed as borrowed must be backed by something the callee cannot
release: an owned local of the caller, the caller's own borrowed parameter, an immortal, or a region
object. A value merely loaded from a mutable location is **not** a guarantor, because the callee may
overwrite that location. Passing `this.child` to a function therefore retains it for the call unless
a callee summary proves the location is not written. This is the classic counting defect; the rule
exists to exclude it and the validator checks it.

**Consuming operations.** Each transfers one count from an owned value, or retains a borrowed one:

- store into a field, element, static, closure environment, or async frame of a counted object;
- return;
- transfer to another domain (after the copy, the original is released normally).

**Store sequence.** `old = load slot; slot = new; Release(old)`. The release comes last, so
self-assignment and a drop that reads the slot are both safe.

**Release placement.** An owned local is released after its last use, or after the region exit when
it is a pin (H2). It is not held to the end of its lexical scope. The only way a program can observe
this is through `weak`; see §5.8 and the pending question Q-MEM-1.

**Balance rule.** On every path that ends in a normal return, each owned value is consumed exactly
once. **Failure paths are exempt**: a runtime failure terminates the domain (CONC-001, §8.6), whose
heap is then released in bulk, so no release is emitted on failure edges and none can be wrong.

**Aggregates.** A struct or enum with reference fields is owned or borrowed as a unit. Copying it
into storage runs generated **copy glue** (retain each reference field of the active case); dropping
it runs **drop glue**. Bitwise moves (growth of a list buffer, return through the hidden destination)
change no count.

**Interface values.** `{object, table}`. Ownership operations act on the object word. The table
pointer is immortal data.

**Collections.** A collection owns its elements. The runtime never interprets element layout: the
allocation site passes the element type's copy and drop glue, and the runtime calls them on insert,
overwrite, remove, clear, and drop. Reading an element yields a borrowed value under the load rule.

**Closures.** A closure value is `{code, environment}`. The environment holds the captured variables
and owns what they reference. Copying the closure retains the environment. A closure proven
non-escaping keeps its environment in a region under H2. Capturing `this` is a strong reference.

**Async frames.** A frame is a counted object in the domain of the task. Three rules:

- **No borrow survives a suspension.** Every reference live across an `await` is owned by the frame.
- **An async call retains its reference arguments into the frame at the call**, because the frame
  outlives the call. This is the one exception to "parameters are borrowed".
- **No region object is live across a suspension.** `await` is a region barrier, as the task
  intrinsics already are. `ref` parameters are not allowed in async functions.

**Statics.** A static field is owned storage in its domain. It is released with the domain.

**What becomes of the existing mechanisms**

- Temporary scopes and fine subregions: unchanged; they are the region class.
- Owned Persistent regions: the shape they prove (a fresh returned value that dies in the same
  block) is exactly "returned owned, released at last use" under counting. They stay until the
  suite shows counting matches them on `benchmarks/memory`; then they can be retired for counted
  types. Not decided here.
- The Persistent arena: remains for immortal data and for kinds not yet migrated during staging.

### 5.6 Guarantees against use after free

The claim is: **no instruction of a safe program reads or writes a freed counted object or a rewound
region object.** It rests on these obligations, each with an independent check.

| # | Obligation | Established by | Checked by |
| --- | --- | --- | --- |
| U1 | A counted object is freed only when no owned value and no owned slot refers to it | Balance rule; store sequence | MIR ownership validator (path-sensitive count balance) |
| U2 | A borrowed value is never used outside its guarantee | Borrow window; G1 | Validator: every borrowed use is dominated by a live guarantor with no intervening invalidating instruction |
| U3 | A region object is not referenced after its region exits | Escape analysis; existing region proofs | Existing backend-neutral region validation (moved by ARCH-002) |
| U4 | Nothing outside a region holds a reference into it | H1 | Validator: a store into a non-region object requires a counted or immortal source classification |
| U5 | A region object's references stay valid for the region's life | H2 pins | Validator: every reference stored into a region object has a pin whose release is after the region exit |
| U6 | No reference is live across a suspension without ownership | Async rules | Validator on the lowered state machine |
| U7 | A weak reference never yields a freed object | Weak cell cleared before the block is freed, in the same domain, with no user code between | Runtime unit tests; poisoning allocator |
| U8 | Runtime-held references (collection elements) are balanced | Glue calls on every mutating entry point | Runtime tests per entry point with a counting element type |

Beyond the validator:

- **Differential runs.** Elision disabled must give identical results and identical final live bytes.
- **Poisoning allocator** in the test profile: freed blocks are overwritten and quarantined.
- **Sanitizer runs** of the runtime.
- **Debug assertions**: a retain or release on a region object, or a count underflow, is a
  controlled failure in the development profile.

Residual risk is listed in the report: the validator is new code of comparable subtlety to the pass
it checks.

### 5.7 Memory reclamation, drop glue, and `IDisposable` are three different things (MEM-005)

| | What it is | Who writes it | When it runs | Observable |
| --- | --- | --- | --- | --- |
| **Reclamation** | Returning a block or a region to the allocator | Runtime | Count zero; region exit; domain teardown | No |
| **Drop glue** | Releasing the references and buffers an object owns | Compiler, per type | Immediately before reclaiming a **counted** object. Never for region objects (H2, H3 make it unnecessary). Not run object by object at domain teardown | No: it runs no user code |
| **`Dispose`** | Releasing an external resource | The programmer | **Only** when called: by `using` at scope exit, or explicitly | Yes |

**Losing the last reference never calls `Dispose`.** Reasons:

1. It would run user code at release points chosen by the optimizer, so behavior would change with
   elision, inlining, and compiler version.
2. It would run user code in the middle of a store (the store sequence releases the old value).
3. It would reintroduce finalizer problems B1 excludes: resurrection, failure during destruction,
   ordering between objects.
4. Region objects are reclaimed in bulk and could never honor it, so the same type would behave
   differently depending on a storage class the programmer cannot see.

Consequences and mitigations:

- A resource that is neither disposed nor still referenced is a **resource leak**. It is not a
  memory-safety problem.
- Operating-system handles created by the standard library are recorded in a per-domain handle
  table owned by the runtime. Domain teardown closes what is still open. That is runtime code, not
  `Dispose`.
- The compiler warns when a local of a type implementing `IDisposable` is created and neither bound
  by `using`, returned, stored, nor passed on.
- The development profile reports handles still open at domain teardown, with their creation site.

`using` contract:

- `Dispose` runs once at scope exit on every normal exit: fallthrough, `return`, `break`,
  `continue`, and `?` propagation.
- It does **not** run when the domain is terminating after a runtime failure. No user code runs
  after a failure; the handle table covers the operating-system side.
- Disposing twice (an explicit call and then `using`) is two ordinary calls; idempotence is the
  type's responsibility.

An alternative that needs the maintainer's choice (Q-MEM-2): let the runtime close a **standard
library** handle when its counted wrapper is reclaimed. It runs no user code, but it makes the
closing time depend on release placement. Recommendation: do not; keep teardown plus diagnostics.

### 5.8 Cycles and weak references (MEM-004)

**Behavior of a strong cycle.** It is never freed while its domain lives. It is freed with the
domain. It is a leak, never a dangling reference. For the main domain "with the domain" means at
process exit.

**What is deterministic.** Acyclic counted garbage is freed at the release point the compiled
program fixes. Nothing else is promised: not cyclic garbage, not the order between unrelated objects,
not the same release point across compiler versions or optimization levels.

**Weak references.**

- Syntax: a `weak` modifier on a field whose type is a nullable class reference. Whether that type
  is spelled `Option<T>` or `T?` follows LANG-003 and is independent of this decision.
- Reading yields an owned strong reference or none. The check and the retain are one step within the
  domain, so the result cannot be freed between them.
- Storing into a weak field is an escape: the target is Counted (H1 covers weak as well as strong).
- Mechanism: the first weak reference to an object allocates a **weak cell** `{target, weak count}`
  and sets a header flag; a per-domain table maps the object to its cell. Reclaiming a flagged object
  clears the cell first. The cell is freed when the target is gone and the weak count is zero.
  Objects never weakly referenced pay nothing but the flag test at reclamation.
- A weak reference does not keep its target alive and does not take part in ownership balance.

**Interaction with release at last use.** An object may be freed before the end of the scope of a
local that names it, if that local is not used again. A weak read can observe that. `Memory.KeepAlive(x)`
extends the last use explicitly. Holding to scope end instead is possible at a cost; it is the
pending question Q-MEM-1.

**Leak detection.**

| Tool | Profile | What it does | What it cannot do |
| --- | --- | --- | --- |
| Live counters (`Memory.LiveBytes`, `Memory.LiveObjects`) | All | Constant-time counters | Explain a leak |
| Teardown report | Development | Lists counted objects still alive after statics and the root frame are released, by type and allocation site | Distinguish a cycle from another unbalanced reference without the next tool |
| `Memory.DetectCycles()` | Development and tests | Walks the domain heap using per-type reference maps and reports strongly connected groups unreachable from roots | Free anything; run in release builds |
| Compile-time warning | All | Closure that captures `this` and is stored in a field of the same object | General cycle detection, which is undecidable from types (a linked list has a cyclic type and acyclic instances) |

Heap walking needs object enumeration and reference maps. Both are development-profile features and
are also what hot reload needs (HOT-003).

**No automatic cycle collector.** A local trial-deletion collector is recorded as MEM-009, DEFERRED,
and is a collector; it is not assumed anywhere.

### 5.9 Specific cases

| Case | Treatment |
| --- | --- |
| Strings | Immutable. Count in the existing header. Literals immortal. Operations keep copying; the "always copy" property becomes a documented runtime contract with a test, because region proofs depend on it |
| Arrays | Count in the header. Element glue for reference elements |
| `List`, `Dictionary`, `StringBuilder` | The header uniquely owns its buffer; growth and rebuild free the old one |
| Native handles | A standard-library class implementing `IDisposable`, registered in the domain handle table |
| Cross-domain values | Copied (CONC-005). No count is shared |
| Aggregate budget | The gated governor becomes the default (MEM-006) |
| Hot reload | The heap belongs to the domain, not to a JIT module (HOT-003) |

### 5.10 What the programmer does explicitly

1. Marks back-references `weak`.
2. Uses `using` for resources.
3. Nothing else: no lifetimes, no ownership annotations.

### 5.11 The evidence gate

**MEM-EXP-1** builds the count word, a size-class allocator, and the ownership pass for class
references on an unmerged branch behind a compiler option, and reports on the BENCH-001 suite: wall
time, peak and final live bytes, count operations emitted, elided, and executed. The maintainer
records thresholds before the run. If they fail, the fallback is §6.2 variant H8 (B1 unchanged) and
a narrower long-running answer through explicit scoped arenas.

## 6. Object model (OBJ)

### 6.1 Position on B1

B1 is kept as the base. Its five approved decisions are preserved **without change**, including
"a pointer to its dynamic descriptor at offset zero".

This plan proposes exactly one amendment to B1's PROPOSED text (§4.1 layout rules) and resolves its
open points.

### 6.2 The header: a proposed amendment to B1 §4.1, compared formally

B1 §4.1 defines a header of `P` bytes. Counting needs a place for the count. The variants:

| | H8 (B1 as written) | H16 (amendment, simple) | H12 (amendment, packed) |
| --- | --- | --- | --- |
| Offset 0 | descriptor pointer | descriptor pointer | descriptor pointer |
| Offset 8 | first field | count (4), flags (4) | count with 3 flag bits (4) |
| First field at | `align_up(8, a)` | `align_up(16, a)` | `align_up(12, a)` |
| Approved decision 1 | preserved | preserved | preserved |
| B1 rules changed | none | 1, 2, 6 and the worked table | 1, 2, 6 and the worked table |
| Counting in the object | **impossible** | yes | yes; count is 29 bits and saturates to immortal |
| Dispatch cost | one load | one load | one load |
| `Empty` | 8 | 16 | 16 |
| `Counter { int }` (4 today) | 16 | 24 | 16 |
| `Base { long; byte }` (16 today) | 24 | 32 | 32 |
| `Derived : Base { byte }` | 32 | 40 | 40 |

Sizes follow B1's own rules (alignment 8, no reuse of base tail padding) and are computed, not
measured.

Variants considered and rejected:

- **Count in a side table keyed by address** with H8: every retain becomes a hash lookup.
- **32-bit descriptor index plus 32-bit count in 8 bytes**: smallest, but it replaces the descriptor
  pointer at offset zero, which is an approved decision. Not proposed.
- **Header only on classes that take part in a hierarchy or are counted**: the same class is region
  at one site and counted at another, so the layout must be uniform (this is B1's U6 argument).

**Recommendation:** H12, because it costs nothing over H8 for classes whose first field has
alignment 4 or less and one word otherwise. H16 is the fallback if packed flags prove awkward.

**Process, preserving B1's stages:**

1. B1 stage 1 (design acceptance) can proceed now with the first-field offset written as a named
   constant `H`, value decided at stage 3.
2. B1 stage 2 (experimental branch) implements `H` as a build parameter.
3. B1 stage 3 measures three configurations on the same suite: H8 without counting (B1 alone), H12
   with counting, H16 with counting. This is the same report as MEM-EXP-1.
4. If MEM-001 is rejected, B1 continues with H8 exactly as written. Nothing in B1 depends on this
   plan.

Strings, arrays, and collections are not class instances and are outside B1. Their count goes in
their existing runtime headers; they need no descriptor pointer in production.

### 6.3 Dependencies and contradictions between B1 and the new proposals

| # | B1 text | New proposal | Relation | Resolution |
| --- | --- | --- | --- | --- |
| 1 | §4.1 header is `P` bytes; worked table; P11, P12 | MEM-002 | **Amendment** to PROPOSED text | §6.2 |
| 2 | §4.1 "no generated code other than the allocation sequence stores to offset 0"; V2 | MEM-002 count word | Extension | H5: same rule for the count word, written only by retain, release, allocator |
| 3 | §4.2 descriptor contents (PROPOSED shape) | Drop glue pointer; development-only reference map | Extension | Added fields; nothing removed |
| 4 | §4.3 descriptors retained by the JIT module; objects never outlive it | Long-lived domains; hot reload | **Conflict in the development profile only** | Production and AOT: descriptors are immortal module data, B1 holds. Development: descriptors in session-stable storage (HOT-002). B1 §4.3 gains a profile note |
| 5 | §14 criterion 3: no task or worker runs module code after release | CONC-002 shared module | Dependency | The module is owned by the execution and outlives every domain; becomes a tested property |
| 6 | "B1 keeps the current per-invocation arena model"; no GC | MEM-001 | Independent | B1 does not need MEM-001. MEM-001 needs B1's header |
| 7 | Decision 3: one constructor in B1; B2 adds overloads | OBJ-005 proposes B2 first | **Ordering choice, PENDING MAINTAINER** | No content conflict. If B2 lands first, its record must define: field initializers run once, in the constructor that calls `base(…)`, not in one that delegates with `this(…)`; and the `this` rule applies across the delegation chain |
| 8 | U9: `virtual` on methods only | OBJ-003 adds properties | Proposed change to an open point | A virtual accessor is not this-confined, so a virtual property on `this` inside a constructor is rejected like N11. B1 case P8 stays valid only for non-virtual properties |
| 9 | U4: downcast yields `Option<Derived>` | OBJ-004 | Compatible | Result type is `Option<Derived>`. The `T?` spelling depends on LANG-003 and is not assumed |
| 10 | §6.3 `this` may not be captured during construction | LANG-001 closures | Compatible | New negative case: a lambda capturing `this` in a constructor |
| 11 | §6.3 an `async` method is not this-confined | CONC-003 | Compatible | Unchanged |
| 12 | §3, N19: class references are not transferable | CONC-005 | Compatible **after revision** | Revision 1 proposed moving class graphs; that is withdrawn to DEFERRED. Class references stay non-transferable |
| 13 | §9 "a virtual call is an escape of its receiver until devirtualized" | MEM-003 | Compatible | Escape decides the storage class; the receiver is still a borrowed argument under G1 |
| 14 | §9 local object elimination never materializes a header | MEM-002 | Compatible | An eliminated object has no count either |
| 15 | §1 no finalizers | MEM-005 | Compatible **after revision** | Revision 1 let drop glue call `Dispose`; withdrawn |
| 16 | `sealed` forbids derivation | LANG-004 exhaustiveness | Corrected | `sealed` does not make a hierarchy closed. Exhaustive `switch` applies to enums only. Closed-hierarchy devirtualization is a whole-program AOT fact, not a language rule |
| 17 | §1 no static fields, delegates, events in B1 | LANG-006, LANG-001 | Independent records | No change to B1 scope |
| 18 | §5 overload resolution collects candidates from bases | LANG-013 extension methods | Ordering | Extension methods are considered only after instance candidates of the whole chain |

No approved B1 decision is contradicted. One PROPOSED rule is amended (row 1) and one open point is
answered differently from B1's own proposal (row 8).

### 6.4 Open points U1 to U10

| ID | Recommendation | Note |
| --- | --- | --- |
| U1 | Accept rule B | |
| U2 | Accept the C# order | |
| U3 | Accept search first; fixed index only if measured | Experiment |
| U4 | `value is Derived d` as the primary form; `value as Derived` yields `Option<Derived>` | |
| U5 | Reserve `base` | Breaking for an identifier named `base`; none in the repository |
| U6 | Uniform header | Required by the storage-class argument in §6.2 |
| U7 | Closed generic base in the generics-in-hierarchies step | |
| U8 | Exact signatures | |
| U9 | Methods **and properties** | Differs from B1's proposal; see row 8 |
| U10 | Error | |

No universal `object` is introduced. Formatting and equality go through interfaces.

## 7. Language surface (LANG)

Rule: adopt what a C# programmer writes daily when the cost is contained; simplify where C# has two
ways; reject what needs a managed runtime. **No item below is assumed by any other section unless it
says so.**

### 7.1 Items that change accepted or existing behavior — PENDING MAINTAINER

| ID | Change | What it breaks | If not approved |
| --- | --- | --- | --- |
| LANG-003 | `T?` as `Option<T>`; `null` as `None` where `T?` is expected; `??`; `?.` | Nothing existing; it reverses the accepted "no `null`" in spelling | `Option<T>` stays the only spelling; `weak` fields and `as` use `Option<T>` |
| LANG-009 | Struct methods receive `this` by reference; `ref` parameters | Programs relying on a method mutating a copy (`reference/structs.md`) | Structs stay copy-on-call; mutation by returning a new value |
| LANG-016 | Unsuffixed real literal becomes `double` | Every program with `float x = 1.5;` needs `1.5f` | Stays `float`, as the specification currently says |
| OBJ-002 (U5) | `base` reserved | An identifier named `base` | Contextual keyword |
| MEM-002 | Class instances grow by the header | Tests and benchmarks that pin sizes or allocation bytes | H8 |
| LANG-006 | Static fields, one copy per domain | Nothing existing; differs from C# process-wide statics | No static fields |
| CONC-009 | `Parallel.Reduce` chunk plan independent of core count | Observable grouping for non-associative combines | Documented as machine-dependent |
| Q-MEM-1 | Release at last use rather than at scope end | Observable only through `weak` | Scope-end release, at a cost to be measured |

### 7.2 Additive items

| Feature | Proposal | ID | Cost |
| --- | --- | --- | --- |
| Lambdas, closures | C# lambda syntax; `Func<…>`/`Action<…>` as compiler-known function types | LANG-001 | New type kind through all layers |
| Events | Library type over a list of `Action<T>`; no keyword | — | Library |
| Errors | Keep `Result`/`Option`/`?`; no exceptions; positions in runtime failures | LANG-002 | Tooling |
| `switch` | Integer, char, string subjects; `is T x`; `when` guards | LANG-004 | Lowering |
| Auto-properties | `{ get; set; }`, `{ get; }`, `{ get; init; }` | LANG-005 | Desugaring |
| Constructors | Overloads, `this(…)` | OBJ-005 | Resolution exists |
| Object initializers | `new T(…) { X = 1 }` | LANG-008 | Desugaring |
| Structs and interfaces | Static satisfaction of constraints; no conversion to an interface value | LANG-007 | Keeps "no boxing" |
| Tuples | Anonymous structs, deconstruction | LANG-010 | Monomorphized struct |
| Operators, indexers | Statically resolved | LANG-011 | Resolution |
| Interface inheritance | Yes; default methods deferred | LANG-012 | Tables |
| Extension methods | C# syntax | LANG-013 | Lookup |
| Iteration | Pattern-based `foreach`; `yield` later | LANG-014 | |
| Reflection | None at run time; `nameof`; compiler-consumed attributes | LANG-015 | |
| Small gaps | `do`-`while`, `%=`, block comments, exponents, digit separators | LANG-017 | Trivial |
| Entry point | `Main(string[] args)`; exit status | LANG-018 | Host |

Not planned: exceptions, runtime reflection, universal `object`, boxing, finalizers, `out`, `goto`,
`dynamic`, LINQ query syntax, shared-memory threads with locks.

### 7.3 Examples (none compiles today)

Closures:

```csharp
// C#
var total = 0;
button.Clicked += (sender, e) => total += e.Amount;
```

```aster
// ASTER proposed (LANG-001)
int total = 0;
button.Clicked.Add(e => total += e.Amount);
```

Absence, today and with the pending sugar:

```aster
// ASTER today
Option<string> name = FindName(id);
int length = 0;
switch (name)
{
    case Some(value):
        length = value.Length;
    case None:
        length = 0;
}

// ASTER proposed only if LANG-003 is approved
string? name = FindName(id);
int length = name?.Length ?? 0;
```

Inheritance and resources, written without any pending item:

```aster
// ASTER proposed (OBJ-002, OBJ-003, MEM-004, MEM-005)
public abstract class Entity
{
    private int id;
    public weak Option<Scene> scene;
    protected Entity(int id) { this.id = id; }
    public abstract void Update(float dt);
}

public sealed class Enemy : Entity
{
    public Enemy(int id) : base(id) { }
    public override void Update(float dt) { }
}

public Result<string, IOError> ReadFirstLine(string path)
{
    using var file = File.Open(path)?;
    return file.ReadLine();
}
```

## 8. Concurrency and async (CONC)

### 8.1 Model

**A domain** owns a heap, statics, a handle table, timers, and a queue of ready tasks. **Exactly one
thread runs a domain at a time.** Tasks in a domain interleave only at `await`. Parallelism is more
domains. Values cross by transfer. This names and extends what `ExecutionContext` isolation already
is.

Alternatives (shared-memory threads with locks; stackful coroutines; ownership typing with
`Send`/`Sync`) are compared in the register under CONC-001; the choice is stackless `async` on
isolated domains.

### 8.2 Turns and reentrancy

- A **turn** is one task running from a resume point to its next suspension, its completion, or a
  failure. A turn is never interrupted by another task of the same domain.
- **A synchronous call never yields.** No other task of the domain runs during a call that contains
  no `await`. This is the atomicity guarantee.
- **State may change across an `await`.** While a task is suspended, other tasks of the same domain
  run and may mutate objects it holds. Invariants must be re-established after each `await`. The
  existing list-version check already turns "modified during `foreach`" into a controlled failure,
  including across an `await`.
- Between turns the domain's native stack holds no ASTER frame. Hot reload and code unloading rely
  on this (§10).

### 8.3 Tasks that do not `await`

- A task without a suspension point runs to completion in one turn and occupies its domain and one
  thread for that time. There is no preemption and no injected polling (kept from the accepted
  model).
- Other tasks of the same domain do not run meanwhile. Other domains are unaffected.
- CPU-bound work belongs in another domain (`Task.Run`). `Task.Yield()` ends the turn voluntarily.
- Cancellation of such a task is observable only where it calls `Task.IsCancellationRequested()`
  (existing contract).

### 8.4 Blocking

| Operation | Inside an `async` function | In synchronous code of the main domain | In synchronous code of a worker domain |
| --- | --- | --- | --- |
| `await task` | Suspends the task | Not available | Not available |
| `task.Wait()` on a task of **another** domain | Allowed; blocks the whole domain (a warning recommends `await`) | Blocks the main thread; the domain runs no other task | Blocks the worker thread; while blocked the thread may run ready work of **other** domains, to a bounded nesting depth, then parks |
| `task.Wait()` on an incomplete task of the **same** domain | Controlled failure: it could never complete | Same | Same |
| Synchronous host I/O | Blocks the domain and its thread | Same | Same; allowed once CONC-008 lands |

`Wait()` never runs other tasks of the calling domain. Doing so would break "a synchronous call
never yields".

The pool has one thread per core and no compensation threads in the first version. A program that
blocks every worker in synchronous I/O stalls; the asynchronous forms exist to avoid that. Whether
compensation is needed is decided by measurement.

### 8.5 Fairness

- Within a domain: ready tasks run in FIFO order of becoming ready.
- Across domains: each thread serves its ready domains in FIFO order; a domain yields its thread
  after a fixed budget of turns if others are ready; the injection queue is polled at a fixed
  interval so new work cannot be starved.
- No time slicing inside a turn. Fairness is at turn granularity only.
- Scheduling order is not part of the language contract. Public outcomes that are deterministic
  today (`WaitAll` result order, lowest-index failure) stay deterministic.

### 8.6 Failure and domain lifetime

- **A runtime failure terminates its domain.** Failures are defects (bounds, division by zero,
  failed assertion), not recoverable conditions, and a failed task may leave shared objects of its
  domain half-updated. Terminating the domain is the safe choice and lets the heap be released in
  bulk, which is why failure paths need no ownership operations (§5.5).
- All tasks of a failed domain end as failed. Tasks in other domains awaiting them observe the
  failure. `task.TryWait()` and its `await` form return `Result<T, TaskError>` so a supervisor
  survives the failure of a child domain. A plain `await` or `Wait()` propagates the failure, as
  today.
- A server isolates each request in its own domain; a defect in one request then costs that request.
- **Worker domain lifetime.** Created by `Task.Run`; destroyed when its root task has completed and
  its result has been transferred out. It is kept alive only by its own unfinished tasks. Nothing in
  another domain's heap can point into it; a `Task<T>` or channel endpoint held elsewhere refers to
  runtime-owned shared state with atomic counts, which is not ASTER heap.
- **Main domain lifetime.** Created before `Main`; when `Main` returns, the root scope is cancelled,
  tasks of the main domain run until they complete or observe cancellation at their next suspension,
  then the process exits. Worker domains still running at that point are abandoned: no `Dispose`
  runs in them, and the operating system reclaims their handles. Whether to wait for them instead is
  the pending question Q-CONC-1.
- A long-lived service or actor is an `async` function looping on a channel in its own domain. No
  separate actor construct is added.

### 8.7 Transfer

- **When:** `Task.Run` arguments, task results, channel messages.
- **What (first version):** scalars; structs and enums of transferable fields; strings; arrays,
  lists, and dictionaries of transferable elements. Sharing inside one transferred value is
  preserved in the copy.
- **What does not cross:** class instances, interface values, closures with non-transferable
  captures, tasks' frames, anything holding a native handle. This keeps B1 N19.
- **How:** the sender serializes into a message buffer owned by the runtime; the receiver
  materializes it into its own heap when it takes the message. Two copies. Page handoff and moving a
  unique graph are DEFERRED optimizations (CONC-005b).
- **Failure:** if the receiver's budget cannot hold the value, the operation fails
  deterministically on the side that requested it, before any partial value is visible.
- **Cost:** proportional to the size of the value, always explicit at a `Task.Run`, a task result,
  or a channel operation.

### 8.8 FFI callbacks

Native code calling back into ASTER is the one place where the "one thread per domain" rule could be
broken from outside. The contract:

| Situation | Rule |
| --- | --- |
| Callback invoked synchronously, on the same thread, during a foreign call made by a task | Allowed. It runs in that domain, inside that turn, as an ordinary nested call. It must be a synchronous function |
| Runtime failure inside a callback | Cannot unwind through native frames. The failure is recorded, the callback returns the default value declared for it, and the failure takes effect when the foreign call returns |
| Callback invoked on another thread, or after the foreign call returned | **Must not enter ASTER code.** The generated thunk detects it and does not run the body. For such APIs the callback is declared **posting**: the thunk copies its scalar arguments into a message for the owning domain and returns immediately with the declared default |
| Lifetime of the function pointer | First version: static functions only, so the code is immortal. A closure as a callback needs a disposable handle that pins its environment; later |

How the thunk finds its domain: through the user-data pointer when the native API provides one;
otherwise through one runtime-internal thread-local slot that is set only while a foreign call is in
progress on that thread. **That thread-local is a narrow exception to the current statement that no
thread-local task state exists** (`reference/concurrency.md`); it needs the maintainer's approval
(Q-CONC-2). Without it, callbacks are limited to APIs with a user-data pointer.

### 8.9 Components

| Component | Design | ID |
| --- | --- | --- |
| Shared code | One finalized module owned by the execution; workers receive entry addresses | CONC-002 |
| Scheduler crate | Out of the Cranelift crate | ARCH-005 |
| Async lowering | General state machine; one state per `await`; typed frame object | CONC-003 |
| Scheduler | Per-thread queues plus injection; stealing only if measured | CONC-004 |
| Channels | `Channel<T>` for transferable `T`, bounded | CONC-006 |
| Structured concurrency | `TaskScope`; cancellation flows down; first failure cancels siblings | CONC-007 |
| I/O | Reactor in the runtime | CONC-008 |
| Data parallelism | Existing `Parallel.*`; reproducible chunk plan (PENDING MAINTAINER) | CONC-009 |
| Shared immutable data | DEFERRED | CONC-010 |

Cost contracts: a suspended task costs its frame plus a task header and no stack; `await` on a
completed task is a branch; a cross-domain operation costs a transfer and a queue operation and is
never implicit; threads are per core, not per task.

## 9. Compiler architecture (ARCH) and performance (PERF)

### 9.1 What stays

Crate boundaries, pipeline order, monomorphization before semantic analysis, explicit region
instructions, fail-closed backend validation.

### 9.2 What changes

| ID | Change | Why |
| --- | --- | --- |
| ARCH-001 | Structural identity replaces span keys in the semantic model | Prerequisite for incremental compilation, the language server, hot reload |
| ARCH-002 | Backend-neutral MIR validation in `aster-mir`, run after every pass in tests | A second backend; ownership operations need an independent checker |
| ARCH-003 | HIR lowering reads types from the semantic model | The class of defect behind D1 |
| ARCH-004 | One constraint-based check per generic body | Errors at the declaration |
| ARCH-005 | `aster-exec` crate for the scheduler and task runtime | AOT needs it without the JIT |
| ARCH-006 | Incremental compilation by declaration | Development loop |
| ARCH-007 | Diagnostic codes; analysis after syntax errors | Tooling |
| PERF-001 | AOT through `cranelift-object`, static runtime, `aster build` | Distribution |
| PERF-002 | MIR inliner and devirtualizer | Accessors are calls today, and calls are barriers for every memory proof |
| PERF-003 | Cranelift only through 1.0 | Complexity budget |
| PERF-004 | No tiered JIT or PGO now | |

### 9.3 Measurement discipline (PERF-005)

Two different kinds of number exist. They are never mixed.

**A. Deterministic invariants.** Exact values for a given revision, target, and profile. They are
compared for equality and **may gate CI**. They detect structural regressions. **They are not
performance.**

| Invariant | Detects |
| --- | --- |
| Result checksum | Wrong code |
| Allocation count and requested bytes by category | A lost elimination or region |
| Final and peak live bytes | A leak; a lost reclamation |
| Count operations emitted and executed (counter in the test profile) | Lost elision |
| MIR instruction count after optimization; number of specializations | A lost optimization; specialization growth |
| Generated code size for a pinned Cranelift version | Code growth |
| Validator findings | An invalid pass output |

This is the rule the repository already applies to `memory_matrix` (only checksums and `MemoryStats`
are frozen).

**B. Performance metrics.** Noisy, machine-dependent. **Never a CI gate.**

| Metric | Protocol |
| --- | --- |
| Wall time by phase | Reference machine; fixed power state; warm-up discarded; at least the existing sample counts; A and B interleaved; median and spread reported |
| Process resident memory | Sampled at workload boundaries, as `aarm_memory_matrix` does |
| Executed instructions and cache misses | Hardware counters where the platform offers them (Linux). Lower noise than time, still a real metric and still advisory |

A decision that claims a speed or memory benefit requires B, measured A/B on one machine, with A
unchanged. A change whose B difference is inside the spread is not a performance change.

**Rule for any optimization:** a workload shows the cost; B is measured A/B; compile-time cost is
reported with the run-time gain; every invariant in §3 holds; the A invariants are updated in the
same pull request with an explanation.

## 10. Hot reload (HOT)

Everything in this section exists **only in the development profile**. Release and AOT builds use
direct calls and module-owned immutable tables.

### 10.1 Terms

- **Generation.** A JIT module holding the code compiled by one reload. Generation 0 is the initial
  build. Finalized code is never modified.
- **Slot.** An entry of the host-owned **function table**: one code pointer, at an address that is
  stable for the session. A slot is identified by the declaration key **and the signature hash**.
- **Type ABI hash.** Field layout, virtual-slot layout, and interface list of a type.
- **Stable descriptor.** A descriptor and its tables in host-owned session storage, identified by
  the declaration key and the type ABI hash.

### 10.2 Indirection (HOT-001, HOT-002)

- Every call between ASTER functions loads its slot and calls indirectly.
- Virtual and interface dispatch read stable descriptors.
- A closure value's code word refers to a slot, not to code.
- A C callback thunk is immortal host code that jumps through a slot.
- String literals and constant data are interned in session-immortal storage, **not** in a
  generation's data section. Otherwise a literal stored in the heap would dangle when its generation
  is unloaded.

### 10.3 Classes of change and when each applies

| Change | Mechanism | Applied | Old code |
| --- | --- | --- | --- |
| Function body, same signature | Store the new pointer in the existing slot (one atomic write) | Immediately; each call picks whichever pointer it loads | Frames already running finish the old body |
| Function signature | **New slot**. Callers compiled against the new signature use it; the old slot keeps the old code | Immediately | Old callers keep calling the old slot. A caller and callee therefore always agree on the ABI |
| Virtual or interface method body | Repoint the entry in the stable descriptor | Immediately | As above |
| Type ABI (fields, virtual slots, interface list) | New descriptor identity | **Only at a global safe point and only if §10.5 holds; otherwise restart** | — |
| New function, type, or specialization | New slot or descriptor | Immediately | — |
| Removed declaration | Slot kept while anything can reach it | — | Old code stays reachable |
| Static field added | New storage; initializer runs when applied | Global safe point | — |
| Static field initializer changed | **Not re-run**; the state is kept | — | Documented |
| Static field type changed or removed while read by live code | Restart | — | — |
| Lambda whose capture layout changed | New slot; existing closures keep the old slot | Immediately | The generation of the old lambda stays pinned |
| Entry point signature | Restart | — | — |

The development profile restricts inlining to trivial accessors and records each inlining as a
dependency: changing an inlined callee recompiles its callers.

### 10.4 Safe points (HOT-004)

- **Turn boundary of a domain.** Between turns a domain has no ASTER frame on its stack (§8.2).
- **Global safe point.** Every domain is at a turn boundary at once. The host requests it; each
  domain stops at its next boundary. A domain inside a long turn, a blocking `Wait()`, or a foreign
  call does not reach it; after a timeout the reload **falls back to restart** and says which domain
  blocked it.
- Changes marked "immediately" need no safe point: slot and descriptor-entry stores are atomic and
  ABI-consistent by construction.

### 10.5 Condition for applying a type ABI change without migration (HOT-003, first version)

All of:

1. A global safe point was reached.
2. The development allocator's per-descriptor live-instance counter for the old type is zero.
3. No suspended async frame and no pinned closure belongs to a function that depends on the old type
   (per-function frame counters; the dependency set comes from MIR).
4. No static field has the old type.

Otherwise: restart, with the failed condition printed. Condition 3 matters: old code could otherwise
create an old-layout instance after the check and hand it to new code.

Migration of live instances (enumerate the heap, copy field by field into the new layout, fix
references) is a later step and needs the object enumeration and reference maps of §5.8.

### 10.6 What may still refer to an old generation, and safe unloading

| Referrer | Development-profile handling |
| --- | --- |
| Function-table and descriptor entries | Repointed at reload; a generation is tracked as "has live slots" until none points into it |
| Return addresses of running frames | A domain records the newest generation at the start of each turn; once every domain has started a turn after a generation was retired, no stack can return into it |
| Suspended async frames (the resume pointer and state numbers belong to one body) | The frame records its generation; per-generation counter incremented at creation, decremented at drop. A suspended task always finishes in the code that suspended it |
| Closures | Hold a slot, not code; they pin a generation only through §10.3's lambda rule |
| Native code holding a callback | Holds an immortal thunk, never generation code |
| Queued task entry addresses | Slots, not code |
| Heap objects | Refer only to stable descriptors and session-immortal literals, never into a generation |

**Unload rule.** A generation may be freed when it has no live slot, no frame counter above zero,
and every domain has passed a turn boundary since it was retired. A domain that never reaches a
boundary delays unloading indefinitely; that is a memory cost, never a safety problem.

**First version:** generations are retained for the whole session and the retained code size is
reported. Unloading under the rule above is added only if the cost is shown to matter.

### 10.7 State

The heap, statics, and handle tables belong to domains, not to generations, so they survive a
reload. This depends on MEM-001 (today all data dies with the run) and LANG-006 (statics are the
state roots).

### 10.8 Fallback

Restart is always available and always explicit about its reason. It is the answer whenever a rule
above is not provably satisfied.

## 11. Platforms and ecosystem (PLAT, TOOL)

| Target | Language | Compiler | Runtime | Tooling | Ecosystem |
| --- | --- | --- | --- | --- | --- |
| CLI tools | `Main(string[] args)` | AOT | Arguments, environment, exit status, standard error, streaming files | `aster build` | |
| Services | Closures, general async | AOT | Reactor, sockets, timers | | HTTP as a package |
| Desktop | FFI with structs and callbacks; `using` | AOT, C ABI | Library loading | Bindings generator later | Toolkits as packages |
| Games | Inheritance, closures, operators | AOT, inliner | Bounded memory | Hot reload | Engines and ECS as packages |
| Browser front end | | A new backend from MIR | Runtime without threads or virtual memory | | **Research** |
| Mobile | | AOT arm64, platform linkers | | SDK integration | **Research**; needs arm64 first |
| macOS, Linux arm64 | | Cranelift supports both | Page backend for macOS | CI runners | After AOT |

FFI (PLAT-001): libraries declared in `Aster.toml`; `ptr<T>` only inside `unsafe`; C-layout structs;
explicit `CString`; callbacks under §8.8. Exact attribute syntax is not fixed by this plan.

Packages stay path and pinned Git; a registry is DEFERRED. The public binary interface is the C ABI
only. Tooling: formatter, language server, debug information, profiling support.

Platform support does not mean the libraries exist. Engines, ECS, HTTP, and UI stay outside the
compiler.

## 12. Compatibility and migration

- ASTER is pre-1.0 and its compatibility policy allows documented breaking changes. **Every breaking
  item is listed in §7.1 and waits for the maintainer.** None is a prerequisite of a non-breaking
  item, except MEM-002 for counting.
- Each approved breaking item ships alone, with a migration note and, where mechanical, a diagnostic
  naming the fix.
- The arena model is not removed. Tier 1 keeps every existing proof and benchmark.

## 13. Trade-offs recorded as policy

| Conflict | Policy |
| --- | --- |
| Speed versus safety | Safety wins in safe code. A check is removed only by a proof re-validated in MIR |
| Speed versus ergonomics | The default is the ergonomic form; the fast form is reachable without `unsafe` |
| Memory safety versus C# aliasing | Counting pays a run-time cost so the surface stays C#. Cycles are the programmer's responsibility |
| Determinism versus elision | The release point is fixed by the compiled program, not by the source scope; no user code observes it except `weak` |
| Convenience versus predictability of resources | `Dispose` is explicit. A forgotten `Dispose` is a diagnosed leak, not a hidden call |
| Dev-loop features versus production cost | Separate profiles |
| Determinism versus throughput | Public outcomes are deterministic; scheduling order is not |
| Isolation versus failure granularity | A failure costs a whole domain. Finer isolation means more domains, chosen by the programmer |
| Feature breadth versus complexity budget | A feature enters the compiler only if a library cannot express it |
