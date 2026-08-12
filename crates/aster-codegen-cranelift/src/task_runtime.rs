//! Host-side ownership of every `Task<T>` handle for one top-level execution.
//!
//! ```text
//! TaskRuntime
//! ├── ExecutionPool     (crate::worker_pool; runs Task.Run and Parallel chunks)
//! ├── driver            (a host-only PreparedProgram that runs MoveNext steps)
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
//! Three `PreparedProgram`s never overlap: the top-level `prepared_main` that
//! runs `Main` (in `execution`), the `driver` here that runs every `MoveNext`,
//! and one worker `PreparedProgram` per pool thread. `MoveNext` runs only on
//! the host, only through `driver`; workers only run `Task.Run` and `Parallel`
//! chunks and never touch this runtime.
//!
//! The pump must never hold a Rust borrow of the runtime across a JIT call
//! that can call back into the runtime. [`TaskRuntime::pump`] therefore takes
//! the driver out with ownership for the duration of the pump and drives every
//! step through a raw `*mut Self`, reborrowing the runtime only in short,
//! non-overlapping windows between `MoveNext` invocations (see `pump_loop`).

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use super::completion_queue::{CompletionQueue, CompletionToken};
use super::execution::PreparedProgram;
use super::worker_pool::{
    ChunkOutcome, ExecutionPool, JobKind, ParallelMemoryBudget, ReduceChunkOutcome, TaskHandle,
    TaskOutcome,
};
use super::{BackendError, ExecutionValue, MemoryStats, mir, scalar};

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

/// Opaque, stable identity for one task within its owning [`TaskRuntime`].
/// Never an arena pointer, never reused; generated code only ever holds the
/// bare integer, never dereferences it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct TaskHandleId(u64);

impl TaskHandleId {
    pub(super) fn to_bits(self) -> i64 {
        i64::from_ne_bytes(self.0.to_ne_bytes())
    }

    pub(super) fn from_bits(bits: i64) -> Self {
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
    move_next: mir::SymbolId,
    state: i32,
    /// Scalar frame, one typed value per slot persisted across the suspension.
    frame: Vec<Option<ExecutionValue>>,
    /// The awaited inner `Task.Run` handle and its completion token, live only
    /// while suspended at the `await`.
    inner: Option<TaskHandle>,
    inner_token: Option<CompletionToken>,
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
pub(super) struct TaskRuntime {
    pool: ExecutionPool,
    /// Host-only program for `MoveNext` steps. `Option` so the pump can take
    /// exclusive ownership across a step without holding a runtime borrow.
    driver: Option<PreparedProgram>,
    completion: Arc<CompletionQueue>,
    entries: HashMap<TaskHandleId, TaskEntry>,
    async_tasks: HashMap<TaskHandleId, AsyncTask>,
    token_to_async: HashMap<CompletionToken, TaskHandleId>,
    ready: VecDeque<TaskHandleId>,
    worker_count: usize,
    task_worker_count: usize,
    parallel_governor: Option<Arc<aster_runtime::MemoryGovernor>>,
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
    driver: Option<PreparedProgram>,
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
    pub(super) fn new(
        module: &Arc<mir::Module>,
        worker_count: usize,
    ) -> Result<Self, BackendError> {
        Self::with_parallel_governor(module, worker_count, None)
    }

    fn with_parallel_governor(
        module: &Arc<mir::Module>,
        worker_count: usize,
        parallel_governor: Option<Arc<aster_runtime::MemoryGovernor>>,
    ) -> Result<Self, BackendError> {
        let pool = ExecutionPool::new(Arc::clone(module), worker_count)?;
        let driver = module_uses_async(module)
            .then(|| PreparedProgram::prepare(module))
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
            #[cfg(feature = "aarm-telemetry")]
            parallel_plans: Vec::new(),
            #[cfg(feature = "aarm-telemetry")]
            parallel_snapshots: Vec::new(),
            next_id: 0,
            next_token: 0,
        })
    }

    #[cfg(feature = "aarm-telemetry")]
    pub(super) fn with_memory_governor(
        module: &Arc<mir::Module>,
        worker_count: usize,
        governor: Arc<aster_runtime::MemoryGovernor>,
    ) -> Result<Self, BackendError> {
        Self::with_parallel_governor(module, worker_count, Some(governor))
    }

    #[cfg(feature = "aarm-telemetry")]
    pub(super) fn parallel_plans(&self) -> &[AarmParallelPlanningTelemetry] {
        &self.parallel_plans
    }

