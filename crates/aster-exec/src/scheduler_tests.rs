//! CONC-004 scheduler behavior proven against a scripted program: per-worker
//! queues with an injection queue, the turn budget, and the helping `Wait()`.
//! No code generator is involved; real programs are covered by the
//! `aster-codegen-cranelift` concurrency suites.

use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

use aster_mir as mir;

use crate::test_support::FakeProvider;
use crate::worker_pool::{
    AsyncMemoryDomain, ChunkOutcome, CombineOutcome, ExecutionPool, MAX_HELP_DEPTH,
    ParallelMemoryBudget, ReduceChunkOutcome, TURN_BUDGET, TaskHandle, TaskMemoryPolicy,
    TaskOutcome, TaskPayload,
};
use crate::{BackendError, CodeProvider, ExecutableProgram, ExecutionValue};

const TIMEOUT: Duration = Duration::from_secs(20);
const CHAIN_FIRST: u32 = 100;
const CHAIN_LAST: u32 = 107;

/// A one-shot gate a scripted job blocks on.
#[derive(Default)]
struct Gate {
    open: Mutex<bool>,
    changed: Condvar,
}

impl Gate {
    fn open(&self) {
        *self.open.lock().unwrap() = true;
        self.changed.notify_all();
    }

    fn wait(&self) {
        let guard = self.open.lock().unwrap();
        let (_guard, result) = self
            .changed
            .wait_timeout_while(guard, TIMEOUT, |open| !*open)
            .unwrap();
        assert!(!result.timed_out(), "gate was never opened");
    }
}

type Hook = Arc<dyn Fn(&ScriptProgram, u32) + Send + Sync>;

/// Records the order of executed symbols and lets a test script each job.
#[derive(Default)]
struct ScriptProgram {
    pool: Mutex<Weak<ExecutionPool>>,
    order: Mutex<Vec<(u32, std::thread::ThreadId)>>,
    hook: Mutex<Option<Hook>>,
    spawned: Mutex<Vec<TaskHandle>>,
}

impl ScriptProgram {
    fn pool(&self) -> Arc<ExecutionPool> {
        self.pool.lock().unwrap().upgrade().expect("pool is alive")
    }

    fn symbols(&self) -> Vec<u32> {
        self.order.lock().unwrap().iter().map(|(s, _)| *s).collect()
    }
}

impl ExecutableProgram for ScriptProgram {
    fn run_task(
        &self,
        symbol: mir::SymbolId,
        _payload: &TaskPayload,
        _cancellation: Arc<aster_runtime::TaskCancellation>,
        _collect_stats: bool,
        _memory_policy: Option<&TaskMemoryPolicy>,
    ) -> (TaskOutcome, Option<aster_runtime::AarmMemoryTelemetry>) {
        self.order
            .lock()
            .unwrap()
            .push((symbol.0, std::thread::current().id()));
        let hook = self.hook.lock().unwrap().clone();
        if let Some(hook) = hook {
            hook(self, symbol.0);
        }
        (
            TaskOutcome::Completed(
                ExecutionValue::Int(i32::try_from(symbol.0).unwrap()),
                aster_runtime::MemoryStats::default(),
            ),
            None,
        )
    }

    fn invoke_move_next_with_cancellation(
        &self,
        _symbol: mir::SymbolId,
        _handle: i64,
        _task_runtime: *mut (),
        _cancellation: Arc<aster_runtime::TaskCancellation>,
        _memory_domain: Option<&AsyncMemoryDomain>,
    ) -> (
        Result<(ExecutionValue, aster_runtime::MemoryStats), BackendError>,
        Option<aster_runtime::AarmMemoryTelemetry>,
    ) {
        (Err(BackendError::new("no async steps")), None)
    }

    fn run_for_chunk(
        &self,
        _: mir::SymbolId,
        _: i32,
        _: i32,
        _: Option<&ParallelMemoryBudget>,
    ) -> ChunkOutcome {
        unreachable!("scripted tests run tasks only")
    }

    fn run_for_each_chunk(
        &self,
        _: mir::SymbolId,
        _: usize,
        _: &[ExecutionValue],
        _: Option<&ParallelMemoryBudget>,
    ) -> ChunkOutcome {
        unreachable!("scripted tests run tasks only")
    }

    fn run_reduce_chunk(
        &self,
        _: mir::SymbolId,
        _: usize,
        _: &ExecutionValue,
        _: &[ExecutionValue],
        _: Option<&ParallelMemoryBudget>,
    ) -> ReduceChunkOutcome {
        unreachable!("scripted tests run tasks only")
    }

