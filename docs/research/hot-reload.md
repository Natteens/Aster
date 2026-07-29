# Hot-reload research

This document describes how stateful hot reload could work in ASTER later. Nothing here is
implemented. What exists today is `aster watch`: every rebuild
produces a fresh JIT session, runs the selected function from scratch, and frees the previous
module. No state survives. Real hot reload means replacing code inside a *running* program
while preserving its state; the two must never be conflated.

## Function indirection table

Today the JIT emits direct calls between compiled ASTER functions. Hot reload requires one
level of indirection: calls go through a per-function slot in a table of function pointers.
Swapping an implementation becomes an atomic pointer store instead of repatching call sites.
The MIR → backend boundary already identifies callees by `SymbolId`, so the table can be
introduced entirely inside the backend when needed.

## Stable function identity

`SymbolId` values are stable only within one compilation. Reload needs an identity that
survives recompilation: a fully qualified path (`namespace.Function`) plus a signature hash.
Matching identities map old slots to new code; unmatched old identities keep their previous
code until unreachable; new identities get new slots.

## Signature versioning and invalidation

Each table slot records a hash of its function's signature (parameter types, return type,
calling convention). A reload whose hash matches swaps in place. A mismatch — or any change
to the layout of a type used by live data — invalidates dependents:

- signature change: all callers of that function must also be from the new module;
- type layout change: every function touching the type, plus live objects of the type, are
  affected and require migration or a restart.

## Safe code replacement

Replacement must happen at a *safe point*: no thread executing (or holding a return address
into) code about to be replaced. For the current single-threaded explicit-invocation model,
the natural safe point is between invocations — e.g. between frames once a lifecycle exists.
Old modules are retired with a grace period: kept alive until no stack references them, then
freed exactly the way `execute` frees its module today.

## State preservation and object migration

State lives in globals and (future) heap objects, plus any future ECS storage. Preserving it across reload
requires: stable storage identity keyed like function identity; per-type version tags; and
migration functions (auto-generated field-by-field copies for compatible changes, user
hooks for incompatible ones). Objects whose type changed layout are migrated eagerly at the
safe point or lazily on first touch; migration failure downgrades the reload to a restart.

## Interaction with classes, ECS, schedules, and tasks

- **Classes**: vtables (once virtual dispatch exists) are updated like the function table;
  instance layout changes trigger the migration path above.
- **ECS**: if a future optional ECS package exists ([ECS research](ecs.md)), component
  arrays would be the dominant state. Component layout changes would require per-archetype
  migration; system signature changes would re-register the system in the schedule. This is
  entirely speculative: ECS is not part of the language, compiler, or runtime today.
- **Schedules/lifecycle**: the schedule is the natural safe point ("end of frame"). A reload
  request is queued and applied between ticks.
- **Running tasks**: long-running tasks holding old code must either finish under the old
  module (grace period) or observe a cooperative cancellation token before replacement.
  Cancellation must be advisory; forcibly killing tasks corrupts state.

## Incompatible changes

Some edits can never hot reload: entry-signature changes, removals of live types, layout
changes without a migration path. The correct behavior is an explicit, honest downgrade:
report *why*, then fall back to recompile-and-restart (what `aster watch` already does).

## What the current architecture already provides

- `aster-runtime` isolates host services behind a registry, so a reloaded module rebinds the
  same symbols.
- The string ABI forbids pointers outliving their JIT session, which is exactly the
  discipline reload retirement requires.
- `execute` frees each JIT session deterministically; watch mode proves sessions do not leak.
- MIR is backend-independent and identifies functions symbolically, leaving room for the
  indirection table without changes to the frontend.

No indirection, state migration, or reload versioning exists today.
