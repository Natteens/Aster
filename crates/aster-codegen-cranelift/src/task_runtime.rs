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
use super::worker_pool::{ChunkOutcome, ExecutionPool, JobKind, TaskHandle, TaskOutcome};
use super::{BackendError, ExecutionValue, MemoryStats, mir, scalar};

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
    next_id: u64,
    next_token: CompletionToken,
}

impl TaskRuntime {
    pub(super) fn new(
        module: &Arc<mir::Module>,
        worker_count: usize,
    ) -> Result<Self, BackendError> {
        let pool = ExecutionPool::new(Arc::clone(module), worker_count)?;
        let driver = PreparedProgram::prepare(module)?;
        Ok(Self {
            pool,
            driver: Some(driver),
            completion: Arc::new(CompletionQueue::new()),
            entries: HashMap::new(),
            async_tasks: HashMap::new(),
            token_to_async: HashMap::new(),
            ready: VecDeque::new(),
            worker_count,
            next_id: 0,
            next_token: 0,
        })
    }

    fn fresh_id(&mut self) -> TaskHandleId {
        let id = TaskHandleId(self.next_id);
        self.next_id = self.next_id.wrapping_add(1);
        id
    }

    // --- plain `Task.Run` / `Wait` (unchanged behavior) -------------------

    /// `Task.Run(function)`: submit one plain task and return its handle.
    pub(super) fn run(&mut self, symbol: mir::SymbolId) -> Result<TaskHandleId, BackendError> {
        let handle = self.pool.submit(symbol, false, None)?;
        let id = self.fresh_id();
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
    ) -> TaskHandleId {
        let id = self.fresh_id();
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
        id
    }

    pub(super) fn async_state(&self, id: TaskHandleId) -> i32 {
        self.async_tasks.get(&id).map_or(0, |task| task.state)
    }

    pub(super) fn async_set_state(&mut self, id: TaskHandleId, state: i32) {
        if let Some(task) = self.async_tasks.get_mut(&id) {
            task.state = state;
        }
    }

    pub(super) fn async_store_slot(
        &mut self,
        id: TaskHandleId,
        index: usize,
        value: ExecutionValue,
    ) {
        if let Some(task) = self.async_tasks.get_mut(&id)
            && let Some(slot) = task.frame.get_mut(index)
        {
            *slot = Some(value);
        }
    }

    pub(super) fn async_load_slot(&self, id: TaskHandleId, index: usize) -> i64 {
        self.async_tasks
            .get(&id)
            .and_then(|task| task.frame.get(index))
            .and_then(|slot| slot.as_ref())
            .map_or(0, scalar::to_bits)
    }

    /// `AsyncSpawnInner`: submit the awaited `Task.Run` target with a fresh
    /// completion token bound to this async task. Only awaited inners carry a
    /// token; plain tasks and Parallel chunks never do.
    pub(super) fn async_spawn_inner(&mut self, id: TaskHandleId, inner: mir::SymbolId) {
        let token = self.next_token;
        self.next_token = self.next_token.wrapping_add(1);
        let submission =
            self.pool
                .submit(inner, false, Some((Arc::clone(&self.completion), token)));
        match submission {
            Ok(handle) => {
                if let Some(task) = self.async_tasks.get_mut(&id) {
                    task.inner = Some(handle);
                    task.inner_token = Some(token);
                }
                self.token_to_async.insert(token, id);
            }
            Err(error) => {
                // The pool is shut down: resolve the async task as a controlled
                // failure now rather than leaving it forever suspended.
                if let Some(task) = self.async_tasks.get_mut(&id) {
                    task.resolved = Some(Err(error));
                }
            }
        }
    }

    pub(super) fn async_await_result(&self, id: TaskHandleId) -> i64 {
        self.async_tasks
            .get(&id)
            .and_then(|task| task.inner_result.as_ref())
            .map_or(0, scalar::to_bits)
    }

