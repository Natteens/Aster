//! Manual, non-scientific timing/memory snapshot for the concurrent
//! constructs delivered across Lote 6 (`Parallel.For`, `Parallel.ForEach`,
//! `Parallel.Reduce`) against their sequential equivalent.
//!
//! Run with:
//!
//! ```console
//! cargo run --release -p aster-codegen-cranelift --example parallel_matrix
//! ```
//!
//! This is deliberately **not** the `memory_matrix`/`compare.mjs` baseline
//! system: that infrastructure is shaped around allocation *region*
//! (temporary vs. persistent) for a single sequential invocation, with no
//! concept of worker count. Retrofitting a worker-count axis into that JSON
//! schema and comparator would be a structural change to established,
//! frozen infrastructure; adding a small, separate, isolated harness here
//! avoids that without duplicating anything the existing one already does.
//!
//! Timing is informative only:
//! - never asserted against a threshold;
//! - never used to claim `Parallel` is "faster" merely because it uses more
//!   than one worker;
//! - never compared across machines, toolchains, or CI runs.
//!
//! ## What can and cannot be separated
//!
//! `execute_with_stats` (the only public entry point) times "JIT + prepare +
//! run" as one span, exactly like `memory_matrix`'s own `jit_and_execute`
//! metric — Cranelift code generation and the actual execution are not
//! measured separately there either. For a `Parallel.*` call specifically,
//! this program additionally cannot separate, without instrumenting the
//! runtime internals (out of scope for a benchmark harness):
//! - the host-side array copy cost (`copy_scalar_array`) from the rest of
//!   the call;
//! - the chunk-dispatch/coordination cost (`ExecutionPool`/`TaskRuntime`)
//!   from the workers' own computation.
//!
//! Both are reported as a single "execution" span per construct; this
//! limitation is recorded here rather than restructuring the runtime to
//! expose sub-spans it was not designed to expose.
//!
//! `Parallel.For`/`Parallel.ForEach` produce no aggregate value by design
//! (Lote 6A/6B: no shared mutable state crosses a worker boundary), so their
//! "checksum" is the count of indices/elements that ran without a controlled
//! error — not a computed sum like `Parallel.Reduce`'s.
//!
//! Worker count is whatever `std::thread::available_parallelism()` reports
//! on this machine: the public API has no override, and adding one would be
//! a new public surface this batch does not introduce.

use std::{fmt::Write as _, time::Instant};

use aster_codegen_cranelift::{ExecutionValue, MemoryStats, execute_with_stats};
use aster_compiler::compile;

const SAMPLES: usize = 5;

struct Timing {
    frontend_compile_ms: f64,
    jit_and_execute_ms: f64,
}

fn timed_compile(source: &str) -> (aster_compiler::mir::Module, f64) {
    let start = Instant::now();
    let module = compile(source)
        .unwrap_or_else(|diagnostics| panic!("benchmark source must compile: {diagnostics:#?}"))
        .mir;
    (module, start.elapsed().as_secs_f64() * 1000.0)
}

/// Times only `execute_with_stats` itself: `frontend_compile_ms` is filled in
/// by the caller from the earlier, separately timed `compile` call, so it is
/// never double-counted here.
fn timed_execute(
    module: &aster_compiler::mir::Module,
    entry: &str,
) -> (ExecutionValue, f64, MemoryStats) {
    let mut samples = Vec::with_capacity(SAMPLES);
    let mut result = None;
    for _ in 0..SAMPLES {
        let start = Instant::now();
        let (value, stats) = execute_with_stats(module, entry)
            .unwrap_or_else(|error| panic!("benchmark entry {entry} must run: {error}"));
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
        if let Some((previous_value, previous_stats)) = &result {
            assert_eq!(
                &value, previous_value,
                "benchmark result changed between samples"
            );
            assert_eq!(
                &stats, previous_stats,
                "memory metrics changed between samples"
            );
        } else {
            result = Some((value, stats));
        }
    }
    samples.sort_by(f64::total_cmp);
    let (value, stats) = result.expect("at least one timing sample");
    (value, samples[SAMPLES / 2], stats)
}

fn report_case(case: &str, source: &str, entry: &str, expected: &ExecutionValue) {
    let (module, frontend_compile_ms) = timed_compile(source);
    let (value, jit_and_execute_ms, memory) = timed_execute(&module, entry);
    let timing = Timing {
        frontend_compile_ms,
        jit_and_execute_ms,
    };
    let status = if value == *expected { "ok" } else { "MISMATCH" };
    println!(
        "{case:<28} status={status:<8} frontend_compile_ms={:>8.3} jit_and_execute_ms={:>9.3} requested_bytes={:>10} used_bytes={:>10} reserved_bytes={:>10} value={value}",
        timing.frontend_compile_ms,
        timing.jit_and_execute_ms,
        memory.requested_bytes,
        memory.used_bytes,
        memory.reserved_bytes,
    );
}

fn task_run_source(tasks: usize, iterations: i64) -> String {
    let mut source = format!(
        "public long Work() {{ \
             long total = 0; \
             for (long i = 0; i < {iterations}; i++) {{ total += i; }} \
             return total; \
         }} \
         public long Main() {{ "
    );
    for task in 0..tasks {
        write!(source, "Task<long> task{task} = Task.Run(Work); ")
            .expect("writing into a String cannot fail");
    }
    source.push_str("long total = 0; ");
    for task in 0..tasks {
        write!(source, "total += task{task}.Wait(); ").expect("writing into a String cannot fail");
    }
    source.push_str("return total; }");
    source
}

