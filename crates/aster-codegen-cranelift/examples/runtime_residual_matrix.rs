//! Compact release-only runtime/JIT residual matrix. Timings are informative
//! machine-local medians and are never correctness thresholds.

use std::time::Instant;

use aster_codegen_cranelift::{ExecutionValue, execute, execute_with_stats};
use aster_compiler::{compile, compile_without_mir_optimizer_for_research, mir};

const SAMPLES: usize = 7;

#[derive(Clone, Copy)]
struct Shape {
    blocks: usize,
    instructions: usize,
    copies: usize,
    binary_operations: usize,
    branches: usize,
}

fn shape(module: &mir::Module) -> Shape {
    let debug = module.to_string();
    Shape {
        blocks: module
            .functions
            .iter()
            .map(|function| function.blocks.len())
            .sum(),
        instructions: module
            .functions
            .iter()
            .flat_map(|function| &function.blocks)
            .map(|block| block.instructions.len())
            .sum(),
        copies: debug.matches("Copy(").count(),
        binary_operations: debug.matches("kind: Binary {").count(),
        branches: module
            .functions
            .iter()
            .flat_map(|function| &function.blocks)
            .filter(|block| matches!(block.terminator, mir::Terminator::Branch { .. }))
            .count(),
    }
}

fn median(mut samples: Vec<f64>) -> f64 {
    samples.sort_by(f64::total_cmp);
    samples[SAMPLES / 2]
}

fn measure(name: &str, source: &str, expected: &ExecutionValue) {
    let baseline = compile_without_mir_optimizer_for_research(source)
        .expect("baseline matrix source compiles")
        .mir;
    let optimized = compile(source)
        .expect("optimized matrix source compiles")
        .mir;
    let baseline_shape = shape(&baseline);
    let optimized_shape = shape(&optimized);

    let mut baseline_compile = Vec::with_capacity(SAMPLES);
    let mut optimized_compile = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let start = Instant::now();
        compile_without_mir_optimizer_for_research(source).expect("baseline source compiles");
        baseline_compile.push(start.elapsed().as_secs_f64() * 1_000.0);
        let start = Instant::now();
        compile(source).expect("optimized source compiles");
        optimized_compile.push(start.elapsed().as_secs_f64() * 1_000.0);
    }

    let mut baseline_execution = Vec::with_capacity(SAMPLES);
    let mut optimized_execution = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let start = Instant::now();
        let value = execute(&baseline, "Run").expect("baseline matrix source executes");
        baseline_execution.push(start.elapsed().as_secs_f64() * 1_000.0);
        assert_eq!(&value, expected);
        let start = Instant::now();
        let value = execute(&optimized, "Run").expect("optimized matrix source executes");
        optimized_execution.push(start.elapsed().as_secs_f64() * 1_000.0);
        assert_eq!(&value, expected);
    }
    let (value, stats) =
        execute_with_stats(&optimized, "Run").expect("optimized matrix source executes");
    assert_eq!(&value, expected);
    println!(
        "case={name:<10} compile_ms={:>7.3}->{:>7.3} jit_exec_ms={:>7.3}->{:>7.3} blocks={:>3}->{:<3} instructions={:>3}->{:<3} copies={:>3}->{:<3} binary={:>3}->{:<3} branches={:>3}->{:<3} allocations={:>7} requested_bytes={:>9} used_bytes={:>9} reserved_bytes={:>9}",
        median(baseline_compile),
        median(optimized_compile),
        median(baseline_execution),
        median(optimized_execution),
        baseline_shape.blocks,
        optimized_shape.blocks,
        baseline_shape.instructions,
        optimized_shape.instructions,
        baseline_shape.copies,
        optimized_shape.copies,
        baseline_shape.binary_operations,
        optimized_shape.binary_operations,
        baseline_shape.branches,
        optimized_shape.branches,
        stats.total_allocations,
        stats.requested_bytes,
        stats.used_bytes,
        stats.reserved_bytes,
    );
}

fn measure_compile_scaling(branches: usize) {
    let mut source = String::from("public int Run() { int value = 0;");
    for _ in 0..branches {
        source.push_str("if (true) { value = value + 1; } else { value = value + 2; }");
    }
    source.push_str("return value; }");

    let baseline = compile_without_mir_optimizer_for_research(&source)
        .expect("baseline scaling source compiles")
        .mir;
    let optimized = compile(&source)
        .expect("optimized scaling source compiles")
        .mir;
    let baseline_shape = shape(&baseline);
    let optimized_shape = shape(&optimized);
    let mut baseline_compile = Vec::with_capacity(SAMPLES);
    let mut optimized_compile = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let start = Instant::now();
        compile_without_mir_optimizer_for_research(&source)
            .expect("baseline scaling source compiles");
        baseline_compile.push(start.elapsed().as_secs_f64() * 1_000.0);
        let start = Instant::now();
        compile(&source).expect("optimized scaling source compiles");
        optimized_compile.push(start.elapsed().as_secs_f64() * 1_000.0);
    }

    println!(
        "case=scale-{branches:<4} compile_ms={:>7.3}->{:>7.3} blocks={:>4}->{:<4} instructions={:>4}->{:<4} branches={:>4}->{:<4}",
        median(baseline_compile),
        median(optimized_compile),
        baseline_shape.blocks,
        optimized_shape.blocks,
        baseline_shape.instructions,
        optimized_shape.instructions,
        baseline_shape.branches,
        optimized_shape.branches,
    );
}

fn main() {
    measure(
        "branch",
        "public long Run() { long total = 0; for (long i = 0; i < 1000000; i++) { if (true) { total += i; } else { total -= i; } } return total; }",
        &ExecutionValue::Long(499_999_500_000),
    );
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
    measure_compile_scaling(34);
    measure_compile_scaling(334);
}
