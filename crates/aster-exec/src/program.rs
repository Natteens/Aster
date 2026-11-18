//! The code-provider seam between the scheduler and a code generator.

use std::sync::Arc;

use aster_mir as mir;

use crate::worker_pool::{
    AsyncMemoryDomain, ChunkOutcome, CombineOutcome, ParallelMemoryBudget, ReduceChunkOutcome,
    TaskMemoryPolicy, TaskOutcome, TaskPayload,
};
use crate::{BackendError, ExecutionValue};

/// One finalized, executable program, shared read-only by the host thread,
/// the async driver and every worker of a pool.
///
/// Every method runs its entry in a brand new `ExecutionContext`, so calls
/// never share arenas. Implementations must be safe to call concurrently from
/// several threads (`Send + Sync`); a JIT implementation justifies that where
/// it implements the trait.
pub trait ExecutableProgram: Send + Sync {
    /// Run one plain `Task.Run` target. Governed jobs receive the frozen
    /// task-domain ceiling through `memory_policy`.
    fn run_task(
        &self,
        symbol: mir::SymbolId,
        payload: &TaskPayload,
        cancellation: Arc<aster_runtime::TaskCancellation>,
        collect_stats: bool,
        memory_policy: Option<&TaskMemoryPolicy>,
    ) -> (TaskOutcome, Option<aster_runtime::AarmMemoryTelemetry>);

    /// Run one async `MoveNext` step on the host with `task_runtime`
    /// registered on the fresh context; yields the machine status as an
    /// `ExecutionValue::Int`.
    fn invoke_move_next_with_cancellation(
        &self,
        symbol: mir::SymbolId,
        handle: i64,
        task_runtime: *mut (),
        cancellation: Arc<aster_runtime::TaskCancellation>,
        memory_domain: Option<&AsyncMemoryDomain>,
    ) -> (
        Result<(ExecutionValue, aster_runtime::MemoryStats), BackendError>,
        Option<aster_runtime::AarmMemoryTelemetry>,
    );

    /// Run `Parallel.For`'s `Body(int)` over `[start, end)`.
    fn run_for_chunk(
        &self,
        symbol: mir::SymbolId,
        start: i32,
        end: i32,
        memory_budget: Option<&ParallelMemoryBudget>,
    ) -> ChunkOutcome;

    /// Run `Parallel.ForEach`'s `Body(T)` over host-owned scalar copies whose
    /// first element is original array position `base`.
    fn run_for_each_chunk(
        &self,
        symbol: mir::SymbolId,
        base: usize,
        values: &[ExecutionValue],
        memory_budget: Option<&ParallelMemoryBudget>,
    ) -> ChunkOutcome;

    /// Run one `Parallel.Reduce` accumulation chunk starting from `identity`.
    fn run_reduce_chunk(
        &self,
        symbol: mir::SymbolId,
        base: usize,
        identity: &ExecutionValue,
        values: &[ExecutionValue],
        memory_budget: Option<&ParallelMemoryBudget>,
    ) -> ReduceChunkOutcome;

    /// Run one `Parallel.Reduce` combine step.
    fn run_combine_step(
        &self,
        symbol: mir::SymbolId,
        left: &ExecutionValue,
        right: &ExecutionValue,
        memory_budget: Option<&ParallelMemoryBudget>,
    ) -> CombineOutcome;
}

/// Source of executable code for a scheduler. Injected into every
/// [`crate::worker_pool::ExecutionPool`] and
/// [`crate::task_runtime::TaskRuntime`]; the scheduler never names a concrete
/// code generator.
pub trait CodeProvider: Send + Sync {
    /// Validate `module` for execution (called once per pool, before any
    /// worker starts).
    fn validate(&self, module: &mir::Module) -> Result<(), BackendError>;

    /// Resolve an entry-function name to the symbol the scheduler submits.
    fn resolve_entry(
        &self,
        module: &mir::Module,
        function_name: &str,
    ) -> Result<mir::SymbolId, BackendError>;

    /// Produce the pool's single executable program for `module` and
    /// `foreign_registry`. Called at most once per pool, on first use, when
    /// the caller did not already supply a program for the same module.
    fn prepare(
        &self,
        module: &mir::Module,
        foreign_registry: &Arc<aster_runtime::ForeignRegistry>,
    ) -> Result<Arc<dyn ExecutableProgram>, BackendError>;
}
