# Memory region benchmark

This benchmark compares two programs with the same result and the same logical
allocation mix:

- `temporary.aster` keeps one object, one array, and one dynamic string local to
  a helper call. Escape analysis can reclaim all three on every return.
- `persistent.aster` returns those values from helper functions. The conservative
  lifetime rule keeps them in persistent storage until the execution ends.

Both programs execute 100,000 iterations and return `4200000`.

## Run the memory-only view

```console
cargo run --release -p aster-cli -- run benchmarks/memory/temporary.aster --function Run --memory-stats
cargo run --release -p aster-cli -- run benchmarks/memory/persistent.aster --function Run --memory-stats
```

The temporary case should finish with `used: 0 bytes`. Reserved capacity remains
visible because arena pages are retained for reuse. The persistent case should
finish with non-zero used memory.

## Run the timed comparison

```console
cargo run --release -p aster-codegen-cranelift --example memory_regions
```

The example compiles each Aster source once, then records five end-to-end JIT
and execution samples per case. It prints the median duration and one stable
memory-stat snapshot.

Compare results only on the same machine, Rust toolchain, Aster revision, and
build profile. The timing is a regression signal, not a universal performance
claim.
