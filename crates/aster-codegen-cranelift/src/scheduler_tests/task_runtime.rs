//! JIT-backed tests moved from `aster-exec::task_runtime` (ARCH-005).

use std::sync::Arc;

use aster_exec::task_runtime::*;
use aster_exec::worker_pool::*;
use aster_exec::{BackendError, ExecutionValue, mir};

use crate::execution::PreparedProgram;
use crate::provider::provider;
use std::sync::mpsc;

use std::thread;

use std::time::Duration;

/// A bare `*mut TaskRuntime` is not `Send`; this wraps one so it can
/// cross to a helper thread in the pump timeout tests below. Only sound
/// because those tests block (with a bounded timeout) until that thread
/// finishes using it, and the runtime itself outlives the whole call.
struct SendPtr(*mut TaskRuntime);
#[allow(unsafe_code)]
unsafe impl Send for SendPtr {}

fn compile(source: &str) -> mir::Module {
    aster_compiler::compile(source)
        .expect("source compiles")
        .mir
}

/// Run the wrapper of the canonical `Calculate` async example through
/// `worker_count` workers and pump it to resolution on a helper thread,
/// bounding the wait so a stuck pump fails the test instead of hanging it.
fn pump_canonical_example(worker_count: usize) -> Result<ExecutionValue, BackendError> {
    let module = Arc::new(compile(
        "public int Compute() { return 42; } \
             public async Task<int> Calculate() { \
                 int offset = 1; \
                 int value = await Task.Run(Compute); \
                 return value + offset; }",
    ));
    let wrapper = module
        .functions
        .iter()
        .find(|function| function.name == "Calculate")
        .expect("Calculate is declared")
        .symbol;
    let mut runtime = TaskRuntime::new(&provider(), &module, worker_count)
        .expect("runtime starts with the requested worker count");
    let driver = PreparedProgram::prepare(&module).expect("driver prepares");
    let pointer = std::ptr::from_mut(&mut runtime).cast::<()>();
    let (value, _) = driver
        .invoke(wrapper, false, Some(pointer), None, None)
        .expect("wrapper registers the async task");
    let ExecutionValue::Long(bits) = value else {
        panic!("wrapper must return a Task<T> handle");
    };
    let handle = TaskHandleId::from_bits(bits);
    // A bare `*mut TaskRuntime` is not `Send`; wrap it (see `SendPtr`
    // below) so it can cross to the helper thread. Sound here because the
    // receiving side blocks (with a bounded timeout) until that thread
    // finishes using it, and `runtime` itself outlives the whole call.
    let runtime_pointer = SendPtr(std::ptr::from_mut(&mut runtime));
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let runtime_pointer = runtime_pointer;
        // SAFETY: `runtime_pointer` stays live for the whole call: this
        // thread is joined (via the timeout below) before `runtime`
        // could otherwise drop, and nothing else touches it meanwhile.
        #[allow(unsafe_code)]
        let outcome = unsafe { TaskRuntime::pump(runtime_pointer.0, handle) };
        let _ = sender.send(outcome);
    });
    let outcome = receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("the pump must resolve without deadlocking")
        .expect("the pump call itself must not fail");
    match outcome {
        TaskOutcome::Completed(value, _) => Ok(value),
        TaskOutcome::Failed(error) => Err(error),
    }
}

// --- Parallel.For / Parallel.ForEach: explicit worker-count coverage ---
//
// `execute`/`execute_with_stats` (the public surface exercised by the
// integration test suites) always pick `available_parallelism()`, so a
// CI runner's own core count silently decides whether "1 worker" or "N
// workers" is what actually gets tested. Driving `TaskRuntime` directly
// here pins the worker count explicitly, closing that gap for `For` and
// `ForEach` the same way Lote 6C already did for `Reduce` below.

fn symbol(module: &mir::Module, name: &str) -> mir::SymbolId {
    module
        .functions
        .iter()
        .find(|function| function.name == name)
        .unwrap_or_else(|| panic!("{name} is declared"))
        .symbol
}

// --- Parallel.Reduce ---------------------------------------------------

