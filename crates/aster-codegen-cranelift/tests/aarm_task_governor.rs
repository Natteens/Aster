#![cfg(feature = "aarm-telemetry")]

use std::sync::Arc;

use aster_codegen_cranelift::{ExecutionValue, execute_with_aarm_task_governor};
use aster_compiler::compile;
use aster_runtime::{ExecutionContext, MemoryGovernor};

fn task_swarm_source() -> &'static str {
    "public int Small() { int[] scratch = new int[1]; scratch[0] = 1; return scratch[0]; } \
     public int Main() { \
       Task<int> a = Task.Run(Small); Task<int> b = Task.Run(Small); \
       Task<int> c = Task.Run(Small); Task<int> d = Task.Run(Small); \
       Task<int> e = Task.Run(Small); Task<int> f = Task.Run(Small); \
       Task<int> g = Task.Run(Small); Task<int> h = Task.Run(Small); \
       return a.Wait() + b.Wait() + c.Wait() + d.Wait() \
         + e.Wait() + f.Wait() + g.Wait() + h.Wait(); \
     }"
}

#[test]
fn governed_task_swarm_is_page_aware_and_queued_tasks_precharge_nothing() {
    let module = compile(task_swarm_source()).expect("source compiles").mir;
    let page = ExecutionContext::AARM_MIN_PAGE_CAPACITY_BYTES;
    for worker_count in [1, 2, 4, 16] {
        let governor = Arc::new(MemoryGovernor::new(2 * page));
        let (value, _, domain) =
            execute_with_aarm_task_governor(&module, "Main", worker_count, Arc::clone(&governor))
                .expect("queued tasks reuse fixed task entitlements");
        assert_eq!(value, ExecutionValue::Int(8));
        let domain = domain.expect("Task.Run freezes a memory domain");
        assert_eq!(domain.task_memory_concurrency_limit, worker_count.min(2));
        assert!(domain.task_context_ceiling_bytes >= page as u64);
        assert_eq!(
            u128::from(domain.main_future_growth_bytes)
                + u128::from(domain.task_context_ceiling_bytes)
                    * u128::try_from(domain.task_memory_concurrency_limit)
                        .expect("concurrency fits u128"),
            u128::try_from(2 * page).expect("headroom fits u128")
        );
        assert_eq!(domain.task_submissions, 8);
        assert_eq!(domain.task_contexts_started, 8);
        assert_eq!(domain.task_contexts_completed, 8);
        assert_eq!(domain.task_context_memory_failures, 0);

        let telemetry = governor.telemetry();
        assert_eq!(telemetry.current_capacity_bytes, 0);
        assert!(telemetry.peak_capacity_bytes <= telemetry.hard_limit_bytes);
        assert_eq!(telemetry.grant_events, 8);
        assert_eq!(telemetry.grant_events, telemetry.release_events);
        assert_eq!(
            telemetry.granted_bytes_cumulative,
            telemetry.released_bytes_cumulative
        );
    }
}

#[test]
fn main_and_task_growth_have_scheduler_independent_frozen_outcomes() {
    let module = compile(
        "public int Work() { int[] scratch = new int[4000]; scratch[0] = 3; return scratch.Length; } \
         public int Main() { \
           int[] retained = new int[1000]; retained[0] = 1; \
           Task<int> a = Task.Run(Work); Task<int> b = Task.Run(Work); \
           int[] growth = new int[4000]; growth[0] = 2; \
           return retained.Length + growth.Length + a.Wait() + b.Wait(); \
         }",
    )
    .expect("source compiles")
    .mir;

    for worker_count in [1, 2, 4, 16] {
        for _ in 0..10 {
            let perturbation = std::thread::spawn(|| {
                for _ in 0..64 {
                    std::thread::yield_now();
                }
            });
            let governor = Arc::new(MemoryGovernor::new(512 * 1024));
            let (value, _, domain) = execute_with_aarm_task_governor(
                &module,
                "Main",
                worker_count,
                Arc::clone(&governor),
            )
            .expect("frozen Main and task entitlements admit the fixed workload");
            perturbation.join().expect("perturbation completes");
            assert_eq!(value, ExecutionValue::Int(13_000));
            let domain = domain.expect("Task.Run freezes a memory domain");
            assert!(domain.main_retained_capacity_bytes > 0);
            assert!(domain.main_future_growth_bytes > 0);
            assert_eq!(domain.task_submissions, 2);
            assert_eq!(domain.task_contexts_completed, 2);
            let telemetry = governor.telemetry();
            assert_eq!(telemetry.current_capacity_bytes, 0);
            assert!(telemetry.peak_capacity_bytes <= telemetry.hard_limit_bytes);
            assert_eq!(telemetry.grant_events, telemetry.release_events);
        }
    }
}

