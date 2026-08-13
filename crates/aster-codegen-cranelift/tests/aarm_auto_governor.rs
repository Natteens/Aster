#![cfg(feature = "aarm-telemetry")]

use aster_codegen_cranelift::{
    AarmBudgetSource, ExecutionValue, execute_with_aarm_auto_async_governor,
    execute_with_aarm_auto_parallel_governor, execute_with_aarm_auto_task_governor,
    execute_with_aarm_exact_async_governor, execute_with_aarm_exact_parallel_governor,
    execute_with_aarm_exact_task_governor,
};
use aster_compiler::compile;

#[test]
fn auto_governor_reuses_existing_plain_parallel_task_and_async_domains() {
    let plain = compile("public int Main() { int[] values = new int[1]; return values.Length; }")
        .expect("plain source compiles")
        .mir;
    let (value, telemetry, _, _, auto) =
        execute_with_aarm_auto_parallel_governor(&plain, "Main", 1).expect("Auto plain succeeds");
    assert_eq!(value, ExecutionValue::Int(1));
    assert_eq!(
        telemetry
            .governor
            .expect("main is governed")
            .hard_limit_bytes,
        auto.resolved_hard_limit_bytes
    );

    let parallel = compile(
        "public void Body(int index) { int[] values = new int[1]; } \
         public int Main() { Parallel.For(0, 4, Body); return 4; }",
    )
    .expect("Parallel source compiles")
    .mir;
    let (value, _, plans, _, auto) = execute_with_aarm_auto_parallel_governor(&parallel, "Main", 4)
        .expect("Auto Parallel succeeds");
    assert_eq!(value, ExecutionValue::Int(4));
    assert_eq!(plans.len(), 1);
    assert!(auto.resolved_hard_limit_bytes > 0);

    let task = compile(
        "public int Work() { int[] values = new int[1]; return values.Length; } \
         public int Main() { return Task.Run(Work).Wait(); }",
    )
    .expect("Task source compiles")
    .mir;
    let (value, _, domain, auto) =
        execute_with_aarm_auto_task_governor(&task, "Main", 4).expect("Auto Task.Run succeeds");
    assert_eq!(value, ExecutionValue::Int(1));
    assert!(domain.is_some());
    assert!(auto.resolved_hard_limit_bytes > 0);

    let asynchronous = compile(
        "public int Work() { int[] values = new int[1]; return values.Length; } \
         public async Task<int> Calculate() { return await Task.Run(Work); } \
         public int Main() { return Calculate().Wait(); }",
    )
    .expect("async source compiles")
    .mir;
    let (value, _, domain, auto) = execute_with_aarm_auto_async_governor(&asynchronous, "Main", 4)
        .expect("Auto async succeeds");
    assert_eq!(value, ExecutionValue::Int(1));
    assert!(domain.is_some());
    assert!(auto.resolved_hard_limit_bytes > 0);
}

#[test]
fn explicit_governor_reuses_existing_plain_parallel_task_and_async_domains() {
    const BUDGET: u64 = 64 * 1024 * 1024;
    let plain = compile("public int Main() { int[] values = new int[1]; return values.Length; }")
        .expect("plain source compiles")
        .mir;
    let (value, telemetry, _, _, budget) =
        execute_with_aarm_exact_parallel_governor(&plain, "Main", 1, BUDGET)
            .expect("explicit plain succeeds");
    assert_eq!(value, ExecutionValue::Int(1));
    assert_eq!(budget.source, AarmBudgetSource::Explicit);
    assert_eq!(budget.requested_explicit_bytes, Some(BUDGET));
    assert_eq!(budget.resolved_hard_limit_bytes, BUDGET);
    assert!(!budget.address_width_clamped);
    assert_eq!(
        telemetry
            .governor
            .expect("main is governed")
            .hard_limit_bytes,
        BUDGET
    );

    let parallel = compile(
        "public void Body(int index) { int[] values = new int[1]; } \
         public int Main() { Parallel.For(0, 4, Body); return 4; }",
    )
    .expect("Parallel source compiles")
    .mir;
    let (value, _, plans, _, budget) =
        execute_with_aarm_exact_parallel_governor(&parallel, "Main", 4, BUDGET)
            .expect("explicit Parallel succeeds");
    assert_eq!(value, ExecutionValue::Int(4));
    assert_eq!(plans.len(), 1);
    assert_eq!(budget.resolved_hard_limit_bytes, BUDGET);

    let task = compile(
        "public int Work() { int[] values = new int[1]; return values.Length; } \
         public int Main() { return Task.Run(Work).Wait(); }",
    )
    .expect("Task source compiles")
    .mir;
    let (value, _, domain, budget) =
        execute_with_aarm_exact_task_governor(&task, "Main", 4, BUDGET)
            .expect("explicit Task.Run succeeds");
    assert_eq!(value, ExecutionValue::Int(1));
    assert!(domain.is_some());
    assert_eq!(budget.resolved_hard_limit_bytes, BUDGET);

    let asynchronous = compile(
        "public int Work() { int[] values = new int[1]; return values.Length; } \
         public async Task<int> Calculate() { return await Task.Run(Work); } \
         public int Main() { return Calculate().Wait(); }",
    )
    .expect("async source compiles")
    .mir;
    let (value, _, domain, budget) =
        execute_with_aarm_exact_async_governor(&asynchronous, "Main", 4, BUDGET)
            .expect("explicit async succeeds");
    assert_eq!(value, ExecutionValue::Int(1));
    assert!(domain.is_some());
    assert_eq!(budget.resolved_hard_limit_bytes, BUDGET);
}

#[test]
fn exact_tiny_budget_allows_allocation_free_work_and_denies_the_first_page() {
    let allocation_free = compile("public int Main() { return 1; }")
        .expect("allocation-free source compiles")
        .mir;
    let (value, telemetry, _, _, budget) =
        execute_with_aarm_exact_parallel_governor(&allocation_free, "Main", 1, 1)
            .expect("allocation-free execution succeeds");
    assert_eq!(value, ExecutionValue::Int(1));
    assert_eq!(budget.resolved_hard_limit_bytes, 1);
    assert_eq!(
        telemetry
            .governor
            .expect("allocation-free main is governed")
            .current_capacity_bytes,
        0
    );

    let allocation =
        compile("public int Main() { int[] values = new int[1]; return values.Length; }")
            .expect("allocating source compiles")
            .mir;
    let error = execute_with_aarm_exact_parallel_governor(&allocation, "Main", 1, 1)
        .expect_err("the first 4 KiB page exceeds a 1-byte explicit limit");
    assert!(
        error
            .to_string()
            .contains("shared execution memory budget of 1 bytes")
    );

    let below_page = execute_with_aarm_exact_parallel_governor(&allocation, "Main", 1, 4095)
        .expect_err("a 4 KiB page does not fit below its exact capacity");
    assert!(
        below_page
            .to_string()
            .contains("shared execution memory budget of 4095 bytes")
    );
    let (value, telemetry, _, _, budget) =
        execute_with_aarm_exact_parallel_governor(&allocation, "Main", 1, 4096)
            .expect("one exact minimum page is admitted");
    assert_eq!(value, ExecutionValue::Int(1));
    assert_eq!(budget.resolved_hard_limit_bytes, 4096);
    assert_eq!(
        telemetry
            .governor
            .expect("minimum-page main is governed")
            .hard_limit_bytes,
        4096
    );
}
