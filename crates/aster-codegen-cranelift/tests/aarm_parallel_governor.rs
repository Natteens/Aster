#![cfg(feature = "aarm-telemetry")]

use std::sync::Arc;

use aster_codegen_cranelift::{ExecutionValue, execute_with_aarm_parallel_governor};
use aster_compiler::compile;
use aster_runtime::{ExecutionContext, MemoryGovernor};

#[test]
fn page_entitlements_choose_the_same_logical_winners_under_scheduler_pressure() {
    let module = compile(
        "public void Body(int index) { int[] scratch = new int[1]; } \
         public int Main() { Parallel.For(0, 4, Body); return 4; }",
    )
    .expect("source compiles")
    .mir;
    for worker_count in [4, 16] {
        for funded_pages in 0..=4 {
            let mut expected_error = None;
            for repetition in 0..10 {
                let perturbation = std::thread::spawn(|| {
                    for _ in 0..64 {
                        std::thread::yield_now();
                    }
                });
                let governor = Arc::new(MemoryGovernor::new(
                    funded_pages * ExecutionContext::AARM_MIN_PAGE_CAPACITY_BYTES,
                ));
                let outcome = execute_with_aarm_parallel_governor(
                    &module,
                    "Main",
                    worker_count,
                    Arc::clone(&governor),
                );
                perturbation
                    .join()
                    .expect("scheduler perturbation completes");

                if funded_pages == 4 {
                    let (value, _, plans, snapshots) = outcome.expect("all chunks have one page");
                    assert_eq!(value, ExecutionValue::Int(4));
                    assert_eq!(
                        plans[0].chunk_budgets_bytes,
                        [u64::try_from(ExecutionContext::AARM_MIN_PAGE_CAPACITY_BYTES)
                            .expect("minimum page capacity fits u64"); 4]
                    );
                    assert_eq!(snapshots.len(), 4);
                } else {
                    let error = outcome.expect_err("first unfunded logical chunk fails");
                    let marker = format!("Parallel logical index {funded_pages}");
                    assert!(
                        error.message().contains(&marker),
                        "unexpected error: {error}"
                    );
                    assert_eq!(
                        expected_error.get_or_insert_with(|| error.message().to_owned()),
                        error.message(),
                        "worker_count {worker_count}, funded_pages {funded_pages}, repetition {repetition}"
                    );
                }
                let telemetry = governor.telemetry();
                assert_eq!(telemetry.current_capacity_bytes, 0);
                assert!(telemetry.peak_capacity_bytes <= telemetry.hard_limit_bytes);
                assert_eq!(
                    telemetry.grant_events,
                    u64::try_from(funded_pages).expect("funded page count fits u64")
                );
                assert_eq!(telemetry.grant_events, telemetry.release_events);
                assert_eq!(
                    telemetry.granted_bytes_cumulative,
                    telemetry.released_bytes_cumulative
                );
            }
        }
    }
}

#[test]
fn oversized_first_page_uses_its_actual_capacity_not_minimum_page_units() {
    let module = compile(
        "public void Body(int index) { int[] scratch = new int[20000]; } \
         public int Main() { Parallel.For(0, 1, Body); return 1; }",
    )
    .expect("source compiles")
    .mir;

    let denied_governor = Arc::new(MemoryGovernor::new(
        ExecutionContext::AARM_DEFAULT_PAGE_CAPACITY_BYTES,
    ));
    let error =
        execute_with_aarm_parallel_governor(&module, "Main", 1, Arc::clone(&denied_governor))
            .expect_err("oversized first page exceeds a regular-page byte ceiling");
    assert!(error.message().contains("Parallel logical index 0"));
    assert_eq!(denied_governor.telemetry().grant_events, 0);

    let admitted_governor = Arc::new(MemoryGovernor::new(
        2 * ExecutionContext::AARM_DEFAULT_PAGE_CAPACITY_BYTES,
    ));
    let (value, _, plans, snapshots) =
        execute_with_aarm_parallel_governor(&module, "Main", 1, Arc::clone(&admitted_governor))
            .expect("actual oversized page fits the larger byte ceiling");
    assert_eq!(value, ExecutionValue::Int(1));
    assert_eq!(
        plans[0].chunk_budgets_bytes,
        [
            u64::try_from(2 * ExecutionContext::AARM_DEFAULT_PAGE_CAPACITY_BYTES)
                .expect("test budget fits u64")
        ]
    );
    assert_eq!(snapshots.len(), 1);
    assert_eq!(
        snapshots[0].total.events.fresh_oversized_page_allocations,
        1
    );
    let telemetry = admitted_governor.telemetry();
    assert!(
        telemetry.peak_capacity_bytes
            > u64::try_from(ExecutionContext::AARM_DEFAULT_PAGE_CAPACITY_BYTES)
                .expect("default page capacity fits u64")
    );
    assert_eq!(telemetry.current_capacity_bytes, 0);
    assert_eq!(telemetry.grant_events, 1);
    assert_eq!(telemetry.release_events, 1);
}

#[test]
fn main_retained_capacity_is_removed_before_parallel_headroom_is_partitioned() {
    let module = compile(
        "public void Body(int value) { int[] scratch = new int[1]; } \
         public int Main() { int[] values = [1, 2, 3, 4]; Parallel.ForEach(values, Body); return values.Length; }",
    )
    .expect("source compiles")
    .mir;
    let governor = Arc::new(MemoryGovernor::new(32 * 1024));

    let (value, memory, plans, worker_snapshots) =
        execute_with_aarm_parallel_governor(&module, "Main", 4, Arc::clone(&governor))
            .expect("governed Parallel.ForEach succeeds");

    assert_eq!(value, ExecutionValue::Int(4));
    assert_eq!(plans.len(), 1);
    assert_eq!(worker_snapshots.len(), 4);
    let plan = &plans[0];
    assert_eq!(plan.operation, "Parallel.ForEach");
    assert_eq!(
        plan.initial_governor_capacity_bytes,
        memory.persistent.arena_capacity_bytes
    );
    assert_eq!(
        plan.initial_governor_capacity_bytes + plan.available_headroom_bytes,
        32 * 1024
    );
    assert_eq!(
        plan.chunk_budgets_bytes.iter().sum::<u64>(),
        plan.available_headroom_bytes
    );
    let during_main = memory.governor.expect("governor telemetry is present");
    assert!(during_main.peak_capacity_bytes <= during_main.hard_limit_bytes);
    assert_eq!(
        during_main.granted_bytes_cumulative - during_main.released_bytes_cumulative,
        during_main.current_capacity_bytes
    );
    assert_eq!(governor.telemetry().current_capacity_bytes, 0);
}

#[test]
fn task_run_remains_ungoverned_in_the_experimental_parallel_entry() {
    let module = compile(
        "public int Work() { int[] values = new int[20000]; return values.Length; } \
         public int Main() { return Task.Run(Work).Wait(); }",
    )
    .expect("source compiles")
    .mir;
    let governor = Arc::new(MemoryGovernor::new(4 * 1024));

    let (value, memory, plans, worker_snapshots) =
        execute_with_aarm_parallel_governor(&module, "Main", 4, Arc::clone(&governor))
            .expect("Task.Run keeps its ordinary independent context");

    assert_eq!(value, ExecutionValue::Int(20_000));
    assert!(plans.is_empty());
    assert!(worker_snapshots.is_empty());
    assert_eq!(
        memory
            .governor
            .expect("main context is governed")
            .peak_capacity_bytes,
        0
    );
    assert_eq!(governor.telemetry().current_capacity_bytes, 0);
}