const REDUCE_SOURCE: &str = "public int AddValue(int accumulator, int value) { return accumulator + value; } \
         public int AddPartial(int left, int right) { return left + right; }";

#[test]
#[cfg(feature = "aarm-telemetry")]
fn governed_plain_task_outcomes_are_scheduler_and_wait_order_independent() {
    const HARD_LIMIT: usize = 64 * 1024;
    let module = Arc::new(compile(
        "public int Small() { int[] scratch = new int[1]; return 1; } \
             public int Large() { int[] scratch = new int[20000]; return 2; }",
    ));
    let small = symbol(&module, "Small");
    let large = symbol(&module, "Large");

    for worker_count in [1, 2, 4, 16] {
        let governor = Arc::new(aster_runtime::MemoryGovernor::new(HARD_LIMIT));
        let mut main = aster_runtime::ExecutionContext::with_memory_governor(Arc::clone(&governor));
        let mut runtime = TaskRuntime::with_task_memory_governor(
            &provider(),
            &module,
            worker_count,
            Arc::clone(&governor),
        )
        .expect("governed task runtime starts");
        let mut expected_failure = None;

        for repetition in 0..10 {
            let a = runtime
                .run_from_context(small, &mut main)
                .expect("small A submits");
            let b = runtime
                .run_from_context(large, &mut main)
                .expect("large B submits");
            let c = runtime
                .run_from_context(small, &mut main)
                .expect("small C submits");
            let d = runtime
                .run_from_context(large, &mut main)
                .expect("large D submits");
            let perturbation = thread::spawn(|| {
                for _ in 0..64 {
                    thread::yield_now();
                }
            });
            let order = if repetition % 2 == 0 {
                [d, a, c, b]
            } else {
                [a, b, c, d]
            };
            let outcomes = order.map(|id| runtime.wait(id).expect("task handle is known"));
            perturbation.join().expect("perturbation completes");

            for (position, outcome) in outcomes.into_iter().enumerate() {
                let should_fail = if repetition % 2 == 0 {
                    matches!(position, 0 | 3)
                } else {
                    matches!(position, 1 | 3)
                };
                match (should_fail, outcome) {
                    (false, TaskOutcome::Completed(ExecutionValue::Int(1), _)) => {}
                    (true, TaskOutcome::Failed(error)) => {
                        assert!(
                            error
                                .message()
                                .contains("deterministic Task.Run memory entitlement"),
                            "unexpected task error: {error}"
                        );
                        assert_eq!(
                            expected_failure.get_or_insert_with(|| error.message().to_owned()),
                            error.message()
                        );
                    }
                    (_, outcome) => panic!("unexpected task outcome: {outcome:?}"),
                }
            }
            assert_eq!(governor.telemetry().current_capacity_bytes, 0);
        }

        runtime.quiesce_workers();
        let domain = runtime
            .task_domain_telemetry()
            .expect("first submission freezes the domain");
        assert_eq!(domain.task_submissions, 40);
        assert_eq!(domain.task_contexts_started, 40);
        assert_eq!(domain.task_contexts_completed, 40);
        assert_eq!(domain.task_context_memory_failures, 20);
        let telemetry = governor.telemetry();
        assert_eq!(telemetry.current_capacity_bytes, 0);
        assert!(telemetry.peak_capacity_bytes <= telemetry.hard_limit_bytes);
        assert_eq!(telemetry.grant_events, telemetry.release_events);
        assert_eq!(
            telemetry.granted_bytes_cumulative,
            telemetry.released_bytes_cumulative
        );
    }
}

