//! JIT-backed tests moved from `aster-exec::worker_pool` (ARCH-005).

use std::sync::{Arc, mpsc};
use std::thread;

use aster_exec::worker_pool::*;
use aster_exec::{ExecutableProgram, ExecutionValue, mir};

use crate::execution::{PreparedProgram, SharedProgram};
use crate::provider::provider;

use std::time::Duration;

const TEST_TIMEOUT: Duration = Duration::from_secs(5);

fn compile(source: &str) -> mir::Module {
    aster_compiler::compile(source)
        .expect("source compiles")
        .mir
}

/// Calls the real (blocking) `TaskHandle::join` on a helper thread and
/// bounds the wait from here, so a stuck worker fails the test instead
/// of hanging it. Callers use this only where a clean completion is
/// expected; a channel-level failure here is itself a test bug.
fn join_with_timeout(handle: TaskHandle) -> TaskOutcome {
    let id = handle.id();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(handle.join());
    });
    receiver
        .recv_timeout(TEST_TIMEOUT)
        .unwrap_or_else(|_| panic!("task {id:?} did not respond before the test timeout"))
        .unwrap_or_else(|error| panic!("task {id:?} channel failed: {error}"))
}

#[test]
fn a_dequeued_pre_cancelled_job_skips_its_body_and_the_worker_stays_reusable() {
    let module = compile(
        "public int FailIfCalled() { int[] values = new int[1]; return values[4]; } \
             public int Value() { return 42; }",
    );
    let fail = module
        .functions
        .iter()
        .find(|function| function.name == "FailIfCalled")
        .expect("failing function exists")
        .symbol;
    let value = module
        .functions
        .iter()
        .find(|function| function.name == "Value")
        .expect("value function exists")
        .symbol;
    let program: Arc<dyn ExecutableProgram> =
        SharedProgram::new(PreparedProgram::prepare(&module).expect("program prepares"));

    let cancelled = Arc::new(TaskControl::new());
    assert!(cancelled.cancel());
    let (cancel_sender, cancel_receiver) = mpsc::channel();
    run_task_job(
        program.as_ref(),
        fail,
        &TaskPayload::default(),
        &cancelled,
        false,
        None,
        &cancel_sender,
    );
    let cancelled_outcome = cancel_receiver.recv().expect("cancelled job responds");
    assert!(matches!(
        cancelled_outcome,
        TaskOutcome::Failed(error)
            if error.is_cancellation() && !error.message().contains("array index")
    ));

    let fresh = Arc::new(TaskControl::new());
    let (value_sender, value_receiver) = mpsc::channel();
    run_task_job(
        program.as_ref(),
        value,
        &TaskPayload::default(),
        &fresh,
        false,
        None,
        &value_sender,
    );
    assert!(matches!(
        value_receiver.recv().expect("fresh job responds"),
        TaskOutcome::Completed(ExecutionValue::Int(42), _)
    ));
    assert!(!fresh.cancellation().is_requested());
}

/// `JobKind` (what a worker actually receives) must stay `Send + 'static`
/// with no raw pointer, `ExecutionContext`, or `PreparedProgram` inside
/// it. This is a compile-time guarantee, not a runtime assertion: a
/// future variant carrying `*mut aster_runtime::ExecutionContext` or a
/// borrowed array pointer would make `JobKind` stop being `Send` (raw
/// pointers are not `Send`) and this test would fail to *compile*, not
/// just fail to pass. `ExecutionValue` (the only thing `ForEachChunk`
/// carries per element) is scalar-only, never a class/array/interface
/// handle (see `values::primitive`).
#[test]
fn job_kind_carries_only_owned_send_data_no_arena_pointer_or_context() {
    fn assert_send<T: Send>() {}
    assert_send::<JobKind>();
    assert_send::<ParallelMemoryBudget>();
    assert_send::<ChunkOutcome>();
    assert_send::<TaskOutcome>();
    assert_send::<Arc<Vec<ExecutionValue>>>();
    // `Parallel.Reduce`'s job/outcome types must hold to the same
    // guarantee: only owned scalars, never a pointer or `ExecutionContext`.
    assert_send::<ReduceChunkOutcome>();
    assert_send::<CombineOutcome>();
    assert_send::<TaskMemoryPolicy>();
}

