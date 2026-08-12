//! Compact release-only runtime/JIT residual matrix. Timings are informative
//! machine-local medians and are never correctness thresholds.

use std::time::Instant;

use aster_codegen_cranelift::{ExecutionValue, execute, execute_with_stats};
use aster_compiler::compile;

const SAMPLES: usize = 7;

fn measure(name: &str, source: &str, expected: &ExecutionValue) {
    let module = compile(source).expect("matrix source compiles").mir;
    let (value, stats) = execute_with_stats(&module, "Run").expect("matrix source executes");
    assert_eq!(&value, expected);

    let mut samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let start = Instant::now();
        let value = execute(&module, "Run").expect("matrix source executes");
        samples.push(start.elapsed().as_secs_f64() * 1_000.0);
        assert_eq!(&value, expected);
    }
    samples.sort_by(f64::total_cmp);
    println!(
        "case={name:<10} median_ms={:>8.3} allocations={:>7} requested_bytes={:>9} used_bytes={:>9} reserved_bytes={:>9}",
        samples[SAMPLES / 2],
        stats.total_allocations,
        stats.requested_bytes,
        stats.used_bytes,
        stats.reserved_bytes,
    );
}

fn main() {
    measure(
        "scalar",
        "public long Run() { long total = 0; for (long i = 0; i < 1000000; i++) { total += i; } return total; }",
        &ExecutionValue::Long(499_999_500_000),
    );
    measure(
        "direct",
        "internal long AddOne(long value) { return value + 1; } public long Run() { long total = 0; for (long i = 0; i < 500000; i++) { total += AddOne(i); } return total; }",
        &ExecutionValue::Long(125_000_250_000),
    );
    measure(
        "generic",
        "internal T Identity<T>(T value) { return value; } public long Run() { long total = 0; for (long i = 0; i < 200000; i++) { total += Identity<long>(i); } return total; }",
        &ExecutionValue::Long(19_999_900_000),
    );
    measure(
        "interface",
        "public interface IValue { int Get(); } public class Value : IValue { public Value() {} public int Get() { return 7; } } public int Run() { IValue value = new Value(); int total = 0; for (int i = 0; i < 500000; i++) { total += value.Get(); } return total; }",
        &ExecutionValue::Int(3_500_000),
    );
    measure(
        "object",
        "public class Box { public int value; public Box() {} } public long Run() { Box box = new Box(); long total = 0; for (int i = 0; i < 1000000; i++) { box.value += 1; total += box.value; } return total; }",
        &ExecutionValue::Long(500_000_500_000),
    );
    measure(
        "array",
        "public long Run() { long[] values = new long[100000]; long total = 0; for (int i = 0; i < 100000; i++) { values[i] = i; } for (int i = 0; i < 100000; i++) { total += values[i]; } return total; }",
        &ExecutionValue::Long(4_999_950_000),
    );
    measure(
        "list",
        "public long Run() { List<long> values = new List<long>(); long total = 0; for (int i = 0; i < 100000; i++) { values.Add(i); } for (int i = 0; i < 100000; i++) { total += values.Get(i); } return total; }",
        &ExecutionValue::Long(4_999_950_000),
    );
    measure(
        "enum",
        "public enum Kind { A, B, C, D } public int Run() { Kind value = Kind.D; int total = 0; for (int i = 0; i < 1000000; i++) { switch (value) { case A: total += 1; case B: total += 2; case C: total += 3; case D: total += 4; } } return total; }",
        &ExecutionValue::Int(4_000_000),
    );
}
