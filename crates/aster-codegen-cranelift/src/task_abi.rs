//! ABI bridge between JIT-generated code and the internal [`task_runtime`].
//!
//! These functions are bound into a `PreparedProgram`'s `JITModule` exactly
//! like `aster_runtime`'s own registry (see `PreparedProgram::prepare` and
//! [`Codegen::new`]), but they live in this crate because they touch
//! [`task_runtime::TaskRuntime`], which `aster-runtime` must never depend on.
//!
//! Every function here follows the same contract as the rest of the ABI:
//! controlled errors are reported through `ExecutionContext::fail`, never a
//! Rust panic. `Task<T>` itself crosses this boundary as a plain integer
//! (`TaskHandleId`, see `task_runtime`), never a pointer, so there is no
//! handle to dereference, double-free, or leak on this side of the ABI at
//! all — ownership of every task entry lives entirely in `TaskRuntime`.

use aster_runtime::ExecutionContext;

use super::task_runtime::{TaskHandleId, TaskRuntime};
use super::worker_pool::TaskOutcome;
use super::{
    AbiParam, BackendError, Codegen, ExecutionValue, Linkage, Module, mir, module_error, types,
};

/// Name and address of every function this module exports to generated
/// code, alongside `aster_runtime::runtime_functions()`.
fn task_functions() -> [(&'static str, *const u8); 7] {
    [
        ("aster_task_run", aster_task_run as *const u8),
        ("aster_task_wait_i8", aster_task_wait_i8 as *const u8),
        ("aster_task_wait_i16", aster_task_wait_i16 as *const u8),
        ("aster_task_wait_i32", aster_task_wait_i32 as *const u8),
        ("aster_task_wait_i64", aster_task_wait_i64 as *const u8),
        ("aster_task_wait_f32", aster_task_wait_f32 as *const u8),
        ("aster_task_wait_f64", aster_task_wait_f64 as *const u8),
    ]
}

/// The ABI symbol that produces `task.Wait()`'s Cranelift-level result for
/// `result_type`. `None` for any type `Task<T>` cannot carry (arena-identity
/// types are already rejected by semantic analysis before MIR exists).
pub(super) fn wait_symbol_for(result_type: &mir::Type) -> Option<&'static str> {
    use mir::Type::{
        Bool, Byte, Char, Double, Float, Int, Long, SByte, Short, UInt, ULong, UShort,
    };
    Some(match result_type {
        Bool | SByte | Byte => "aster_task_wait_i8",
        Short | UShort => "aster_task_wait_i16",
        Int | UInt | Char => "aster_task_wait_i32",
        Long | ULong => "aster_task_wait_i64",
        Float => "aster_task_wait_f32",
        Double => "aster_task_wait_f64",
        _ => return None,
    })
}

impl Codegen {
    /// Declare every [`task_functions`] symbol as an importable function on
    /// `self.jit`, recording its `FuncId` in `self.runtime_ids` alongside
    /// `aster_runtime`'s own registry. `Task<T>` itself is declared as a
    /// plain `i64` (see `declarations::clif_value_type`), so
    /// `aster_task_run` returns one and every `aster_task_wait_*` takes one.
    pub(super) fn declare_task_functions(&mut self) -> Result<(), BackendError> {
        for (name, _) in task_functions() {
            let mut signature = self.jit.make_signature();
            signature.params.push(AbiParam::new(self.pointer_type));
            if name == "aster_task_run" {
                signature.params.push(AbiParam::new(types::I32));
                signature.returns.push(AbiParam::new(types::I64));
            } else {
                signature.params.push(AbiParam::new(types::I64));
                let result = match name {
                    "aster_task_wait_i8" => types::I8,
                    "aster_task_wait_i16" => types::I16,
                    "aster_task_wait_i32" => types::I32,
                    "aster_task_wait_i64" => types::I64,
                    "aster_task_wait_f32" => types::F32,
                    "aster_task_wait_f64" => types::F64,
                    _ => unreachable!("task_functions lists every symbol handled above"),
                };
                signature.returns.push(AbiParam::new(result));
            }
            let id = self
                .jit
                .declare_function(name, Linkage::Import, &signature)
                .map_err(module_error)?;
            self.runtime_ids.insert(name, id);
        }
        Ok(())
    }
}