#[test]
#[cfg(feature = "aarm-telemetry")]
fn queued_plain_tasks_do_not_precharge_their_entitlements() {
    let module = Arc::new(compile("public int One() { return 1; }"));
    let one = symbol(&module, "One");
    let governor = Arc::new(aster_runtime::MemoryGovernor::new(64 * 1024));
    let mut main = aster_runtime::ExecutionContext::with_memory_governor(Arc::clone(&governor));
    let mut runtime =
        TaskRuntime::with_task_memory_governor(&provider(), &module, 2, Arc::clone(&governor))
            .expect("governed task runtime starts");

    let handles = (0..16)
        .map(|_| {
            runtime
                .run_from_context(one, &mut main)
                .expect("queued task submits")
        })
        .collect::<Vec<_>>();
    assert_eq!(governor.telemetry().current_capacity_bytes, 0);
    for handle in handles {
        assert!(matches!(
            runtime.wait(handle).expect("queued task completes"),
            TaskOutcome::Completed(ExecutionValue::Int(1), _)
        ));
    }
    runtime.quiesce_workers();
    let domain = runtime.task_domain_telemetry().expect("domain exists");
    assert_eq!(domain.task_memory_concurrency_limit, 2);
    assert_eq!(domain.task_submissions, 16);
    assert_eq!(domain.task_contexts_completed, 16);
    assert_eq!(governor.telemetry().grant_events, 0);
}

#[test]
#[cfg(feature = "aarm-telemetry")]
fn main_and_tasks_do_not_race_for_unpartitioned_future_headroom() {
    let module = Arc::new(compile(
        "public int Small() { int[] scratch = new int[1]; return 1; }",
    ));
    let small = symbol(&module, "Small");

    for worker_count in [1, 2, 4, 16] {
        for _ in 0..10 {
            let governor = Arc::new(aster_runtime::MemoryGovernor::new(64 * 1024));
            let mut main =
                aster_runtime::ExecutionContext::with_memory_governor(Arc::clone(&governor));
            assert!(!aster_runtime::context::aster_rt_array_new(&raw mut main, 1, 1).is_null());
            let mut runtime = TaskRuntime::with_task_memory_governor(
                &provider(),
                &module,
                worker_count,
                Arc::clone(&governor),
            )
            .expect("governed task runtime starts");

            let first = runtime
                .run_from_context(small, &mut main)
                .expect("first task submits");
            let second = runtime
                .run_from_context(small, &mut main)
                .expect("second task submits");
            assert!(aster_runtime::context::aster_rt_array_new(&raw mut main, 10_000, 4).is_null());
            assert!(main.take_error().is_some_and(|error| {
                error.contains("deterministic Task.Run main-context memory entitlement")
            }));

            for handle in [second, first] {
                assert!(matches!(
                    runtime.wait(handle).expect("task outcome is cached"),
                    TaskOutcome::Completed(ExecutionValue::Int(1), _)
                ));
            }
            runtime.quiesce_workers();
            assert_eq!(
                governor.telemetry().current_capacity_bytes,
                u64::try_from(aster_runtime::ExecutionContext::AARM_MIN_PAGE_CAPACITY_BYTES)
                    .expect("minimum page fits u64")
            );
            drop(runtime);
            drop(main);
            assert_eq!(governor.telemetry().current_capacity_bytes, 0);
        }
    }
}

#[test]
fn waiting_twice_on_the_same_plain_id_returns_the_same_outcome() {
    let module = compile("public int Compute() { return 42; }");
    let symbol = module
        .functions
        .iter()
        .find(|function| function.name == "Compute")
        .expect("Compute is declared")
        .symbol;
    let mut runtime = TaskRuntime::new(&provider(), &Arc::new(module), 1).expect("runtime starts");
    let id = runtime.run(symbol).expect("task is accepted");
    let first = runtime.wait(id).expect("first wait succeeds");
    let second = runtime
        .wait(id)
        .expect("second wait replays the cached result");
    assert_eq!(first, second);
}

#[test]
fn a_plain_task_never_waited_is_dropped_cleanly() {
    let module = compile("public int Compute() { return 1; }");
    let symbol = module
        .functions
        .iter()
        .find(|function| function.name == "Compute")
        .expect("Compute is declared")
        .symbol;
    let mut runtime = TaskRuntime::new(&provider(), &Arc::new(module), 2).expect("runtime starts");
    for _ in 0..5 {
        runtime.run(symbol).expect("task is accepted");
    }
    drop(runtime);
}

