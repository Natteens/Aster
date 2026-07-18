# 00 — Goals and status

## Objective

This directory records the initial design direction for Aster. It separates language
design from the grammar currently accepted by the bootstrap compiler. Unless a rule is
also documented in `docs/compiler/grammar.md`, it must not be interpreted as implemented.

The examples in this specification are design examples. “Valid” means valid under the
proposal in the document, not necessarily accepted by the current compiler.

## Proposed design goals

- Native, predictable performance without a managed runtime or .NET dependency.
- Compact, readable syntax with types written before names.
- Memory safety inspired by Rust, with less annotation in common cases.
- Safe concurrency by construction.
- Explicit behavior: potentially expensive, unsafe, or lossy operations should be visible.
- A small, learnable core suitable for a hand-written compiler.
- Future support for Windows, Linux, x86-64, and ARM64.

ECS remains an unscheduled research topic for a possible optional library. No ECS syntax,
package, compiler support, or runtime is currently part of Aster. Game-engine facilities and
lifecycle conventions are outside this core-language specification.

## Proposed syntax character

```aster
int add(int left, int right)
{
    return left + right;
}

int answer = add(20, 22);
```

Functions do not use `fn`, and return types appear before function names rather than
after `->`.

## Proposed rules

1. Types precede declared names.
2. Mutability and ownership effects must be explicit or statically inferable without
   changing program meaning.
3. Compilation errors should identify a source range and explain a recovery action.
4. Undefined behavior must not be part of safe Aster.
5. Platform-dependent behavior must be documented where it is introduced.

## Valid design example

```aster
int distance = 42;
const int maxDistance = 100;
```

## Invalid design example

```aster
fn distance() -> int { return 42; }
```

This conflicts with the proposed declaration style: Aster does not use `fn` or `->`.

## OPEN QUESTIONS

- **OPEN QUESTION:** Which editions or language-version mechanism will stabilize syntax?
- **OPEN QUESTION:** Which platforms and ABIs are guaranteed by the first release?
- **OPEN QUESTION:** Is source compatibility or semantic stability promised before 1.0?
- **OPEN QUESTION:** Which capabilities require an explicit `unsafe` context?
