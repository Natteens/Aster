//! Backend-neutral execution scheduler for Aster programs (ARCH-005).
//!
//! This crate owns the worker pool, the task runtime, the completion queue and
//! the `extern "C"` task/async/parallel ABI functions that generated code
//! calls. It depends on `aster-mir` and `aster-runtime` only: it has no
//! dependency on Cranelift or on any other code generator.
//!
//! Machine code reaches the scheduler through an injected [`CodeProvider`].
//! The provider validates a module, resolves entry names and prepares one
//! finalized [`ExecutableProgram`] per pool; the scheduler then runs tasks,
//! `MoveNext` steps and parallel chunks through that trait object. The JIT
//! provider lives in `aster-codegen-cranelift`; a future AOT provider can
//! implement the same traits without the JIT.
//!
//! Modules are public so that a provider crate can reach the scheduler types;
//! the API is an internal contract between Aster crates, not a stable
//! user-facing surface.

// Internal engine contract: documenting every `# Errors`/`# Panics` section
// would restate `BackendError` for each of dozens of scheduler hooks.
#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    // `extern "C"` ABI entry points take the generated-code pointer contract
    // (null-checked) and are public only so provider-side tests can call them.
    clippy::not_unsafe_ptr_arg_deref,
    clippy::return_self_not_must_use
)]

pub mod async_abi;
pub mod completion_queue;
mod error;
mod program;
pub mod scalar;
pub mod task_abi;
pub mod task_runtime;
pub mod worker_pool;

pub use aster_mir as mir;
pub use error::{BackendError, ExecutionValue};
pub use program::{CodeProvider, ExecutableProgram};

#[cfg(test)]
mod test_support;

#[cfg(test)]
mod scheduler_tests;
