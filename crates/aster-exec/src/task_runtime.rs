//! Host-side ownership of every `Task<T>` handle for one top-level execution.
//!
//! ```text
//! TaskRuntime
//! ├── ExecutionPool     (crate::worker_pool; runs Task.Run and Parallel chunks)
//! ├── driver            (the pool's ExecutableProgram, which runs MoveNext steps)
//! ├── completion queue  (Arc<CompletionQueue>; workers signal awaited inners)
//! ├── plain task entries (keyed by a stable, opaque TaskHandleId)
//! └── async task entries (the two-state machines behind async functions)
//! ```
//!
//! A `Task<T>` value in generated code is never a pointer: it is a plain
//! `TaskHandleId` (an integer), looked up in one of the two tables here.
//!
//! ## Populations and borrow discipline
//!
//! One finalized program serves the whole execution: the program that runs
//! `Main` (in the code provider's crate) is shared as an `ExecutableProgram`, adopted by the pool,
//! run by every worker thread, and is also the `driver` here that runs every
//! `MoveNext`. `MoveNext` runs only on the host, only through `driver`; workers
//! only run `Task.Run` and `Parallel` chunks and never touch this runtime.
//!
//! The pump must never hold a Rust borrow of the runtime across a JIT call
//! that can call back into the runtime. [`TaskRuntime::pump`] therefore takes
//! the driver out with ownership for the duration of the pump and drives every
//! step through a raw `*mut Self`, reborrowing the runtime only in short,
//! non-overlapping windows between `MoveNext` invocations (see `pump_loop`).

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use crate::completion_queue::{CompletionQueue, CompletionToken};
use crate::worker_pool::{
    AsyncInnerPhaseGate, AsyncMemoryDomain, ChunkOutcome, ExecutionPool, JobKind,
    ParallelMemoryBudget, ReduceChunkOutcome, TaskControl, TaskHandle, TaskMemoryDomain,
    TaskOutcome, TaskPayload,
};
use crate::{BackendError, CodeProvider, ExecutableProgram, ExecutionValue, mir, scalar};
use aster_runtime::MemoryStats;

/// Host-side record of one deterministic governed Parallel budget plan.
#[cfg(feature = "aarm-telemetry")]
#[doc(hidden)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AarmParallelPlanningTelemetry {
    pub operation: &'static str,
    pub initial_governor_capacity_bytes: u64,
    pub available_headroom_bytes: u64,
    pub chunk_budgets_bytes: Vec<u64>,
}

/// Frozen plan and observational counters for one governed plain-task domain.
#[cfg(feature = "aarm-telemetry")]
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AarmTaskMemoryDomainTelemetry {
    pub initial_governor_capacity_bytes: u64,
    pub available_headroom_bytes: u64,
    pub main_retained_capacity_bytes: u64,
    pub main_future_growth_bytes: u64,
    pub main_local_capacity_ceiling_bytes: u64,
    pub task_context_ceiling_bytes: u64,
    pub task_memory_concurrency_limit: usize,
    pub task_submissions: u64,
    pub task_contexts_started: u64,
    pub task_contexts_completed: u64,
    pub task_context_memory_failures: u64,
    pub active_page_fast_path_allocations: u64,
    pub fresh_page_allocations: u64,
}

