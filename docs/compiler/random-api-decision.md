# Decision record: Random API ergonomics

**Status.** Part A (`Random.FromEntropy()`) is **ACCEPTED and implemented**. Part B (a global,
Unity-style `Random.Range`/`Random.Value` API) and Part C (floating-point range overloads) are
**PROPOSED** and deliberately not implemented; this record states why and what a future change would
need.

| | |
| --- | --- |
| Baseline | `d73c30ab91af5dc137e319d8d3d4c769f68a416b`, release 0.69.1 |
| Related | [random reference](../reference/random.md), [standard library extension contract](standard-library-extension.md), [concurrency reference](../reference/concurrency.md), [classes reference](../reference/classes.md) |
| References | C# `System.Random` (`new Random()` is seeded by the runtime) and Unity `UnityEngine.Random` (`Range`, `value`, `InitState`), used for ergonomics only |

## 1. Problem

Today every `Random` needs an explicit seed:

```aster
Random random = new Random(123);
int damage = random.NextInt(10, 20);
```

That is exactly right for reproducible simulation, tests, and replays, but a game also wants an
unpredictable generator, and the only way to get one was to feed the clock into `new Random(seed)`:
a millisecond clock repeats when two generators are created in the same millisecond, and the clock is
unavailable in workers. The question is the smallest change that fixes this without weakening the
deterministic contract or hiding state.

## 2. Evidence from the repository

- `stdlib/aster/random/random.aster`: `Random` is a class with one `ulong` field; `NextULong` and
  `NextUInt` call the `__RandomMix` intrinsic, and every other method is ASTER code built on them.
  `__RandomMix` stays: a controlled benchmark measured an ASTER replacement about 1.8 times slower on
  these hot paths.
- **Constructor overloads are unsupported** (`docs/reference/classes.md`), so a parameterless
  `new Random()` cannot coexist with `Random(ulong seed)` without a language feature.
- **There are no static fields.** A global generator would need new storage somewhere else.
- **Workers are isolated.** Each `Task`/`Parallel` worker has its own `ExecutionContext`, and host
  operations (terminal, filesystem, clocks, foreign calls) are rejected in worker bodies, directly or
  through helpers. A worker already receives explicit values by copy.
- Host reads are runtime intrinsics bound by name; the clock reads (`Clock.MonotonicMilliseconds`,
  `UnixMilliseconds`) are the existing pattern for a stateless, nullary host read.
- Passing `123` to a `ulong` parameter works since the integer-constant call-argument fix, so
  `new Random(123)` is already convenient.

## 3. Part A — `Random.FromEntropy()` (accepted)

```aster
Random random = Random.FromEntropy();
int damage = random.NextInt(10, 20);
float chance = random.NextFloat();
```

Answers to the required decisions:

1. **`Random()` without a seed?** Not as a constructor: constructor overloads do not exist, and adding
   them is a separate language feature. A static factory gives the same capability with an honest name
   that states the seed source. `Random.FromEntropy()` is a static method that creates and returns a
   new generator.
2. **Seed source and owner.** The seed is a fresh 64-bit value from the operating system's entropy,
   read by a new runtime function `aster_rt_random_entropy_seed` behind the intrinsic
   `aster.random::__RandomEntropySeed`. The runtime owns the read, as it owns the clock. It uses the
   standard library's `RandomState`, which seeds per-thread hash keys from the OS random source and
   advances them on every call, so no new dependency, no `unsafe`, and no shared state are needed.
   Consecutive calls and calls on different threads differ. It is a seed for a non-cryptographic
   generator, not a secret.
3. **No weak default.** There is no clock, counter, or constant fallback. If a future platform cannot
   provide entropy, the read must fail in a controlled way rather than return a predictable value.
4. **State, reuse, and workers.** There is no new state: the entropy is consumed once, to build an
   ordinary `Random` that the caller owns, stores, and reuses. The read is a host operation, so it is
   rejected in worker bodies by the existing worker validation (the message now names
   `Random.FromEntropy`). Workers get explicit seeds, which also keeps parallel work reproducible: the
   main thread draws seeds from one generator and passes them as `ulong` arguments.