/// Bind every [`task_functions`] address into `builder`'s symbol table, for
/// use from `PreparedProgram::prepare` alongside `aster_runtime::runtime_functions()`.
pub(super) fn bind_task_functions(builder: &mut super::JITBuilder) {
    for (name, address) in task_functions() {
        builder.symbol(name, address);
    }
}

/// Reborrow the `TaskRuntime` the host registered via
/// `ExecutionContext::set_task_runtime`, or report the controlled "no task
/// support here" error and return `None`.
///
/// # Safety
/// The host that called `set_task_runtime` must guarantee the pointer is a
/// live `TaskRuntime` for the whole duration of this invocation (see
/// `execution::execute_resolved`), and that it is never aliased by any
/// other live reference while this call runs (this crate only ever calls
/// into a `TaskRuntime` from the single thread running the top-level
/// entry function; a worker's own `PreparedProgram` never registers one).
#[allow(unsafe_code)]
unsafe fn task_runtime(context: &mut ExecutionContext) -> Option<&mut TaskRuntime> {
    let pointer = context.task_runtime()?;
    // SAFETY: forwarded from this function's own contract, documented above.
    Some(unsafe { &mut *pointer.cast::<TaskRuntime>() })
}

/// `aster.core.Task.Run(function)`. `function_symbol` is `SymbolId::0` of a
/// resolved, zero-parameter free function or static method, embedded as a
/// compile-time constant by codegen. Submits one task to the `TaskRuntime`
/// the host registered and returns its `TaskHandleId` bits as a plain `i64`
/// (never a pointer), or a sentinel on a controlled error (no task runtime
/// registered, or the pool already shut down).
extern "C" fn aster_task_run(context: *mut ExecutionContext, function_symbol: i32) -> i64 {
    if context.is_null() {
        return 0;
    }
    // SAFETY: generated code passes the live, host-owned context as its
    // hidden first argument; it cannot outlive the invocation.
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    // SAFETY: `task_runtime`'s own contract, upheld by the host.
    #[allow(unsafe_code)]
    let Some(runtime) = (unsafe { task_runtime(context) }) else {
        context
            .fail("Task.Run is not available from this entry point (no task runtime registered)");
        return 0;
    };
    let symbol = mir::SymbolId(u32::from_ne_bytes(function_symbol.to_ne_bytes()));
    match runtime.run(symbol) {
        Ok(id) => id.to_bits(),
        Err(error) => {
            context.fail(error.message().to_owned());
            0
        }
    }
}

/// Join `id`'s task through the host's `TaskRuntime` (joining exactly once;
/// repeat calls on the same id replay the cached result, see
/// `task_runtime::TaskRuntime::wait`) and report every failure mode through
/// `context.fail` instead of panicking: a controlled Aster runtime error
/// from the task, a disconnected response channel (the worker terminated
/// before answering), an unknown id, or a result of the wrong concrete type
/// (which validated MIR never produces, but this stays defensive rather
/// than transmuting blindly).
fn wait(context: *mut ExecutionContext, handle: i64) -> Option<ExecutionValue> {
    if context.is_null() {
        return None;
    }
    // SAFETY: generated code passes the live, host-owned context as its
    // hidden first argument; it cannot outlive the invocation.
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    let Some(pointer) = context.task_runtime() else {
        context.fail(
            "Task<T>.Wait is not available from this entry point (no task runtime registered)",
        );
        return None;
    };
    let runtime = pointer.cast::<TaskRuntime>();
    let id = TaskHandleId::from_bits(handle);
    // Classify with a borrow that ends before any pump step runs, then either
    // join a plain task or drive the async pump. For an async task no `&mut
    // TaskRuntime` is held across `pump`: it reborrows the runtime through the
    // per-step context pointer instead (see `task_runtime`). `context` above
    // is a different object than the runtime, so this holds no aliasing borrow.
    // SAFETY: the host guarantees `runtime` is live and unaliased for the call.
    #[allow(unsafe_code)]
    let is_async = unsafe { (*runtime).is_async_handle(id) };
    #[allow(unsafe_code)]
    let outcome = if is_async {
        // SAFETY: `pump`'s own contract; no runtime borrow is live across it.
        unsafe { TaskRuntime::pump(runtime, id) }
    } else {
        // SAFETY: short-lived exclusive borrow for a single plain join.
        unsafe { (*runtime).wait(id) }
    };
    match outcome {
        Ok(TaskOutcome::Completed(value, _stats)) => Some(value),
        Ok(TaskOutcome::Failed(error)) | Err(error) => {
            // A `TaskOutcome::Failed` message already carries the "Aster
            // runtime error: " prefix from the worker's own invocation (see
            // `execution::PreparedProgram::invoke`); this call's own
            // top-level invocation adds that same prefix again once this
            // returns, so strip a duplicate here instead of doubling it.
            let message = error
                .message()
                .strip_prefix("Aster runtime error: ")
                .unwrap_or(error.message());
            context.fail(message.to_owned());
            None
        }
    }
}

