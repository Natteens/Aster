#![cfg(feature = "aarm-telemetry")]

use std::sync::Arc;

use aster_codegen_cranelift::{ExecutionValue, execute_with_aarm_async_governor};
use aster_compiler::compile;
use aster_runtime::{ExecutionContext, MemoryGovernor};

fn run(
    source: &str,
    workers: usize,
    limit: usize,
) -> Result<
    (
        ExecutionValue,
        aster_runtime::AarmMemoryTelemetry,
        aster_codegen_cranelift::AarmAsyncMemoryDomainTelemetry,
        aster_runtime::MemoryGovernorTelemetry,
    ),
    String,
> {
    let module = compile(source).map_err(|error| format!("{error:?}"))?.mir;
    let governor = Arc::new(MemoryGovernor::new(limit));
    let result = execute_with_aarm_async_governor(&module, "Main", workers, Arc::clone(&governor));
    let governor_telemetry = governor.telemetry();
    result
        .map(|(value, telemetry, domain)| {
            (
                value,
                telemetry,
                domain.expect("an async Wait freezes the domain"),
                governor_telemetry,
            )
        })
        .map_err(|error| error.to_string())
}

#[test]
fn governed_async_covers_before_inner_after_and_main_growth() {
    let source = "public int Scratch(int count) { int[] values = new int[count]; values[0] = 1; return values.Length; } \
         public int Inner() { return Scratch(4000); } \
         public async Task<int> Calculate() { int before = Scratch(1000); int value = await Task.Run(Inner); int after = Scratch(1000); return before + value + after; } \
         public int Main() { Task<int> task = Calculate(); int[] retained = new int[1000]; retained[0] = 1; int value = task.Wait(); int[] after = new int[1000]; after[0] = 2; return value + retained.Length + after.Length; }";

    for workers in [1, 2, 4, 16] {
        for _ in 0..8 {
            let (value, _, domain, governor) =
                run(source, workers, 512 * 1024).expect("ample governed async succeeds");
            assert_eq!(value, ExecutionValue::Int(8_000));
            assert!(domain.main_retained_capacity_bytes > 0);
            assert!(domain.main_future_growth_bytes > 0);
            assert_eq!(domain.move_next_contexts_started, 2);
            assert_eq!(domain.move_next_contexts_completed, 2);
            assert_eq!(domain.inner_contexts_started, 1);
            assert_eq!(domain.inner_contexts_completed, 1);
            assert_eq!(domain.move_next_memory_failures, 0);
            assert_eq!(domain.inner_memory_failures, 0);
            assert!(governor.peak_capacity_bytes <= governor.hard_limit_bytes);
            assert_eq!(governor.current_capacity_bytes, 0);
            assert_eq!(governor.grant_events, governor.release_events);
            assert_eq!(
                governor.granted_bytes_cumulative,
                governor.released_bytes_cumulative
            );
        }
    }
}

#[test]
fn async_domain_plan_is_page_aware_for_zero_through_three_pages() {
    let source = "public int Inner() { return 7; } \
         public async Task<int> Calculate() { int value = await Task.Run(Inner); return value; } \
         public int Main() { return Calculate().Wait(); }";
    let page = ExecutionContext::AARM_MIN_PAGE_CAPACITY_BYTES;
    let cases = [
        (0, [0, 0, 0]),
        (page, [page, 0, 0]),
        (2 * page, [page, page, 0]),
        (3 * page, [page, page, page]),
        (3 * page + 2, [page + 1, page + 1, page]),
    ];
    for (headroom, expected) in cases {
        let (value, _, domain, governor) =
            run(source, 4, headroom).expect("allocation-free async executes at any entitlement");
        assert_eq!(value, ExecutionValue::Int(7));
        assert_eq!(
            [
                usize::try_from(domain.move_next_context_ceiling_bytes).unwrap(),
                usize::try_from(domain.awaited_inner_context_ceiling_bytes).unwrap(),
                usize::try_from(domain.main_future_growth_bytes).unwrap(),
            ],
            expected
        );
        assert_eq!(governor.current_capacity_bytes, 0);
        assert_eq!(governor.grant_events, 0);
    }
}