    fn run_combine_step(
        &self,
        _: mir::SymbolId,
        _: &ExecutionValue,
        _: &ExecutionValue,
        _: Option<&ParallelMemoryBudget>,
    ) -> CombineOutcome {
        unreachable!("scripted tests run tasks only")
    }
}

fn scripted_pool(workers: usize) -> (Arc<ExecutionPool>, Arc<ScriptProgram>) {
    let module = Arc::new(
        aster_compiler::compile("public int Alpha() { return 1; }")
            .expect("source compiles")
            .mir,
    );
    let provider = Arc::new(FakeProvider::default()) as Arc<dyn CodeProvider>;
    let pool = Arc::new(ExecutionPool::new(provider, module, workers).expect("pool"));
    let program = Arc::new(ScriptProgram::default());
    *program.pool.lock().unwrap() = Arc::downgrade(&pool);
    pool.adopt_program(Arc::clone(&program) as Arc<dyn ExecutableProgram>);
    pool.ensure_workers(workers).expect("workers start");
    (pool, program)
}

fn submit(pool: &ExecutionPool, symbol: u32) -> TaskHandle {
    pool.submit(mir::SymbolId(symbol), false, None)
        .expect("job is accepted")
}

fn wait_until(condition: impl Fn() -> bool) {
    let start = Instant::now();
    while !condition() {
        assert!(start.elapsed() < TIMEOUT, "condition never became true");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn completed(outcome: TaskOutcome) -> i32 {
    match outcome {
        TaskOutcome::Completed(ExecutionValue::Int(value), _) => value,
        other => panic!("task did not complete: {other:?}"),
    }
}

/// A worker serves its own local queue first, but yields to the injection
/// queue after `TURN_BUDGET` consecutive local turns when it has work.
#[test]
fn a_domain_yields_its_thread_after_the_turn_budget_when_others_are_ready() {
    const LOCAL_JOBS: u32 = 200;
    let (pool, program) = scripted_pool(1);
    let started = Arc::new(Gate::default());
    let release = Arc::new(Gate::default());
    {
        let (started, release) = (Arc::clone(&started), Arc::clone(&release));
        *program.hook.lock().unwrap() = Some(Arc::new(move |program, symbol| {
            if symbol == 1 {
                // Runs on the worker: these submissions land in its local queue.
                for _ in 0..LOCAL_JOBS {
                    let handle = submit(&program.pool(), 2);
                    program.spawned.lock().unwrap().push(handle);
                }
                started.open();
                release.wait();
            }
        }));
    }

    let first = submit(&pool, 1);
    started.wait();
    let injected = submit(&pool, 3);
    release.open();

    assert_eq!(completed(first.join().unwrap()), 1);
    assert_eq!(completed(injected.join().unwrap()), 3);
    for handle in std::mem::take(&mut *program.spawned.lock().unwrap()) {
        assert_eq!(completed(handle.join().unwrap()), 2);
    }
    let order = program.symbols();
    assert_eq!(order.len(), 2 + LOCAL_JOBS as usize);
    let budget = TURN_BUDGET;
    assert!(order[1..=budget].iter().all(|symbol| *symbol == 2));
    assert_eq!(
        order[budget + 1],
        3,
        "the injected job runs right after one budget of local turns"
    );
    assert!(order[budget + 2..].iter().all(|symbol| *symbol == 2));
}

/// With the injection queue empty a worker keeps serving its local queue
/// beyond the budget; nothing waits for it.
#[test]
fn the_budget_does_not_idle_a_worker_when_nobody_else_is_ready() {
    let (pool, program) = scripted_pool(1);
    let started = Arc::new(Gate::default());
    let release = Arc::new(Gate::default());
    {
        let (started, release) = (Arc::clone(&started), Arc::clone(&release));
        *program.hook.lock().unwrap() = Some(Arc::new(move |program, symbol| {
            if symbol == 1 {
                for _ in 0..150 {
                    let handle = submit(&program.pool(), 2);
                    program.spawned.lock().unwrap().push(handle);
                }
                started.open();
                release.wait();
            }
        }));
    }
    let first = submit(&pool, 1);
    started.wait();
    release.open();
    first.join().unwrap();
    for handle in std::mem::take(&mut *program.spawned.lock().unwrap()) {
        assert_eq!(completed(handle.join().unwrap()), 2);
    }
    assert_eq!(program.symbols().len(), 151);
}

/// A waiter runs queued tasks on its own thread instead of blocking, even
/// when every worker is busy.
#[test]
fn a_helping_wait_runs_queued_tasks_while_every_worker_is_busy() {
    let (pool, program) = scripted_pool(1);
    let started = Arc::new(Gate::default());
    let release = Arc::new(Gate::default());
    {
        let (started, release) = (Arc::clone(&started), Arc::clone(&release));
        *program.hook.lock().unwrap() = Some(Arc::new(move |_, symbol| {
            if symbol == 1 {
                started.open();
                release.wait();
            }
        }));
    }
    let blocker = submit(&pool, 1);
    started.wait();
    let queued = [submit(&pool, 10), submit(&pool, 11), submit(&pool, 12)];
    let mut results = Vec::new();
    for handle in queued {
        results.push(completed(pool.join_helping(handle).unwrap()));
    }
    // All three completed on this thread while the only worker stayed blocked.
    assert_eq!(results, vec![10, 11, 12]);
    assert_eq!(pool.help_counters().0, 3);
    let me = std::thread::current().id();
    let ran_here = program
        .order
        .lock()
        .unwrap()
        .iter()
        .filter(|(symbol, thread)| *symbol >= 10 && *thread == me)
        .count();
    assert_eq!(ran_here, 3);
    release.open();
    blocker.join().unwrap();
}

/// Helping nests at most `MAX_HELP_DEPTH` deep; beyond it the waiter blocks.
#[test]
fn helping_waits_nest_at_most_four_deep_then_block() {
    assert_eq!(MAX_HELP_DEPTH, 4);
    let (pool, program) = scripted_pool(1);
    let started = Arc::new(Gate::default());
    let release = Arc::new(Gate::default());
    {
        let (started, release) = (Arc::clone(&started), Arc::clone(&release));
        *program.hook.lock().unwrap() = Some(Arc::new(move |program, symbol| {
            if symbol == 1 {
                started.open();
                release.wait();
            } else if (CHAIN_FIRST..CHAIN_LAST).contains(&symbol) {
                // Each chain job submits and helpingly waits for the next one.
                let pool = program.pool();
                let next = submit(&pool, symbol + 1);
                assert_eq!(
                    completed(pool.join_helping(next).unwrap()),
                    i32::try_from(symbol + 1).unwrap()
                );
            }
        }));
    }
    let blocker = submit(&pool, 1);
    started.wait();
    let head = submit(&pool, CHAIN_FIRST);

    // The host helps four levels deep (jobs 100..=103 on this thread); job
    // 104 waits for 105 with the depth exhausted, so it blocks until the
    // worker is released.
    let releaser = {
        let (pool, release) = (Arc::clone(&pool), Arc::clone(&release));
        std::thread::spawn(move || {
            wait_until(|| pool.help_counters().1 == MAX_HELP_DEPTH);
            std::thread::sleep(Duration::from_millis(50));
            release.open();
        })
    };
    assert_eq!(completed(pool.join_helping(head).unwrap()), 100);
    releaser.join().unwrap();
    blocker.join().unwrap();

    assert_eq!(
        pool.help_counters().1,
        MAX_HELP_DEPTH,
        "never deeper than 4"
    );
    let me = std::thread::current().id();
    let here = program
        .order
        .lock()
        .unwrap()
        .iter()
        .filter(|(symbol, thread)| *symbol >= CHAIN_FIRST && *thread == me)
        .count();
    assert_eq!(
        here, MAX_HELP_DEPTH,
        "exactly four chain jobs ran by helping"
    );
    let order = program.symbols();
    assert_eq!(order.iter().filter(|s| **s >= CHAIN_FIRST).count(), 8);
}

/// Results are independent of which thread ran each task, and the pool shuts
/// down cleanly after a throughput-style burst.
#[test]
fn many_tasks_complete_with_deterministic_results_and_helping_waits() {
    let (pool, program) = scripted_pool(4);
    let handles: Vec<_> = (0..2000).map(|index| submit(&pool, index)).collect();
    let results: Vec<i32> = handles
        .into_iter()
        .map(|handle| completed(pool.join_helping(handle).unwrap()))
        .collect();
    assert_eq!(results, (0..2000).collect::<Vec<i32>>());
    assert_eq!(program.symbols().len(), 2000);
    pool.shutdown();
}
