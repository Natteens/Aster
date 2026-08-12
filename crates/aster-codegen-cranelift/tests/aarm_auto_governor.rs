#![cfg(feature = "aarm-telemetry")]

use aster_codegen_cranelift::{
    ExecutionValue, execute_with_aarm_auto_async_governor,
    execute_with_aarm_auto_parallel_governor, execute_with_aarm_auto_task_governor,
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
