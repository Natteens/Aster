//! Deterministic memory facts for the long-running BENCH-001 workloads.
//!
//! Each workload exposes `Run` (N iterations) and `RunLong` (10N iterations).
//! The checksums are computed independently of ASTER. The memory assertions
//! record current behavior as class A invariants: they are exact facts about
//! today's region model, not performance targets. A change to the memory model
//! is expected to change them and must update this file with an explanation.

use aster_codegen_cranelift::PreparedSequentialExecution;
use aster_compiler::compile;
use aster_runtime::MemoryStats;

struct LongRunning {
    name: &'static str,
    source: &'static str,
    expected_run: i64,
    expected_long: i64,
}

const WORKLOADS: [LongRunning; 4] = [
    LongRunning {
        name: "longrun_frame",
        source: include_str!("../../../benchmarks/performance/longrun_frame.aster"),
        expected_run: 16_351_259,
        expected_long: 164_097_002,
    },
    LongRunning {
        name: "longrun_request",
        source: include_str!("../../../benchmarks/performance/longrun_request.aster"),
        expected_run: 45_498_889,
        expected_long: 455_188_889,
    },
    LongRunning {
        name: "cache_churn",
        source: include_str!("../../../benchmarks/performance/cache_churn.aster"),
        expected_run: 9_703_250,
        expected_long: 99_613_250,
    },
    LongRunning {
        name: "collection_growth",
        source: include_str!("../../../benchmarks/performance/collection_growth.aster"),
        expected_run: 56_269,
        expected_long: 767_462,
    },
];

fn run(workload: &LongRunning, entry: &str) -> (i64, MemoryStats) {
    let compilation = compile(workload.source)
        .unwrap_or_else(|diagnostics| panic!("{}: {diagnostics:?}", workload.name));
    let program = PreparedSequentialExecution::prepare(&compilation.mir, entry)
        .unwrap_or_else(|error| panic!("{} {entry}: {error}", workload.name));
    let (value, stats) = program
        .invoke_with_stats()
        .unwrap_or_else(|error| panic!("{} {entry}: {error}", workload.name));
    let checksum = match value {
        aster_codegen_cranelift::ExecutionValue::Long(value) => value,
        other => panic!("{} {entry}: unexpected result {other:?}", workload.name),
    };
    (checksum, stats)
}

fn facts(stats: &MemoryStats) -> (u64, u64, u64, u64) {
    (
        stats.total_allocations,
        stats.requested_bytes,
        stats.used_bytes,
        stats.peak_used_bytes,
    )
}

#[test]
fn long_running_workloads_have_independent_checksums_at_both_scales() {
    for workload in &WORKLOADS {
        assert_eq!(
            run(workload, "Run").0,
            workload.expected_run,
            "{}",
            workload.name
        );
        assert_eq!(
            run(workload, "RunLong").0,
            workload.expected_long,
            "{}",
            workload.name
        );
    }
}

#[test]
fn memory_statistics_are_deterministic_across_runs() {
    for workload in &WORKLOADS {
        for entry in ["Run", "RunLong"] {
            let first = facts(&run(workload, entry).1);
            let second = facts(&run(workload, entry).1);
            assert_eq!(first, second, "{} {entry}", workload.name);
        }
    }
}

/// Current behavior, recorded so that a memory-model change is visible: every
/// value these workloads stop using escapes its function, so the region model
/// retains it until the run ends and live bytes grow with the iteration count.
/// The MEM-EXP-1 gate (decision closure §4.C) requires the opposite once
/// counting exists: live bytes at 10N within 64 KiB of live bytes at N.
#[test]
fn live_memory_grows_with_iteration_count_under_the_current_region_model() {
    for workload in &WORKLOADS {
        let short = run(workload, "Run").1.used_bytes;
        let long = run(workload, "RunLong").1.used_bytes;
        assert!(
            long > short.saturating_mul(5),
            "{}: used {short} at N and {long} at 10N; if this now stays flat, update the \
             memory-model status in docs/proposals/architecture-2026/07-tasks.md",
            workload.name
        );
    }
}

#[test]
fn report_memory_facts() {
    // Printed for the baseline procedure (`--nocapture`); not asserted here.
    for workload in &WORKLOADS {
        for entry in ["Run", "RunLong"] {
            let (_, stats) = run(workload, entry);
            println!(
                "{} {entry}: allocations={} requested={} used={} peak_used={} reserved={}",
                workload.name,
                stats.total_allocations,
                stats.requested_bytes,
                stats.used_bytes,
                stats.peak_used_bytes,
                stats.reserved_bytes
            );
        }
    }
}