#[test]
fn single_worker_pool_runs_a_task() {
    let module = Arc::new(compile("public int Run() { return 40 + 2; }"));
    let pool = ExecutionPool::new(provider(), module, 1).expect("pool starts");
    assert_eq!(pool.prepared_worker_count(), 0);
    let run = pool.resolve("Run").expect("Run resolves");

    let handle = pool.submit(run, false, None).expect("task is accepted");
    assert_eq!(pool.prepared_worker_count(), 1);
    match join_with_timeout(handle) {
        TaskOutcome::Completed(value, _) => assert_eq!(value, ExecutionValue::Int(42)),
        TaskOutcome::Failed(error) => panic!("unexpected failure: {error}"),
    }

    pool.shutdown();
}

#[test]
fn worker_demand_is_capped_before_allocating_preparation_state() {
    let module = Arc::new(compile("public int Run() { return 42; }"));
    let pool = ExecutionPool::new(provider(), module, 1).expect("pool starts");

    pool.ensure_workers(usize::MAX)
        .expect("demand is capped by the configured worker limit");
    assert_eq!(pool.prepared_worker_count(), 1);

    pool.shutdown();
}

#[test]
fn multiple_workers_run_many_tasks_of_the_same_function() {
    let module = Arc::new(compile("public int Run() { return 40 + 2; }"));
    let pool = ExecutionPool::new(provider(), module, 4).expect("pool starts");
    pool.ensure_workers(4).expect("four workers prepare");
    assert_eq!(pool.prepared_worker_count(), 4);
    let run = pool.resolve("Run").expect("Run resolves");

    let handles: Vec<_> = (0..20)
        .map(|_| pool.submit(run, false, None).expect("task is accepted"))
        .collect();

    for handle in handles {
        match join_with_timeout(handle) {
            TaskOutcome::Completed(value, _) => assert_eq!(value, ExecutionValue::Int(42)),
            TaskOutcome::Failed(error) => panic!("unexpected failure: {error}"),
        }
    }

    pool.shutdown();
}

/// T-CONC-EXP-1 evidence: one preparation serves every worker; each live
/// worker holds the shared program; shutdown releases every worker hold.
#[test]
fn an_adopted_program_is_the_one_every_worker_runs() {
    let module = Arc::new(compile("public int Run() { return 40 + 2; }"));
    let program: Arc<dyn ExecutableProgram> =
        SharedProgram::new(PreparedProgram::prepare(&module).expect("prepares"));
    let pool = ExecutionPool::new(provider(), Arc::clone(&module), 3).expect("pool starts");
    pool.adopt_program(Arc::clone(&program));
    pool.ensure_workers(3).expect("workers start");
    let shared = pool.shared_program().expect("shared program");
    assert!(
        Arc::ptr_eq(&shared, &program),
        "the pool must not prepare its own"
    );
    // Caller, pool, three workers, and `shared` above.
    assert_eq!(Arc::strong_count(&program), 6);
    let run = pool.resolve("Run").expect("Run resolves");
    match join_with_timeout(pool.submit(run, false, None).expect("accepted")) {
        TaskOutcome::Completed(value, _) => assert_eq!(value, ExecutionValue::Int(42)),
        TaskOutcome::Failed(error) => panic!("unexpected failure: {error}"),
    }
    pool.shutdown();
    drop(shared);
    assert_eq!(Arc::strong_count(&program), 2);
}

#[test]
fn workers_share_one_prepared_program_and_release_it_at_shutdown() {
    let module = Arc::new(compile(
        "public int Work() { int t = 0; for (int i = 0; i < 100000; i++) { t = (t + i) % 1000; } return t; }",
    ));
    let pool = ExecutionPool::new(provider(), module, 4).expect("pool starts");
    pool.ensure_workers(4).expect("four workers prepare");
    // One reference held by the pool, one per running worker thread.
    assert_eq!(pool.shared_program_holders(), 5);
    let work = pool.resolve("Work").expect("Work resolves");
    let handles: Vec<_> = (0..64)
        .map(|_| pool.submit(work, false, None).expect("task is accepted"))
        .collect();
    for handle in handles {
        match join_with_timeout(handle) {
            TaskOutcome::Completed(value, _) => assert_eq!(value, ExecutionValue::Int(0)),
            TaskOutcome::Failed(error) => panic!("unexpected failure: {error}"),
        }
    }
    pool.shutdown();
    assert_eq!(pool.shared_program_holders(), 1);
}