5. **Global mutable generator.** None is introduced (see Part B).
6. **Deterministic contract.** `Random(ulong seed)`, every explicitly seeded sequence, and
   `__RandomMix` are untouched; the tests pin reference values for seeds 0, 1, and 123.
7. **Ranges.** Unchanged (Part C). The existing methods keep the documented half-open meaning: the
   integer upper bound is exclusive and the floating results are in `[0, 1)`.
8. **Allocation and performance.** Creating a generator allocates what it always did (one small
   object; the stress counters show no string or array allocation). The entropy read costs roughly
   40 ns per call in a release build (about 82 ms extra for two million generators on the development
   machine, startup subtracted), paid once per generator. A hundred-million-number loop took the same
   time for a seeded and an entropy-seeded generator (median 423 ms and 413 ms, startup included),
   because the generator is the same object after construction. The MIR of `NextULong` and
   `NextUInt` is unchanged: one `RandomMix` intrinsic and no call.

Implementation, in the same layers as the clock reads: the stdlib placeholder and static factory, the
`RandomEntropySeed` HIR and MIR intrinsic, the lowering arms, backend shape validation (a nullary call
returning `ulong`), Cranelift dispatch to the runtime function, the registry entry, and the worker host
operation list. No unrelated compiler feature changed.

## 4. Part B — static `Random.Range` / `Random.Value` (proposed, not implemented)

```aster
// PROPOSED, not implemented.
int damage = Random.Range(10, 20);
float chance = Random.Value();
```

This needs a **generator that the language does not have a place to store**: ASTER has no static
fields, so the state would have to live in the runtime. The requirements and the recommended design if
the need is demonstrated:

- **Where the state lives.** In the `ExecutionContext`, lazily created on first use. A context is
  per execution and per worker, which preserves worker isolation, needs no lock, and gives each
  worker its own stream. A process-wide generator would need synchronization and would make results
  depend on scheduling, so it is rejected.
- **Seeding.** Unseeded by default (drawn from the same entropy read, once per context), with an
  explicit `Random.InitState(ulong seed)` for reproducible runs (the Unity name for this). Whether a
  worker context may draw entropy is a policy decision: today host operations are rejected in workers,
  so either the context seed is derived from the parent (deterministic) or the rule is relaxed for
  this one read.
- **Cost.** Every call becomes a runtime call to context state (the same shape as `__RandomMix`), plus
  a first-use check. It is not free like a local `Random` field access that the compiler can keep in a
  register, and it hides a mutable global from the type system.
- **Why not now.** It requires new runtime state, a worker policy decision, and a determinism story
  (what `InitState` means across `Task` boundaries). The instance API already covers every case, with
  `FromEntropy` removing the only missing capability. A static convenience API is therefore deferred
  until a concrete need shows that storing a generator in a field is not enough.

## 5. Part C — floating-point range overloads (proposed, not implemented)

```aster
// PROPOSED, not implemented.
float speed = random.NextFloat(5.0f, 8.0f);   // 5.0 up to, but not including, 8.0
double scale = random.NextDouble(0.5d, 2.0d);
```

Methods may be overloaded by parameter count, so these would not be ambiguous with `NextFloat()`. The
semantics to specify are half-open `[minInclusive, maxExclusive)` to match `NextInt`, a controlled
error through the existing invalid-range path when `min >= max` or an argument is not finite, and a
guard against `min + value * (max - min)` rounding up to `max` (redraw, which keeps the result
uniform, rather than clamp). It is plain ASTER with no compiler change, which is why it is separable;
it is left out of this change to keep the review focused on the entropy seed.

## 6. Decisions

| ID | Question | Outcome |
| --- | --- | --- |
| R1 | Unseeded generators | Accepted: `Random.FromEntropy()` with an OS-entropy seed |
| R2 | A `Random()` constructor | Not possible without constructor overloads; the factory is the substitute |
| R3 | Global generator and static `Random.Range` | Deferred (proposed): needs context-owned state and a worker policy |
| R4 | Float range overloads | Deferred (proposed): half-open, redraw guard, no compiler change |
| R5 | Replacing `__RandomMix` | Out of scope; measured slower |
