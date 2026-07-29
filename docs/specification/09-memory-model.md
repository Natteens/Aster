# 09 — Memory model

## Objective

State the safety guarantees ASTER intends to provide while keeping ownership syntax,
allocation strategy, and concurrency rules open until they can be designed together.

## Proposed syntax

The following illustrates intent only; `read` and `write` borrowing syntax for ordinary
functions has not been adopted:

```aster
void inspect(Buffer read buffer)
{
    Log("Buffer inspecionado");
}

void clear(Buffer write buffer)
{
    buffer.length = 0;
}
```

## Proposed rules

- Safe ASTER must prevent use-after-free, double-free, dangling references, invalid
  aliasing, and data races at compile time or through a defined safe runtime mechanism.
- Values have a single, deterministic lifetime even if most lifetimes are inferred.
- Mutation requires exclusive writable access to the affected storage.
- Shared readable access cannot overlap incompatible mutation.
- Moving a non-copy value invalidates the old binding.
- Destruction is deterministic when ownership ends; exact destructor semantics are open.
- Safe code has no undefined behavior from memory access.
- No garbage collector or reference-counting model is silently assumed by this draft.
- Class instances have identity, but their allocation, ownership, and reclamation model is
  not accepted yet.
- Concurrency syntax and synchronization primitives are outside this initial chapter.

## Valid design example

Conceptually valid if `read` and `write` are adopted for function parameters:

```aster
Buffer data = load();
inspect(data);
clear(data);
```

The accesses are sequential and do not overlap.

## Invalid design examples

Conceptually invalid under the proposed safety guarantees:

```aster
Buffer data = load();
Buffer moved = move(data);
inspect(data);              // use after move
```

```aster
Buffer write first = borrowWrite(data);
Buffer write second = borrowWrite(data); // overlapping writable access
```

These snippets use placeholder design syntax and are not claims about compiler support.

## OPEN QUESTIONS

### PROPOSED — Class memory management

1. **Ownership with moves and borrowing** — deterministic, high-performance, and compatible
   with static safety; can expose lifetime complexity and make shared object graphs harder.
2. **Tracing garbage collection for classes** — ergonomic cycles and sharing; adds a runtime,
   pause/throughput tradeoffs, and less deterministic reclamation.
3. **Reference counting with explicit weak references** — deterministic reclamation in acyclic
   graphs and familiar ownership; adds atomic/non-atomic count cost and requires cycle handling.

**Recommendation:** PROPOSED — start from ownership and borrowing for deterministic native
execution, then evaluate an explicit shared-ownership library type. Do not accept this until
class ergonomics and cyclic object graphs have representative examples.

### PROPOSED — Class allocation location

1. **Classes are always heap allocated** — stable identity and simple references; every class
   instance pays allocation/indirection costs.
2. **Compiler escape analysis chooses placement** — preserves semantics with possible stack
   allocation; complicates predictable performance and debugging.
3. **Syntax selects placement** — maximum control; leaks storage concerns into common code.

**Recommendation:** PROPOSED — define identity independently of physical placement and permit
escape analysis only when it cannot change observable behavior or destruction timing.

### PROPOSED — Lifetime notation

1. **Fully inferred lifetimes** — least verbose; complex failures may be harder to resolve.
2. **Inferred by default with optional annotations** — ergonomic common path and an escape hatch;
   expands language surface.
3. **Always explicit for borrowed values** — predictable; conflicts with the less-verbose goal.

**Recommendation:** PROPOSED — infer lifetimes by default and investigate optional annotations
only after diagnostics prove inference alone insufficient.

- **OPEN QUESTION:** What final syntax expresses ownership, moves, shared reads, and writes?
- **OPEN QUESTION:** Which types are implicitly copied, and can users define copy behavior?
- **OPEN QUESTION:** Are lifetime annotations ever exposed to users?
- **OPEN QUESTION:** How are heap allocation, smart pointers, shared ownership, and weak references expressed?
- **OPEN QUESTION:** Does ASTER have destructors, and can destruction fail or be asynchronous?
- **OPEN QUESTION:** What is the precise memory ordering and concurrency model?
- **OPEN QUESTION:** How is `unsafe` entered, audited, and bounded?
- **OPEN QUESTION:** What guarantees apply across foreign-function boundaries?