#[test]
fn workers_run_different_functions_of_the_same_module_regardless_of_completion_order() {
    let module = Arc::new(compile(
        "public int Answer() { return 42; } public int Double(int value) { return value * 2; } public int Run() { return Double(Answer()); }",
    ));
    let pool = ExecutionPool::new(provider(), module, 3).expect("pool starts");
    let answer = pool.resolve("Answer").expect("Answer resolves");
    let run = pool.resolve("Run").expect("Run resolves");

    // Submitted out of the order results are asserted in, so a pass
    // proves results are matched to their own task, not to arrival order.
    let run_handle = pool.submit(run, false, None).expect("task is accepted");
    let answer_handle = pool.submit(answer, false, None).expect("task is accepted");

    match join_with_timeout(answer_handle) {
        TaskOutcome::Completed(value, _) => assert_eq!(value, ExecutionValue::Int(42)),
        TaskOutcome::Failed(error) => panic!("unexpected failure: {error}"),
    }
    match join_with_timeout(run_handle) {
        TaskOutcome::Completed(value, _) => assert_eq!(value, ExecutionValue::Int(84)),
        TaskOutcome::Failed(error) => panic!("unexpected failure: {error}"),
    }

    pool.shutdown();
}

#[test]
fn each_task_gets_independent_non_accumulating_metrics() {
    let module = Arc::new(compile(
        "public int Run() { int[] values = [20, 22]; return values[0] + values[1]; }",
    ));
    let pool = ExecutionPool::new(provider(), module, 2).expect("pool starts");
    let run = pool.resolve("Run").expect("Run resolves");

    let handles: Vec<_> = (0..6)
        .map(|_| pool.submit(run, true, None).expect("task is accepted"))
        .collect();

    for handle in handles {
        match join_with_timeout(handle) {
            TaskOutcome::Completed(value, stats) => {
                assert_eq!(value, ExecutionValue::Int(42));
                assert_eq!(stats.total_allocations, 1);
                assert_eq!(stats.array_allocations, 1);
                // No persistent leftover from any other task's arena and
                // the temporary array is fully rewound before return.
                assert_eq!(stats.used_bytes, 0);
            }
            TaskOutcome::Failed(error) => panic!("unexpected failure: {error}"),
        }
    }

    pool.shutdown();
}

#[test]
fn a_controlled_error_in_one_task_does_not_block_later_tasks() {
    let module = Arc::new(compile(
        "public int Failing() { int[] values = new int[1]; return values[5]; } public int Ok() { return 1; }",
    ));
    let pool = ExecutionPool::new(provider(), module, 2).expect("pool starts");
    let failing = pool.resolve("Failing").expect("Failing resolves");
    let ok = pool.resolve("Ok").expect("Ok resolves");

    let failing_handle = pool.submit(failing, false, None).expect("task is accepted");
    let ok_handle = pool.submit(ok, false, None).expect("task is accepted");

    match join_with_timeout(failing_handle) {
        TaskOutcome::Failed(error) => assert!(error.message().contains("array index 5")),
        TaskOutcome::Completed(..) => panic!("expected a controlled runtime error"),
    }
    match join_with_timeout(ok_handle) {
        TaskOutcome::Completed(value, _) => assert_eq!(value, ExecutionValue::Int(1)),
        TaskOutcome::Failed(error) => panic!("unaffected task failed: {error}"),
    }

    pool.shutdown();
}

#[test]
fn shutdown_still_runs_every_already_accepted_job() {
    let module = Arc::new(compile("public int Run() { return 1; }"));
    let pool = ExecutionPool::new(provider(), module, 2).expect("pool starts");
    let run = pool.resolve("Run").expect("Run resolves");

    let handles: Vec<_> = (0..10)
        .map(|_| pool.submit(run, false, None).expect("task is accepted"))
        .collect();

    // Nothing has been joined yet: every job is still only queued or
    // in flight when shutdown begins draining the queue.
    pool.shutdown();

    for handle in handles {
        match join_with_timeout(handle) {
            TaskOutcome::Completed(value, _) => assert_eq!(value, ExecutionValue::Int(1)),
            TaskOutcome::Failed(error) => panic!("unexpected failure: {error}"),
        }
    }
}