#[test]
fn task_failures_are_cached_per_handle_and_independent_of_wait_order() {
    let forward = compile(
        "public int Small() { int[] scratch = new int[1]; return scratch.Length; } \
         public int Large() { int[] scratch = new int[20000]; return scratch.Length; } \
         public int Main() { Task<int> a = Task.Run(Small); Task<int> b = Task.Run(Large); return a.Wait() + b.Wait(); }",
    )
    .expect("forward source compiles")
    .mir;
    let reverse = compile(
        "public int Small() { int[] scratch = new int[1]; return scratch.Length; } \
         public int Large() { int[] scratch = new int[20000]; return scratch.Length; } \
         public int Main() { Task<int> a = Task.Run(Small); Task<int> b = Task.Run(Large); return b.Wait() + a.Wait(); }",
    )
    .expect("reverse source compiles")
    .mir;

    for worker_count in [1, 2, 4, 16] {
        let mut expected = None;
        for module in [&forward, &reverse] {
            for _ in 0..10 {
                let governor = Arc::new(MemoryGovernor::new(64 * 1024));
                let error = execute_with_aarm_task_governor(
                    module,
                    "Main",
                    worker_count,
                    Arc::clone(&governor),
                )
                .expect_err("the large task exceeds its fixed local entitlement");
                assert!(
                    error
                        .message()
                        .contains("deterministic Task.Run memory entitlement"),
                    "unexpected error: {error}"
                );
                assert_eq!(
                    expected.get_or_insert_with(|| error.message().to_owned()),
                    error.message()
                );
                assert_eq!(governor.telemetry().current_capacity_bytes, 0);
            }
        }
    }
}

#[test]
fn oversized_first_task_page_is_checked_as_actual_byte_capacity() {
    let module = compile(
        "public int Large() { int[] scratch = new int[20000]; return scratch.Length; } \
         public int Main() { return Task.Run(Large).Wait(); }",
    )
    .expect("source compiles")
    .mir;

    let denied = Arc::new(MemoryGovernor::new(
        ExecutionContext::AARM_DEFAULT_PAGE_CAPACITY_BYTES,
    ));
    let error = execute_with_aarm_task_governor(&module, "Main", 1, Arc::clone(&denied))
        .expect_err("minimum-page reasoning must not round down an oversized page");
    assert!(
        error
            .message()
            .contains("deterministic Task.Run memory entitlement")
    );
    assert_eq!(denied.telemetry().grant_events, 0);

    let admitted = Arc::new(MemoryGovernor::new(192 * 1024));
    let (value, _, domain) =
        execute_with_aarm_task_governor(&module, "Main", 1, Arc::clone(&admitted))
            .expect("actual oversized capacity fits the larger byte entitlement");
    assert_eq!(value, ExecutionValue::Int(20_000));
    assert!(
        domain.expect("domain exists").task_context_ceiling_bytes
            > ExecutionContext::AARM_DEFAULT_PAGE_CAPACITY_BYTES as u64
    );
    let telemetry = admitted.telemetry();
    assert_eq!(telemetry.grant_events, 1);
    assert_eq!(telemetry.grant_events, telemetry.release_events);
}

#[test]
fn experimental_task_domain_rejects_unintegrated_async_and_parallel() {
    let async_module = compile(
        "public int One() { return 1; } \
         public async Task<int> Later() { int value = await Task.Run(One); return value; } \
         public int Main() { return Later().Wait(); }",
    )
    .expect("async source compiles")
    .mir;
    let parallel_module = compile(
        "public void Body(int index) { } public int Main() { Parallel.For(0, 1, Body); return 1; }",
    )
    .expect("Parallel source compiles")
    .mir;

    let async_error = execute_with_aarm_task_governor(
        &async_module,
        "Main",
        2,
        Arc::new(MemoryGovernor::new(64 * 1024)),
    )
    .expect_err("governed async is deferred");
    assert!(async_error.message().contains("does not support async"));

    let parallel_error = execute_with_aarm_task_governor(
        &parallel_module,
        "Main",
        2,
        Arc::new(MemoryGovernor::new(64 * 1024)),
    )
    .expect_err("mixed governed Task/Parallel is deferred");
    assert!(parallel_error.message().contains("mixed Parallel"));
}

#[test]
fn zero_usable_task_page_is_a_controlled_task_entitlement_failure() {
    let module = compile(
        "public int Small() { int[] scratch = new int[1]; return scratch.Length; } \
         public int Main() { return Task.Run(Small).Wait(); }",
    )
    .expect("source compiles")
    .mir;
    let governor = Arc::new(MemoryGovernor::new(0));
    let error = execute_with_aarm_task_governor(&module, "Main", 16, Arc::clone(&governor))
        .expect_err("zero headroom cannot admit a minimum task page");
    assert!(
        error
            .message()
            .contains("deterministic Task.Run memory entitlement"),
        "unexpected error: {error}"
    );
    let telemetry = governor.telemetry();
    assert_eq!(telemetry.current_capacity_bytes, 0);
    assert_eq!(telemetry.grant_events, 0);
    assert_eq!(telemetry.denial_events, 0);
}
