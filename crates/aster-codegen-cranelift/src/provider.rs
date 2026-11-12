//! Cranelift implementation of the `aster-exec` code-provider seam.
//!
//! The scheduler (`aster-exec`) never names the JIT: it asks an injected
//! [`CodeProvider`] to validate a module, resolve entry names and prepare one
//! finalized [`ExecutableProgram`]. This provider answers with the
//! Cranelift-compiled [`SharedProgram`], compiled once per pool exactly as the
//! scheduler did before the split.

use std::sync::Arc;

use aster_exec::worker_pool::{
    AsyncMemoryDomain, ChunkOutcome, CombineOutcome, ParallelMemoryBudget, ReduceChunkOutcome,
    TaskMemoryPolicy, TaskOutcome, TaskPayload,
};
use aster_exec::{CodeProvider, ExecutableProgram};

use super::execution::{PreparedProgram, SharedProgram};
use super::{BackendError, ExecutionValue, MemoryStats, mir, select_entry, validate_module};

/// Provider that compiles modules with the Cranelift JIT.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct CraneliftProvider;

/// The provider every scheduler in this crate is constructed with.
pub(super) fn provider() -> Arc<dyn CodeProvider> {
    Arc::new(CraneliftProvider)
}

impl CodeProvider for CraneliftProvider {
    fn validate(&self, module: &mir::Module) -> Result<(), BackendError> {
        validate_module(module)
    }

    fn resolve_entry(
        &self,
        module: &mir::Module,
        function_name: &str,
    ) -> Result<mir::SymbolId, BackendError> {
        select_entry(module, function_name).map(|function| function.symbol)
    }

    fn prepare(
        &self,
        module: &mir::Module,
        foreign_registry: &Arc<aster_runtime::ForeignRegistry>,
    ) -> Result<Arc<dyn ExecutableProgram>, BackendError> {
        let prepared = PreparedProgram::prepare_with_foreign_registry(module, foreign_registry)?;
        Ok(SharedProgram::new(prepared))
    }
}

impl ExecutableProgram for SharedProgram {
    fn run_task(
        &self,
        symbol: mir::SymbolId,
        payload: &TaskPayload,
        cancellation: Arc<aster_runtime::TaskCancellation>,
        collect_stats: bool,
        memory_policy: Option<&TaskMemoryPolicy>,
    ) -> (TaskOutcome, Option<aster_runtime::AarmMemoryTelemetry>) {
        PreparedProgram::run_task(
            self,
            symbol,
            payload,
            cancellation,
            collect_stats,
            memory_policy,
        )
    }

    fn invoke_move_next_with_cancellation(
        &self,
        symbol: mir::SymbolId,
        handle: i64,
        task_runtime: *mut (),
        cancellation: Arc<aster_runtime::TaskCancellation>,
        memory_domain: Option<&AsyncMemoryDomain>,
    ) -> (
        Result<(ExecutionValue, MemoryStats), BackendError>,
        Option<aster_runtime::AarmMemoryTelemetry>,
    ) {
        PreparedProgram::invoke_move_next_with_cancellation(
            self,
            symbol,
            handle,
            task_runtime,
            cancellation,
            memory_domain,
        )
    }

    fn run_for_chunk(
        &self,
        symbol: mir::SymbolId,
        start: i32,
        end: i32,
        memory_budget: Option<&ParallelMemoryBudget>,
    ) -> ChunkOutcome {
        PreparedProgram::run_for_chunk(self, symbol, start, end, memory_budget)
    }

    fn run_for_each_chunk(
        &self,
        symbol: mir::SymbolId,
        base: usize,
        values: &[ExecutionValue],
        memory_budget: Option<&ParallelMemoryBudget>,
    ) -> ChunkOutcome {
        PreparedProgram::run_for_each_chunk(self, symbol, base, values, memory_budget)
    }

    fn run_reduce_chunk(
        &self,
        symbol: mir::SymbolId,
        base: usize,
        identity: &ExecutionValue,
        values: &[ExecutionValue],
        memory_budget: Option<&ParallelMemoryBudget>,
    ) -> ReduceChunkOutcome {
        PreparedProgram::run_reduce_chunk(self, symbol, base, identity, values, memory_budget)
    }

    fn run_combine_step(
        &self,
        symbol: mir::SymbolId,
        left: &ExecutionValue,
        right: &ExecutionValue,
        memory_budget: Option<&ParallelMemoryBudget>,
    ) -> CombineOutcome {
        PreparedProgram::run_combine_step(self, symbol, left, right, memory_budget)
    }
}
