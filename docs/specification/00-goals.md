# Design goals

Aster explores a native language where low-level work can remain direct without forcing every
program to read like systems code. These goals guide design decisions; they are not a list of
implemented features. The [language reference](../reference/) and
[implemented grammar](../compiler/grammar.md) describe what the compiler accepts today.

## Direct code

Source should show intent without avoidable ceremony. Types appear before names, control flow uses
familiar forms, and ordinary code should not require compiler-specific annotations merely to be
readable. Concision is useful only when it leaves meaning intact.

**Practical consequence:** a small function should look like the work it performs, not like setup
for the compiler or runtime.

## Concrete types

Generics produce concrete specializations. Layouts, calls, and dispatch should not depend on
implicit type erasure, hidden boxing, or a universal runtime object.

**Practical consequence:** `Box<int>` and `Box<string>` are distinct concrete types, while repeated
uses of `Box<int>` share the same specialization.

## Predictable behavior

Evaluation order, representation, and effects must not rely on surprising rules. Value types copy
their data; reference types preserve identity; calls evaluate receivers and arguments in a defined
order.

**Practical consequence:** refactoring an expression should not silently reorder effects or change
whether data is copied or shared.

## Explicit failure

Expected absence and recoverable errors belong in the type system. `Option<T>` and `Result<T, E>`
make those paths visible to callers without introducing implicit nulls or exception-based control
flow.

**Practical consequence:** a function signature shows when its caller must handle “no value” or an
expected error.

## Understandable cost

Ergonomics must not disguise relevant work. Allocation, dynamic interface dispatch, copying large
values, lossy conversion, and future unsafe operations need semantics that a programmer can reason
about.

**Practical consequence:** the compiler may perform complex lowering internally, but it must not
invent hidden source-level meaning or pretend an expensive operation is free.

## The compiler is a tool

Diagnostics, installation, the CLI, examples, and documentation are part of the language. A clear
error or one-command example is as important to daily use as a well-factored compiler phase.

**Practical consequence:** supported behavior should be checkable with `aster check`, runnable with
`aster run`, and documented from a user's point of view.

## Parallelism is research

Safe, deterministic parallel execution is a long-term research direction, not a current feature.
Aster has no automatic parallelism, thread API, GPU backend, or HVM integration today. Any future
model must keep synchronization, scheduling cost, and observable behavior understandable. Research
may learn from systems such as Bend and HVM without committing Aster to their architecture.

## Status and open questions

The compiler is experimental and does not promise source or semantic stability before 1.0.
Platform guarantees, long-lived ownership, unsafe boundaries, concurrency, and AOT distribution
remain design work. Accepted behavior belongs in reference documentation; unresolved proposals are
tracked in [Open questions](10-open-questions.md) and future work in the [roadmap](../roadmap.md).