    pub(super) fn async_set_result(&mut self, id: TaskHandleId, value: ExecutionValue) {
        if let Some(task) = self.async_tasks.get_mut(&id) {
            task.candidate = Some(value);
        }
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
        // SAFETY: same contract; each reborrow inside is short-lived and never
        // spans a driver step.
        let outcome = unsafe { Self::pump_loop(runtime, &driver, target) };
        // SAFETY: restore ownership on every path so the runtime stays usable.
        unsafe {
            (*runtime).driver = Some(driver);
        }
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
        unsafe { (*runtime).seed_ready(target) };
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
                    unsafe { (*runtime).on_completion(token) };
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
    fn seed_ready(&mut self, target: TaskHandleId) {
        if let Some(task) = self.async_tasks.get(&target)
            && task.resolved.is_none()
            && task.state == 0
            && !self.ready.contains(&target)
        {
            self.ready.push_back(target);
        }
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
            Ok((ExecutionValue::Int(PENDING), _)) => {}
            Ok((ExecutionValue::Int(COMPLETED), _)) => {
                let outcome = match task.candidate.clone() {
                    Some(value) => Ok(TaskOutcome::Completed(value, MemoryStats::default())),
                    None => Err(BackendError::new(
                        "an async task completed without producing a result",
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
    /// to resume (inner success). An unknown token is ignored, never a crash.
    fn on_completion(&mut self, token: CompletionToken) {
        let Some(handle) = self.token_to_async.remove(&token) else {
            return;
        };
        let Some(task) = self.async_tasks.get_mut(&handle) else {
            return;
        };
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
    }

    // --- Parallel ---------------------------------------------------------

    /// `Parallel.For(start, end, Body)`: run `Body` over `[start, end)` in
    /// contiguous, worker-balanced chunks, block until all finish, and
    /// propagate the failure with the smallest logical index, if any.
    pub(super) fn parallel_for(
        &self,
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
        let Ok(total) = usize::try_from(total) else {
            return Ok(());
        };
        if total == 0 {
            return Ok(());
        }
        let boundaries = chunk_boundaries(total, self.worker_count);
        let mut receivers = Vec::with_capacity(boundaries.len());
        let mut cursor = i64::from(start);
        for length in boundaries {
            let chunk_start = i32::try_from(cursor).expect("chunk start fits i32");
            cursor += i64::try_from(length).expect("chunk length fits i64");
            let chunk_end = i32::try_from(cursor).expect("chunk end fits i32");
            receivers.push(self.pool.submit_parallel(JobKind::ForChunk {
                symbol: body,
                start: chunk_start,
                end: chunk_end,
            })?);
        }
        collect_chunks(receivers)
    }

    /// `Parallel.ForEach(values, Body)`: `values` are already the host-owned
    /// scalar copies (see `async_abi`); no array pointer ever reaches a worker.
    pub(super) fn parallel_for_each(
        &self,
        values: Vec<ExecutionValue>,
        body: mir::SymbolId,
    ) -> Result<(), BackendError> {
        if values.is_empty() {
            return Ok(());
        }
        let boundaries = chunk_boundaries(values.len(), self.worker_count);
        let mut receivers = Vec::with_capacity(boundaries.len());
        let mut values = values;
        let mut base = 0;
        for length in boundaries {
            let rest = values.split_off(length);
            let chunk_values = std::mem::replace(&mut values, rest);
            receivers.push(self.pool.submit_parallel(JobKind::ForEachChunk {
                symbol: body,
                base,
                values: chunk_values,
            })?);
            base += length;
        }
        collect_chunks(receivers)
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
fn collect_chunks(
    receivers: Vec<std::sync::mpsc::Receiver<ChunkOutcome>>,
) -> Result<(), BackendError> {
    let mut first_error: Option<(i64, BackendError)> = None;
    for receiver in receivers {
        let outcome = receiver
            .recv()
            .map_err(|_| BackendError::new("a Parallel worker disconnected before finishing"))?;
        if let Some((index, error)) = outcome.first_error {
            if first_error
                .as_ref()
                .is_none_or(|(current, _)| index < *current)
            {
                first_error = Some((index, error));
            }
        }
    }
    match first_error {
        Some((_, error)) => Err(error),
        None => Ok(()),
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
                            | mir::Intrinsic::ParallelForEach,
                        ..
                    }
                )
            })
        })
    })
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
    fn chunk_boundaries_never_exceed_workers_or_iterations() {
        assert_eq!(chunk_boundaries(0, 4), Vec::<usize>::new());
        assert_eq!(chunk_boundaries(1, 4), vec![1]);
        assert_eq!(chunk_boundaries(10, 4), vec![3, 3, 2, 2]);
        assert_eq!(chunk_boundaries(3, 8), vec![1, 1, 1]);
        assert_eq!(chunk_boundaries(1000, 1).iter().sum::<usize>(), 1000);
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
            .invoke(wrapper, false, Some(pointer))
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
            .invoke(wrapper, false, Some(pointer))
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
}