#[test]
fn an_async_task_created_and_never_awaited_does_not_execute_its_body() {
    let module = Arc::new(compile(
        "public int Compute() { return 1; } \
             public async Task<int> Calculate() { int v = await Task.Run(Compute); return v; }",
    ));
    let wrapper = module
        .functions
        .iter()
        .find(|function| function.name == "Calculate")
        .expect("Calculate is declared")
        .symbol;
    let mut runtime = TaskRuntime::new(&provider(), &module, 1).expect("runtime starts");
    let driver = PreparedProgram::prepare(&module).expect("driver prepares");
    let pointer = std::ptr::from_mut(&mut runtime).cast::<()>();
    let (value, _) = driver
        .invoke(wrapper, false, Some(pointer), None, None)
        .expect("wrapper registers the async task without running its body");
    let ExecutionValue::Long(bits) = value else {
        panic!("wrapper must return a Task<T> handle");
    };
    let handle = TaskHandleId::from_bits(bits);

    let (state, resolved, inner_submitted) = runtime
        .async_task_probe(handle)
        .expect("the async task is registered");
    assert_eq!(state, 0, "MoveNext never ran: state is still 0");
    assert!(!resolved, "an unawaited task is never resolved");
    assert!(!inner_submitted, "no inner Task.Run was ever submitted");

    // Dropping a runtime with a pending, never-awaited async task must
    // not hang or panic.
    drop(runtime);
}

#[test]
fn parallel_for_same_error_contract_with_one_two_or_many_workers() {
    // Every index but 7 is safe against a length-8 array; index 7 alone
    // overflows the length-7 slot it is deliberately given, so the
    // deterministic "smallest failing index" is unambiguous regardless
    // of how many workers actually run the range.
    let source = "public void Body(int index) { \
             int size = index == 7 ? 7 : 8; \
             int[] a = new int[size]; \
             int x = a[index]; \
         }";
    for worker_count in [1, 2, 6] {
        let module = compile(source);
        let body = symbol(&module, "Body");
        let mut runtime = TaskRuntime::new(&provider(), &Arc::new(module), worker_count)
            .expect("runtime starts with the requested worker count");
        let error = runtime
            .parallel_for(0, 16, body)
            .expect_err("index 7 must fail regardless of worker count");
        assert!(
            error.message().contains("array index 7"),
            "worker_count {worker_count}: unexpected error {error}"
        );
    }
}

#[test]
fn parallel_for_succeeds_identically_with_one_two_or_many_workers() {
    let source = "public void Body(int index) { }";
    for worker_count in [1, 2, 6] {
        let module = compile(source);
        let body = symbol(&module, "Body");
        let mut runtime = TaskRuntime::new(&provider(), &Arc::new(module), worker_count)
            .expect("runtime starts with the requested worker count");
        runtime
            .parallel_for(0, 500, body)
            .unwrap_or_else(|error| panic!("worker_count {worker_count}: {error}"));
    }
}

#[test]
fn parallel_for_each_same_error_contract_with_one_two_or_many_workers() {
    let source = "public void Body(int value) { \
             int size = value == 7 ? 7 : 8; \
             int[] a = new int[size]; \
             int x = a[value]; \
         }";
    let values: Vec<ExecutionValue> = (0..16).map(ExecutionValue::Int).collect();
    for worker_count in [1, 2, 6] {
        let module = compile(source);
        let body = symbol(&module, "Body");
        let mut runtime = TaskRuntime::new(&provider(), &Arc::new(module), worker_count)
            .expect("runtime starts with the requested worker count");
        let error = runtime
            .parallel_for_each(values.clone(), body)
            .expect_err("array position 7 must fail regardless of worker count");
        assert!(
            error.message().contains("array index 7"),
            "worker_count {worker_count}: unexpected error {error}"
        );
    }
}

#[test]
fn parallel_for_and_for_each_are_stable_across_repeated_runs_at_a_fixed_worker_count() {
    let for_source = "public void Body(int index) { }";
    let module = Arc::new(compile(for_source));
    let body = symbol(&module, "Body");
    let mut runtime = TaskRuntime::new(&provider(), &module, 4).expect("runtime starts");
    for repetition in 0..10 {
        runtime
            .parallel_for(0, 300, body)
            .unwrap_or_else(|error| panic!("repetition {repetition}: {error}"));
    }
}