#[test]
fn move_next_and_inner_entitlement_failures_are_controlled_and_repeatable() {
    let page = ExecutionContext::AARM_MIN_PAGE_CAPACITY_BYTES;
    let move_failure = "public int Scratch() { int[] values = new int[1]; return values.Length; } \
         public int Inner() { return 1; } \
         public async Task<int> Calculate() { int before = Scratch(); int value = await Task.Run(Inner); return before + value; } \
         public int Main() { return Calculate().Wait(); }";
    let inner_failure = "public int Inner() { int[] values = new int[1]; return values.Length; } \
         public async Task<int> Calculate() { int value = await Task.Run(Inner); return value; } \
         public int Main() { return Calculate().Wait(); }";

    for _ in 0..16 {
        let move_error = run(move_failure, 4, 0).expect_err("zero MoveNext quota rejects a page");
        assert!(
            move_error.contains("deterministic async MoveNext memory entitlement"),
            "unexpected error: {move_error}"
        );
        let inner_error = run(inner_failure, 4, page).expect_err("zero inner quota rejects a page");
        assert!(
            inner_error.contains("deterministic async awaited-inner memory entitlement"),
            "unexpected error: {inner_error}"
        );
    }
}

#[test]
fn oversized_inner_page_uses_its_actual_byte_capacity() {
    let source = "public int Inner() { int[] values = new int[20000]; return values.Length; } \
         public async Task<int> Calculate() { int value = await Task.Run(Inner); return value; } \
         public int Main() { return Calculate().Wait(); }";
    let regular = ExecutionContext::AARM_DEFAULT_PAGE_CAPACITY_BYTES;
    let denied = run(source, 4, 3 * regular)
        .expect_err("a request-sized oversized page exceeds the fixed inner byte ceiling");
    assert!(denied.contains("deterministic async awaited-inner memory entitlement"));

    let (value, _, domain, governor) =
        run(source, 4, 9 * regular).expect("the actual oversized capacity fits a larger ceiling");
    assert_eq!(value, ExecutionValue::Int(20_000));
    assert!(domain.awaited_inner_context_ceiling_bytes > regular as u64);
    assert_eq!(governor.grant_events, 1);
    assert_eq!(governor.grant_events, governor.release_events);
}

#[test]
fn resumed_move_next_and_main_post_wait_keep_their_frozen_limits() {
    let page = ExecutionContext::AARM_MIN_PAGE_CAPACITY_BYTES;
    let resumed_failure = "public int Inner() { return 1; } \
         public async Task<int> Calculate() { int value = await Task.Run(Inner); int[] values = new int[2000]; return value + values.Length; } \
         public int Main() { return Calculate().Wait(); }";
    let main_failure = "public int Inner() { return 1; } \
         public async Task<int> Calculate() { int value = await Task.Run(Inner); return value; } \
         public int Main() { int value = Calculate().Wait(); int[] values = new int[2000]; return value + values.Length; }";

    let resumed =
        run(resumed_failure, 4, 3 * page).expect_err("resumed MoveNext keeps its one-page ceiling");
    assert!(resumed.contains("deterministic async MoveNext memory entitlement"));
    let main = run(main_failure, 4, 3 * page)
        .expect_err("post-Wait Main keeps its one-page future entitlement");
    assert!(main.contains("deterministic async Main memory entitlement"));
}

#[test]
fn repeated_wait_replays_without_new_contexts_or_grants() {
    let source = "public int Inner() { int[] values = new int[1]; return values.Length; } \
         public async Task<int> Calculate() { int value = await Task.Run(Inner); return value; } \
         public int Main() { Task<int> task = Calculate(); return task.Wait() + task.Wait(); }";
    let (value, _, domain, governor) = run(source, 4, 256 * 1024).expect("repeated Wait succeeds");
    assert_eq!(value, ExecutionValue::Int(2));
    assert_eq!(domain.async_handles_created, 1);
    assert_eq!(domain.move_next_contexts_started, 2);
    assert_eq!(domain.inner_contexts_started, 1);
    assert_eq!(governor.grant_events, 1);
    assert_eq!(governor.grant_events, governor.release_events);
}