    #[cfg(feature = "aarm-telemetry")]
    pub(super) fn parallel_snapshots(&self) -> &[aster_runtime::AarmMemoryTelemetry] {
        &self.parallel_snapshots
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

    /// `Task.Run(function)`: submit one plain task and return its handle.
    pub(super) fn run(&mut self, symbol: mir::SymbolId) -> Result<TaskHandleId, BackendError> {
        self.pool.ensure_workers(self.pending_worker_demand())?;
        let id = self.fresh_id()?;
        let handle = self.pool.submit(symbol, false, None)?;
        self.entries.insert(id, TaskEntry::Pending(handle));
        Ok(id)
    }

    pub(super) fn is_async_handle(&self, id: TaskHandleId) -> bool {
        self.async_tasks.contains_key(&id)
    }

    /// `task.Wait()` for a plain task: join once, cache, replay on every call.
    pub(super) fn wait(&mut self, id: TaskHandleId) -> Result<TaskOutcome, BackendError> {
        let entry = self
            .entries
            .remove(&id)
            .ok_or_else(|| BackendError::new("Task<T>.Wait received an unknown task handle"))?;
        let result = match entry {
            TaskEntry::Resolved(result) => result,
            TaskEntry::Pending(handle) => handle.join(),
        };
        self.entries.insert(id, TaskEntry::Resolved(result.clone()));
        result
    }

    // --- async spawn and MoveNext intrinsics ------------------------------

    /// The async wrapper's `AsyncSpawn`: register a lazy state machine and
    /// return its handle. The body does not run until the task is pumped.
    pub(super) fn async_spawn(
        &mut self,
        move_next: mir::SymbolId,
        slot_count: usize,
    ) -> Result<TaskHandleId, BackendError> {
        let id = self.fresh_id()?;
        self.async_tasks.insert(
            id,
            AsyncTask {
                move_next,
                state: 0,
                frame: vec![None; slot_count],
                inner: None,
                inner_token: None,
                inner_result: None,
                candidate: None,
                resolved: None,
            },
        );
        Ok(id)
    }

    pub(super) fn async_state(&self, id: TaskHandleId) -> Result<i32, BackendError> {
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

    pub(super) fn async_set_state(
        &mut self,
        id: TaskHandleId,
        state: i32,
    ) -> Result<(), BackendError> {
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

    pub(super) fn async_store_slot(
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

    pub(super) fn async_load_slot(
        &self,
        id: TaskHandleId,
        index: usize,
    ) -> Result<i64, BackendError> {
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
    pub(super) fn async_spawn_inner(
        &mut self,
        id: TaskHandleId,
        inner: mir::SymbolId,
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
        let handle = self
            .pool
            .submit(inner, false, Some((Arc::clone(&self.completion), token)))?;
        let task = self
            .async_tasks
            .get_mut(&id)
            .ok_or_else(|| BackendError::new("async task disappeared during inner spawn"))?;
        task.inner = Some(handle);
        task.inner_token = Some(token);
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
        self.task_worker_count.max(
            plain
                .saturating_add(async_inners)
                .saturating_add(1)
                .min(self.worker_count),
        )
    }

    pub(super) fn async_await_result(&self, id: TaskHandleId) -> Result<i64, BackendError> {
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

    pub(super) fn async_set_result(
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
    pub(super) unsafe fn pump(
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
            Some(driver) => unsafe { Self::pump_loop(runtime, driver, target) },
            None => Err(BackendError::new("the async pump lost its driver")),
        };
        drop(guard);
        outcome
    }

    /// # Safety
    /// See [`Self::pump`]. Every `(*runtime)` reborrow below is a single
    /// statement and is dropped before `driver.invoke_move_next` runs, so no
    /// runtime borrow is ever live during a `MoveNext` step's ABI callbacks.
    #[allow(unsafe_code)]
    unsafe fn pump_loop(
        runtime: *mut Self,
        driver: &PreparedProgram,
        target: TaskHandleId,
    ) -> Result<TaskOutcome, BackendError> {
        // SAFETY: short reborrow, immediately dropped.
        let completion = Arc::clone(unsafe { &(*runtime).completion });
        // SAFETY: short reborrow, immediately dropped.
        unsafe { (*runtime).seed_ready(target) }?;
        loop {
            // SAFETY: short reborrow, immediately dropped.
            if let Some(outcome) = unsafe { (*runtime).resolved_outcome(target) } {
                return outcome;
            }
            // SAFETY: each reborrow inside the loop body is short-lived and
            // never spans `invoke_move_next`.
            while let Some((move_next, handle)) = unsafe { (*runtime).pop_ready() } {
                // No runtime borrow is held here: the step's ABI callbacks
                // reborrow `runtime` freshly through the context pointer.
                let step =
                    driver.invoke_move_next(move_next, handle.to_bits(), runtime.cast::<()>());
                // SAFETY: short reborrow, immediately dropped.
                unsafe { (*runtime).apply_move_next(handle, step) };
            }
            // SAFETY: short reborrow, immediately dropped.
            if let Some(outcome) = unsafe { (*runtime).resolved_outcome(target) } {
                return outcome;
            }
            match completion.pop() {
                Some(token) => {
                    // SAFETY: short reborrow, immediately dropped.
                    let completion = unsafe { (*runtime).on_completion(token) };
                    if let Err(error) = completion {
                        // SAFETY: short reborrow, immediately dropped. Cache
                        // the failure so a repeated Wait cannot block after the
                        // bad token has already been consumed.
                        unsafe { (*runtime).resolve_error(target, error.clone()) };
                        return Err(error);
                    }
                }
                None => {
                    return Err(BackendError::new(
                        "the async completion queue closed while a task was still pending",
                    ));
                }
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

    fn pop_ready(&mut self) -> Option<(mir::SymbolId, TaskHandleId)> {
        while let Some(handle) = self.ready.pop_front() {
            if let Some(task) = self.async_tasks.get(&handle)
                && task.resolved.is_none()
            {
                return Some((task.move_next, handle));
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
            task.resolved = Some(Err(error));
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
            Err(error) => task.resolved = Some(Err(error)),
            Ok((ExecutionValue::Int(PENDING), _)) => {
                if task.state != 1 || task.inner.is_none() || task.inner_token.is_none() {
                    task.resolved = Some(Err(BackendError::new(
                        "an async task suspended without a registered awaited operation",
                    )));
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
                task.resolved = Some(outcome);
            }
            Ok(_) => {
                task.resolved = Some(Err(BackendError::new(
                    "an async MoveNext step returned an invalid status",
                )));
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

    fn collect_parallel_chunks(
        &mut self,
        receivers: Vec<std::sync::mpsc::Receiver<ChunkOutcome>>,
    ) -> Result<(), BackendError> {
        let collected = collect_chunks_observed(receivers);
        #[cfg(feature = "aarm-telemetry")]
        self.parallel_snapshots.extend(collected.telemetry);
        collected.result
    }

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
    pub(super) fn parallel_for(
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
    pub(super) fn parallel_for_each(
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
    pub(super) fn parallel_reduce(
        &mut self,
        values: Vec<ExecutionValue>,
        identity: ExecutionValue,
        accumulate: mir::SymbolId,
        combine: mir::SymbolId,
    ) -> Result<ExecutionValue, BackendError> {
        if values.is_empty() {
            return Ok(identity);
        }
        let boundaries = chunk_boundaries(values.len(), self.worker_count);
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
        if let Some((index, error)) = outcome.first_error {
            if first_error
                .as_ref()
                .is_none_or(|(current, _)| index < *current)
            {
                first_error = Some((index, error));
            }
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
pub(super) fn module_uses_tasks(module: &mir::Module) -> bool {
    module.functions.iter().any(|function| {
        function.blocks.iter().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction,
                    mir::Instruction::CallIntrinsic {
                        intrinsic: mir::Intrinsic::TaskRun
                            | mir::Intrinsic::TaskWait
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

fn module_uses_async(module: &mir::Module) -> bool {
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
    use std::thread;
    use std::time::Duration;

    use super::*;

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
        let mut runtime = TaskRuntime::new(&module, 4).expect("runtime starts");

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
    fn independent_static_task_submissions_prepare_together() {
        let module = Arc::new(compile(
            "public int Work() { return 1; } public int Main() { Task<int> a = Task.Run(Work); Task<int> b = Task.Run(Work); return a.Wait() + b.Wait(); }",
        ));
        let runtime = TaskRuntime::new(&module, 4).expect("runtime starts");
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
    fn waiting_on_an_unknown_id_is_a_controlled_error() {
        let module = compile("public int Compute() { return 1; }");
        let mut runtime =
            TaskRuntime::new(&Arc::new(module), 1).expect("runtime starts with no tasks yet");
        let bogus = TaskHandleId(u64::MAX);
        assert!(runtime.wait(bogus).is_err());
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
        let mut runtime = TaskRuntime::new(&Arc::new(module), 1).expect("runtime starts");
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
    fn waiting_twice_on_the_same_plain_id_returns_the_same_outcome() {
        let module = compile("public int Compute() { return 42; }");
        let symbol = module
            .functions
            .iter()
            .find(|function| function.name == "Compute")
            .expect("Compute is declared")
            .symbol;
        let mut runtime = TaskRuntime::new(&Arc::new(module), 1).expect("runtime starts");
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
        let mut runtime = TaskRuntime::new(&Arc::new(module), 2).expect("runtime starts");
        for _ in 0..5 {
            runtime.run(symbol).expect("task is accepted");
        }
        drop(runtime);
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
        let mut runtime = TaskRuntime::new(&module, worker_count)
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
        let mut runtime = TaskRuntime::new(&module, 1).expect("runtime starts");
        let driver = PreparedProgram::prepare(&module).expect("driver prepares");
        let pointer = std::ptr::from_mut(&mut runtime).cast::<()>();
        let (value, _) = driver
            .invoke(wrapper, false, Some(pointer), None, None)
            .expect("wrapper registers the async task without running its body");
        let ExecutionValue::Long(bits) = value else {
            panic!("wrapper must return a Task<T> handle");
        };
        let handle = TaskHandleId::from_bits(bits);

        let task = runtime
            .async_tasks
            .get(&handle)
            .expect("the async task is registered");
        assert_eq!(task.state, 0, "MoveNext never ran: state is still 0");
        assert!(
            task.resolved.is_none(),
            "an unawaited task is never resolved"
        );
        assert!(
            task.inner.is_none() && task.inner_token.is_none(),
            "no inner Task.Run was ever submitted"
        );

        // Dropping a runtime with a pending, never-awaited async task must
        // not hang or panic.
        drop(runtime);
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
        let mut runtime = TaskRuntime::new(&Arc::new(module), 2).expect("runtime starts");
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
        let mut runtime = TaskRuntime::new(&Arc::new(module), 1).expect("runtime starts");
        runtime.next_id = u64::MAX;

        let error = runtime.run(symbol).expect_err("id exhaustion must fail");
        assert!(error.message().contains("id space is exhausted"));
        assert!(runtime.entries.is_empty());
    }

    #[test]
    fn completion_token_exhaustion_is_controlled_instead_of_wrapping() {
        let module = compile("public int Compute() { return 1; }");
        let symbol = module.functions[0].symbol;
        let mut runtime = TaskRuntime::new(&Arc::new(module), 1).expect("runtime starts");
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
        let mut runtime = TaskRuntime::new(&Arc::new(module), 1).expect("runtime starts");
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
        let mut runtime = TaskRuntime::new(&Arc::new(module), 1).expect("runtime starts");
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
            let mut runtime = TaskRuntime::new(&Arc::new(module), worker_count)
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
            let mut runtime = TaskRuntime::new(&Arc::new(module), worker_count)
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
            let mut runtime = TaskRuntime::new(&Arc::new(module), worker_count)
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
        let mut runtime = TaskRuntime::new(&module, 4).expect("runtime starts");
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
            let mut runtime =
                TaskRuntime::with_memory_governor(&module, worker_count, Arc::clone(&governor))
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
            let mut runtime = TaskRuntime::with_memory_governor(&module, 4, Arc::clone(&governor))
                .expect("governed runtime starts");
            let mut expected_error = None;
            for repetition in 0..20 {
                let error = if reduce {
                    runtime
                        .parallel_reduce(
                            values.clone(),
                            ExecutionValue::Int(0),
                            accumulate,
                            combine,
                        )
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

    // --- Parallel.Reduce ---------------------------------------------------

    const REDUCE_SOURCE: &str = "public int AddValue(int accumulator, int value) { return accumulator + value; } \
         public int AddPartial(int left, int right) { return left + right; }";

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

    #[test]
    fn parallel_reduce_gives_the_same_result_with_one_two_or_many_workers() {
        let values: Vec<ExecutionValue> = (1..=50).map(ExecutionValue::Int).collect();
        let expected = ExecutionValue::Int((1..=50).sum());
        for worker_count in [1, 2, 8] {
            let module = compile(REDUCE_SOURCE);
            let accumulate = symbol(&module, "AddValue");
            let combine = symbol(&module, "AddPartial");
            let mut runtime = TaskRuntime::new(&Arc::new(module), worker_count)
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
        let mut runtime = TaskRuntime::new(&Arc::new(module), 4).expect("runtime starts");
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
        let mut runtime = TaskRuntime::new(&Arc::new(module), 4).expect("runtime starts");
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
        let mut runtime = TaskRuntime::with_memory_governor(&module, 4, Arc::clone(&governor))
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
        let mut runtime = TaskRuntime::new(&module, 4).expect("runtime starts");
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
        let mut runtime = TaskRuntime::new(&module, 2).expect("runtime starts");

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
        let mut runtime = TaskRuntime::new(&module, 4).expect("runtime starts");

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
}