fn sequential_task_source(tasks: usize, iterations: i64) -> String {
    let mut source = format!(
        "public long Work() {{ \
             long total = 0; \
             for (long i = 0; i < {iterations}; i++) {{ total += i; }} \
             return total; \
         }} \
         public long Main() {{ long total = 0; "
    );
    for _ in 0..tasks {
        source.push_str("total += Work(); ");
    }
    source.push_str("return total; }");
    source
}

fn repeated_task_source(tasks: usize, iterations: i64, parallel: bool) -> String {
    let call = if parallel {
        "Task.Run(Work).Wait()"
    } else {
        "Work()"
    };
    format!(
        "public long Work() {{ long total = 0; for (long i = 0; i < {iterations}; i++) {{ total += i; }} return total; }} \
         public long Main() {{ long total = 0; int task = 0; while (task < {tasks}) {{ total += {call}; task += 1; }} return total; }}"
    )
}

fn task_cases() {
    for &(tasks, iterations) in &[
        (1_usize, 0_i64),
        (2, 0),
        (4, 0),
        (8, 0),
        (16, 0),
        (1, 100_000),
        (16, 100_000),
        (1, 1_000_000),
        (16, 1_000_000),
        (1, 10_000_000),
        (16, 10_000_000),
    ] {
        let task_count = i64::try_from(tasks).expect("benchmark task count fits in long");
        let expected = task_count * iterations * (iterations - 1) / 2;
        report_case(
            &format!("sequential_{tasks}x{iterations}"),
            &sequential_task_source(tasks, iterations),
            "Main",
            &ExecutionValue::Long(expected),
        );
        report_case(
            &format!("task_run_{tasks}x{iterations}"),
            &task_run_source(tasks, iterations),
            "Main",
            &ExecutionValue::Long(expected),
        );
    }

    for &(tasks, iterations) in &[(100_usize, 0_i64), (1_000, 0), (100, 100_000)] {
        let task_count = i64::try_from(tasks).expect("benchmark task count fits in long");
        let expected = task_count * iterations * (iterations - 1) / 2;
        report_case(
            &format!("sequential_reused_{tasks}x{iterations}"),
            &repeated_task_source(tasks, iterations, false),
            "Main",
            &ExecutionValue::Long(expected),
        );
        report_case(
            &format!("task_reused_{tasks}x{iterations}"),
            &repeated_task_source(tasks, iterations, true),
            "Main",
            &ExecutionValue::Long(expected),
        );
    }
}

fn main() {
    let worker_count = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    println!("Lote 6 Parallel.* manual timing snapshot (informative only, not a benchmark)");
    println!("worker_count (available_parallelism) = {worker_count}");
    println!();

    for &elements in &[1_i64, 32, 1_000, 100_000, 1_000_000] {
        println!("--- elements = {elements} ---");
        let expected_sum = ExecutionValue::Long(elements * (elements + 1) / 2);

        report_case(
            "sequential_sum",
            &format!(
                "public long Main() {{ \
                     long total = 0; \
                     for (long i = 1; i <= {elements}; i++) {{ total += i; }} \
                     return total; \
                 }}"
            ),
            "Main",
            &expected_sum,
        );

        report_case(
            "parallel_reduce_sum",
            &format!(
                "public long AddValue(long accumulator, long value) {{ return accumulator + value; }} \
                 public long AddPartial(long left, long right) {{ return left + right; }} \
                 public long[] Range() {{ \
                     long[] values = new long[{elements}]; \
                     for (int i = 0; i < {elements}; i++) {{ values[i] = i + 1; }} \
                     return values; \
                 }} \
                 public long Main() {{ return Parallel.Reduce(Range(), 0L, AddValue, AddPartial); }}"
            ),
            "Main",
            &expected_sum,
        );

        // `Parallel.For`/`ForEach` produce no aggregate by design; the
        // "checksum" here is simply that every index/element ran without a
        // controlled error, reported as the range length itself.
        report_case(
            "parallel_for_dispatch_only",
            &format!(
                "public void Body(int index) {{ }} \
                 public long Main() {{ Parallel.For(0, {elements}, Body); return {elements}L; }}"
            ),
            "Main",
            &ExecutionValue::Long(elements),
        );

        report_case(
            "parallel_for_each_dispatch_only",
            &format!(
                "public void Body(long value) {{ }} \
                 public long[] Range() {{ \
                     long[] values = new long[{elements}]; \
                     for (int i = 0; i < {elements}; i++) {{ values[i] = i; }} \
                     return values; \
                 }} \
                 public long Main() {{ Parallel.ForEach(Range(), Body); return {elements}L; }}"
            ),
            "Main",
            &ExecutionValue::Long(elements),
        );

        println!();
    }

    task_cases();

    println!(
        "Limitation: copy cost (ForEach/Reduce host-side array copy) and coordination cost \
         (chunk dispatch, worker pool creation/teardown) are not separable from \
         jit_and_execute_ms without instrumenting task_runtime.rs/worker_pool.rs internals; \
         this harness reports them combined rather than restructuring the runtime."
    );
}
