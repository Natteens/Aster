# Deterministic random numbers

`aster.random.Random` is an explicitly seeded mutable SplitMix64 generator. It advances one 64-bit
state word and applies SplitMix64's fixed output permutation. Its sequence is deterministic across
ASTER targets for a given seed; it is not cryptographically secure and has no global or implicitly
seeded instance.

The seeded sequence is a reproducibility contract for this library version. A future deliberate
sequence change would be called out under ASTER's compatibility and release policy rather than
silently varying by host, architecture, or process.

```aster
using aster.random;

Random random = new Random(123UL);
int index = random.NextInt(0, 10);
double unit = random.NextDouble();
```

`NextULong`, `NextUInt`, and `NextBool` produce scalar values. `NextInt(minInclusive,
maxExclusive)` and `NextLong` use rejection sampling, so awkward ranges have no modulo bias; an
empty or reversed range is a controlled contract failure. The zero-argument `NextFloat` and
`NextDouble` are always in `[0, 1)`. Each call advances the same instance, and equal seeds/call sequences produce equal
results on Windows and Linux.

## Floating-point ranges

```aster
using aster.random;

Random random = new Random(7UL);
float speed = random.NextFloat(5.0f, 8.0f);     // 5.0 up to, but not including, 8.0
double scale = random.NextDouble(0.5d, 2.0d);   // 0.5 up to, but not including, 2.0
```

`NextFloat(minInclusive, maxExclusive)` and `NextDouble(minInclusive, maxExclusive)` return a value in
the half-open interval `[minInclusive, maxExclusive)`; the zero-argument forms are unchanged and still
cover `[0, 1)`.

- **Errors.** Both bounds must be finite and `minInclusive < maxExclusive`. NaN, either infinity, equal
  bounds, and reversed bounds are the same controlled runtime error as an empty integer range.
- **The upper bound is never returned.** The result is `minInclusive + unit * (maxExclusive -
  minInclusive)` where `unit` is the same value the zero-argument method would return. If rounding
  would reach `maxExclusive`, the call returns `minInclusive` instead. There is no redraw loop, so
  every call finishes and consumes exactly one sample (`NextUInt` for `float`, `NextULong` for
  `double`).
- **Large intervals.** `NextFloat` computes in `double`, which cannot overflow for any two finite
  `float` bounds. `NextDouble` takes the span in halves when `maxExclusive - minInclusive` would
  overflow, so `NextDouble(-MaxValue, MaxValue)` is finite and in range.
- **Precision limit.** The result has at most 24 (`float`) or 53 (`double`) random bits, so an interval
  wider than the number of representable values cannot hit all of them, and an interval only a few
  representable values wide yields only those values. For adjacent bounds the interval contains just
  `minInclusive`, which is what is returned. The distribution is uniform over the scaled `unit`
  values, not over every representable number in the interval.
- **Reproducibility.** Equal seeds give equal sequences. Mixing ranged and unranged calls is fine: each
  call advances the generator by exactly one sample.

Bounded integers use rejection sampling against the exact 64-bit sample space, including ranges
whose size is not a power of two. Floating results use the high 24 (`float`) or 53 (`double`) bits
of a mixed sample. A zero seed is valid; it does not create a stuck state.

`Random` is an ordinary mutable class. It may be created and used locally in a worker, but an
instance is not transferable or shared across Task/Parallel boundaries.

## Unpredictable seeds

`Random.FromEntropy()` creates a generator whose seed is 64 bits read from the operating system's
random source (`getrandom(2)` on Linux, `BCryptGenRandom` on Windows), through the runtime, which owns
this host read. Each call reads a fresh sample, so generators created back to back are independent. Two samples can
collide, with probability about 2^-64 per pair, so equal seeds are possible but negligible. No global
state is involved:

```aster
using aster.random;

Random random = Random.FromEntropy();
int damage = random.NextInt(10, 20);   // 10 to 19 (the upper bound is exclusive)
float chance = random.NextFloat();     // 0.0 up to, but not including, 1.0
```

The seed is read once, when the generator is created. Every later number costs exactly what it costs
for an explicitly seeded generator, and the explicit `new Random(seed)` sequences are unchanged and
fully reproducible.

The operating system supplies only the seed. The numbers that follow are `SplitMix64` output, which is
a fast, noncryptographic generator: knowing a few outputs lets anyone predict the rest. `Random` is
not suitable for cryptography, with or without `FromEntropy`.

The read can fail. If the operating system's random source is unavailable, the program stops with a
controlled runtime error (`Random.FromEntropy could not read operating-system entropy: …`), the same
way other runtime failures are reported; there is no clock, counter, or constant fallback seed. A
platform other than Linux and Windows has no supported source and reports the same error.

`FromEntropy` is a host operation, like the clock reads, so it is rejected in a `Task` or `Parallel`
worker body (directly or through a helper). Workers receive an explicit seed instead, which also keeps
parallel runs reproducible: draw the seeds on the main thread and pass them as `ulong` arguments:

```aster
public int Roll(ulong seed)
{
    Random random = new Random(seed);
    return random.NextInt(1, 7);
}

Random seeds = Random.FromEntropy();
Task<int> first = Task.Run(Roll, seeds.NextULong());
Task<int> second = Task.Run(Roll, seeds.NextULong());
```

Constructor overloads do not exist, so there is no parameterless `new Random()`; and there is no
global generator or static `Random.Range`. See the
[random API decision](../compiler/random-api-decision.md) for the reasons and the design that would
be needed.
