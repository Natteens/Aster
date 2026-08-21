//! Release-only array/loop diagnostic matrix. Timings are machine-local
//! evidence, never correctness thresholds.

use std::time::Instant;

use aster_codegen_cranelift::{ExecutionValue, PreparedSequentialExecution};
use aster_compiler::{compile, compile_without_array_loop_optimization_for_research, mir};

const SAMPLES: usize = 11;
const WARMUPS: usize = 2;

struct Case {
    name: &'static str,
    source: &'static str,
    expected: ExecutionValue,
}

#[derive(Clone, Copy)]
struct Shape {
    blocks: usize,
    instructions: usize,
    copies: usize,
    binary_operations: usize,
    branches: usize,
    array_lengths: usize,
    loop_array_lengths: usize,
    array_indices: usize,
    proven_indices: usize,
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
        array_lengths: debug.matches("ArrayLength(").count(),
        loop_array_lengths: module
            .functions
            .iter()
            .flat_map(|function| &function.blocks)
            .filter(|block| matches!(block.terminator, mir::Terminator::Branch { .. }))
            .flat_map(|block| &block.instructions)
            .filter(|instruction| format!("{instruction:?}").contains("ArrayLength("))
            .count(),
        array_indices: debug.matches("Index {").count(),
        proven_indices: debug.matches("bounds: Proven").count(),
    }
}

fn percentile(samples: &[f64], numerator: usize, denominator: usize) -> f64 {
    let index = (samples.len() - 1) * numerator / denominator;
    samples[index]
}

#[allow(clippy::too_many_lines)]
fn measure(case: &Case) {
    let baseline = compile_without_array_loop_optimization_for_research(case.source)
        .expect("baseline array/loop matrix source compiles");
    let optimized = compile(case.source).expect("array/loop matrix source compiles");
    let baseline_shape = shape(&baseline.mir);
    let optimized_shape = shape(&optimized.mir);
    let baseline_prepared = PreparedSequentialExecution::prepare(&baseline.mir, "Run")
        .expect("baseline array/loop matrix prepares");
    let optimized_prepared = PreparedSequentialExecution::prepare(&optimized.mir, "Run")
        .expect("array/loop matrix prepares");
    for _ in 0..WARMUPS {
        assert_eq!(
            baseline_prepared
                .invoke()
                .expect("baseline warm invocation succeeds"),
            case.expected
        );
        assert_eq!(
            optimized_prepared
                .invoke()
                .expect("warm invocation succeeds"),
            case.expected
        );
    }

    let mut baseline_prepared_ms = Vec::with_capacity(SAMPLES);
    let mut optimized_prepared_ms = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let start = Instant::now();
        let value = baseline_prepared
            .invoke()
            .expect("baseline prepared invocation succeeds");
        baseline_prepared_ms.push(start.elapsed().as_secs_f64() * 1_000.0);
        assert_eq!(value, case.expected);
        let start = Instant::now();
        let value = optimized_prepared
            .invoke()
            .expect("optimized prepared invocation succeeds");
        optimized_prepared_ms.push(start.elapsed().as_secs_f64() * 1_000.0);
        assert_eq!(value, case.expected);
    }
    baseline_prepared_ms.sort_by(f64::total_cmp);
    optimized_prepared_ms.sort_by(f64::total_cmp);

    let mut baseline_compile_jit_exec_ms = Vec::with_capacity(SAMPLES);
    let mut optimized_compile_jit_exec_ms = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let start = Instant::now();
        let compilation = compile_without_array_loop_optimization_for_research(case.source)
            .expect("baseline matrix source recompiles");
        let execution = PreparedSequentialExecution::prepare(&compilation.mir, "Run")
            .expect("baseline recompiled matrix prepares");
        let value = execution
            .invoke()
            .expect("baseline recompiled matrix executes");
        baseline_compile_jit_exec_ms.push(start.elapsed().as_secs_f64() * 1_000.0);
        assert_eq!(value, case.expected);
        let start = Instant::now();
        let compilation = compile(case.source).expect("matrix source recompiles");
        let execution = PreparedSequentialExecution::prepare(&compilation.mir, "Run")
            .expect("recompiled matrix prepares");
        let value = execution.invoke().expect("recompiled matrix executes");
        optimized_compile_jit_exec_ms.push(start.elapsed().as_secs_f64() * 1_000.0);
        assert_eq!(value, case.expected);
    }
    baseline_compile_jit_exec_ms.sort_by(f64::total_cmp);
    optimized_compile_jit_exec_ms.sort_by(f64::total_cmp);

    let (value, stats) = optimized_prepared
        .invoke_with_stats()
        .expect("matrix statistics invocation succeeds");
    assert_eq!(value, case.expected);
    println!(
        "case={:<12} prepared_before={:.3}/{:.3}/{:.3} prepared_after={:.3}/{:.3}/{:.3} compile_jit_exec_before={:.3}/{:.3}/{:.3} compile_jit_exec_after={:.3}/{:.3}/{:.3} blocks={}->{} instructions={}->{} copies={}->{} binary={}->{} branches={}->{} lengths={}->{} loop_lengths={}->{} indices={}->{} proven={}->{} allocations={} requested={} peak_live={} final_live={} capacity={}",
        case.name,
        percentile(&baseline_prepared_ms, 1, 4),
        percentile(&baseline_prepared_ms, 1, 2),
        percentile(&baseline_prepared_ms, 3, 4),
        percentile(&optimized_prepared_ms, 1, 4),
        percentile(&optimized_prepared_ms, 1, 2),
        percentile(&optimized_prepared_ms, 3, 4),
        percentile(&baseline_compile_jit_exec_ms, 1, 4),
        percentile(&baseline_compile_jit_exec_ms, 1, 2),
        percentile(&baseline_compile_jit_exec_ms, 3, 4),
        percentile(&optimized_compile_jit_exec_ms, 1, 4),
        percentile(&optimized_compile_jit_exec_ms, 1, 2),
        percentile(&optimized_compile_jit_exec_ms, 3, 4),
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
        baseline_shape.array_lengths,
        optimized_shape.array_lengths,
        baseline_shape.loop_array_lengths,
        optimized_shape.loop_array_lengths,
        baseline_shape.array_indices,
        optimized_shape.array_indices,
        baseline_shape.proven_indices,
        optimized_shape.proven_indices,
        stats.total_allocations,
        stats.requested_bytes,
        stats.peak_used_bytes,
        stats.used_bytes,
        stats.reserved_bytes,
    );
}