/// Frozen plan and observational counters for one governed async domain.
#[cfg(feature = "aarm-telemetry")]
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AarmAsyncMemoryDomainTelemetry {
    pub initial_governor_capacity_bytes: u64,
    pub available_headroom_bytes: u64,
    pub main_retained_capacity_bytes: u64,
    pub main_future_growth_bytes: u64,
    pub main_local_capacity_ceiling_bytes: u64,
    pub move_next_context_ceiling_bytes: u64,
    pub awaited_inner_context_ceiling_bytes: u64,
    pub temporal_borrowing_enabled: bool,
    pub phase_context_ceiling_bytes: u64,
    pub async_handles_created: u64,
    pub move_next_contexts_started: u64,
    pub move_next_contexts_completed: u64,
    pub inner_contexts_started: u64,
    pub inner_contexts_completed: u64,
    pub move_next_memory_failures: u64,
    pub inner_memory_failures: u64,
    pub move_next_fast_path_allocations: u64,
    pub move_next_fresh_page_allocations: u64,
    pub inner_fast_path_allocations: u64,
    pub inner_fresh_page_allocations: u64,
    pub phase_wait_events: u64,
    pub phase_borrowed_contexts: u64,
    pub peak_simultaneous_governed_async_contexts: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TaskMemoryDomainPlan {
    main_future_growth_bytes: u64,
    task_context_ceiling_bytes: u64,
    task_memory_concurrency_limit: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AsyncMemoryDomainPlan {
    main_future_growth: u64,
    fixed_move_next_ceiling: u64,
    fixed_inner_ceiling: u64,
    phase_context_ceiling: u64,
}

/// Opaque, stable identity for one task within its owning [`TaskRuntime`].
/// Never an arena pointer, never reused; generated code only ever holds the
/// bare integer, never dereferences it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TaskHandleId(u64);

impl TaskHandleId {
    pub fn to_bits(self) -> i64 {
        i64::from_ne_bytes(self.0.to_ne_bytes())
    }

    pub fn from_bits(bits: i64) -> Self {
        Self(u64::from_ne_bytes(bits.to_ne_bytes()))
    }
}

/// `MoveNext` status, matching `mir_lowering::async_machine`.
const PENDING: i32 = 0;
const COMPLETED: i32 = 1;

enum TaskEntry {
    Pending(TaskHandle),
    Resolved(Result<TaskOutcome, BackendError>),
}

/// One async state machine owned entirely on the host. Holds only scalar
/// values and opaque ids: no arena pointer, no `ExecutionContext`, and no
/// reference to any Aster object ever lives here.
struct AsyncTask {
    control: Arc<TaskControl>,
    move_next: mir::SymbolId,
    state: i32,
    /// Scalar frame, one typed value per slot persisted across the suspension.
    frame: Vec<Option<ExecutionValue>>,
    /// The awaited inner `Task.Run` handle and its completion token, live only
    /// while suspended at the `await`.
    inner: Option<TaskHandle>,
    inner_token: Option<CompletionToken>,
    /// Closed while its submitting `MoveNext` context is live. The worker waits
    /// before it creates a governed context or acquires any real capacity.
    inner_phase_gate: Option<Arc<AsyncInnerPhaseGate>>,
    /// The completed inner result, cached once its token is signalled.
    inner_result: Option<ExecutionValue>,
    /// The value published by `MoveNext` via `AsyncSetResult`, promoted to the
    /// final result only if the step completes without a controlled error.
    candidate: Option<ExecutionValue>,
    /// The final outcome once resolved; replayed on every later `Wait`.
    resolved: Option<Result<TaskOutcome, BackendError>>,
}

/// Owns one [`ExecutionPool`], the async driver, the completion queue, and
/// every task entry for the lifetime of one top-level execution. Dropping it
/// shuts the pool and completion queue down and releases every entry.
pub struct TaskRuntime {
    pool: ExecutionPool,
    /// The pool's shared program, used on the host for `MoveNext` steps.
    /// `Option` so the pump can take exclusive ownership across a step without
    /// holding a runtime borrow; taking it also rejects nested pumping.
    driver: Option<Arc<dyn ExecutableProgram>>,
    completion: Arc<CompletionQueue>,
    entries: HashMap<TaskHandleId, TaskEntry>,
    async_tasks: HashMap<TaskHandleId, AsyncTask>,
    token_to_async: HashMap<CompletionToken, TaskHandleId>,
    ready: VecDeque<TaskHandleId>,
    worker_count: usize,
    task_worker_count: usize,
    parallel_governor: Option<Arc<aster_runtime::MemoryGovernor>>,
    task_governor: Option<Arc<aster_runtime::MemoryGovernor>>,
    task_memory_domain: Option<Arc<TaskMemoryDomain>>,
    async_governor: Option<Arc<aster_runtime::MemoryGovernor>>,
    async_memory_domain: Option<Arc<AsyncMemoryDomain>>,
    async_handles_created: u64,
    #[cfg(feature = "aarm-telemetry")]
    task_domain_telemetry: Option<AarmTaskMemoryDomainTelemetry>,
    #[cfg(feature = "aarm-telemetry")]
    async_domain_telemetry: Option<AarmAsyncMemoryDomainTelemetry>,
    #[cfg(feature = "aarm-telemetry")]
    parallel_plans: Vec<AarmParallelPlanningTelemetry>,
    #[cfg(feature = "aarm-telemetry")]
    parallel_snapshots: Vec<aster_runtime::AarmMemoryTelemetry>,
    next_id: u64,
    next_token: CompletionToken,
}

/// Owns the async driver while a pump is active and restores it even if a host
/// panic unwinds through the pump. The raw pointer follows `pump`'s exclusive,
/// stack-bounded runtime contract.
struct DriverRestore {
    runtime: *mut TaskRuntime,
    driver: Option<Arc<dyn ExecutableProgram>>,
}

impl Drop for DriverRestore {
    fn drop(&mut self) {
        let Some(driver) = self.driver.take() else {
            return;
        };
        // SAFETY: `DriverRestore` is created only inside `TaskRuntime::pump`,
        // whose contract keeps `runtime` live and exclusively owned until this
        // guard is dropped, including during unwinding.
        #[allow(unsafe_code)]
        unsafe {
            (*self.runtime).driver = Some(driver);
        }
    }
}

impl TaskRuntime {
    // Production paths share their program through `with_program`; these
    // prepare their own and remain for tests and research entry points.
    #[cfg_attr(not(any(test, feature = "aarm-telemetry")), allow(dead_code))]
    pub fn new(
        provider: &Arc<dyn CodeProvider>,
        module: &Arc<mir::Module>,
        worker_count: usize,
    ) -> Result<Self, BackendError> {
        Self::new_with_foreign_registry(
            provider,
            module,
            worker_count,
            &Arc::new(aster_runtime::ForeignRegistry::new()),
        )
    }

    // Production paths share their program through `with_program`; these
    // prepare their own and remain for tests and research entry points.
    #[cfg_attr(not(any(test, feature = "aarm-telemetry")), allow(dead_code))]
    pub fn new_with_foreign_registry(
        provider: &Arc<dyn CodeProvider>,
        module: &Arc<mir::Module>,
        worker_count: usize,
        foreign_registry: &Arc<aster_runtime::ForeignRegistry>,
    ) -> Result<Self, BackendError> {
        Self::with_governors(
            provider,
            module,
            worker_count,
            None,
            None,
            None,
            foreign_registry,
            None,
        )
    }

    /// Runtime whose pool and async driver reuse `program`, the caller's
    /// already finalized program for the same module and foreign registry, so
    /// one execution compiles the module once.
    pub fn with_program(
        provider: &Arc<dyn CodeProvider>,
        module: &Arc<mir::Module>,
        worker_count: usize,
        foreign_registry: &Arc<aster_runtime::ForeignRegistry>,
        program: Arc<dyn ExecutableProgram>,
    ) -> Result<Self, BackendError> {
        Self::with_governors(
            provider,
            module,
            worker_count,
            None,
            None,
            None,
            foreign_registry,
            Some(program),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn with_governors(
        provider: &Arc<dyn CodeProvider>,
        module: &Arc<mir::Module>,
        worker_count: usize,
        parallel_governor: Option<Arc<aster_runtime::MemoryGovernor>>,
        task_governor: Option<Arc<aster_runtime::MemoryGovernor>>,
        async_governor: Option<Arc<aster_runtime::MemoryGovernor>>,
        foreign_registry: &Arc<aster_runtime::ForeignRegistry>,
        program: Option<Arc<dyn ExecutableProgram>>,
    ) -> Result<Self, BackendError> {
        let pool = ExecutionPool::new_with_foreign_registry(
            Arc::clone(provider),
            Arc::clone(module),
            worker_count,
            Arc::clone(foreign_registry),
        )?;
        if let Some(program) = program {
            pool.adopt_program(program);
        }
        let driver = module_uses_async(module)
            .then(|| pool.shared_program())
            .transpose()?;
        let task_worker_count = module_task_call_sites(module).min(worker_count).max(1);
        Ok(Self {
            pool,
            driver,
            completion: Arc::new(CompletionQueue::new()),
            entries: HashMap::new(),
            async_tasks: HashMap::new(),
            token_to_async: HashMap::new(),
            ready: VecDeque::new(),
            worker_count,
            task_worker_count,
            parallel_governor,
            task_governor,
            task_memory_domain: None,
            async_governor,
            async_memory_domain: None,
            async_handles_created: 0,
            #[cfg(feature = "aarm-telemetry")]
            task_domain_telemetry: None,
            #[cfg(feature = "aarm-telemetry")]
            async_domain_telemetry: None,
            #[cfg(feature = "aarm-telemetry")]
            parallel_plans: Vec::new(),
            #[cfg(feature = "aarm-telemetry")]
            parallel_snapshots: Vec::new(),
            next_id: 0,
            next_token: 0,
        })
    }

    #[cfg(feature = "aarm-telemetry")]
    pub fn with_memory_governor(
        provider: &Arc<dyn CodeProvider>,
        module: &Arc<mir::Module>,
        worker_count: usize,
        governor: Arc<aster_runtime::MemoryGovernor>,
    ) -> Result<Self, BackendError> {
        Self::with_governors(
            provider,
            module,
            worker_count,
            Some(governor),
            None,
            None,
            &Arc::new(aster_runtime::ForeignRegistry::new()),
            None,
        )
    }

    #[cfg(feature = "aarm-telemetry")]
    pub fn with_task_memory_governor(
        provider: &Arc<dyn CodeProvider>,
        module: &Arc<mir::Module>,
        worker_count: usize,
        governor: Arc<aster_runtime::MemoryGovernor>,
    ) -> Result<Self, BackendError> {
        Self::with_governors(
            provider,
            module,
            worker_count,
            None,
            Some(governor),
            None,
            &Arc::new(aster_runtime::ForeignRegistry::new()),
            None,
        )
    }

    #[cfg(feature = "aarm-telemetry")]
    pub fn with_async_memory_governor(
        provider: &Arc<dyn CodeProvider>,
        module: &Arc<mir::Module>,
        worker_count: usize,
        governor: Arc<aster_runtime::MemoryGovernor>,
    ) -> Result<Self, BackendError> {
        Self::with_governors(
            provider,
            module,
            worker_count,
            None,
            None,
            Some(governor),
            &Arc::new(aster_runtime::ForeignRegistry::new()),
            None,
        )
    }

    #[cfg(feature = "aarm-telemetry")]
    pub fn parallel_plans(&self) -> &[AarmParallelPlanningTelemetry] {
        &self.parallel_plans
    }

    #[cfg(feature = "aarm-telemetry")]
    pub fn parallel_snapshots(&self) -> &[aster_runtime::AarmMemoryTelemetry] {
        &self.parallel_snapshots
    }

    #[cfg(feature = "aarm-telemetry")]
    pub fn task_domain_telemetry(&self) -> Option<AarmTaskMemoryDomainTelemetry> {
        let mut telemetry = self.task_domain_telemetry?;
        let counters = self.task_memory_domain.as_ref()?.counters();
        telemetry.task_submissions = counters.submissions;
        telemetry.task_contexts_started = counters.contexts_started;
        telemetry.task_contexts_completed = counters.contexts_completed;
        telemetry.task_context_memory_failures = counters.memory_failures;
        telemetry.active_page_fast_path_allocations = counters.active_page_fast_path_allocations;
        telemetry.fresh_page_allocations = counters.fresh_page_allocations;
        Some(telemetry)
    }

    #[cfg(feature = "aarm-telemetry")]
    pub fn async_domain_telemetry(&self) -> Option<AarmAsyncMemoryDomainTelemetry> {
        let mut telemetry = self.async_domain_telemetry?;
        let counters = self.async_memory_domain.as_ref()?.counters();
        telemetry.async_handles_created = self.async_handles_created;
        telemetry.move_next_contexts_started = counters.move_next_contexts_started;
        telemetry.move_next_contexts_completed = counters.move_next_contexts_completed;
        telemetry.inner_contexts_started = counters.inner_contexts_started;
        telemetry.inner_contexts_completed = counters.inner_contexts_completed;
        telemetry.move_next_memory_failures = counters.move_next_memory_failures;
        telemetry.inner_memory_failures = counters.inner_memory_failures;
        telemetry.move_next_fast_path_allocations = counters.move_next_fast_path_allocations;
        telemetry.move_next_fresh_page_allocations = counters.move_next_fresh_page_allocations;
        telemetry.inner_fast_path_allocations = counters.inner_fast_path_allocations;
        telemetry.inner_fresh_page_allocations = counters.inner_fresh_page_allocations;
        telemetry.phase_wait_events = counters.phase_wait_events;
        telemetry.phase_borrowed_contexts = counters.phase_borrowed_contexts;
        telemetry.peak_simultaneous_governed_async_contexts = counters.peak_live_contexts;
        Some(telemetry)
    }

    #[cfg(feature = "aarm-telemetry")]
    pub fn quiesce_workers(&self) {
        self.pool.shutdown();
    }

    fn fresh_id(&mut self) -> Result<TaskHandleId, BackendError> {
        let id = TaskHandleId(self.next_id);
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| BackendError::new("task handle id space is exhausted"))?;
        Ok(id)
    }

    fn fresh_token(&mut self) -> Result<CompletionToken, BackendError> {
        let token = self.next_token;
        self.next_token = self
            .next_token
            .checked_add(1)
            .ok_or_else(|| BackendError::new("async completion token space is exhausted"))?;
        Ok(token)
    }

    // --- plain `Task.Run` / `Wait` (unchanged behavior) -------------------

    /// `Task.Run(function, arguments...)`: submit one plain task and return its handle.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn run(&mut self, symbol: mir::SymbolId) -> Result<TaskHandleId, BackendError> {
        self.submit_plain_task(symbol, TaskPayload::default(), None)
    }

    pub fn run_from_context(
        &mut self,
        symbol: mir::SymbolId,
        main_context: &mut aster_runtime::ExecutionContext,
    ) -> Result<TaskHandleId, BackendError> {
        let memory_domain = if self.task_governor.is_some() {
            Some(self.activate_task_memory_domain(main_context)?)
        } else {
            None
        };
        self.submit_plain_task(symbol, TaskPayload::default(), memory_domain)
    }

    pub fn run_with_payload_from_context(
        &mut self,
        symbol: mir::SymbolId,
        payload: TaskPayload,
        main_context: &mut aster_runtime::ExecutionContext,
    ) -> Result<TaskHandleId, BackendError> {
        let memory_domain = if self.task_governor.is_some() {
            Some(self.activate_task_memory_domain(main_context)?)
        } else {
            None
        };
        self.submit_plain_task(symbol, payload, memory_domain)
    }

    fn submit_plain_task(
        &mut self,
        symbol: mir::SymbolId,
        payload: TaskPayload,
        memory_domain: Option<Arc<TaskMemoryDomain>>,
    ) -> Result<TaskHandleId, BackendError> {
        self.pool.ensure_workers(self.pending_worker_demand())?;
        let id = self.fresh_id()?;
        let handle = match memory_domain {
            Some(domain) => self.pool.submit_governed_task(symbol, payload, domain)?,
            None => self
                .pool
                .submit_with_payload(symbol, payload, false, None)?,
        };
        self.entries.insert(id, TaskEntry::Pending(handle));
        Ok(id)
    }

    pub fn cancel(&self, id: TaskHandleId) -> Result<bool, BackendError> {
        if let Some(entry) = self.entries.get(&id) {
            return Ok(match entry {
                TaskEntry::Pending(handle) => handle.cancel(),
                TaskEntry::Resolved(_) => false,
            });
        }
        if let Some(task) = self.async_tasks.get(&id) {
            return Ok(task.resolved.is_none() && task.control.cancel());
        }
        Err(BackendError::new(
            "Task.Cancel received an unknown task handle",
        ))
    }

    fn activate_task_memory_domain(
        &mut self,
        main_context: &mut aster_runtime::ExecutionContext,
    ) -> Result<Arc<TaskMemoryDomain>, BackendError> {
        if let Some(domain) = &self.task_memory_domain {
            return Ok(Arc::clone(domain));
        }
        let governor = self
            .task_governor
            .as_ref()
            .map(Arc::clone)
            .ok_or_else(|| BackendError::new("task memory governor is unavailable"))?;
        if !main_context.is_governed_by(&governor) {
            return Err(BackendError::new(
                "main context does not use the Task.Run memory governor",
            ));
        }
        let snapshot = governor.telemetry();
        let main_retained = main_context
            .retained_arena_capacity_bytes()
            .ok_or_else(|| BackendError::new("main retained arena capacity overflow"))?;
        let main_retained_u64 = u64::try_from(main_retained)
            .map_err(|_| BackendError::new("main retained arena capacity exceeds telemetry"))?;
        if snapshot.current_capacity_bytes < main_retained_u64 {
            return Err(BackendError::new(
                "governed main capacity is not owned by the Task.Run memory governor",
            ));
        }
        let available_headroom_bytes = snapshot
            .hard_limit_bytes
            .checked_sub(snapshot.current_capacity_bytes)
            .ok_or_else(|| BackendError::new("memory governor capacity exceeds its hard limit"))?;
        let plan = plan_task_memory_domain(
            available_headroom_bytes,
            main_retained_u64,
            self.worker_count,
        )?;
        let main_future_growth = usize::try_from(plan.main_future_growth_bytes)
            .map_err(|_| BackendError::new("main Task.Run entitlement exceeds host limits"))?;
        let main_local_capacity_ceiling = main_context
            .freeze_task_domain_main_limit(main_future_growth)
            .ok_or_else(|| BackendError::new("main Task.Run entitlement exceeds host limits"))?;
        #[cfg(not(feature = "aarm-telemetry"))]
        let _ = main_local_capacity_ceiling;
        let task_context_ceiling = usize::try_from(plan.task_context_ceiling_bytes)
            .map_err(|_| BackendError::new("Task.Run entitlement exceeds host limits"))?;
        let domain = Arc::new(TaskMemoryDomain::new(
            governor,
            task_context_ceiling,
            plan.task_memory_concurrency_limit,
        ));
        #[cfg(feature = "aarm-telemetry")]
        {
            self.task_domain_telemetry = Some(AarmTaskMemoryDomainTelemetry {
                initial_governor_capacity_bytes: snapshot.current_capacity_bytes,
                available_headroom_bytes,
                main_retained_capacity_bytes: main_retained_u64,
                main_future_growth_bytes: plan.main_future_growth_bytes,
                main_local_capacity_ceiling_bytes: u64::try_from(main_local_capacity_ceiling)
                    .expect("execution capacity ceilings fit governor telemetry"),
                task_context_ceiling_bytes: plan.task_context_ceiling_bytes,
                task_memory_concurrency_limit: plan.task_memory_concurrency_limit,
                ..AarmTaskMemoryDomainTelemetry::default()
            });
        }
        self.task_memory_domain = Some(Arc::clone(&domain));
        Ok(domain)
    }

    fn activate_async_memory_domain(
        &mut self,
        main_context: &mut aster_runtime::ExecutionContext,
    ) -> Result<Arc<AsyncMemoryDomain>, BackendError> {
        if let Some(domain) = &self.async_memory_domain {
            return Ok(Arc::clone(domain));
        }
        let governor = self
            .async_governor
            .as_ref()
            .map(Arc::clone)
            .ok_or_else(|| BackendError::new("async memory governor is unavailable"))?;
        if !main_context.is_governed_by(&governor) {
            return Err(BackendError::new(
                "main context does not use the async memory governor",
            ));
        }
        let snapshot = governor.telemetry();
        let main_retained = main_context
            .retained_arena_capacity_bytes()
            .ok_or_else(|| BackendError::new("main retained arena capacity overflow"))?;
        let main_retained_u64 = u64::try_from(main_retained)
            .map_err(|_| BackendError::new("main retained arena capacity exceeds telemetry"))?;
        if snapshot.current_capacity_bytes < main_retained_u64 {
            return Err(BackendError::new(
                "governed main capacity is not owned by the async memory governor",
            ));
        }
        let available_headroom_bytes = snapshot
            .hard_limit_bytes
            .checked_sub(snapshot.current_capacity_bytes)
            .ok_or_else(|| BackendError::new("memory governor capacity exceeds its hard limit"))?;
        let plan = plan_async_memory_domain(available_headroom_bytes, main_retained_u64)?;
        let main_future_growth = usize::try_from(plan.main_future_growth)
            .map_err(|_| BackendError::new("main async entitlement exceeds host limits"))?;
        let main_local_capacity_ceiling = main_context
            .freeze_async_domain_main_limit(main_future_growth)
            .ok_or_else(|| BackendError::new("main async entitlement exceeds host limits"))?;
        #[cfg(not(feature = "aarm-telemetry"))]
        let _ = main_local_capacity_ceiling;
        let phase_limit = usize::try_from(plan.phase_context_ceiling)
            .map_err(|_| BackendError::new("async phase entitlement exceeds host limits"))?;
        let fixed_move_next_limit = usize::try_from(plan.fixed_move_next_ceiling)
            .map_err(|_| BackendError::new("async MoveNext entitlement exceeds host limits"))?;
        let fixed_inner_limit = usize::try_from(plan.fixed_inner_ceiling).map_err(|_| {
            BackendError::new("async awaited-inner entitlement exceeds host limits")
        })?;
        let domain = Arc::new(AsyncMemoryDomain::new(
            governor,
            phase_limit,
            fixed_move_next_limit,
            fixed_inner_limit,
        ));
        #[cfg(feature = "aarm-telemetry")]
        {
            self.async_domain_telemetry = Some(AarmAsyncMemoryDomainTelemetry {
                initial_governor_capacity_bytes: snapshot.current_capacity_bytes,
                available_headroom_bytes,
                main_retained_capacity_bytes: main_retained_u64,
                main_future_growth_bytes: plan.main_future_growth,
                main_local_capacity_ceiling_bytes: u64::try_from(main_local_capacity_ceiling)
                    .expect("execution capacity ceilings fit governor telemetry"),
                move_next_context_ceiling_bytes: plan.phase_context_ceiling,
                awaited_inner_context_ceiling_bytes: plan.phase_context_ceiling,
                temporal_borrowing_enabled: plan.phase_context_ceiling
                    > plan.fixed_move_next_ceiling
                    || plan.phase_context_ceiling > plan.fixed_inner_ceiling,
                phase_context_ceiling_bytes: plan.phase_context_ceiling,
                ..AarmAsyncMemoryDomainTelemetry::default()
            });
        }
        self.async_memory_domain = Some(Arc::clone(&domain));
        Ok(domain)
    }

    pub fn is_async_handle(&self, id: TaskHandleId) -> bool {
        self.async_tasks.contains_key(&id)
    }

    /// `task.Wait()` for a plain task: join once, cache, replay on every call.
    pub fn wait(&mut self, id: TaskHandleId) -> Result<TaskOutcome, BackendError> {
        let entry = self
            .entries
            .remove(&id)
            .ok_or_else(|| BackendError::new("Task<T>.Wait received an unknown task handle"))?;
        let result = match entry {
            TaskEntry::Resolved(result) => result,
            // Master plan §8.4: only a worker thread may run other domains' work
            // while it waits; the main thread blocks and runs nothing else.
            TaskEntry::Pending(handle) if self.pool.on_worker_thread() => {
                self.pool.join_helping(handle)
            }
            TaskEntry::Pending(handle) => handle.join(),
        };
        self.entries.insert(id, TaskEntry::Resolved(result.clone()));
        result
    }

    // --- async spawn and MoveNext intrinsics ------------------------------

    /// The async wrapper's `AsyncSpawn`: register a lazy state machine and
    /// return its handle. The body does not run until the task is pumped.
    pub fn async_spawn(
        &mut self,
        move_next: mir::SymbolId,
        slot_count: usize,
    ) -> Result<TaskHandleId, BackendError> {
        let id = self.fresh_id()?;
        self.async_tasks.insert(
            id,
            AsyncTask {
                control: Arc::new(TaskControl::new()),
                move_next,
                state: 0,
                frame: vec![None; slot_count],
                inner: None,
                inner_token: None,
                inner_phase_gate: None,
                inner_result: None,
                candidate: None,
                resolved: None,
            },
        );
        self.async_handles_created = self
            .async_handles_created
            .checked_add(1)
            .ok_or_else(|| BackendError::new("async handle telemetry overflow"))?;
        Ok(id)
    }

    /// Test hook: `(state, resolved, inner_submitted)` of an async task, or `None`
    /// for an unknown handle.
    #[doc(hidden)]
    pub fn async_task_probe(&self, id: TaskHandleId) -> Option<(i32, bool, bool)> {
        self.async_tasks.get(&id).map(|task| {
            (
                task.state,
                task.resolved.is_some(),
                task.inner.is_some() || task.inner_token.is_some(),
            )
        })
    }

    pub fn async_state(&self, id: TaskHandleId) -> Result<i32, BackendError> {
        let task = self
            .async_tasks
            .get(&id)
            .ok_or_else(|| BackendError::new("async state received an unknown task handle"))?;
        if !matches!(task.state, 0 | 1) {
            return Err(BackendError::new(format!(
                "async task has invalid state {}",
                task.state
            )));
        }
        Ok(task.state)
    }

    pub fn async_set_state(&mut self, id: TaskHandleId, state: i32) -> Result<(), BackendError> {
        let task = self.async_tasks.get_mut(&id).ok_or_else(|| {
            BackendError::new("async state update received an unknown task handle")
        })?;
        if task.resolved.is_some() || task.state != 0 || state != 1 {
            return Err(BackendError::new(format!(
                "invalid async state transition {} -> {state}",
                task.state
            )));
        }
        task.state = state;
        Ok(())
    }

    pub fn async_store_slot(
        &mut self,
        id: TaskHandleId,
        index: usize,
        value: ExecutionValue,
    ) -> Result<(), BackendError> {
        let task = self
            .async_tasks
            .get_mut(&id)
            .ok_or_else(|| BackendError::new("async slot store received an unknown task handle"))?;
        if task.resolved.is_some() || task.state != 0 {
            return Err(BackendError::new(
                "async slot store is only valid in state 0",
            ));
        }
        let slot = task.frame.get_mut(index).ok_or_else(|| {
            BackendError::new(format!("async frame slot {index} is out of range"))
        })?;
        if slot.is_some() {
            return Err(BackendError::new(format!(
                "async frame slot {index} was stored more than once"
            )));
        }
        *slot = Some(value);
        Ok(())
    }

    pub fn async_load_slot(&self, id: TaskHandleId, index: usize) -> Result<i64, BackendError> {
        let task = self
            .async_tasks
            .get(&id)
            .ok_or_else(|| BackendError::new("async slot load received an unknown task handle"))?;
        if task.resolved.is_some() || task.state != 1 {
            return Err(BackendError::new(
                "async slot load is only valid in state 1",
            ));
        }
        let value = task
            .frame
            .get(index)
            .ok_or_else(|| BackendError::new(format!("async frame slot {index} is out of range")))?
            .as_ref()
            .ok_or_else(|| {
                BackendError::new(format!("async frame slot {index} is uninitialized"))
            })?;
        Ok(scalar::to_bits(value))
    }

    /// `AsyncSpawnInner`: submit the awaited `Task.Run` target with a fresh
    /// completion token bound to this async task. Only awaited inners carry a
    /// token; plain tasks and Parallel chunks never do.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn async_spawn_inner(
        &mut self,
        id: TaskHandleId,
        inner: mir::SymbolId,
    ) -> Result<(), BackendError> {
        self.async_spawn_inner_with_payload(id, inner, TaskPayload::default())
    }

    pub fn async_spawn_inner_with_payload(
        &mut self,
        id: TaskHandleId,
        inner: mir::SymbolId,
        payload: TaskPayload,
    ) -> Result<(), BackendError> {
        let task = self.async_tasks.get(&id).ok_or_else(|| {
            BackendError::new("async inner spawn received an unknown task handle")
        })?;
        if task.resolved.is_some()
            || task.state != 0
            || task.inner.is_some()
            || task.inner_token.is_some()
        {
            return Err(BackendError::new(
                "async task attempted to spawn more than one awaited inner task",
            ));
        }
        let token = self.fresh_token()?;
        self.pool.ensure_workers(self.pending_worker_demand())?;
        let completion = (Arc::clone(&self.completion), token);
        let (handle, phase_gate) = match self.async_memory_domain.as_ref().map(Arc::clone) {
            Some(domain) => {
                let phase_gate = AsyncMemoryDomain::new_inner_phase_gate();
                let handle = self.pool.submit_governed_async_inner(
                    inner,
                    payload,
                    domain,
                    Arc::clone(&phase_gate),
                    completion,
                )?;
                (handle, Some(phase_gate))
            }
            None => (
                self.pool
                    .submit_with_payload(inner, payload, false, Some(completion))?,
                None,
            ),
        };
        let task = self
            .async_tasks
            .get_mut(&id)
            .ok_or_else(|| BackendError::new("async task disappeared during inner spawn"))?;
        task.inner = Some(handle);
        task.inner_token = Some(token);
        task.inner_phase_gate = phase_gate;
        self.token_to_async.insert(token, id);
        Ok(())
    }

    fn pending_worker_demand(&self) -> usize {
        let plain = self
            .entries
            .values()
            .filter(|entry| matches!(entry, TaskEntry::Pending(_)))
            .count();
        let async_inners = self
            .async_tasks
            .values()
            .filter(|task| task.inner.is_some())
            .count();
        let demand = self.task_worker_count.max(
            plain
                .saturating_add(async_inners)
                .saturating_add(1)
                .min(self.worker_count),
        );
        self.task_memory_domain
            .as_ref()
            .map_or(demand, |domain| demand.min(domain.concurrency_limit()))
    }

    pub fn async_await_result(&self, id: TaskHandleId) -> Result<i64, BackendError> {
        let task = self.async_tasks.get(&id).ok_or_else(|| {
            BackendError::new("async await result received an unknown task handle")
        })?;
        if task.resolved.is_some() || task.state != 1 {
            return Err(BackendError::new(
                "async await result is only available while resuming state 1",
            ));
        }
        task.inner_result
            .as_ref()
            .map(scalar::to_bits)
            .ok_or_else(|| BackendError::new("async awaited result is not ready"))
    }

    pub fn async_set_result(
        &mut self,
        id: TaskHandleId,
        value: ExecutionValue,
    ) -> Result<(), BackendError> {
        let task = self
            .async_tasks
            .get_mut(&id)
            .ok_or_else(|| BackendError::new("async result received an unknown task handle"))?;
        if task.resolved.is_some() || task.state != 1 {
            return Err(BackendError::new(
                "async result can only be published from state 1",
            ));
        }
        if task.candidate.is_some() {
            return Err(BackendError::new(
                "async result was published more than once",
            ));
        }
        task.candidate = Some(value);
        Ok(())
    }

    // --- the pump ---------------------------------------------------------

    /// Drive `target`'s async state machine to resolution on the host thread.
    ///
    /// Takes the driver out with ownership for the whole pump so no JIT step
    /// runs while a borrow of the runtime is held (a re-entrant pump, which
    /// valid Aster never produces, is turned into a controlled error rather
    /// than aliasing). Restores the driver on every exit path.
    ///
    /// # Safety
    /// `runtime` must be a live, exclusively-owned `TaskRuntime` for the whole
    /// call, with no other Rust reference to it held across this call (see the
    /// caller in `task_abi`).
    #[allow(unsafe_code)]
    pub unsafe fn pump(
        runtime: *mut Self,
        target: TaskHandleId,
    ) -> Result<TaskOutcome, BackendError> {
        // SAFETY: the caller's contract; this reborrow is dropped before any
        // driver step runs.
        let Some(driver) = (unsafe { (*runtime).driver.take() }) else {
            return Err(BackendError::new(
                "the async pump cannot be re-entered (nested pumping is not supported)",
            ));
        };
        let guard = DriverRestore {
            runtime,
            driver: Some(driver),
        };
        let outcome = match guard.driver.as_ref() {
            // SAFETY: same contract; each reborrow inside is short-lived and
            // never spans a driver step.
            Some(driver) => unsafe { Self::pump_loop(runtime, driver.as_ref(), target) },
            None => Err(BackendError::new("the async pump lost its driver")),
        };
        drop(guard);
        outcome
    }

    /// Freeze the governed async domain, if configured, before the first
    /// unresolved async step executes. The Main borrow ends before `pump`
    /// invokes JIT code.
    #[allow(unsafe_code)]
    pub(crate) unsafe fn pump_from_context(
        runtime: *mut Self,
        target: TaskHandleId,
        main_context: &mut aster_runtime::ExecutionContext,
    ) -> Result<TaskOutcome, BackendError> {
        // SAFETY: the caller provides the same exclusive runtime pointer as
        // `pump`; this short reborrow ends before any MoveNext invocation.
        let already_resolved = unsafe { (*runtime).resolved_outcome(target) };
        if let Some(outcome) = already_resolved {
            return outcome;
        }
        // SAFETY: same short-reborrow contract.
        if unsafe { (*runtime).async_governor.is_some() } {
            // SAFETY: no runtime borrow spans the later pump.
            unsafe { (*runtime).activate_async_memory_domain(main_context) }?;
        }
        // SAFETY: forwarded caller contract; the Main borrow is no longer used.
        unsafe { Self::pump(runtime, target) }
    }

    /// # Safety
    /// See [`Self::pump`]. Every `(*runtime)` reborrow below is a single
    /// statement and is dropped before `driver.invoke_move_next` runs, so no
    /// runtime borrow is ever live during a `MoveNext` step's ABI callbacks.
    #[allow(unsafe_code)]
    unsafe fn pump_loop(
        runtime: *mut Self,
        driver: &dyn ExecutableProgram,
        target: TaskHandleId,
    ) -> Result<TaskOutcome, BackendError> {
        // SAFETY: short reborrow, immediately dropped.
        let completion = Arc::clone(unsafe { &(*runtime).completion });
        // SAFETY: short reborrow, immediately dropped.
        unsafe { (*runtime).seed_ready(target) }?;
        loop {
            // SAFETY: short reborrow, immediately dropped.
            if let Some(outcome) = unsafe { (*runtime).resolved_outcome(target) } {
                // SAFETY: short reborrow, immediately dropped.
                unsafe { (*runtime).settle_governed_inner(target) };
                return outcome;
            }
            // SAFETY: each reborrow inside the loop body is short-lived and
            // never spans `invoke_move_next`.
            while let Some((move_next, handle, cancellation)) = unsafe { (*runtime).pop_ready() } {
                // SAFETY: short reborrow ending before the JIT invocation.
                let memory_domain =
                    unsafe { (*runtime).async_memory_domain.as_ref().map(Arc::clone) };
                let permit = memory_domain
                    .as_ref()
                    .map(AsyncMemoryDomain::acquire_move_next);
                // No runtime borrow is held here: the step's ABI callbacks
                // reborrow `runtime` freshly through the context pointer.
                let (step, telemetry) = driver.invoke_move_next_with_cancellation(
                    move_next,
                    handle.to_bits(),
                    runtime.cast::<()>(),
                    cancellation,
                    memory_domain.as_deref(),
                );
                if let Some(domain) = &memory_domain {
                    domain.record_move_next_outcome(&step, telemetry.as_ref());
                }
                drop(permit);
                // `invoke_move_next` owns and drops its fresh context before
                // returning. Only now may the queued inner construct its own
                // governed context and reuse the phase entitlement.
                unsafe { (*runtime).open_async_inner_phase(handle) };
                // SAFETY: short reborrow, immediately dropped.
                unsafe { (*runtime).apply_move_next(handle, step) };
            }
            // SAFETY: short reborrow, immediately dropped.
            if let Some(outcome) = unsafe { (*runtime).resolved_outcome(target) } {
                // SAFETY: short reborrow, immediately dropped.
                unsafe { (*runtime).settle_governed_inner(target) };
                return outcome;
            }
            if let Some(token) = completion.pop() {
                // SAFETY: short reborrow, immediately dropped.
                let completion = unsafe { (*runtime).on_completion(token) };
                if let Err(error) = completion {
                    // SAFETY: short reborrow, immediately dropped. Cache
                    // the failure so a repeated Wait cannot block after the
                    // bad token has already been consumed.
                    unsafe { (*runtime).resolve_error(target, error.clone()) };
                    // SAFETY: short reborrow, immediately dropped.
                    unsafe { (*runtime).settle_governed_inner(target) };
                    return Err(error);
                }
            } else {
                // SAFETY: short reborrow, immediately dropped.
                unsafe { (*runtime).settle_governed_inner(target) };
                return Err(BackendError::new(
                    "the async completion queue closed while a task was still pending",
                ));
            }
        }
    }

    /// Queue `target`'s first `MoveNext` step if it has not started yet.
    fn seed_ready(&mut self, target: TaskHandleId) -> Result<(), BackendError> {
        let task = self
            .async_tasks
            .get(&target)
            .ok_or_else(|| BackendError::new("async pump received an unknown task handle"))?;
        if task.resolved.is_none() && task.state == 0 && !self.ready.contains(&target) {
            self.ready.push_back(target);
        }
        Ok(())
    }

    fn pop_ready(
        &mut self,
    ) -> Option<(
        mir::SymbolId,
        TaskHandleId,
        Arc<aster_runtime::TaskCancellation>,
    )> {
        while let Some(handle) = self.ready.pop_front() {
            if let Some(task) = self.async_tasks.get_mut(&handle) {
                if task.resolved.is_some() {
                    continue;
                }
                if !task.control.begin_or_resume() {
                    task.resolved = Some(Ok(TaskOutcome::Failed(BackendError::cancellation())));
                    continue;
                }
                return Some((task.move_next, handle, task.control.cancellation()));
            }
        }
        None
    }

    /// The cached final outcome of `target`, if it has resolved.
    fn resolved_outcome(&self, target: TaskHandleId) -> Option<Result<TaskOutcome, BackendError>> {
        self.async_tasks.get(&target)?.resolved.clone()
    }

    fn resolve_error(&mut self, target: TaskHandleId, error: BackendError) {
        if let Some(task) = self.async_tasks.get_mut(&target) {
            let _ = task.control.finish(TaskOutcome::Failed(error.clone()));
            task.resolved = Some(Err(error));
        }
    }

    fn open_async_inner_phase(&self, handle: TaskHandleId) {
        if self.async_memory_domain.is_none() {
            return;
        }
        let Some(gate) = self
            .async_tasks
            .get(&handle)
            .and_then(|task| task.inner_phase_gate.as_ref())
        else {
            return;
        };
        AsyncMemoryDomain::open_inner_phase(gate);
    }

    /// On a governed failure after an inner was accepted, wait for the inner
    /// context to finish before returning to Main. Its token remains mapped so
    /// a late completion notification is harmless during a later serial pump.
    fn settle_governed_inner(&mut self, target: TaskHandleId) {
        if self.async_memory_domain.is_none() {
            return;
        }
        let inner = self
            .async_tasks
            .get_mut(&target)
            .and_then(|task| task.inner.take());
        if let Some(inner) = inner {
            let _ = inner.join();
        }
    }

    /// Apply one completed `MoveNext` step. A controlled error (including any
    /// `context.fail`, which makes `invoke` return `Err`) always wins over a
    /// candidate result, so a failure and a stored result never double-resolve.
    fn apply_move_next(
        &mut self,
        handle: TaskHandleId,
        step: Result<(ExecutionValue, MemoryStats), BackendError>,
    ) {
        let Some(task) = self.async_tasks.get_mut(&handle) else {
            return;
        };
        match step {
            Err(error) => {
                let _ = task.control.finish(TaskOutcome::Failed(error.clone()));
                task.resolved = Some(Err(error));
            }
            Ok((ExecutionValue::Int(PENDING), _)) => {
                if task.state != 1 || task.inner.is_none() || task.inner_token.is_none() {
                    let error = BackendError::new(
                        "an async task suspended without a registered awaited operation",
                    );
                    let _ = task.control.finish(TaskOutcome::Failed(error.clone()));
                    task.resolved = Some(Err(error));
                }
            }
            Ok((ExecutionValue::Int(COMPLETED), _)) => {
                let outcome = match (
                    task.state,
                    task.inner.is_none(),
                    task.inner_token.is_none(),
                    task.candidate.clone(),
                ) {
                    (1, true, true, Some(value)) => {
                        Ok(TaskOutcome::Completed(value, MemoryStats::default()))
                    }
                    _ => Err(BackendError::new(
                        "an async task completed from an invalid state or without producing a result",
                    )),
                };
                task.resolved = Some(match outcome {
                    Ok(outcome) => Ok(task.control.finish(outcome)),
                    Err(error) => {
                        let _ = task.control.finish(TaskOutcome::Failed(error.clone()));
                        Err(error)
                    }
                });
            }
            Ok(_) => {
                let error = BackendError::new("an async MoveNext step returned an invalid status");
                let _ = task.control.finish(TaskOutcome::Failed(error.clone()));
                task.resolved = Some(Err(error));
            }
        }
    }

    /// Handle one signalled completion token: receive the inner task's cached
    /// outcome and either fail the async task (inner error) or make it ready
    /// to resume (inner success). Unknown or stale tokens are controlled
    /// runtime errors so the pump cannot silently lose its only wakeup.
    fn on_completion(&mut self, token: CompletionToken) -> Result<(), BackendError> {
        let Some(handle) = self.token_to_async.remove(&token) else {
            return Err(BackendError::new(format!(
                "async completion queue produced unknown token {token}"
            )));
        };
        let Some(task) = self.async_tasks.get_mut(&handle) else {
            return Err(BackendError::new(
                "async completion token referenced a missing task",
            ));
        };
        if task.inner_token != Some(token) {
            return Err(BackendError::new(
                "async completion token did not match the suspended task",
            ));
        }
        task.inner_token = None;
        task.inner_phase_gate = None;
        if task.resolved.is_some() {
            return Ok(());
        }
        let outcome = match task.inner.take() {
            Some(inner) => inner.join(),
            None => Err(BackendError::new(
                "an awaited inner task was lost before completion",
            )),
        };
        match outcome {
            Ok(TaskOutcome::Completed(value, _)) => {
                task.inner_result = Some(value);
                self.ready.push_back(handle);
            }
            Ok(TaskOutcome::Failed(error)) | Err(error) => {
                let _ = task.control.finish(TaskOutcome::Failed(error.clone()));
                task.resolved = Some(Err(error));
            }
        }
        Ok(())
    }

    // --- Parallel ---------------------------------------------------------

    fn parallel_memory_budgets(
        &mut self,
        operation: &'static str,
        chunk_count: usize,
    ) -> Result<Vec<Option<ParallelMemoryBudget>>, BackendError> {
        let Some(governor) = self.parallel_governor.as_ref().map(Arc::clone) else {
            return Ok(vec![None; chunk_count]);
        };
        let snapshot = governor.telemetry();
        let available_headroom_bytes = snapshot
            .hard_limit_bytes
            .checked_sub(snapshot.current_capacity_bytes)
            .ok_or_else(|| BackendError::new("memory governor capacity exceeds its hard limit"))?;
        let shares = parallel_chunk_budgets(available_headroom_bytes, chunk_count)?;
        #[cfg(feature = "aarm-telemetry")]
        self.parallel_plans.push(AarmParallelPlanningTelemetry {
            operation,
            initial_governor_capacity_bytes: snapshot.current_capacity_bytes,
            available_headroom_bytes,
            chunk_budgets_bytes: shares.clone(),
        });
        #[cfg(not(feature = "aarm-telemetry"))]
        let _ = operation;
        shares
            .into_iter()
            .map(|share| {
                usize::try_from(share)
                    .map(|local_limit_bytes| {
                        Some(ParallelMemoryBudget {
                            governor: Arc::clone(&governor),
                            local_limit_bytes,
                        })
                    })
                    .map_err(|_| {
                        BackendError::new("Parallel memory budget exceeds the addressable range")
                    })
            })
            .collect()
    }

    #[allow(clippy::unused_self)]
    // Telemetry builds record through `self`; keep one API shape across builds.
    fn collect_parallel_chunks(
        &mut self,
        receivers: Vec<std::sync::mpsc::Receiver<ChunkOutcome>>,
    ) -> Result<(), BackendError> {
        let collected = collect_chunks_observed(receivers);
        #[cfg(feature = "aarm-telemetry")]
        self.parallel_snapshots.extend(collected.telemetry);
        collected.result
    }

    #[allow(clippy::unused_self)]
    // Telemetry builds record through `self`; keep one API shape across builds.
    fn collect_parallel_reduce_chunks(
        &mut self,
        receivers: Vec<std::sync::mpsc::Receiver<ReduceChunkOutcome>>,
    ) -> Result<Vec<ExecutionValue>, BackendError> {
        let collected = collect_reduce_chunks_observed(receivers);
        #[cfg(feature = "aarm-telemetry")]
        self.parallel_snapshots.extend(collected.telemetry);
        collected.result
    }

    /// `Parallel.For(start, end, Body)`: run `Body` over `[start, end)` in
    /// contiguous, worker-balanced chunks, block until all finish, and
    /// propagate the failure with the smallest logical index, if any.
    pub fn parallel_for(
        &mut self,
        start: i32,
        end: i32,
        body: mir::SymbolId,
    ) -> Result<(), BackendError> {
        if end < start {
            return Err(BackendError::new(format!(
                "Parallel.For range end {end} is before start {start}"
            )));
        }
        let total = i64::from(end) - i64::from(start);
        let total = usize::try_from(total).map_err(|_| {
            BackendError::new("Parallel.For range exceeds the addressable iteration count")
        })?;
        if total == 0 {
            return Ok(());
        }
        let boundaries = chunk_boundaries(total, self.worker_count);
        let budgets = self.parallel_memory_budgets("Parallel.For", boundaries.len())?;
        self.pool.ensure_workers(boundaries.len())?;
        let mut receivers = Vec::with_capacity(boundaries.len());
        let mut cursor = i64::from(start);
        for (length, memory_budget) in boundaries.into_iter().zip(budgets) {
            let chunk_start = i32::try_from(cursor)
                .map_err(|_| BackendError::new("Parallel.For chunk start exceeds `int`"))?;
            cursor =
                cursor
                    .checked_add(i64::try_from(length).map_err(|_| {
                        BackendError::new("Parallel.For chunk length exceeds `long`")
                    })?)
                    .ok_or_else(|| BackendError::new("Parallel.For chunk boundary overflow"))?;
            let chunk_end = i32::try_from(cursor)
                .map_err(|_| BackendError::new("Parallel.For chunk end exceeds `int`"))?;
            let submission = self.pool.submit_parallel(JobKind::ForChunk {
                symbol: body,
                start: chunk_start,
                end: chunk_end,
                memory_budget,
            });
            match submission {
                Ok(receiver) => receivers.push(receiver),
                Err(error) => {
                    let _ = self.collect_parallel_chunks(receivers);
                    return Err(error);
                }
            }
        }
        self.collect_parallel_chunks(receivers)
    }

    /// `Parallel.ForEach(values, Body)`: `values` are already the host-owned
    /// scalar copies (see `async_abi`); no array pointer ever reaches a worker.
    pub fn parallel_for_each(
        &mut self,
        values: Vec<ExecutionValue>,
        body: mir::SymbolId,
    ) -> Result<(), BackendError> {
        if values.is_empty() {
            return Ok(());
        }
        let boundaries = chunk_boundaries(values.len(), self.worker_count);
        let budgets = self.parallel_memory_budgets("Parallel.ForEach", boundaries.len())?;
        self.pool.ensure_workers(boundaries.len())?;
        let mut receivers = Vec::with_capacity(boundaries.len());
        let values = Arc::new(values);
        let mut base = 0;
        for (length, memory_budget) in boundaries.into_iter().zip(budgets) {
            let range = base..base + length;
            let submission = self.pool.submit_parallel(JobKind::ForEachChunk {
                symbol: body,
                base,
                values: Arc::clone(&values),
                range,
                memory_budget,
            });
            match submission {
                Ok(receiver) => receivers.push(receiver),
                Err(error) => {
                    let _ = self.collect_parallel_chunks(receivers);
                    return Err(error);
                }
            }
            base += length;
        }
        self.collect_parallel_chunks(receivers)
    }

    /// `Parallel.Reduce(values, identity, Accumulate, Combine)`: `values` are
    /// already the host-owned scalar copies (see `async_abi`); no array
    /// pointer ever reaches a worker. An empty array returns `identity`
    /// without running `Accumulate` or `Combine` at all. Chunk partials are
    /// collected in chunk-index order (never completion order) and folded
    /// left to right with `Combine`.
    pub fn parallel_reduce(
        &mut self,
        values: Vec<ExecutionValue>,
        identity: ExecutionValue,
        accumulate: mir::SymbolId,
        combine: mir::SymbolId,
    ) -> Result<ExecutionValue, BackendError> {
        if values.is_empty() {
            return Ok(identity);
        }
        // Chunking depends only on the input length (Q-CONC-4); the worker
        // count only affects scheduling.
        let boundaries = reduce_chunk_boundaries(values.len());
        let budgets =
            self.parallel_memory_budgets("Parallel.Reduce accumulate", boundaries.len())?;
        self.pool.ensure_workers(boundaries.len())?;
        let mut receivers = Vec::with_capacity(boundaries.len());
        let values = Arc::new(values);
        let mut base = 0;
        for (length, memory_budget) in boundaries.into_iter().zip(budgets) {
            let range = base..base + length;
            let submission = self.pool.submit_reduce_chunk(JobKind::ReduceChunk {
                symbol: accumulate,
                base,
                identity: identity.clone(),
                values: Arc::clone(&values),
                range,
                memory_budget,
            });
            match submission {
                Ok(receiver) => receivers.push(receiver),
                Err(error) => {
                    let _ = self.collect_parallel_reduce_chunks(receivers);
                    return Err(error);
                }
            }
            base += length;
        }
        let partials = self.collect_parallel_reduce_chunks(receivers)?;
        self.combine_partials(combine, partials)
    }

    /// Fold `partials` (one owned scalar per chunk, already ordered by chunk
    /// index) left to right with `Combine`, submitting at most
    /// `partials.len() - 1` combine jobs to this same pool, one at a time,
    /// waiting for each result before submitting the next. This never
    /// reenters the `PreparedProgram` that is running the call that started
    /// the reduction: every `Combine` call runs on a pool worker, exactly
    /// like an accumulation chunk.
    fn combine_partials(
        &mut self,
        combine: mir::SymbolId,
        mut partials: Vec<ExecutionValue>,
    ) -> Result<ExecutionValue, BackendError> {
        // `parallel_reduce` only calls this with at least one partial: an
        // empty `values` array returns early, and every chunk boundary is
        // non-empty, so `collect_reduce_chunks` never returns an empty `Vec`
        // once `values` was non-empty.
        let mut accumulator = partials.remove(0);
        for right in partials {
            let memory_budget = self
                .parallel_memory_budgets("Parallel.Reduce combine", 1)?
                .pop()
                .expect("one combine budget was planned");
            let receiver = self.pool.submit_combine_step(JobKind::CombineStep {
                symbol: combine,
                left: accumulator.clone(),
                right,
                memory_budget,
            })?;
            let outcome = receiver.recv().map_err(|_| {
                BackendError::new("a Parallel.Reduce combine worker disconnected before finishing")
            })?;
            #[cfg(feature = "aarm-telemetry")]
            if let Some(telemetry) = outcome.telemetry {
                self.parallel_snapshots.push(telemetry);
            }
            accumulator = outcome.result?;
        }
        Ok(accumulator)
    }
}

impl Drop for TaskRuntime {
    fn drop(&mut self) {
        // Close the completion queue so nothing can stay blocked on it, then
        // let the pool's own `Drop` join every worker. The driver and every
        // task entry drop with `self`, releasing all host-side frames.
        self.completion.close();
    }
}

/// Freeze one uniform plain-task entitlement and a deterministic maximum
/// number of simultaneously live governed task contexts. Task slots receive
/// useful minimum-page capacity before surplus is shared with Main.
fn plan_task_memory_domain(
    available_headroom_bytes: u64,
    main_retained_capacity_bytes: u64,
    worker_count: usize,
) -> Result<TaskMemoryDomainPlan, BackendError> {
    if worker_count == 0 {
        return Err(BackendError::new(
            "Task.Run memory planning requires at least one worker",
        ));
    }
    let minimum_page = u64::try_from(aster_runtime::ExecutionContext::AARM_MIN_PAGE_CAPACITY_BYTES)
        .expect("minimum arena page capacity fits u64");
    let local_limit =
        u64::try_from(aster_runtime::ExecutionContext::AARM_MAX_CONTEXT_CAPACITY_BYTES)
            .expect("execution allocation limit fits governor telemetry");
    let usable_page_slots = available_headroom_bytes / minimum_page;
    let concurrency_limit = if usable_page_slots == 0 {
        1
    } else {
        usize::try_from(usable_page_slots)
            .unwrap_or(usize::MAX)
            .min(worker_count)
    };
    let concurrency = u64::try_from(concurrency_limit)
        .map_err(|_| BackendError::new("Task.Run memory concurrency exceeds telemetry"))?;
    let initial_task_ceiling = if usable_page_slots == 0 {
        available_headroom_bytes.min(local_limit)
    } else {
        minimum_page
    };
    let initial_task_total = initial_task_ceiling
        .checked_mul(concurrency)
        .ok_or_else(|| BackendError::new("Task.Run memory plan overflow"))?;
    let remaining = available_headroom_bytes
        .checked_sub(initial_task_total)
        .ok_or_else(|| BackendError::new("Task.Run memory plan exceeds available headroom"))?;
    let participants = concurrency
        .checked_add(1)
        .ok_or_else(|| BackendError::new("Task.Run memory participant count overflow"))?;
    let uniform_extra =
        (remaining / participants).min(local_limit.saturating_sub(initial_task_ceiling));
    let task_context_ceiling_bytes = initial_task_ceiling
        .checked_add(uniform_extra)
        .ok_or_else(|| BackendError::new("Task.Run context entitlement overflow"))?;
    let task_total = task_context_ceiling_bytes
        .checked_mul(concurrency)
        .ok_or_else(|| BackendError::new("Task.Run aggregate entitlement overflow"))?;
    let main_local_headroom = local_limit.saturating_sub(main_retained_capacity_bytes);
    let main_future_growth_bytes = available_headroom_bytes
        .checked_sub(task_total)
        .ok_or_else(|| BackendError::new("Task.Run aggregate entitlement exceeds headroom"))?
        .min(main_local_headroom);

    Ok(TaskMemoryDomainPlan {
        main_future_growth_bytes,
        task_context_ceiling_bytes,
        task_memory_concurrency_limit: concurrency_limit,
    })
}

/// Freeze Main's post-Wait entitlement using the prior page-aware role split,
/// then permit each mutually-exclusive async phase to use the full captured
/// headroom up to the existing per-context safety ceiling.
fn plan_async_memory_domain(
    available_headroom_bytes: u64,
    main_retained_capacity_bytes: u64,
) -> Result<AsyncMemoryDomainPlan, BackendError> {
    let local_limit =
        u64::try_from(aster_runtime::ExecutionContext::AARM_MAX_CONTEXT_CAPACITY_BYTES)
            .expect("execution allocation limit fits governor telemetry");
    let main_limit = local_limit.saturating_sub(main_retained_capacity_bytes);
    let caps = [local_limit, local_limit, main_limit];
    let raw = parallel_chunk_budgets(available_headroom_bytes, caps.len())?;
    let mut budgets = [0_u64; 3];
    let mut excess = 0_u64;
    for (index, share) in raw.into_iter().enumerate() {
        budgets[index] = share.min(caps[index]);
        excess = excess
            .checked_add(share - budgets[index])
            .ok_or_else(|| BackendError::new("async memory plan overflow"))?;
    }
    for (budget, cap) in budgets.iter_mut().zip(caps) {
        let additional = excess.min(cap - *budget);
        *budget += additional;
        excess -= additional;
    }
    Ok(AsyncMemoryDomainPlan {
        fixed_move_next_ceiling: budgets[0],
        fixed_inner_ceiling: budgets[1],
        main_future_growth: budgets[2],
        phase_context_ceiling: available_headroom_bytes.min(local_limit),
    })
}

/// Split fixed available governed headroom by logical chunk index. When not
/// every chunk can receive one minimum arena page, earlier chunks receive
/// whole-page entitlement first and the next chunk receives the sub-page
/// tail. Once every chunk has one page, surplus bytes are split evenly and
/// earlier chunks receive one extra byte until the remainder is exhausted.
#[doc(hidden)]
pub fn parallel_chunk_budgets(
    available_headroom_bytes: u64,
    chunk_count: usize,
) -> Result<Vec<u64>, BackendError> {
    if chunk_count == 0 {
        return Ok(Vec::new());
    }
    let chunks = u64::try_from(chunk_count)
        .map_err(|_| BackendError::new("Parallel chunk count exceeds the addressable range"))?;
    let minimum_page = u64::try_from(aster_runtime::ExecutionContext::AARM_MIN_PAGE_CAPACITY_BYTES)
        .expect("minimum arena page capacity fits u64");
    let page_winners = (available_headroom_bytes / minimum_page).min(chunks);
    let guaranteed_bytes = page_winners
        .checked_mul(minimum_page)
        .expect("page winners are bounded by available headroom");
    let remaining = available_headroom_bytes - guaranteed_bytes;
    let winner_count =
        usize::try_from(page_winners).expect("page winner count is bounded by the chunk count");
    let mut budgets = Vec::new();
    budgets
        .try_reserve_exact(chunk_count)
        .map_err(|_| BackendError::new("Parallel memory plan exceeds host limits"))?;
    budgets.resize(chunk_count, 0);
    budgets[..winner_count].fill(minimum_page);
    if winner_count < chunk_count {
        budgets[winner_count] = remaining;
    } else {
        let base = remaining / chunks;
        let remainder = remaining % chunks;
        for (index, budget) in budgets.iter_mut().enumerate() {
            *budget +=
                base + u64::from(u64::try_from(index).expect("chunk index fits u64") < remainder);
        }
    }
    Ok(budgets)
}

/// Split `total` iterations into at most `worker_count` contiguous, balanced
/// chunks: never more chunks than iterations, never fewer than one, and no
/// per-iteration job explosion.
fn chunk_boundaries(total: usize, worker_count: usize) -> Vec<usize> {
    if total == 0 {
        return Vec::new();
    }
    let chunks = worker_count.max(1).min(total);
    let base = total / chunks;
    let remainder = total % chunks;
    (0..chunks)
        .map(|index| base + usize::from(index < remainder))
        .collect()
}

/// Maximum number of `Parallel.Reduce` chunks (Q-CONC-4).
pub const REDUCE_MAX_CHUNKS: usize = 64;

/// `Parallel.Reduce` chunk lengths. They depend only on `length`, never on the
/// worker count: `chunks = min(length, 64)` and chunk `i` covers
/// `[i * length / chunks, (i + 1) * length / chunks)`. Every chunk is
/// non-empty, and an empty input has no chunks.
pub fn reduce_chunk_boundaries(length: usize) -> Vec<usize> {
    let chunks = length.min(REDUCE_MAX_CHUNKS);
    // `u128` arithmetic cannot overflow for any `usize` length.
    let edge = |index: usize| -> usize {
        let value = (index as u128) * (length as u128) / (chunks as u128);
        usize::try_from(value).expect("a chunk edge never exceeds the length")
    };
    (0..chunks).map(|i| edge(i + 1) - edge(i)).collect()
}

/// Wait for every accepted chunk and propagate the failure with the smallest
/// logical index, independent of completion order.
#[cfg(test)]
fn collect_chunks(
    receivers: Vec<std::sync::mpsc::Receiver<ChunkOutcome>>,
) -> Result<(), BackendError> {
    collect_chunks_observed(receivers).result
}

struct CollectedChunks {
    result: Result<(), BackendError>,
    #[cfg(feature = "aarm-telemetry")]
    telemetry: Vec<aster_runtime::AarmMemoryTelemetry>,
}

fn collect_chunks_observed(
    receivers: Vec<std::sync::mpsc::Receiver<ChunkOutcome>>,
) -> CollectedChunks {
    let mut first_error: Option<(i64, BackendError)> = None;
    let mut disconnected = false;
    #[cfg(feature = "aarm-telemetry")]
    let mut telemetry = Vec::with_capacity(receivers.len());
    for receiver in receivers {
        let Ok(outcome) = receiver.recv() else {
            disconnected = true;
            continue;
        };
        #[cfg(feature = "aarm-telemetry")]
        if let Some(snapshot) = outcome.telemetry {
            telemetry.push(snapshot);
        }
        if let Some((index, error)) = outcome.first_error
            && first_error
                .as_ref()
                .is_none_or(|(current, _)| index < *current)
        {
            first_error = Some((index, error));
        }
    }
    let result = match first_error {
        Some((_, error)) => Err(error),
        None if disconnected => Err(BackendError::new(
            "a Parallel worker disconnected before finishing",
        )),
        None => Ok(()),
    };
    CollectedChunks {
        result,
        #[cfg(feature = "aarm-telemetry")]
        telemetry,
    }
}

/// Wait for every accepted `Parallel.Reduce` chunk (draining every receiver
/// regardless of an earlier failure) and either propagate the failure with
/// the smallest logical array position, independent of completion order, or
/// return every chunk's owned partial result in chunk-index order.
#[cfg(test)]
fn collect_reduce_chunks(
    receivers: Vec<std::sync::mpsc::Receiver<ReduceChunkOutcome>>,
) -> Result<Vec<ExecutionValue>, BackendError> {
    collect_reduce_chunks_observed(receivers).result
}

struct CollectedReduceChunks {
    result: Result<Vec<ExecutionValue>, BackendError>,
    #[cfg(feature = "aarm-telemetry")]
    telemetry: Vec<aster_runtime::AarmMemoryTelemetry>,
}

fn collect_reduce_chunks_observed(
    receivers: Vec<std::sync::mpsc::Receiver<ReduceChunkOutcome>>,
) -> CollectedReduceChunks {
    let mut first_error: Option<(i64, BackendError)> = None;
    let mut partials = Vec::with_capacity(receivers.len());
    let mut disconnected = false;
    #[cfg(feature = "aarm-telemetry")]
    let mut telemetry = Vec::with_capacity(receivers.len());
    for receiver in receivers {
        let Ok(outcome) = receiver.recv() else {
            disconnected = true;
            continue;
        };
        #[cfg(feature = "aarm-telemetry")]
        if let Some(snapshot) = outcome.telemetry {
            telemetry.push(snapshot);
        }
        match outcome.result {
            Ok(value) => partials.push(value),
            Err((index, error)) => {
                if first_error
                    .as_ref()
                    .is_none_or(|(current, _)| index < *current)
                {
                    first_error = Some((index, error));
                }
            }
        }
    }
    let result = match first_error {
        Some((_, error)) => Err(error),
        None if disconnected => Err(BackendError::new(
            "a Parallel.Reduce worker disconnected before finishing",
        )),
        None => Ok(partials),
    };
    CollectedReduceChunks {
        result,
        #[cfg(feature = "aarm-telemetry")]
        telemetry,
    }
}

/// Whether `module` uses any concurrency intrinsic and therefore needs a
/// [`TaskRuntime`] for the duration of a top-level execution. The only place
/// this crate inspects MIR to decide that; a fully sequential module never
/// creates a pool, driver, or completion queue.
pub fn module_uses_tasks(module: &mir::Module) -> bool {
    module.functions.iter().any(|function| {
        function.blocks.iter().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction,
                    mir::Instruction::CallIntrinsic {
                        intrinsic: mir::Intrinsic::TaskRun
                            | mir::Intrinsic::TaskWait
                            | mir::Intrinsic::TaskWaitAll
                            | mir::Intrinsic::TaskCancel
                            | mir::Intrinsic::AsyncSpawn
                            | mir::Intrinsic::AsyncSpawnInner
                            | mir::Intrinsic::ParallelFor
                            | mir::Intrinsic::ParallelForEach
                            | mir::Intrinsic::ParallelReduce,
                        ..
                    }
                )
            })
        })
    })
}

