use std::time::{Duration, Instant};

use aster_codegen_cranelift::{ExecutionValue, MemoryStats, execute_with_stats};
use aster_compiler::compile;

const SAMPLES: usize = 5;
const EXPECTED_RESULT: i32 = 4_200_000;
const TEMPORARY_SOURCE: &str = include_str!("../../../benchmarks/memory/temporary.aster");
const PERSISTENT_SOURCE: &str = include_str!("../../../benchmarks/memory/persistent.aster");

struct Measurement {
    name: &'static str,
    median: Duration,
    stats: MemoryStats,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("memory benchmark failed: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let temporary = benchmark("temporary", TEMPORARY_SOURCE)?;
    let persistent = benchmark("persistent", PERSISTENT_SOURCE)?;

    if temporary.stats.used_bytes != 0 {
        return Err(format!(
            "temporary workload retained {} used bytes",
            temporary.stats.used_bytes
        ));
    }
    if persistent.stats.used_bytes == 0 {
        return Err(String::from(
            "persistent workload unexpectedly retained zero used bytes",
        ));
    }
    if temporary.stats.total_allocations != persistent.stats.total_allocations {
        return Err(format!(
            "workloads executed different allocation counts: {} versus {}",
            temporary.stats.total_allocations, persistent.stats.total_allocations
        ));
    }

    println!(
        "{:<12} {:>12} {:>12} {:>12} {:>12} {:>12}",
        "case", "median ms", "allocations", "requested", "used", "peak used"
    );
    print_measurement(&temporary);
    print_measurement(&persistent);
    println!();
    println!(
        "temporary reserved: {} bytes; persistent reserved: {} bytes",
        temporary.stats.reserved_bytes, persistent.stats.reserved_bytes
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
        "{:<12} {:>12.3} {:>12} {:>12} {:>12} {:>12}",
        measurement.name,
        measurement.median.as_secs_f64() * 1_000.0,
        measurement.stats.total_allocations,
        measurement.stats.requested_bytes,
        measurement.stats.used_bytes,
        measurement.stats.peak_used_bytes
    );
}
