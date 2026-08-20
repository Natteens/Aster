//! Informational release matrix for Task arguments, deterministic `WaitAll`,
//! and cooperative cancellation. Timings are machine-local medians and are
//! never correctness thresholds.
//!
//! Run with:
//!
//! ```console
//! cargo run --release -p aster-codegen-cranelift --example task_ergonomics_matrix
//! ```

use std::{fmt::Write as _, time::Instant};

use aster_codegen_cranelift::{ExecutionValue, execute};
use aster_compiler::compile;

const SAMPLES: usize = 7;

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

fn measure(name: &str, source: &str, expected: &ExecutionValue) {
    let mut compile_samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let started = Instant::now();
        compile(source).unwrap_or_else(|diagnostics| {
            panic!("{name} benchmark source must compile: {diagnostics:#?}")
        });
        compile_samples.push(started.elapsed().as_secs_f64() * 1_000.0);
    }
    let module = compile(source)
        .unwrap_or_else(|diagnostics| panic!("{name} source must compile: {diagnostics:#?}"))
        .mir;
    let mut execution_samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let started = Instant::now();
        let value = execute(&module, "Main")
            .unwrap_or_else(|error| panic!("{name} benchmark must execute: {error}"));
        execution_samples.push(started.elapsed().as_secs_f64() * 1_000.0);
        assert_eq!(&value, expected, "{name} checksum");
    }
    println!(
        "case={name:<22} frontend_ms={:>8.3} jit_execute_ms={:>9.3} value={expected}",
        median(compile_samples),
        median(execution_samples),
    );
}

fn repeated(shape: &str, iterations: i32) -> String {
    let (declaration, setup, call) = match shape {
        "zero" => (
            "public int Work() { return 42; }",
            "",
            "Task.Run(Work).Wait()",
        ),
        "one" => (
            "public int Work(int value) { return value; }",
            "",
            "Task.Run(Work, 42).Wait()",
        ),
        "four" => (
            "public int Work(int a, int b, int c, int d) { return a + b + c + d; }",
            "",
            "Task.Run(Work, 10, 11, 12, 9).Wait()",
        ),
        "struct" => (
            "public struct Payload { public int left; public long right; } public int Work(Payload value) { return value.left + (int)value.right; }",
            "Payload payload = Payload { left: 20, right: 22L };",
            "Task.Run(Work, payload).Wait()",
        ),
        _ => unreachable!("known benchmark shape"),
    };
    format!(
        "{declaration} public int Main() {{ {setup} int total = 0; int i = 0; while (i < {iterations}) {{ total += {call}; i += 1; }} return total; }}"
    )
}

fn wait_all(task_count: usize) -> String {
    let mut source = String::from(
        "public int Work(int value) { int total = value; int i = 0; while (i < 10000) { total = total + 1; i += 1; } return total; } public int Main() { ",
    );
    for index in 0..task_count {
        write!(source, "Task<int> task{index} = Task.Run(Work, {index}); ")
            .expect("writing to a String cannot fail");
    }
    source.push_str("int[] values = Task.WaitAll([");
    for index in 0..task_count {
        if index != 0 {
            source.push_str(", ");
        }
        write!(source, "task{index}").expect("writing to a String cannot fail");
    }
    source.push_str("]); int total = 0; for (int i = 0; i < values.Length; i++) { total += values[i]; } return total; }");
    source
}

fn main() {
    println!("Task ergonomics release matrix (informative only)");
    let iterations = 1_000;
    for shape in ["zero", "one", "four", "struct"] {
        measure(
            &format!("task_{shape}_1k"),
            &repeated(shape, iterations),
            &ExecutionValue::Int(42 * iterations),
        );
    }
    for task_count in [2_usize, 8, 64] {
        let expected = i32::try_from(task_count * 10_000 + task_count * (task_count - 1) / 2)
            .expect("small checksum fits int");
        measure(
            &format!("wait_all_{task_count}"),
            &wait_all(task_count),
            &ExecutionValue::Int(expected),
        );
    }
    measure(
        "cancel_request",
        "public int Work(int limit) { int i = 0; while (i < limit) { if (Task.IsCancellationRequested()) { return i; } i += 1; } return i; } public int Main() { Task<int> task = Task.Run(Work, 100000000); return task.Cancel() ? 1 : 0; }",
        &ExecutionValue::Int(1),
    );
    measure(
        "explicit_check_loop",
        "public int Work(int limit) { int i = 0; while (i < limit) { if (Task.IsCancellationRequested()) { return -1; } i += 1; } return i; } public int Main() { return Task.Run(Work, 1000000).Wait(); }",
        &ExecutionValue::Int(1_000_000),
    );
}