#[test]
#[cfg(feature = "aarm-telemetry")]
fn governed_parallel_for_is_scheduler_independent_under_memory_pressure() {
    const HARD_LIMIT: usize = 64 * 1024;
    let source = "public void Body(int index) { \
             int size = index == 2 || index == 5 || index == 9 ? 20000 : 1; \
             int[] scratch = new int[size]; \
         }";
    for worker_count in [1, 2, 4, 16] {
        let module = Arc::new(compile(source));
        let body = symbol(&module, "Body");
        let governor = Arc::new(aster_runtime::MemoryGovernor::new(HARD_LIMIT));
        let mut runtime = TaskRuntime::with_memory_governor(
            &provider(),
            &module,
            worker_count,
            Arc::clone(&governor),
        )
        .expect("governed runtime starts");
        let mut expected_error = None;
        for repetition in 0..20 {
            let error = runtime
                .parallel_for(0, 16, body)
                .expect_err("logical index 2 exceeds every fixed chunk ceiling");
            assert!(
                error.message().contains("Parallel logical index 2"),
                "worker_count {worker_count}, repetition {repetition}: {error}"
            );
            assert_eq!(
                expected_error.get_or_insert_with(|| error.message().to_owned()),
                error.message(),
                "worker_count {worker_count}: diagnostic changed between repetitions"
            );
            let telemetry = governor.telemetry();
            assert_eq!(telemetry.current_capacity_bytes, 0);
            assert!(telemetry.peak_capacity_bytes <= telemetry.hard_limit_bytes);
            assert_eq!(telemetry.grant_events, telemetry.release_events);
            assert_eq!(
                telemetry.granted_bytes_cumulative,
                telemetry.released_bytes_cumulative
            );
        }
        for plan in runtime.parallel_plans() {
            assert_eq!(
                plan.chunk_budgets_bytes.iter().sum::<u64>(),
                plan.available_headroom_bytes
            );
        }
    }
}

#[test]
#[cfg(feature = "aarm-telemetry")]
fn governed_for_each_and_reduce_keep_the_same_logical_failure() {
    const HARD_LIMIT: usize = 16 * 1024;
    let module = Arc::new(compile(
        "public void Each(int value) { int size = value == 2 || value == 5 ? 20000 : 1; int[] scratch = new int[size]; } \
             public int Accumulate(int total, int value) { int size = value == 2 || value == 5 ? 20000 : 1; int[] scratch = new int[size]; return total + value; } \
             public int Combine(int left, int right) { return left + right; }",
    ));
    let each = symbol(&module, "Each");
    let accumulate = symbol(&module, "Accumulate");
    let combine = symbol(&module, "Combine");
    let values = (0..8).map(ExecutionValue::Int).collect::<Vec<_>>();

    for reduce in [false, true] {
        let governor = Arc::new(aster_runtime::MemoryGovernor::new(HARD_LIMIT));
        let mut runtime =
            TaskRuntime::with_memory_governor(&provider(), &module, 4, Arc::clone(&governor))
                .expect("governed runtime starts");
        let mut expected_error = None;
        for repetition in 0..20 {
            let error = if reduce {
                runtime
                    .parallel_reduce(values.clone(), ExecutionValue::Int(0), accumulate, combine)
                    .expect_err("Reduce position 2 exceeds its fixed ceiling")
            } else {
                runtime
                    .parallel_for_each(values.clone(), each)
                    .expect_err("ForEach position 2 exceeds its fixed ceiling")
            };
            assert!(
                error.message().contains("Parallel logical index 2"),
                "repetition {repetition}: {error}"
            );
            assert_eq!(
                expected_error.get_or_insert_with(|| error.message().to_owned()),
                error.message()
            );
            assert_eq!(governor.telemetry().current_capacity_bytes, 0);
        }
        let telemetry = governor.telemetry();
        assert!(telemetry.peak_capacity_bytes <= telemetry.hard_limit_bytes);
        assert_eq!(telemetry.grant_events, telemetry.release_events);
        assert_eq!(
            telemetry.granted_bytes_cumulative,
            telemetry.released_bytes_cumulative
        );
    }
}