extern "C" fn aster_task_wait_i32(context: *mut ExecutionContext, handle: i64) -> i32 {
    match wait(context, handle) {
        Some(ExecutionValue::Int(value)) => value,
        Some(ExecutionValue::UInt(value)) => i32::from_ne_bytes(value.to_ne_bytes()),
        Some(ExecutionValue::Char(value)) => i32::from_ne_bytes((value as u32).to_ne_bytes()),
        _ => 0,
    }
}

extern "C" fn aster_task_wait_i64(context: *mut ExecutionContext, handle: i64) -> i64 {
    match wait(context, handle) {
        Some(ExecutionValue::Long(value)) => value,
        Some(ExecutionValue::ULong(value)) => i64::from_ne_bytes(value.to_ne_bytes()),
        _ => 0,
    }
}

extern "C" fn aster_task_wait_f32(context: *mut ExecutionContext, handle: i64) -> f32 {
    match wait(context, handle) {
        Some(ExecutionValue::Float(value)) => value,
        _ => 0.0,
    }
}

extern "C" fn aster_task_wait_f64(context: *mut ExecutionContext, handle: i64) -> f64 {
    match wait(context, handle) {
        Some(ExecutionValue::Double(value)) => value,
        _ => 0.0,
    }
}

extern "C" fn aster_task_wait_i8(context: *mut ExecutionContext, handle: i64) -> i8 {
    match wait(context, handle) {
        Some(ExecutionValue::SByte(value)) => value,
        Some(ExecutionValue::Byte(value)) => i8::from_ne_bytes(value.to_ne_bytes()),
        Some(ExecutionValue::Bool(value)) => i8::from(value),
        _ => 0,
    }
}

extern "C" fn aster_task_wait_i16(context: *mut ExecutionContext, handle: i64) -> i16 {
    match wait(context, handle) {
        Some(ExecutionValue::Short(value)) => value,
        Some(ExecutionValue::UShort(value)) => i16::from_ne_bytes(value.to_ne_bytes()),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn waiting_on_an_invalid_handle_is_a_controlled_error_not_a_dereference() {
        let module = aster_compiler::compile("public int Compute() { return 1; }")
            .expect("source compiles")
            .mir;
        let mut runtime =
            TaskRuntime::new(&Arc::new(module), 1).expect("runtime starts with no tasks yet");
        let mut context = ExecutionContext::new();
        context.set_task_runtime(std::ptr::from_mut(&mut runtime).cast::<()>());

        let bogus_handle: i64 = i64::MAX;
        let result = aster_task_wait_i32(&raw mut context, bogus_handle);

        assert_eq!(result, 0);
        assert!(
            context
                .take_error()
                .is_some_and(|error| error.contains("unknown task handle"))
        );
    }

    #[test]
    fn a_null_context_never_dereferences_anything() {
        assert_eq!(aster_task_run(std::ptr::null_mut(), 0), 0);
        assert_eq!(aster_task_wait_i32(std::ptr::null_mut(), 0), 0);
    }
}