#[test]
fn submitting_after_shutdown_fails() {
    let module = Arc::new(compile("public int Run() { return 1; }"));
    let pool = ExecutionPool::new(provider(), module, 1).expect("pool starts");
    let run = pool.resolve("Run").expect("Run resolves");

    pool.shutdown();

    let result = pool.submit(run, false, None);
    assert!(result.is_err());
}

#[test]
fn many_temporary_tasks_across_multiple_workers_all_end_with_zero_used_bytes() {
    let module = Arc::new(compile(
        "public int Run() { int[] values = [20, 22]; return values[0] + values[1]; }",
    ));
    let pool = ExecutionPool::new(provider(), module, 4).expect("pool starts");
    let run = pool.resolve("Run").expect("Run resolves");

    let handles: Vec<_> = (0..24)
        .map(|_| pool.submit(run, true, None).expect("task is accepted"))
        .collect();

    for handle in handles {
        match join_with_timeout(handle) {
            TaskOutcome::Completed(value, stats) => {
                assert_eq!(value, ExecutionValue::Int(42));
                assert_eq!(stats.used_bytes, 0);
            }
            TaskOutcome::Failed(error) => panic!("unexpected failure: {error}"),
        }
    }

    pool.shutdown();
}

#[test]
fn many_persistent_tasks_on_one_worker_do_not_accumulate_memory() {
    let module = Arc::new(compile(
        "public interface IBox { int Get(); } public class Box : IBox { public int value; public int Get() { return value; } } public int Run() { Box box = new Box(); box.value = 1; IBox view = box; return view.Get(); }",
    ));
    // A single worker serializes every task, so this is the strictest
    // check that persistent storage from one task never leaks into the
    // next one on the same thread.
    let pool = ExecutionPool::new(provider(), module, 1).expect("pool starts");
    let run = pool.resolve("Run").expect("Run resolves");

    let mut expected_used_bytes = None;
    for _ in 0..15 {
        let handle = pool.submit(run, true, None).expect("task is accepted");
        match join_with_timeout(handle) {
            TaskOutcome::Completed(value, stats) => {
                assert_eq!(value, ExecutionValue::Int(1));
                assert!(stats.used_bytes > 0);
                let expected = *expected_used_bytes.get_or_insert(stats.used_bytes);
                assert_eq!(stats.used_bytes, expected);
            }
            TaskOutcome::Failed(error) => panic!("unexpected failure: {error}"),
        }
    }

    pool.shutdown();
}

#[test]
fn interleaved_temporary_and_persistent_tasks_keep_independent_stats() {
    let module = Arc::new(compile(
        "public interface IBox { int Get(); } public class Box : IBox { public int value; public int Get() { return value; } } public int Persistent() { Box box = new Box(); box.value = 1; IBox view = box; return view.Get(); } public int Temporary() { int[] values = [20, 22]; return values[0] + values[1]; }",
    ));
    let pool = ExecutionPool::new(provider(), module, 2).expect("pool starts");
    let persistent = pool.resolve("Persistent").expect("Persistent resolves");
    let temporary = pool.resolve("Temporary").expect("Temporary resolves");

    for round in 0..8 {
        let symbol = if round % 2 == 0 {
            persistent
        } else {
            temporary
        };
        let handle = pool.submit(symbol, true, None).expect("task is accepted");
        match join_with_timeout(handle) {
            TaskOutcome::Completed(value, stats) => {
                if round % 2 == 0 {
                    assert_eq!(value, ExecutionValue::Int(1));
                    assert!(stats.used_bytes > 0);
                } else {
                    assert_eq!(value, ExecutionValue::Int(42));
                    assert_eq!(stats.used_bytes, 0);
                }
            }
            TaskOutcome::Failed(error) => panic!("unexpected failure: {error}"),
        }
    }

    pool.shutdown();
}

