//! Provider and program fakes for scheduler unit tests. Nothing here needs a
//! code generator; the real JIT-backed scheduler tests live in
//! `aster-codegen-cranelift`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use aster_mir as mir;

use crate::worker_pool::{
    AsyncMemoryDomain, ChunkOutcome, CombineOutcome, ParallelMemoryBudget, ReduceChunkOutcome,
    TaskMemoryPolicy, TaskOutcome, TaskPayload,
};
use crate::{BackendError, CodeProvider, ExecutableProgram, ExecutionValue};

/// Program that "executes" a task by returning its symbol id as an `Int`, folds
/// `Parallel.Reduce` values by integer addition, and records every call so a
/// test can prove the scheduler went through the provider.
#[derive(Default)]
pub struct FakeProgram {
    pub tasks_run: AtomicUsize,
    pub for_ranges: Mutex<Vec<(i32, i32)>>,
}

fn int(value: &ExecutionValue) -> i32 {
    match value {
        ExecutionValue::Int(value) => *value,
        other => panic!("fake program only folds ints, got {other:?}"),
    }
}

impl ExecutableProgram for FakeProgram {
    fn run_task(
        &self,
        symbol: mir::SymbolId,
        _payload: &TaskPayload,
        _cancellation: Arc<aster_runtime::TaskCancellation>,
        _collect_stats: bool,
        _memory_policy: Option<&TaskMemoryPolicy>,
    ) -> (TaskOutcome, Option<aster_runtime::AarmMemoryTelemetry>) {
        self.tasks_run.fetch_add(1, Ordering::SeqCst);
        let id = i32::try_from(symbol.0).expect("small symbol ids in tests");
        (
            TaskOutcome::Completed(
                ExecutionValue::Int(id),
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
        (
            Err(BackendError::new("fake program has no async steps")),
            None,
        )
    }

    fn run_for_chunk(
        &self,
        _symbol: mir::SymbolId,
        start: i32,
        end: i32,
        _memory_budget: Option<&ParallelMemoryBudget>,
    ) -> ChunkOutcome {
        self.for_ranges
            .lock()
            .expect("range log is not poisoned")
            .push((start, end));
        ChunkOutcome {
            first_error: None,
            #[cfg(feature = "aarm-telemetry")]
            telemetry: None,
        }
    }

    fn run_for_each_chunk(
        &self,
        _symbol: mir::SymbolId,
        _base: usize,
        _values: &[ExecutionValue],
        _memory_budget: Option<&ParallelMemoryBudget>,
    ) -> ChunkOutcome {
        ChunkOutcome {
            first_error: None,
            #[cfg(feature = "aarm-telemetry")]
            telemetry: None,
        }
    }

    fn run_reduce_chunk(
        &self,
        _symbol: mir::SymbolId,
        _base: usize,
        identity: &ExecutionValue,
        values: &[ExecutionValue],
        _memory_budget: Option<&ParallelMemoryBudget>,
    ) -> ReduceChunkOutcome {
        let total = values
            .iter()
            .fold(int(identity), |sum, value| sum + int(value));
        ReduceChunkOutcome {
            result: Ok(ExecutionValue::Int(total)),
            #[cfg(feature = "aarm-telemetry")]
            telemetry: None,
        }
    }

    fn run_combine_step(
        &self,
        _symbol: mir::SymbolId,
        left: &ExecutionValue,
        right: &ExecutionValue,
        _memory_budget: Option<&ParallelMemoryBudget>,
    ) -> CombineOutcome {
        CombineOutcome {
            result: Ok(ExecutionValue::Int(int(left) + int(right))),
            #[cfg(feature = "aarm-telemetry")]
            telemetry: None,
        }
    }
}

/// Provider that accepts any module, resolves entries by name and hands out one
/// shared [`FakeProgram`], counting preparations.
#[derive(Default)]
pub struct FakeProvider {
    pub program: Arc<FakeProgram>,
    pub prepared: AtomicUsize,
    pub validated: AtomicUsize,
    pub fail_prepare: bool,
}

pub fn test_provider() -> Arc<dyn CodeProvider> {
    Arc::new(FakeProvider::default())
}

impl CodeProvider for FakeProvider {
    fn validate(&self, _module: &mir::Module) -> Result<(), BackendError> {
        self.validated.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn resolve_entry(
        &self,
        module: &mir::Module,
        function_name: &str,
    ) -> Result<mir::SymbolId, BackendError> {
        module
            .functions
            .iter()
            .find(|function| function.name == function_name)
            .map(|function| function.symbol)
            .ok_or_else(|| BackendError::new(format!("no function `{function_name}`")))
    }

    fn prepare(
        &self,
        _module: &mir::Module,
        _foreign_registry: &Arc<aster_runtime::ForeignRegistry>,
    ) -> Result<Arc<dyn ExecutableProgram>, BackendError> {
        self.prepared.fetch_add(1, Ordering::SeqCst);
        if self.fail_prepare {
            return Err(BackendError::new("provider cannot prepare code"));
        }
        Ok(Arc::clone(&self.program) as Arc<dyn ExecutableProgram>)
    }
}

/// Scheduler behavior proven against the fake provider, with no code
/// generator in the build: tasks, parallel chunks and reductions reach the
/// injected program, and the provider is asked for code once per pool.
mod through_the_provider {
    use super::*;
    use crate::task_runtime::TaskRuntime;
    use crate::worker_pool::ExecutionPool;

    fn module() -> Arc<mir::Module> {
        let module = aster_compiler::compile(
            "public int Alpha() { return 1; } public int Beta() { return 2; }",
        )
        .expect("source compiles")
        .mir;
        Arc::new(module)
    }

    fn symbol(module: &mir::Module, name: &str) -> mir::SymbolId {
        module
            .functions
            .iter()
            .find(|function| function.name == name)
            .unwrap_or_else(|| panic!("{name} is declared"))
            .symbol
    }

    fn as_dyn(provider: &Arc<FakeProvider>) -> Arc<dyn CodeProvider> {
        Arc::clone(provider) as Arc<dyn CodeProvider>
    }

    #[test]
    fn a_task_runs_through_the_injected_program() {
        let module = module();
        let alpha = symbol(&module, "Alpha");
        let provider = Arc::new(FakeProvider::default());
        let mut runtime = TaskRuntime::new(&as_dyn(&provider), &module, 2).expect("runtime starts");

        let handle = runtime.run(alpha).expect("task is accepted");
        let outcome = runtime.wait(handle).expect("task resolves");

        let id = i32::try_from(alpha.0).expect("small symbol id");
        assert_eq!(
            outcome,
            TaskOutcome::Completed(
                ExecutionValue::Int(id),
                aster_runtime::MemoryStats::default()
            )
        );
        assert_eq!(provider.validated.load(Ordering::SeqCst), 1);
        assert_eq!(provider.prepared.load(Ordering::SeqCst), 1);
        assert_eq!(provider.program.tasks_run.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn every_worker_of_a_pool_shares_one_program_prepared_once() {
        let module = module();
        let beta = symbol(&module, "Beta");
        let provider = Arc::new(FakeProvider::default());
        let pool = ExecutionPool::new(as_dyn(&provider), Arc::clone(&module), 4).expect("pool");

        let handles = (0..16)
            .map(|_| pool.submit(beta, false, None).expect("job is accepted"))
            .collect::<Vec<_>>();
        pool.ensure_workers(4).expect("workers start");
        for handle in handles {
            assert!(matches!(
                handle.join().expect("worker answers"),
                TaskOutcome::Completed(ExecutionValue::Int(_), _)
            ));
        }

        assert_eq!(provider.prepared.load(Ordering::SeqCst), 1);
        assert_eq!(provider.program.tasks_run.load(Ordering::SeqCst), 16);
        pool.shutdown();
    }

    #[test]
    fn an_adopted_program_replaces_provider_preparation() {
        let module = module();
        let alpha = symbol(&module, "Alpha");
        let provider = Arc::new(FakeProvider::default());
        let pool = ExecutionPool::new(as_dyn(&provider), module, 1).expect("pool");
        let adopted = Arc::new(FakeProgram::default());
        pool.adopt_program(Arc::clone(&adopted) as Arc<dyn ExecutableProgram>);

        pool.submit(alpha, false, None)
            .expect("job is accepted")
            .join()
            .expect("worker answers");

        assert_eq!(provider.prepared.load(Ordering::SeqCst), 0);
        assert_eq!(adopted.tasks_run.load(Ordering::SeqCst), 1);
        assert_eq!(provider.program.tasks_run.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_provider_that_cannot_prepare_code_yields_a_controlled_error() {
        let module = module();
        let alpha = symbol(&module, "Alpha");
        let provider = Arc::new(FakeProvider {
            fail_prepare: true,
            ..FakeProvider::default()
        });
        let pool = ExecutionPool::new(as_dyn(&provider), module, 2).expect("pool");

        let error = pool
            .submit(alpha, false, None)
            .err()
            .expect("submission fails without code");

        assert!(error.message().contains("cannot prepare code"));
    }

    #[test]
    fn entry_names_are_resolved_by_the_provider() {
        let module = module();
        let provider = Arc::new(FakeProvider::default());
        let pool = ExecutionPool::new(as_dyn(&provider), Arc::clone(&module), 1).expect("pool");

        assert_eq!(
            pool.resolve("Beta").expect("resolves"),
            symbol(&module, "Beta")
        );
        assert!(pool.resolve("Missing").is_err());
    }

    #[test]
    fn parallel_for_covers_the_range_through_the_program() {
        let module = module();
        let body = symbol(&module, "Alpha");
        let provider = Arc::new(FakeProvider::default());
        let mut runtime = TaskRuntime::new(&as_dyn(&provider), &module, 3).expect("runtime starts");

        runtime.parallel_for(0, 100, body).expect("range runs");

        let mut ranges = provider
            .program
            .for_ranges
            .lock()
            .expect("log is not poisoned")
            .clone();
        ranges.sort_unstable();
        assert_eq!(ranges.first().map(|range| range.0), Some(0));
        assert_eq!(ranges.last().map(|range| range.1), Some(100));
        assert!(ranges.windows(2).all(|pair| pair[0].1 == pair[1].0));
    }

    #[test]
    fn parallel_reduce_folds_chunks_through_the_program_in_order() {
        let module = module();
        let accumulate = symbol(&module, "Alpha");
        let combine = symbol(&module, "Beta");
        let provider = Arc::new(FakeProvider::default());
        let mut runtime = TaskRuntime::new(&as_dyn(&provider), &module, 4).expect("runtime starts");
        let values = (1..=1000).map(ExecutionValue::Int).collect::<Vec<_>>();

        let total = runtime
            .parallel_reduce(values, ExecutionValue::Int(0), accumulate, combine)
            .expect("reduction runs");

        assert_eq!(total, ExecutionValue::Int(500_500));
    }
}
