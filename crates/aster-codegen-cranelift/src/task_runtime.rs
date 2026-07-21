//! Host-side ownership of every `Task<T>` handle for one top-level
//! execution.
//!
//! ```text
//! TaskRuntime
//! ├── ExecutionPool   (crate::worker_pool; unchanged)
//! └── task entries    (keyed by an opaque, stable `TaskHandleId`)
//! ```
//!
//! A `Task<T>` value in generated code is never a pointer: it is a plain
//! `TaskHandleId` (an integer), looked up in this table. That makes every
//! `unsafe` in `task_abi.rs` independent of how many times, or how few, the
//! Aster program calls `Wait()` on it:
//!
//! * a fresh id from [`TaskRuntime::run`] always names a live entry;
//! * [`TaskRuntime::wait`] joins the pending handle **once** and caches the
//!   result in place, so a second `Wait()` on the same id (or on an alias
//!   holding the same id) replays the cached, owned result instead of
//!   touching the channel again — no double-free, no double-recv;
//! * an id `TaskRuntime` never issued, or one from a different execution,
//!   is simply absent from the table: a controlled error, not a dereference;
//! * a task that is never waited just stays a `Pending` entry until this
//!   `TaskRuntime` (owned by one top-level execution) is dropped, which
//!   drops every entry and, through `ExecutionPool`'s own `Drop`, shuts the
//!   pool down — no leak, no dangling handle, no explicit cleanup call
//!   required from callers.

use std::collections::HashMap;
use std::sync::Arc;

use super::worker_pool::{ExecutionPool, TaskHandle, TaskOutcome};
use super::{BackendError, mir};

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

enum TaskEntry {
    Pending(TaskHandle),
    Resolved(Result<TaskOutcome, BackendError>),
}

/// Owns one [`ExecutionPool`] and every task entry spawned against it for
/// the lifetime of one top-level execution. Dropping it drops every
/// still-pending handle (safe: see the module doc) and shuts the pool down.
pub(super) struct TaskRuntime {
    pool: ExecutionPool,
    entries: HashMap<TaskHandleId, TaskEntry>,
    next_id: u64,
}

impl TaskRuntime {
    pub(super) fn new(module: Arc<mir::Module>, worker_count: usize) -> Result<Self, BackendError> {
        Ok(Self {
            pool: ExecutionPool::new(module, worker_count)?,
            entries: HashMap::new(),
            next_id: 0,
        })
    }

    /// `Task.Run(function)`: submit one task and return the id that names
    /// it in this runtime, or a controlled error if the pool already
    /// rejected the submission.
    pub(super) fn run(&mut self, symbol: mir::SymbolId) -> Result<TaskHandleId, BackendError> {
        let handle = self.pool.submit(symbol, false)?;
        let id = TaskHandleId(self.next_id);
        self.next_id = self.next_id.wrapping_add(1);
        self.entries.insert(id, TaskEntry::Pending(handle));
        Ok(id)
    }

    /// `task.Wait()`: join `id`'s handle the first time, cache the owned
    /// result, and return a clone of it on every call (this one and any
    /// later one, including through an alias holding the same id). An
    /// unknown id (never issued, or from a different `TaskRuntime`) is a
    /// controlled error, never a dereference.
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
}

/// Whether `module` contains any `Task.Run`/`Wait` intrinsic, anywhere in
/// any function. The only place this crate inspects MIR to decide whether a
/// top-level execution needs a [`TaskRuntime`] at all.
pub(super) fn module_uses_tasks(module: &mir::Module) -> bool {
    module.functions.iter().any(|function| {
        function.blocks.iter().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction,
                    mir::Instruction::CallIntrinsic {
                        intrinsic: mir::Intrinsic::TaskRun | mir::Intrinsic::TaskWait,
                        ..
                    }
                )
            })
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compile(source: &str) -> mir::Module {
        aster_compiler::compile(source)
            .expect("source compiles")
            .mir
    }

    #[test]
    fn module_uses_tasks_is_false_without_task_run_or_wait() {
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
    fn waiting_on_an_unknown_id_is_a_controlled_error() {
        let module = compile("public int Compute() { return 1; }");
        let mut runtime =
            TaskRuntime::new(Arc::new(module), 1).expect("runtime starts with no tasks yet");
        let bogus = TaskHandleId(u64::MAX);
        assert!(runtime.wait(bogus).is_err());
    }

    #[test]
    fn waiting_twice_on_the_same_id_returns_the_same_outcome() {
        let module = compile("public int Compute() { return 42; }");
        let symbol = module
            .functions
            .iter()
            .find(|function| function.name == "Compute")
            .expect("Compute is declared")
            .symbol;
        let mut runtime = TaskRuntime::new(Arc::new(module), 1).expect("runtime starts");
        let id = runtime.run(symbol).expect("task is accepted");

        let first = runtime.wait(id).expect("first wait succeeds");
        let second = runtime
            .wait(id)
            .expect("second wait replays the cached result");
        assert_eq!(first, second);
    }

    #[test]
    fn a_tasks_controlled_error_is_observed_identically_on_every_wait() {
        let module =
            compile("public int Failing() { int[] values = new int[1]; return values[5]; }");
        let symbol = module
            .functions
            .iter()
            .find(|function| function.name == "Failing")
            .expect("Failing is declared")
            .symbol;
        let mut runtime = TaskRuntime::new(Arc::new(module), 1).expect("runtime starts");
        let id = runtime.run(symbol).expect("task is accepted");

        let TaskOutcome::Failed(first) = runtime.wait(id).expect("first wait completes") else {
            panic!("Failing must produce a controlled TaskOutcome::Failed, not a channel error");
        };
        let TaskOutcome::Failed(second) = runtime.wait(id).expect("second wait completes") else {
            panic!("second wait must replay the same controlled outcome");
        };
        assert_eq!(first, second);
        assert!(first.message().contains("array index 5"));
    }

    #[test]
    fn a_task_never_waited_is_dropped_cleanly_when_the_runtime_shuts_down() {
        let module = compile("public int Compute() { return 1; }");
        let symbol = module
            .functions
            .iter()
            .find(|function| function.name == "Compute")
            .expect("Compute is declared")
            .symbol;
        let mut runtime = TaskRuntime::new(Arc::new(module), 2).expect("runtime starts");
        for _ in 0..5 {
            runtime.run(symbol).expect("task is accepted");
        }
        // No explicit cleanup call: dropping `runtime` here must join every
        // worker and release every still-pending entry without hanging.
        drop(runtime);
    }
}