#[test]
fn multiple_async_handles_reuse_one_frozen_domain_in_wait_order() {
    let source = "public int A() { int[] values = new int[1]; return 10 + values.Length; } \
         public int B() { int[] values = new int[1]; return 20 + values.Length; } \
         public async Task<int> LaterA() { int value = await Task.Run(A); return value; } \
         public async Task<int> LaterB() { int value = await Task.Run(B); return value; } \
         public int Main() { Task<int> a = LaterA(); Task<int> b = LaterB(); return b.Wait() + a.Wait(); }";
    let (value, _, domain, governor) =
        run(source, 16, 256 * 1024).expect("serial pumps share one domain");
    assert_eq!(value, ExecutionValue::Int(32));
    assert_eq!(domain.async_handles_created, 2);
    assert_eq!(domain.move_next_contexts_started, 4);
    assert_eq!(domain.inner_contexts_started, 2);
    assert_eq!(governor.current_capacity_bytes, 0);
    assert_eq!(governor.grant_events, governor.release_events);
}

#[test]
fn awaited_inner_scalar_results_outlive_worker_contexts() {
    let source = "public float FloatValue() { return 1.5f; } public bool BoolValue() { return true; } public char CharValue() { return 'Z'; } \
         public async Task<float> LaterFloat() { float value = await Task.Run(FloatValue); return value; } \
         public async Task<bool> LaterBool() { bool value = await Task.Run(BoolValue); return value; } \
         public async Task<char> LaterChar() { char value = await Task.Run(CharValue); return value; } \
         public int Main() { float f = LaterFloat().Wait(); bool b = LaterBool().Wait(); char c = LaterChar().Wait(); return f == 1.5f && b && c == 'Z' ? 1 : 0; }";
    let (value, _, domain, governor) =
        run(source, 4, 256 * 1024).expect("all supported scalar results survive teardown");
    assert_eq!(value, ExecutionValue::Int(1));
    assert_eq!(domain.inner_contexts_started, 3);
    assert_eq!(domain.inner_contexts_completed, 3);
    assert_eq!(governor.current_capacity_bytes, 0);
}

#[test]
fn async_capture_occurs_at_first_wait_after_main_retention() {
    let source = "public int Inner() { return 1; } \
         public async Task<int> Calculate() { int value = await Task.Run(Inner); return value; } \
         public int Main() { Task<int> task = Calculate(); int[] retained = new int[1000]; retained[0] = 1; return task.Wait() + retained.Length; }";
    let (value, _, domain, _) = run(source, 4, 256 * 1024).expect("capture succeeds");
    assert_eq!(value, ExecutionValue::Int(1001));
    assert!(domain.main_retained_capacity_bytes > 0);
    assert_eq!(
        domain.initial_governor_capacity_bytes,
        domain.main_retained_capacity_bytes
    );
}

#[test]
fn experimental_async_domain_rejects_plain_tasks_and_parallel() {
    let mixed_task = compile(
        "public int Inner() { return 1; } public int Plain() { return 2; } \
         public async Task<int> Later() { int value = await Task.Run(Inner); return value; } \
         public int Main() { Task<int> plain = Task.Run(Plain); return Later().Wait() + plain.Wait(); }",
    )
    .expect("mixed source compiles")
    .mir;
    let mixed_parallel = compile(
        "public int Inner() { return 1; } public void Body(int index) { } \
         public async Task<int> Later() { int value = await Task.Run(Inner); return value; } \
         public int Main() { Parallel.For(0, 1, Body); return Later().Wait(); }",
    )
    .expect("mixed source compiles")
    .mir;
    let governor = Arc::new(MemoryGovernor::new(256 * 1024));
    let task_error =
        execute_with_aarm_async_governor(&mixed_task, "Main", 4, Arc::clone(&governor))
            .expect_err("independent Task.Run is rejected");
    assert!(task_error.message().contains("independent plain Task.Run"));
    let parallel_error = execute_with_aarm_async_governor(&mixed_parallel, "Main", 4, governor)
        .expect_err("mixed Parallel is rejected");
    assert!(parallel_error.message().contains("mixed Parallel"));
}
