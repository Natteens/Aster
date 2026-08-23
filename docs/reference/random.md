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
empty or reversed range is a controlled contract failure. `NextFloat` and `NextDouble` are always
in `[0, 1)`. Each call advances the same instance, and equal seeds/call sequences produce equal
results on Windows and Linux.

Bounded integers use rejection sampling against the exact 64-bit sample space, including ranges
whose size is not a power of two. Floating results use the high 24 (`float`) or 53 (`double`) bits
of a mixed sample. A zero seed is valid; it does not create a stuck state.

`Random` is an ordinary mutable class. It may be created and used locally in a worker, but an
instance is not transferable or shared across Task/Parallel boundaries.
