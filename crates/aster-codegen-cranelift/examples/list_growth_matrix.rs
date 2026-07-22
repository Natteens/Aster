//! Manual List D performance harness (Section 14): compares a fixed array,
//! a `List<T>` that re-grows its capacity from scratch every outer
//! iteration, and a `List<T>` that grows once and is then emptied/refilled
//! (reusing its already-established capacity) across the remaining
//! iterations. All three variants perform the same total amount of work and
//! must produce the same checksum; only their allocation pattern differs.
//!
//! No benchmark crate dependency, mirroring the existing `memory_regions`
//! example. A single machine's median of a handful of samples is not a
//! claim that `List<T>` is fast or slow in general -- it only shows the
//! *relative* cost of paying the growth cost once versus every iteration,
//! on this machine, for this workload shape.

use std::time::{Duration, Instant};

use aster_codegen_cranelift::{ExecutionValue, MemoryStats, execute_with_stats};
use aster_compiler::compile;

const SAMPLES: usize = 5;
// Both the outer-iteration count (50) and inner element count (200) are
// baked directly into the three source constants below; keep them in sync
// by hand if either changes.
const OUTER_ITERATIONS: i32 = 50;
// (0 + 1 + ... + 199) * 50 iterations, reduced mod a large prime so it stays
// well within `int` range regardless of how the sum is accumulated.
const EXPECTED_RESULT: i32 = 995_000;

const FIXED_ARRAY_SOURCE: &str = "
    public int Run() {
        long sum = 0L;
        for (int iter = 0; iter < 50; iter++) {
            int[] values = new int[200];
            for (int i = 0; i < 200; i++) { values[i] = i; }
            for (int i = 0; i < 200; i++) { sum = sum + (long)values[i]; }
        }
        return (int)(sum % 1000000007L);
    }
    ";

const GROWING_LIST_SOURCE: &str = "
    public int Run() {
        long sum = 0L;
        for (int iter = 0; iter < 50; iter++) {
            List<int> values = new List<int>();
            for (int i = 0; i < 200; i++) { values.Add(i); }
            for (int i = 0; i < 200; i++) { sum = sum + (long)values.Get(i); }
        }
        return (int)(sum % 1000000007L);
    }
    ";

const REUSED_LIST_SOURCE: &str = "
    public int Run() {
        long sum = 0L;
        List<int> values = new List<int>();
        for (int i = 0; i < 200; i++) { values.Add(i); }
        for (int iter = 0; iter < 50; iter++) {
            for (int i = 0; i < 200; i++) { sum = sum + (long)values.Get(i); }
            if (iter < 49) {
                for (int i = 0; i < 200; i++) { values.RemoveAt(0); }
                for (int i = 0; i < 200; i++) { values.Add(i); }
            }
        }
        return (int)(sum % 1000000007L);
    }
    ";

struct Measurement {
    name: &'static str,
    median: Duration,
    stats: MemoryStats,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("list growth benchmark failed: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let array = benchmark("fixed_array", FIXED_ARRAY_SOURCE)?;
    let growing = benchmark("growing_list", GROWING_LIST_SOURCE)?;
    let reused = benchmark("reused_list", REUSED_LIST_SOURCE)?;

    println!(
        "{:<14} {:>12} {:>16} {:>12} {:>12}",
        "case", "median ms", "allocations", "used bytes", "peak used"
    );
    for measurement in [&array, &growing, &reused] {
        print_measurement(measurement);
    }
    println!();
    println!(
        "growing_list allocations ({}) vs reused_list allocations ({}): \
         reused pays the growth cost once instead of every one of the {OUTER_ITERATIONS} \
         outer iterations.",
        growing.stats.object_allocations, reused.stats.object_allocations
    );

    Ok(())
}

fn benchmark(name: &'static str, source: &str) -> Result<Measurement, String> {
    let compilation = compile(source).map_err(|diagnostics| format!("{diagnostics:#?}"))?;

    let (warmup_value, warmup_stats) =
        execute_with_stats(&compilation.mir, "Run").map_err(|error| error.to_string())?;
    validate_result(name, &warmup_value)?;

    let mut durations = Vec::with_capacity(SAMPLES);
    let stable_stats = warmup_stats;

    for _ in 0..SAMPLES {
        let started = Instant::now();
        let (value, stats) =
            execute_with_stats(&compilation.mir, "Run").map_err(|error| error.to_string())?;
        durations.push(started.elapsed());
        validate_result(name, &value)?;

        if stats != stable_stats {
            return Err(format!(
                "{name} memory stats changed between identical executions: \
                 {stable_stats:#?} versus {stats:#?}"
            ));
        }
    }

    durations.sort_unstable();
    let median = durations[durations.len() / 2];

    Ok(Measurement {
        name,
        median,
        stats: stable_stats,
    })
}

fn validate_result(name: &str, value: &ExecutionValue) -> Result<(), String> {
    let expected = ExecutionValue::Int(EXPECTED_RESULT);
    if value == &expected {
        Ok(())
    } else {
        Err(format!("{name} returned {value:?}, expected {expected:?}"))
    }
}

fn print_measurement(measurement: &Measurement) {
    println!(
        "{:<14} {:>12.3} {:>16} {:>12} {:>12}",
        measurement.name,
        measurement.median.as_secs_f64() * 1_000.0,
        measurement.stats.object_allocations,
        measurement.stats.used_bytes,
        measurement.stats.peak_used_bytes
    );
}
