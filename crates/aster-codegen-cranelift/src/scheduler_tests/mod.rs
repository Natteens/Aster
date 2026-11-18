//! Scheduler tests that run real Cranelift-compiled programs.
//!
//! The scheduler lives in `aster-exec`, which cannot depend on the JIT, so the
//! tests that need machine code (compile Aster source, run it through the
//! worker pool, task runtime and ABI) live here and drive the scheduler through
//! the Cranelift [`super::provider`]. Scheduler-only tests that need no code
//! live next to the scheduler in `aster-exec`.

mod task_abi;
mod task_runtime;
mod worker_pool;