#[test]
fn a_controlled_error_between_two_valid_tasks_does_not_affect_either() {
    let module = Arc::new(compile(
        "public int Ok() { return 7; } public int Failing() { int[] values = new int[1]; return values[5]; }",
    ));
    let pool = ExecutionPool::new(provider(), module, 2).expect("pool starts");
    let ok = pool.resolve("Ok").expect("Ok resolves");
    let failing = pool.resolve("Failing").expect("Failing resolves");

    let first = pool.submit(ok, false, None).expect("task is accepted");
    let middle = pool.submit(failing, false, None).expect("task is accepted");
    let last = pool.submit(ok, false, None).expect("task is accepted");

    match join_with_timeout(first) {
        TaskOutcome::Completed(value, _) => assert_eq!(value, ExecutionValue::Int(7)),
        TaskOutcome::Failed(error) => panic!("unexpected failure: {error}"),
    }
    match join_with_timeout(middle) {
        TaskOutcome::Failed(error) => assert!(error.message().contains("array index 5")),
        TaskOutcome::Completed(..) => panic!("expected a controlled runtime error"),
    }
    match join_with_timeout(last) {
        TaskOutcome::Completed(value, _) => assert_eq!(value, ExecutionValue::Int(7)),
        TaskOutcome::Failed(error) => panic!("unexpected failure: {error}"),
    }

    pool.shutdown();
}

#[test]
fn dropping_one_tasks_outcome_does_not_affect_another() {
    let module = Arc::new(compile(
        "public int First() { return 11; } public int Second() { return 22; }",
    ));
    let pool = ExecutionPool::new(provider(), module, 2).expect("pool starts");
    let first_symbol = pool.resolve("First").expect("First resolves");
    let second_symbol = pool.resolve("Second").expect("Second resolves");

    let first_handle = pool
        .submit(first_symbol, false, None)
        .expect("task is accepted");
    let second_handle = pool
        .submit(second_symbol, false, None)
        .expect("task is accepted");

    let first_outcome = join_with_timeout(first_handle);
    drop(first_outcome);

    match join_with_timeout(second_handle) {
        TaskOutcome::Completed(value, _) => assert_eq!(value, ExecutionValue::Int(22)),
        TaskOutcome::Failed(error) => panic!("unexpected failure: {error}"),
    }

    pool.shutdown();
}

#[test]
fn multiple_independent_pools_do_not_interfere() {
    let module_a = Arc::new(compile("public int Run() { return 1; }"));
    let module_b = Arc::new(compile("public int Run() { return 2; }"));

    let pool_a = ExecutionPool::new(provider(), module_a, 1).expect("pool a starts");
    let pool_b = ExecutionPool::new(provider(), module_b, 1).expect("pool b starts");
    let run_a = pool_a.resolve("Run").expect("Run resolves in a");
    let run_b = pool_b.resolve("Run").expect("Run resolves in b");

    let handle_a = pool_a.submit(run_a, false, None).expect("task is accepted");
    let handle_b = pool_b.submit(run_b, false, None).expect("task is accepted");

    match join_with_timeout(handle_a) {
        TaskOutcome::Completed(value, _) => assert_eq!(value, ExecutionValue::Int(1)),
        TaskOutcome::Failed(error) => panic!("unexpected failure: {error}"),
    }

    // Shutting down one pool must not disturb the other.
    pool_a.shutdown();

    match join_with_timeout(handle_b) {
        TaskOutcome::Completed(value, _) => assert_eq!(value, ExecutionValue::Int(2)),
        TaskOutcome::Failed(error) => panic!("unexpected failure: {error}"),
    }

    pool_b.shutdown();
}

#[test]
fn shutdown_with_an_empty_queue_terminates_cleanly() {
    let module = Arc::new(compile("public int Run() { return 1; }"));
    let pool = ExecutionPool::new(provider(), module, 3).expect("pool starts");

    pool.shutdown();

    assert!(
        pool.submit(pool.resolve("Run").expect("Run resolves"), false, None)
            .is_err()
    );
}

#[test]
fn shutdown_is_idempotent_and_drop_does_not_rejoin_workers() {
    let module = Arc::new(compile("public int Run() { return 1; }"));
    let pool = ExecutionPool::new(provider(), module, 2).expect("pool starts");

    pool.shutdown();
    // Calling it again must not try to join an already-joined
    // `JoinHandle`, and the pool's own `Drop` will call it once more.
    pool.shutdown();

    let run = pool.resolve("Run").expect("Run resolves");
    assert!(pool.submit(run, false, None).is_err());
}