pub fn module_uses_async(module: &mir::Module) -> bool {
    module.functions.iter().any(|function| {
        function.blocks.iter().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction,
                    mir::Instruction::CallIntrinsic {
                        intrinsic: mir::Intrinsic::AsyncSpawn | mir::Intrinsic::AsyncSpawnInner,
                        ..
                    }
                )
            })
        })
    })
}

#[cfg(feature = "aarm-telemetry")]
pub fn module_uses_plain_task_run(module: &mir::Module) -> bool {
    module.functions.iter().any(|function| {
        function.blocks.iter().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction,
                    mir::Instruction::CallIntrinsic {
                        intrinsic: mir::Intrinsic::TaskRun,
                        ..
                    }
                )
            })
        })
    })
}

#[cfg(feature = "aarm-telemetry")]
pub fn module_uses_parallel(module: &mir::Module) -> bool {
    module.functions.iter().any(|function| {
        function.blocks.iter().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction,
                    mir::Instruction::CallIntrinsic {
                        intrinsic: mir::Intrinsic::ParallelFor
                            | mir::Intrinsic::ParallelForEach
                            | mir::Intrinsic::ParallelReduce,
                        ..
                    }
                )
            })
        })
    })
}

fn module_task_call_sites(module: &mir::Module) -> usize {
    module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .filter(|instruction| {
            matches!(
                instruction,
                mir::Instruction::CallIntrinsic {
                    intrinsic: mir::Intrinsic::TaskRun | mir::Intrinsic::AsyncSpawnInner,
                    ..
                }
            )
        })
        .count()
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;
    use crate::test_support::test_provider;

    fn compile(source: &str) -> mir::Module {
        aster_compiler::compile(source)
            .expect("source compiles")
            .mir
    }

    #[test]
    fn task_worker_demand_tracks_live_submissions_not_static_call_sites() {
        let module = Arc::new(compile(
            "public int Work() { return 1; } public Task<int> Start() { return Task.Run(Work); } public int Main() { Task<int> a = Start(); Task<int> b = Start(); return a.Wait() + b.Wait(); }",
        ));
        let work = module
            .functions
            .iter()
            .find(|function| function.name == "Work")
            .expect("Work exists")
            .symbol;
        let mut runtime = TaskRuntime::new(&test_provider(), &module, 4).expect("runtime starts");

        assert_eq!(runtime.pending_worker_demand(), 1);
        let first = runtime.run(work).expect("first task starts");
        assert_eq!(runtime.pending_worker_demand(), 2);
        let second = runtime.run(work).expect("second task starts");
        assert_eq!(runtime.pending_worker_demand(), 3);

        runtime.wait(first).expect("first task completes");
        assert_eq!(runtime.pending_worker_demand(), 2);
        runtime.wait(second).expect("second task completes");
        assert_eq!(runtime.pending_worker_demand(), 1);
    }

    #[test]
    fn waiting_on_an_unknown_id_is_a_controlled_error() {
        let module = compile("public int Compute() { return 1; }");
        let mut runtime = TaskRuntime::new(&test_provider(), &Arc::new(module), 1)
            .expect("runtime starts with no tasks yet");
        let bogus = TaskHandleId(u64::MAX);
        assert!(runtime.wait(bogus).is_err());
    }

    #[test]
    fn many_plain_task_run_waits_never_produce_a_completion_token() {
        let module = compile("public int Compute() { return 1; }");
        let symbol = module
            .functions
            .iter()
            .find(|function| function.name == "Compute")
            .expect("Compute is declared")
            .symbol;
        let mut runtime =
            TaskRuntime::new(&test_provider(), &Arc::new(module), 2).expect("runtime starts");
        for _ in 0..50 {
            let id = runtime.run(symbol).expect("task is accepted");
            runtime.wait(id).expect("plain wait succeeds");
        }
        assert_eq!(
            runtime.next_token, 0,
            "no plain task ever mints a completion token"
        );
        assert!(runtime.token_to_async.is_empty());
    }

    #[test]
    fn task_handle_exhaustion_is_controlled_instead_of_wrapping() {
        let module = compile("public int Compute() { return 1; }");
        let symbol = module.functions[0].symbol;
        let mut runtime =
            TaskRuntime::new(&test_provider(), &Arc::new(module), 1).expect("runtime starts");
        runtime.next_id = u64::MAX;

        let error = runtime.run(symbol).expect_err("id exhaustion must fail");
        assert!(error.message().contains("id space is exhausted"));
        assert!(runtime.entries.is_empty());
    }

    #[test]
    fn async_driver_reuses_the_callers_program() {
        let module = Arc::new(compile(
            "public int Compute() { return 1; } public async Task<int> Run() { return await Task.Run(Compute); }",
        ));
        let program: Arc<dyn ExecutableProgram> =
            Arc::new(crate::test_support::FakeProgram::default());
        let runtime = TaskRuntime::with_program(
            &test_provider(),
            &module,
            2,
            &Arc::new(aster_runtime::ForeignRegistry::new()),
            Arc::clone(&program),
        )
        .expect("runtime starts");
        let driver = runtime.driver.as_ref().expect("async module has a driver");
        assert!(Arc::ptr_eq(driver, &program));
    }

    #[test]
    fn module_uses_tasks_is_false_without_concurrency() {
        let module = compile("public int Run() { return 1; }");
        assert!(!module_uses_tasks(&module));
    }

    #[test]
    fn module_uses_tasks_is_true_with_task_run() {
        let module = compile(
            "public int Compute() { return 1; } public int Main() { return Task.Run(Compute).Wait(); }",
        );
        assert!(module_uses_tasks(&module));
    }

    #[test]
    fn independent_static_task_submissions_prepare_together() {
        let module = Arc::new(compile(
            "public int Work() { return 1; } public int Main() { Task<int> a = Task.Run(Work); Task<int> b = Task.Run(Work); return a.Wait() + b.Wait(); }",
        ));
        let runtime = TaskRuntime::new(&test_provider(), &module, 4).expect("runtime starts");
        assert_eq!(runtime.task_worker_count, 2);
        assert_eq!(runtime.pending_worker_demand(), 2);
    }

    #[test]
    fn async_driver_is_only_required_by_async_modules() {
        let task = compile(
            "public int Work() { return 42; } public int Run() { Task<int> task = Task.Run(Work); return task.Wait(); }",
        );
        assert!(!module_uses_async(&task));

        let async_module = compile(
            "public int Work() { return 42; } public async Task<int> Run() { int value = await Task.Run(Work); return value; }",
        );
        assert!(module_uses_async(&async_module));
    }

    #[test]
    fn a_move_next_step_returning_an_invalid_status_is_a_controlled_error() {
        // `MoveNext` only ever legitimately returns `0` (pending) or `1`
        // (completed); anything else can only come from adulterated MIR or a
        // corrupted handle, and must resolve the task with a controlled
        // error rather than being silently accepted as one of the two valid
        // states.
        let module = compile("public int Compute() { return 1; }");
        let move_next = module.functions[0].symbol;
        let mut runtime =
            TaskRuntime::new(&test_provider(), &Arc::new(module), 1).expect("runtime starts");
        let handle = runtime
            .async_spawn(move_next, 0)
            .expect("async task registers");

        runtime.apply_move_next(
            handle,
            Ok((ExecutionValue::Int(42), MemoryStats::default())),
        );

        let task = runtime
            .async_tasks
            .get(&handle)
            .expect("the task is still registered");
        let resolved = task
            .resolved
            .clone()
            .expect("an invalid status must resolve the task, not leave it pending");
        let error = resolved.expect_err("an invalid status must be a controlled error");
        assert!(error.message().contains("invalid status"));
    }

    #[test]
    fn a_suspended_async_task_keeps_its_own_cancellation_control_until_resume() {
        let module = compile("public int Compute() { return 1; }");
        let move_next = module.functions[0].symbol;
        let mut runtime =
            TaskRuntime::new(&test_provider(), &Arc::new(module), 1).expect("runtime starts");
        let id = runtime
            .async_spawn(move_next, 0)
            .expect("async task registers");
        let control = Arc::clone(&runtime.async_tasks[&id].control);
        let cancellation = control.cancellation();

        assert!(control.begin_or_resume(), "the first async phase starts");
        runtime.async_tasks.get_mut(&id).expect("task exists").state = 1;
        assert!(runtime.cancel(id).expect("known async handle cancels"));
        assert!(cancellation.is_requested());

        runtime.ready.push_back(id);
        assert!(
            runtime.pop_ready().is_none(),
            "a cancelled suspended task must not resume"
        );
        let outcome = runtime
            .resolved_outcome(id)
            .expect("resume attempt resolves cancellation")
            .expect("cancellation is a task outcome");
        assert!(matches!(
            outcome,
            TaskOutcome::Failed(error) if error.is_cancellation()
        ));
        assert!(!runtime.cancel(id).expect("terminal handle remains known"));
        assert_eq!(
            Arc::strong_count(&control),
            2,
            "only the task and test own control"
        );
    }

    #[test]
    fn completion_token_exhaustion_is_controlled_instead_of_wrapping() {
        let module = compile("public int Compute() { return 1; }");
        let symbol = module.functions[0].symbol;
        let mut runtime =
            TaskRuntime::new(&test_provider(), &Arc::new(module), 1).expect("runtime starts");
        let outer = runtime
            .async_spawn(symbol, 0)
            .expect("outer async handle is allocated");
        runtime.next_token = u64::MAX;

        let error = runtime
            .async_spawn_inner(outer, symbol)
            .expect_err("token exhaustion must fail");
        assert!(error.message().contains("token space is exhausted"));
        assert!(runtime.token_to_async.is_empty());
    }

    #[test]
    fn invalid_async_slots_and_state_transitions_are_controlled() {
        let module = compile("public int Compute() { return 1; }");
        let symbol = module.functions[0].symbol;
        let mut runtime =
            TaskRuntime::new(&test_provider(), &Arc::new(module), 1).expect("runtime starts");
        let outer = runtime
            .async_spawn(symbol, 0)
            .expect("outer async handle is allocated");

        assert!(
            runtime
                .async_store_slot(outer, 0, ExecutionValue::Int(1))
                .is_err()
        );
        assert!(runtime.async_load_slot(outer, 0).is_err());
        assert!(runtime.async_set_state(outer, 7).is_err());
        assert!(runtime.on_completion(99).is_err());
    }

    #[test]
    fn driver_guard_restores_the_driver_during_unwind() {
        let module = compile(
            "public int Compute() { return 1; } public async Task<int> Run() { return await Task.Run(Compute); }",
        );
        let mut runtime =
            TaskRuntime::new(&test_provider(), &Arc::new(module), 1).expect("runtime starts");
        let runtime_pointer = std::ptr::from_mut(&mut runtime);
        let driver = runtime.driver.take().expect("driver is present");

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = DriverRestore {
                runtime: runtime_pointer,
                driver: Some(driver),
            };
            panic!("simulated host panic while pumping");
        }));

        assert!(result.is_err());
        assert!(runtime.driver.is_some());
    }

    #[test]
    fn reduce_chunk_boundaries_depend_only_on_length() {
        assert_eq!(reduce_chunk_boundaries(0), Vec::<usize>::new());
        assert_eq!(reduce_chunk_boundaries(1), vec![1]);
        assert_eq!(reduce_chunk_boundaries(63), vec![1; 63]);
        assert_eq!(reduce_chunk_boundaries(64), vec![1; 64]);
        let sixty_five = reduce_chunk_boundaries(65);
        assert_eq!(sixty_five.len(), 64);
        assert_eq!(sixty_five.iter().sum::<usize>(), 65);
        assert_eq!(sixty_five.iter().filter(|&&n| n == 2).count(), 1);
        for length in [1usize, 2, 63, 64, 65, 100, 1000, 4097, 100_003] {
            let chunks = length.min(64);
            let lengths = reduce_chunk_boundaries(length);
            assert_eq!(lengths.len(), chunks);
            let mut start = 0usize;
            for (index, &len) in lengths.iter().enumerate() {
                assert!(len > 0);
                assert_eq!(
                    start,
                    index * length / chunks,
                    "length {length} chunk {index}"
                );
                start += len;
                assert_eq!(start, (index + 1) * length / chunks);
            }
            assert_eq!(start, length);
        }
        // Near the top of the 32-bit element range, and the full usize range,
        // `i * length` must not overflow.
        for length in [i32::MAX as usize, usize::MAX] {
            let lengths = reduce_chunk_boundaries(length);
            assert_eq!(lengths.len(), 64);
            let mut total = 0usize;
            for len in &lengths {
                total = total.checked_add(*len).expect("lengths sum to the input");
            }
            assert_eq!(total, length);
            let (min, max) = (lengths.iter().min().unwrap(), lengths.iter().max().unwrap());
            assert!(max - min <= 1);
        }
    }

    #[test]
    fn chunk_boundaries_never_exceed_workers_or_iterations() {
        assert_eq!(chunk_boundaries(0, 4), Vec::<usize>::new());
        assert_eq!(chunk_boundaries(1, 4), vec![1]);
        assert_eq!(chunk_boundaries(10, 4), vec![3, 3, 2, 2]);
        assert_eq!(chunk_boundaries(3, 8), vec![1, 1, 1]);
        assert_eq!(chunk_boundaries(1000, 1).iter().sum::<usize>(), 1000);
    }

    #[test]
    fn parallel_memory_partitions_are_page_aware_and_exact() {
        let page = u64::try_from(aster_runtime::ExecutionContext::AARM_MIN_PAGE_CAPACITY_BYTES)
            .expect("minimum page capacity fits u64");
        assert_eq!(parallel_chunk_budgets(0, 0).unwrap(), Vec::<u64>::new());
        assert_eq!(parallel_chunk_budgets(0, 4).unwrap(), [0, 0, 0, 0]);
        assert_eq!(parallel_chunk_budgets(10, 1).unwrap(), [10]);
        assert_eq!(
            parallel_chunk_budgets(page - 1, 3).unwrap(),
            [page - 1, 0, 0]
        );
        assert_eq!(parallel_chunk_budgets(page, 3).unwrap(), [page, 0, 0]);
        assert_eq!(parallel_chunk_budgets(page + 1, 3).unwrap(), [page, 1, 0]);
        assert_eq!(
            parallel_chunk_budgets(2 * page - 1, 3).unwrap(),
            [page, page - 1, 0]
        );
        assert_eq!(
            parallel_chunk_budgets(2 * page, 3).unwrap(),
            [page, page, 0]
        );
        assert_eq!(
            parallel_chunk_budgets(3 * page - 1, 3).unwrap(),
            [page, page, page - 1]
        );
        assert_eq!(
            parallel_chunk_budgets(3 * page, 3).unwrap(),
            [page, page, page]
        );
        assert_eq!(
            parallel_chunk_budgets(3 * page + 2, 3).unwrap(),
            [page + 1, page + 1, page]
        );
        assert_eq!(
            parallel_chunk_budgets(5 * page / 2, 3).unwrap(),
            [page, page, page / 2]
        );
    }

    #[test]
    fn task_memory_domain_planning_is_page_aware_uniform_and_bounded() {
        let page = u64::try_from(aster_runtime::ExecutionContext::AARM_MIN_PAGE_CAPACITY_BYTES)
            .expect("minimum page capacity fits u64");
        let local_limit =
            u64::try_from(aster_runtime::ExecutionContext::AARM_MAX_CONTEXT_CAPACITY_BYTES)
                .expect("local limit fits u64");

        let cases = [
            (0, 4, 1, 0, 0),
            (page - 1, 4, 1, page - 1, 0),
            (page, 4, 1, page, 0),
            (2 * page, 4, 2, page, 0),
            (4 * page, 4, 4, page, 0),
            (4 * page + 10, 4, 4, page + 2, 2),
        ];
        for (headroom, workers, concurrency, task_ceiling, main_growth) in cases {
            let plan = plan_task_memory_domain(headroom, 0, workers).expect("plan succeeds");
            assert_eq!(plan.task_memory_concurrency_limit, concurrency);
            assert_eq!(plan.task_context_ceiling_bytes, task_ceiling);
            assert_eq!(plan.main_future_growth_bytes, main_growth);
            assert!(
                u128::from(plan.main_future_growth_bytes)
                    + u128::from(plan.task_context_ceiling_bytes)
                        * u128::try_from(plan.task_memory_concurrency_limit)
                            .expect("concurrency fits u128")
                    <= u128::from(headroom)
            );
        }

        let almost_full_main = local_limit - 100;
        let bounded = plan_task_memory_domain(u64::MAX, almost_full_main, 16)
            .expect("large addressable plan succeeds");
        assert_eq!(bounded.main_future_growth_bytes, 100);
        assert_eq!(bounded.task_context_ceiling_bytes, local_limit);
        assert_eq!(bounded.task_memory_concurrency_limit, 16);
    }

    #[test]
    fn async_memory_domain_planning_preserves_main_share_and_borrows_phase_headroom() {
        let page = u64::try_from(aster_runtime::ExecutionContext::AARM_MIN_PAGE_CAPACITY_BYTES)
            .expect("minimum page capacity fits u64");
        let cases = [
            (0, [0, 0, 0, 0]),
            (page - 1, [page - 1, page - 1, 0, 0]),
            (page, [page, page, 0, 0]),
            (2 * page, [2 * page, page, page, 0]),
            (3 * page, [3 * page, page, page, page]),
            (3 * page + 2, [3 * page + 2, page + 1, page + 1, page]),
            (
                6 * page + 2,
                [6 * page + 2, 2 * page + 1, 2 * page + 1, 2 * page],
            ),
        ];
        for (headroom, expected) in cases {
            let first = plan_async_memory_domain(headroom, 0).expect("plan succeeds");
            let second = plan_async_memory_domain(headroom, 0).expect("plan is stable");
            assert_eq!(first, second);
            assert_eq!(
                [
                    first.phase_context_ceiling,
                    first.fixed_move_next_ceiling,
                    first.fixed_inner_ceiling,
                    first.main_future_growth,
                ],
                expected
            );
            assert_eq!(
                first.fixed_move_next_ceiling
                    + first.fixed_inner_ceiling
                    + first.main_future_growth,
                headroom
            );
        }
    }

    #[test]
    fn async_memory_domain_planning_respects_main_and_context_caps() {
        let local = u64::try_from(aster_runtime::ExecutionContext::AARM_MAX_CONTEXT_CAPACITY_BYTES)
            .expect("local limit fits u64");
        let plan = plan_async_memory_domain(u64::MAX, local - 7).expect("large plan succeeds");
        assert_eq!(plan.phase_context_ceiling, local);
        assert_eq!(plan.fixed_move_next_ceiling, local);
        assert_eq!(plan.fixed_inner_ceiling, local);
        assert_eq!(plan.main_future_growth, 7);
        assert!(
            u128::from(plan.fixed_move_next_ceiling)
                + u128::from(plan.fixed_inner_ceiling)
                + u128::from(plan.main_future_growth)
                <= u128::from(u64::MAX)
        );
    }

    #[test]
    fn parallel_memory_partitions_preserve_large_headroom_exactly() {
        let first = parallel_chunk_budgets(u64::MAX, 3).expect("boundary plan succeeds");
        let second = parallel_chunk_budgets(u64::MAX, 3).expect("same plan succeeds again");
        assert_eq!(first, second);
        assert_eq!(first.len(), 3);
        assert_eq!(
            first.iter().map(|share| u128::from(*share)).sum::<u128>(),
            u128::from(u64::MAX)
        );
    }

    #[test]
    fn disconnected_chunk_does_not_prevent_draining_later_outcomes() {
        let (disconnected_tx, disconnected_rx) = mpsc::channel();
        drop(disconnected_tx);
        let (logical_tx, logical_rx) = mpsc::channel();
        logical_tx
            .send(ChunkOutcome {
                first_error: Some((7, BackendError::new("later logical failure"))),
                #[cfg(feature = "aarm-telemetry")]
                telemetry: None,
            })
            .expect("logical outcome is queued");

        let error = collect_chunks(vec![disconnected_rx, logical_rx])
            .expect_err("all receivers are drained before selection");
        assert!(error.message().contains("later logical failure"));
    }

    /// Deterministic proof that chunk partials are ordered by chunk index,
    /// never by arrival order: messages are sent in reverse of receiver
    /// order (simulating chunk 2 completing before chunks 0 and 1), and
    /// `collect_reduce_chunks` must still return them index-ordered. No
    /// sleep, no real thread race: the ordering guarantee is structural
    /// (`Receiver::recv` on receiver *i* only ever observes sender *i*'s
    /// message), and this test exercises that structure directly.
    #[test]
    fn reduce_chunk_partials_are_ordered_by_chunk_index_not_send_order() {
        let (tx0, rx0) = mpsc::channel();
        let (tx1, rx1) = mpsc::channel();
        let (tx2, rx2) = mpsc::channel();
        tx2.send(ReduceChunkOutcome {
            result: Ok(ExecutionValue::Int(3)),
            #[cfg(feature = "aarm-telemetry")]
            telemetry: None,
        })
        .expect("chunk 2 sends first");
        tx0.send(ReduceChunkOutcome {
            result: Ok(ExecutionValue::Int(1)),
            #[cfg(feature = "aarm-telemetry")]
            telemetry: None,
        })
        .expect("chunk 0 sends second");
        tx1.send(ReduceChunkOutcome {
            result: Ok(ExecutionValue::Int(2)),
            #[cfg(feature = "aarm-telemetry")]
            telemetry: None,
        })
        .expect("chunk 1 sends last");

        let partials = collect_reduce_chunks(vec![rx0, rx1, rx2]).expect("every chunk succeeded");
        assert_eq!(
            partials,
            vec![
                ExecutionValue::Int(1),
                ExecutionValue::Int(2),
                ExecutionValue::Int(3)
            ]
        );
    }

    /// Deterministic proof that the smallest *logical* index wins regardless
    /// of which failing chunk's message arrives first.
    #[test]
    fn reduce_chunk_error_selection_picks_the_smallest_logical_index_regardless_of_send_order() {
        let (tx0, rx0) = mpsc::channel();
        let (tx1, rx1) = mpsc::channel();
        // The chunk covering the *later* logical position reports first.
        tx1.send(ReduceChunkOutcome {
            result: Err((5, BackendError::new("late position failure"))),
            #[cfg(feature = "aarm-telemetry")]
            telemetry: None,
        })
        .expect("chunk 1 sends first");
        tx0.send(ReduceChunkOutcome {
            result: Err((2, BackendError::new("early position failure"))),
            #[cfg(feature = "aarm-telemetry")]
            telemetry: None,
        })
        .expect("chunk 0 sends second");

        let error = collect_reduce_chunks(vec![rx0, rx1])
            .expect_err("an accumulation error must propagate");
        assert!(error.message().contains("early position failure"));
    }

    /// Every accepted chunk is drained even when one receiver disconnects,
    /// mirroring `disconnected_chunk_does_not_prevent_draining_later_outcomes`.
    #[test]
    fn disconnected_reduce_chunk_does_not_prevent_draining_a_later_failure() {
        let (disconnected_tx, disconnected_rx) = mpsc::channel();
        drop(disconnected_tx);
        let (logical_tx, logical_rx) = mpsc::channel();
        logical_tx
            .send(ReduceChunkOutcome {
                result: Err((7, BackendError::new("later logical failure"))),
                #[cfg(feature = "aarm-telemetry")]
                telemetry: None,
            })
            .expect("logical outcome is queued");

        let error = collect_reduce_chunks(vec![disconnected_rx, logical_rx])
            .expect_err("all receivers are drained before selection");
        assert!(error.message().contains("later logical failure"));
    }
}