fn main() {
    let cases = [
        Case {
            name: "allocate",
            source: "public long Run() { int[] values = new int[5000000]; long result = values.Length; return result; }",
            expected: ExecutionValue::Long(5_000_000),
        },
        Case {
            name: "scalar",
            source: "public long Run() { long total = 0; for (int i = 0; i < 5000000; i++) { total += i; } return total; }",
            expected: ExecutionValue::Long(12_499_997_500_000),
        },
        Case {
            name: "write",
            source: "public long Run() { int[] values = new int[5000000]; for (int i = 0; i < values.Length; i++) { values[i] = i; } long result = values[4999999]; return result; }",
            expected: ExecutionValue::Long(4_999_999),
        },
        Case {
            name: "read",
            source: "public long Run() { int[] values = new int[5000000]; long total = 0; for (int i = 0; i < values.Length; i++) { total += values[i]; } return total; }",
            expected: ExecutionValue::Long(0),
        },
        Case {
            name: "read-write",
            source: "public long Run() { int[] values = new int[5000000]; for (int i = 0; i < values.Length; i++) { values[i] = i; } for (int i = 0; i < values.Length; i++) { values[i] = values[i] + 1; } long result = values[4999999]; return result; }",
            expected: ExecutionValue::Long(5_000_000),
        },
        Case {
            name: "modulo",
            source: "public long Run() { long total = 0; for (int i = 0; i < 5000000; i++) { total += i % 97; } return total; }",
            expected: ExecutionValue::Long(239_998_879),
        },
        Case {
            name: "full",
            source: "public long Run() { int[] values = new int[5000000]; for (int i = 0; i < values.Length; i++) { values[i] = i % 97; } long total = 0; for (int i = 0; i < values.Length; i++) { total += values[i]; } return total; }",
            expected: ExecutionValue::Long(239_998_879),
        },
        Case {
            name: "constant",
            source: "public long Run() { int[] values = new int[1]; values[0] = 7; long total = 0; for (int i = 0; i < 5000000; i++) { total += values[0]; } return total; }",
            expected: ExecutionValue::Long(35_000_000),
        },
        Case {
            name: "random",
            source: "public long Run() { int[] values = new int[5000000]; for (int i = 0; i < values.Length; i++) { values[i] = i; } long total = 0; for (int i = 0; i < values.Length; i++) { int index = (i * 17) % 5000000; total += values[index]; } return total; }",
            expected: ExecutionValue::Long(12_499_997_500_000),
        },
    ];
    let selected = std::env::var("ASTER_ARRAY_CASE").ok();
    for case in &cases {
        if selected.as_deref().is_none_or(|name| name == case.name) {
            measure(case);
        }
    }
}