#[test]
fn parallel_reduce_gives_the_same_result_with_one_two_or_many_workers() {
    let values: Vec<ExecutionValue> = (1..=50).map(ExecutionValue::Int).collect();
    let expected = ExecutionValue::Int((1..=50).sum());
    for worker_count in [1, 2, 8] {
        let module = compile(REDUCE_SOURCE);
        let accumulate = symbol(&module, "AddValue");
        let combine = symbol(&module, "AddPartial");
        let mut runtime = TaskRuntime::new(&provider(), &Arc::new(module), worker_count)
            .expect("runtime starts with the requested worker count");
        let result = runtime
            .parallel_reduce(values.clone(), ExecutionValue::Int(0), accumulate, combine)
            .expect("reduction succeeds");
        assert_eq!(
            result, expected,
            "worker_count {worker_count} produced a different result"
        );
    }
}

#[test]
fn parallel_reduce_empty_array_returns_identity_directly() {
    let module = compile(REDUCE_SOURCE);
    let accumulate = symbol(&module, "AddValue");
    let combine = symbol(&module, "AddPartial");
    let mut runtime = TaskRuntime::new(&provider(), &Arc::new(module), 4).expect("runtime starts");
    let result = runtime
        .parallel_reduce(Vec::new(), ExecutionValue::Int(99), accumulate, combine)
        .expect("an empty array never fails");
    assert_eq!(result, ExecutionValue::Int(99));
}

#[test]
fn parallel_reduce_combines_partials_left_to_right_in_chunk_order() {
    // `Combine(left, right) = left * 100 + right` is sensitive to
    // combination order: getting chunk order wrong changes the result.
    // With 4 workers over 4 single-element chunks (`[1, 2, 3, 4]`),
    // `Accumulate` returns each element itself (identity `0`), so the
    // only way to reach `1020304` is folding strictly left to right in
    // chunk (array-position) order.
    let module = compile(
        "public int AddValue(int accumulator, int value) { return accumulator + value; } \
             public int Weighted(int left, int right) { return left * 100 + right; }",
    );
    let accumulate = symbol(&module, "AddValue");
    let combine = symbol(&module, "Weighted");
    let mut runtime = TaskRuntime::new(&provider(), &Arc::new(module), 4).expect("runtime starts");
    let values = vec![
        ExecutionValue::Int(1),
        ExecutionValue::Int(2),
        ExecutionValue::Int(3),
        ExecutionValue::Int(4),
    ];
    let result = runtime
        .parallel_reduce(values, ExecutionValue::Int(0), accumulate, combine)
        .expect("reduction succeeds");
    assert_eq!(result, ExecutionValue::Int(1_020_304));
}

#[test]
#[cfg(feature = "aarm-telemetry")]
fn governed_reduce_combine_reuses_released_headroom_sequentially() {
    const HARD_LIMIT: usize = 64 * 1024;
    let module = Arc::new(compile(
        "public int AddValue(int total, int value) { return total + value; } \
             public int AddPartial(int left, int right) { int[] scratch = new int[1]; return left + right; }",
    ));
    let accumulate = symbol(&module, "AddValue");
    let combine = symbol(&module, "AddPartial");
    let governor = Arc::new(aster_runtime::MemoryGovernor::new(HARD_LIMIT));
    let mut runtime =
        TaskRuntime::with_memory_governor(&provider(), &module, 4, Arc::clone(&governor))
            .expect("governed runtime starts");

    let result = runtime
        .parallel_reduce(
            (1..=4).map(ExecutionValue::Int).collect(),
            ExecutionValue::Int(0),
            accumulate,
            combine,
        )
        .expect("governed reduction succeeds");
    assert_eq!(result, ExecutionValue::Int(10));
    let plans = runtime.parallel_plans();
    assert_eq!(plans[0].operation, "Parallel.Reduce accumulate");
    assert_eq!(plans[0].chunk_budgets_bytes, [16 * 1024; 4]);
    assert_eq!(plans.len(), 4);
    for plan in &plans[1..] {
        assert_eq!(plan.operation, "Parallel.Reduce combine");
        assert_eq!(plan.initial_governor_capacity_bytes, 0);
        assert_eq!(plan.available_headroom_bytes, HARD_LIMIT as u64);
        assert_eq!(plan.chunk_budgets_bytes, [HARD_LIMIT as u64]);
    }
    let telemetry = governor.telemetry();
    assert_eq!(telemetry.current_capacity_bytes, 0);
    assert_eq!(telemetry.grant_events, 3);
    assert_eq!(telemetry.release_events, 3);
}

