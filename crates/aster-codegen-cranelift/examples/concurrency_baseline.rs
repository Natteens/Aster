//! Concurrency baseline: `Task.Run` and `Parallel` workloads at explicit worker
//! counts (BENCH-001, task T-BENCH-2).
//!
//! `perf_baseline` cannot run these programs because its prepared sequential
//! execution rejects `Task.Run`. Each sample here is one complete execution
//! through `execute_with_worker_count`: JIT preparation of the main module and of
//! every worker's module, the run, and teardown. Phases are not separated.
//!
//! Checksums are computed independently of ASTER and are the only asserted
//! values. Timings are informative: compare medians on one machine only.
//!
//! ```console
//! cargo run --release -p aster-codegen-cranelift --example concurrency_baseline
//! cargo run --release -p aster-codegen-cranelift --example concurrency_baseline -- --workers 1,2,4,8 --samples 7
//! ```

#![allow(clippy::must_use_candidate)]

use std::time::Instant;

use aster_codegen_cranelift::{ExecutionValue, execute_with_worker_count};
use aster_compiler::compile;

pub struct Workload {
    pub name: &'static str,
    pub source: &'static str,
    pub expected: i64,
}

pub const WORKLOADS: [Workload; 2] = [
    Workload {
        name: "task_swarm",
        source: include_str!("../../../benchmarks/performance/task_swarm.aster"),
        expected: 160_530_328,
    },
    Workload {
        name: "parallel_scaling",
        source: include_str!("../../../benchmarks/performance/parallel_scaling.aster"),
        expected: 6_552_039_487,
    },
];

pub struct Sample {
    pub workers: usize,
    pub checksum: i64,
    pub nanos: Vec<u128>,
}

/// Runs one workload `samples` times at `workers` workers and verifies every
/// checksum.
///
/// # Errors
///
/// Returns a message for a compile error, a runtime error, or a wrong checksum.
pub fn measure(workload: &Workload, workers: usize, samples: usize) -> Result<Sample, String> {
    let compilation = compile(workload.source).map_err(|diagnostics| {
        format!("frontend failed with {} diagnostic(s)", diagnostics.len())
    })?;
    let mut nanos = Vec::with_capacity(samples);
    for _ in 0..samples {
        let started = Instant::now();
        let (value, _) = execute_with_worker_count(&compilation.mir, "Run", workers)
            .map_err(|error| error.to_string())?;
        nanos.push(started.elapsed().as_nanos());
        let ExecutionValue::Long(checksum) = value else {
            return Err(format!("unexpected result {value:?}"));
        };
        if checksum != workload.expected {
            return Err(format!(
                "checksum {checksum} differs from expected {}",
                workload.expected
            ));
        }
    }
    Ok(Sample {
        workers,
        checksum: workload.expected,
        nanos,
    })
}

fn parse_list(value: &str) -> Result<Vec<usize>, String> {
    value
        .split(',')
        .map(|item| match item.parse::<usize>() {
            Ok(count) if (1..=256).contains(&count) => Ok(count),
            _ => Err(format!("invalid count `{item}` (expected 1..=256)")),
        })
        .collect()
}

/// Parses `--workers a,b,c` and `--samples n`.
///
/// # Errors
///
/// Returns a message for an unknown flag or an invalid value.
pub fn parse_args<I: IntoIterator<Item = String>>(
    arguments: I,
) -> Result<(Vec<usize>, usize), String> {
    let mut workers = vec![1, 2, 4];
    let mut samples = 5;
    let mut arguments = arguments.into_iter();
    while let Some(flag) = arguments.next() {
        let value = arguments
            .next()
            .ok_or_else(|| format!("{flag} requires a value"))?;
        match flag.as_str() {
            "--workers" => workers = parse_list(&value)?,
            "--samples" => {
                samples = parse_list(&value)?
                    .first()
                    .copied()
                    .ok_or("--samples requires a value")?;
            }
            _ => return Err(format!("unknown argument `{flag}`")),
        }
    }
    Ok((workers, samples))
}

fn main() {
    let (workers, samples) = match parse_args(std::env::args().skip(1)) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("error: {message}");
            std::process::exit(2);
        }
    };
    let mut failed = false;
    for workload in &WORKLOADS {
        for &count in &workers {
            match measure(workload, count, samples) {
                Ok(sample) => {
                    let mut sorted = sample.nanos.clone();
                    sorted.sort_unstable();
                    #[allow(clippy::cast_precision_loss)]
                    let median_ms = sorted[sorted.len() / 2] as f64 / 1_000_000.0;
                    println!(
                        "{:<18} workers={:<3} checksum={} median={median_ms:.3} ms",
                        workload.name, sample.workers, sample.checksum
                    );
                }
                Err(error) => {
                    failed = true;
                    println!("{:<18} workers={count:<3} FAIL: {error}", workload.name);
                }
            }
        }
    }
    if failed {
        std::process::exit(1);
    }
}