#[test]
fn parallel_reduce_repeated_executions_with_the_same_worker_count_return_the_same_result() {
    let module = Arc::new(compile(REDUCE_SOURCE));
    let accumulate = symbol(&module, "AddValue");
    let combine = symbol(&module, "AddPartial");
    let mut runtime = TaskRuntime::new(&provider(), &module, 4).expect("runtime starts");
    let values: Vec<ExecutionValue> = (1..=30).map(ExecutionValue::Int).collect();
    for _ in 0..10 {
        let result = runtime
            .parallel_reduce(values.clone(), ExecutionValue::Int(0), accumulate, combine)
            .expect("reduction succeeds");
        assert_eq!(result, ExecutionValue::Int((1..=30).sum()));
    }
}

#[test]
fn a_parallel_reduce_accumulate_error_does_not_contaminate_a_later_reduction() {
    let module = Arc::new(compile(
        "public int Boom(int accumulator, int value) { int[] a = new int[1]; return a[value]; } \
             public int AddPartial(int left, int right) { return left + right; }",
    ));
    let boom = symbol(&module, "Boom");
    let combine = symbol(&module, "AddPartial");
    let mut runtime = TaskRuntime::new(&provider(), &module, 2).expect("runtime starts");

    let failing = runtime.parallel_reduce(
        vec![ExecutionValue::Int(5)],
        ExecutionValue::Int(0),
        boom,
        combine,
    );
    assert!(failing.is_err(), "the out-of-bounds access must fail");

    let succeeding = runtime.parallel_reduce(
        vec![ExecutionValue::Int(0)],
        ExecutionValue::Int(0),
        boom,
        combine,
    );
    assert_eq!(
        succeeding,
        Ok(ExecutionValue::Int(0)),
        "a later, valid reduction must not be affected by the earlier failure"
    );
}

#[test]
fn a_parallel_reduce_combine_error_does_not_contaminate_a_later_reduction() {
    let module = Arc::new(compile(
        "public int AddValue(int accumulator, int value) { return accumulator + value; } \
             public int BoomCombine(int left, int right) { int[] a = new int[1]; return a[left + right]; }",
    ));
    let accumulate = symbol(&module, "AddValue");
    let boom_combine = symbol(&module, "BoomCombine");
    let mut runtime = TaskRuntime::new(&provider(), &module, 4).expect("runtime starts");

    // Two single-element chunks (4 workers, 2 elements) force exactly one
    // combine step; `left + right` (1) is out of bounds against `a`.
    let failing = runtime.parallel_reduce(
        vec![ExecutionValue::Int(0), ExecutionValue::Int(1)],
        ExecutionValue::Int(0),
        accumulate,
        boom_combine,
    );
    assert!(
        failing.is_err(),
        "the combine step's failure must propagate"
    );

    let succeeding = runtime.parallel_reduce(
        vec![ExecutionValue::Int(1)],
        ExecutionValue::Int(0),
        accumulate,
        boom_combine,
    );
    assert_eq!(
        succeeding,
        Ok(ExecutionValue::Int(1)),
        "a later reduction needing no combine step must not be affected"
    );
}

#[test]
fn pumping_with_a_single_worker_does_not_deadlock() {
    // With one worker, `Compute` runs there while the host thread blocks
    // on the completion queue, exactly the scenario the algorithm in the
    // module doc exists to make safe.
    assert_eq!(
        pump_canonical_example(1).expect("resolves"),
        ExecutionValue::Int(43)
    );
}

#[test]
fn pumping_with_multiple_workers_resolves_the_same_way() {
    assert_eq!(
        pump_canonical_example(4).expect("resolves"),
        ExecutionValue::Int(43)
    );
}
