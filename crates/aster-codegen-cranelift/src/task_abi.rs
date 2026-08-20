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
fn task_functions() -> [(&'static str, *const u8); 16] {
    [
        ("aster_task_run", aster_task_run as *const u8),
        ("aster_task_run_args", aster_task_run_args as *const u8),
        ("aster_task_cancel", aster_task_cancel as *const u8),
        (
            "aster_task_cancellation_requested",
            aster_task_cancellation_requested as *const u8,
        ),
        ("aster_task_wait_i8", aster_task_wait_i8 as *const u8),
        ("aster_task_wait_i16", aster_task_wait_i16 as *const u8),
        ("aster_task_wait_i32", aster_task_wait_i32 as *const u8),
        ("aster_task_wait_i64", aster_task_wait_i64 as *const u8),
        ("aster_task_wait_f32", aster_task_wait_f32 as *const u8),
        ("aster_task_wait_f64", aster_task_wait_f64 as *const u8),
        (
            "aster_task_wait_all_i8",
            aster_task_wait_all_i8 as *const u8,
        ),
        (
            "aster_task_wait_all_i16",
            aster_task_wait_all_i16 as *const u8,
        ),
        (
            "aster_task_wait_all_i32",
            aster_task_wait_all_i32 as *const u8,
        ),
        (
            "aster_task_wait_all_i64",
            aster_task_wait_all_i64 as *const u8,
        ),
        (
            "aster_task_wait_all_f32",
            aster_task_wait_all_f32 as *const u8,
        ),
        (
            "aster_task_wait_all_f64",
            aster_task_wait_all_f64 as *const u8,
        ),
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

pub(super) fn wait_all_symbol_for(result_type: &mir::Type) -> Option<&'static str> {
    wait_symbol_for(result_type).and_then(|symbol| match symbol {
        "aster_task_wait_i8" => Some("aster_task_wait_all_i8"),
        "aster_task_wait_i16" => Some("aster_task_wait_all_i16"),
        "aster_task_wait_i32" => Some("aster_task_wait_all_i32"),
        "aster_task_wait_i64" => Some("aster_task_wait_all_i64"),
        "aster_task_wait_f32" => Some("aster_task_wait_all_f32"),
        "aster_task_wait_f64" => Some("aster_task_wait_all_f64"),
        _ => None,
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
            } else if name == "aster_task_run_args" {
                signature.params.push(AbiParam::new(types::I32));
                signature.params.push(AbiParam::new(self.pointer_type));
                signature.params.push(AbiParam::new(types::I32));
                signature.returns.push(AbiParam::new(types::I64));
            } else if name == "aster_task_cancel" {
                signature.params.push(AbiParam::new(types::I64));
                signature.returns.push(AbiParam::new(types::I8));
            } else if name == "aster_task_cancellation_requested" {
                signature.returns.push(AbiParam::new(types::I8));
            } else if name.starts_with("aster_task_wait_all_") {
                signature.params.push(AbiParam::new(self.pointer_type));
                signature.returns.push(AbiParam::new(self.pointer_type));
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

extern "C" fn aster_task_run_args(
    context: *mut ExecutionContext,
    function_symbol: i32,
    payload: *const u8,
    payload_size: i32,
) -> i64 {
    if context.is_null() {
        return 0;
    }
    // SAFETY: generated code passes its live context for this synchronous
    // copy-and-enqueue call.
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    let Ok(payload_size) = usize::try_from(payload_size) else {
        context.fail("Task.Run received an invalid argument frame size");
        return 0;
    };
    let payload = match super::worker_pool::TaskPayload::copy_from(payload, payload_size) {
        Ok(payload) => payload,
        Err(error) => {
            context.fail(error.message().to_owned());
            return 0;
        }
    };
    let Some(pointer) = context.task_runtime() else {
        context
            .fail("Task.Run is not available from this entry point (no task runtime registered)");
        return 0;
    };
    let symbol = mir::SymbolId(u32::from_ne_bytes(function_symbol.to_ne_bytes()));
    let runtime = pointer.cast::<TaskRuntime>();
    // SAFETY: the host owns and exclusively exposes this runtime for the
    // duration of the top-level execution.
    #[allow(unsafe_code)]
    match unsafe { (*runtime).run_with_payload_from_context(symbol, payload, context) } {
        Ok(id) => id.to_bits(),
        Err(error) => {
            context.fail(error.message().to_owned());
            0
        }
    }
}

extern "C" fn aster_task_cancel(context: *mut ExecutionContext, handle: i64) -> i8 {
    if context.is_null() {
        return 0;
    }
    // SAFETY: generated code passes its live context.
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    let Some(pointer) = context.task_runtime() else {
        context.fail(
            "Task<T>.Cancel is not available from this entry point (no task runtime registered)",
        );
        return 0;
    };
    let runtime = pointer.cast::<TaskRuntime>();
    // SAFETY: short exclusive runtime borrow; no JIT reentry occurs.
    #[allow(unsafe_code)]
    match unsafe { (*runtime).cancel(TaskHandleId::from_bits(handle)) } {
        Ok(accepted) => i8::from(accepted),
        Err(error) => {
            context.fail(error.message().to_owned());
            0
        }
    }
}

extern "C" fn aster_task_cancellation_requested(context: *mut ExecutionContext) -> i8 {
    if context.is_null() {
        return 0;
    }
    // SAFETY: generated code passes its live context and the query does not
    // retain it.
    #[allow(unsafe_code)]
    i8::from(unsafe { &*context }.is_task_cancellation_requested())
}

/// Bind every [`task_functions`] address into `builder`'s symbol table, for
/// use from `PreparedProgram::prepare` alongside `aster_runtime::runtime_functions()`.
pub(super) fn bind_task_functions(builder: &mut super::JITBuilder) {
    for (name, address) in task_functions() {
        builder.symbol(name, address);
    }
}

/// Zero-argument `aster.core.Task.Run(function)`. `function_symbol` identifies
/// a resolved free function or static method and is embedded as a compile-time
/// constant by codegen. Submits one task to the `TaskRuntime`
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
    let Some(pointer) = context.task_runtime() else {
        context
            .fail("Task.Run is not available from this entry point (no task runtime registered)");
        return 0;
    };
    let symbol = mir::SymbolId(u32::from_ne_bytes(function_symbol.to_ne_bytes()));
    let runtime = pointer.cast::<TaskRuntime>();
    // SAFETY: the host keeps this runtime live and exclusively reachable from
    // the top-level invocation. `context` is a separate owned object whose
    // governed local ceiling may be frozen by the runtime before submission.
    #[allow(unsafe_code)]
    match unsafe { (*runtime).run_from_context(symbol, context) } {
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
        // SAFETY: `pump_from_context`'s own contract; no runtime borrow is
        // live across it. The Main context borrow is used only to freeze the
        // async domain before any MoveNext step begins.
        unsafe { TaskRuntime::pump_from_context(runtime, id, context) }
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

fn report_wait_type_mismatch(
    context: *mut ExecutionContext,
    expected: &str,
    value: &ExecutionValue,
) {
    if context.is_null() {
        return;
    }
    // SAFETY: this helper is called only from the wait ABI entry points with
    // the same live context pointer generated code passed to `wait`.
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    context.fail(format!(
        "Task<T>.Wait expected {expected}, but the task produced {}",
        execution_value_kind(value)
    ));
}

fn execution_value_kind(value: &ExecutionValue) -> &'static str {
    match value {
        ExecutionValue::Bool(_) => "bool",
        ExecutionValue::SByte(_) => "sbyte",
        ExecutionValue::Byte(_) => "byte",
        ExecutionValue::Short(_) => "short",
        ExecutionValue::UShort(_) => "ushort",
        ExecutionValue::Int(_) => "int",
        ExecutionValue::UInt(_) => "uint",
        ExecutionValue::Long(_) => "long",
        ExecutionValue::ULong(_) => "ulong",
        ExecutionValue::Float(_) => "float",
        ExecutionValue::Double(_) => "double",
        ExecutionValue::Char(_) => "char",
        ExecutionValue::String(_) => "string",
        ExecutionValue::Void => "void",
    }
}

extern "C" fn aster_task_wait_i32(context: *mut ExecutionContext, handle: i64) -> i32 {
    match wait(context, handle) {
        Some(ExecutionValue::Int(value)) => value,
        Some(ExecutionValue::UInt(value)) => i32::from_ne_bytes(value.to_ne_bytes()),
        Some(ExecutionValue::Char(value)) => i32::from_ne_bytes((value as u32).to_ne_bytes()),
        Some(value) => {
            report_wait_type_mismatch(context, "an i32-compatible result", &value);
            0
        }
        None => 0,
    }
}

extern "C" fn aster_task_wait_i64(context: *mut ExecutionContext, handle: i64) -> i64 {
    match wait(context, handle) {
        Some(ExecutionValue::Long(value)) => value,
        Some(ExecutionValue::ULong(value)) => i64::from_ne_bytes(value.to_ne_bytes()),
        Some(value) => {
            report_wait_type_mismatch(context, "an i64-compatible result", &value);
            0
        }
        None => 0,
    }
}

extern "C" fn aster_task_wait_f32(context: *mut ExecutionContext, handle: i64) -> f32 {
    match wait(context, handle) {
        Some(ExecutionValue::Float(value)) => value,
        Some(value) => {
            report_wait_type_mismatch(context, "float", &value);
            0.0
        }
        None => 0.0,
    }
}

extern "C" fn aster_task_wait_f64(context: *mut ExecutionContext, handle: i64) -> f64 {
    match wait(context, handle) {
        Some(ExecutionValue::Double(value)) => value,
        Some(value) => {
            report_wait_type_mismatch(context, "double", &value);
            0.0
        }
        None => 0.0,
    }
}

extern "C" fn aster_task_wait_i8(context: *mut ExecutionContext, handle: i64) -> i8 {
    match wait(context, handle) {
        Some(ExecutionValue::SByte(value)) => value,
        Some(ExecutionValue::Byte(value)) => i8::from_ne_bytes(value.to_ne_bytes()),
        Some(ExecutionValue::Bool(value)) => i8::from(value),
        Some(value) => {
            report_wait_type_mismatch(context, "an i8-compatible result", &value);
            0
        }
        None => 0,
    }
}

extern "C" fn aster_task_wait_i16(context: *mut ExecutionContext, handle: i64) -> i16 {
    match wait(context, handle) {
        Some(ExecutionValue::Short(value)) => value,
        Some(ExecutionValue::UShort(value)) => i16::from_ne_bytes(value.to_ne_bytes()),
        Some(value) => {
            report_wait_type_mismatch(context, "an i16-compatible result", &value);
            0
        }
        None => 0,
    }
}

#[derive(Clone, Copy)]
enum WaitAllKind {
    I8,
    I16,
    I32,
    I64,
    F32,
    F64,
}

impl WaitAllKind {
    fn size(self) -> i32 {
        match self {
            Self::I8 => 1,
            Self::I16 => 2,
            Self::I32 | Self::F32 => 4,
            Self::I64 | Self::F64 => 8,
        }
    }
}

fn task_outcome(
    runtime: *mut TaskRuntime,
    id: TaskHandleId,
    context: &mut ExecutionContext,
) -> Result<TaskOutcome, BackendError> {
    // SAFETY: callers hold the host's exclusive runtime access for this ABI
    // call. Pump reborrows only between MoveNext invocations.
    #[allow(unsafe_code)]
    if unsafe { (*runtime).is_async_handle(id) } {
        // SAFETY: same contract as the single-task Wait path.
        #[allow(unsafe_code)]
        unsafe {
            TaskRuntime::pump_from_context(runtime, id, context)
        }
    } else {
        // SAFETY: short exclusive borrow for one cached/joined result.
        #[allow(unsafe_code)]
        unsafe {
            (*runtime).wait(id)
        }
    }
}

fn wait_all(
    context: *mut ExecutionContext,
    tasks: *mut aster_runtime::AsterArray,
    kind: WaitAllKind,
) -> *mut aster_runtime::AsterArray {
    if context.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: generated code passes the live caller context.
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    let Some(runtime_pointer) = context.task_runtime() else {
        context.fail(
            "Task.WaitAll is not available from this entry point (no task runtime registered)",
        );
        return std::ptr::null_mut();
    };
    if tasks.is_null() {
        context.fail("Task.WaitAll received a null task array");
        return std::ptr::null_mut();
    }
    let length = aster_runtime::aster_rt_array_length(context, tasks);
    if length < 0 || aster_runtime::context::aster_rt_has_error(context) != 0 {
        return std::ptr::null_mut();
    }
    let Ok(capacity) = usize::try_from(length) else {
        context.fail("Task.WaitAll task count exceeds host limits");
        return std::ptr::null_mut();
    };
    let mut outcomes = Vec::new();
    if outcomes.try_reserve_exact(capacity).is_err() {
        context.fail("Task.WaitAll could not allocate completion storage");
        return std::ptr::null_mut();
    }
    for index in 0..length {
        let element = aster_runtime::aster_rt_array_element(context, tasks, index);
        if element.is_null() {
            return std::ptr::null_mut();
        }
        // SAFETY: Task<T>[] has validated eight-byte handle elements and the
        // runtime returned the address of the requested initialized slot.
        #[allow(unsafe_code)]
        let bits = unsafe { element.cast::<i64>().read_unaligned() };
        outcomes.push(task_outcome(
            runtime_pointer.cast::<TaskRuntime>(),
            TaskHandleId::from_bits(bits),
            context,
        ));
    }
    for outcome in &outcomes {
        match outcome {
            Ok(TaskOutcome::Failed(error)) | Err(error) if !error.is_cancellation() => {
                let message = error
                    .message()
                    .strip_prefix("Aster runtime error: ")
                    .unwrap_or(error.message());
                context.fail(message.to_owned());
                return std::ptr::null_mut();
            }
            _ => {}
        }
    }
    if outcomes.iter().any(|outcome| {
        matches!(outcome, Ok(TaskOutcome::Failed(error)) | Err(error) if error.is_cancellation())
    })
    {
        context.fail("task was cancelled");
        return std::ptr::null_mut();
    }
    write_wait_all_results(context, length, kind, outcomes)
}

fn write_wait_all_results(
    context: &mut ExecutionContext,
    length: i32,
    kind: WaitAllKind,
    outcomes: Vec<Result<TaskOutcome, BackendError>>,
) -> *mut aster_runtime::AsterArray {
    let result = aster_runtime::context::aster_rt_array_new(context, length, kind.size());
    if result.is_null() {
        return std::ptr::null_mut();
    }
    for (index, outcome) in outcomes.into_iter().enumerate() {
        let Ok(index) = i32::try_from(index) else {
            context.fail("Task.WaitAll result index exceeds ASTER limits");
            return std::ptr::null_mut();
        };
        let destination = aster_runtime::aster_rt_array_element(context, result, index);
        if destination.is_null() {
            return std::ptr::null_mut();
        }
        let value = match outcome {
            Ok(TaskOutcome::Completed(value, _)) => value,
            Ok(TaskOutcome::Failed(error)) | Err(error) => {
                let message = error
                    .message()
                    .strip_prefix("Aster runtime error: ")
                    .unwrap_or(error.message());
                context.fail(message.to_owned());
                return std::ptr::null_mut();
            }
        };
        // SAFETY: destination belongs to the new caller-owned array and the
        // typed ABI kind fixes its slot width.
        #[allow(unsafe_code)]
        unsafe {
            match (kind, value) {
                (WaitAllKind::I8, ExecutionValue::Bool(value)) => {
                    destination.cast::<i8>().write(i8::from(value));
                }
                (WaitAllKind::I8, ExecutionValue::SByte(value)) => {
                    destination.cast::<i8>().write(value);
                }
                (WaitAllKind::I8, ExecutionValue::Byte(value)) => {
                    destination.cast::<u8>().write(value);
                }
                (WaitAllKind::I16, ExecutionValue::Short(value)) => {
                    destination.cast::<i16>().write_unaligned(value);
                }
                (WaitAllKind::I16, ExecutionValue::UShort(value)) => {
                    destination.cast::<u16>().write_unaligned(value);
                }
                (WaitAllKind::I32, ExecutionValue::Int(value)) => {
                    destination.cast::<i32>().write_unaligned(value);
                }
                (WaitAllKind::I32, ExecutionValue::UInt(value)) => {
                    destination.cast::<u32>().write_unaligned(value);
                }
                (WaitAllKind::I32, ExecutionValue::Char(value)) => {
                    destination.cast::<u32>().write_unaligned(value as u32);
                }
                (WaitAllKind::I64, ExecutionValue::Long(value)) => {
                    destination.cast::<i64>().write_unaligned(value);
                }
                (WaitAllKind::I64, ExecutionValue::ULong(value)) => {
                    destination.cast::<u64>().write_unaligned(value);
                }
                (WaitAllKind::F32, ExecutionValue::Float(value)) => {
                    destination.cast::<f32>().write_unaligned(value);
                }
                (WaitAllKind::F64, ExecutionValue::Double(value)) => {
                    destination.cast::<f64>().write_unaligned(value);
                }
                (_, value) => {
                    context.fail(format!(
                        "Task.WaitAll result type mismatch: produced {}",
                        execution_value_kind(&value)
                    ));
                    return std::ptr::null_mut();
                }
            }
        }
    }
    result
}

extern "C" fn aster_task_wait_all_i8(
    context: *mut ExecutionContext,
    tasks: *mut aster_runtime::AsterArray,
) -> *mut aster_runtime::AsterArray {
    wait_all(context, tasks, WaitAllKind::I8)
}

extern "C" fn aster_task_wait_all_i16(
    context: *mut ExecutionContext,
    tasks: *mut aster_runtime::AsterArray,
) -> *mut aster_runtime::AsterArray {
    wait_all(context, tasks, WaitAllKind::I16)
}

extern "C" fn aster_task_wait_all_i32(
    context: *mut ExecutionContext,
    tasks: *mut aster_runtime::AsterArray,
) -> *mut aster_runtime::AsterArray {
    wait_all(context, tasks, WaitAllKind::I32)
}

extern "C" fn aster_task_wait_all_i64(
    context: *mut ExecutionContext,
    tasks: *mut aster_runtime::AsterArray,
) -> *mut aster_runtime::AsterArray {
    wait_all(context, tasks, WaitAllKind::I64)
}

extern "C" fn aster_task_wait_all_f32(
    context: *mut ExecutionContext,
    tasks: *mut aster_runtime::AsterArray,
) -> *mut aster_runtime::AsterArray {
    wait_all(context, tasks, WaitAllKind::F32)
}

extern "C" fn aster_task_wait_all_f64(
    context: *mut ExecutionContext,
    tasks: *mut aster_runtime::AsterArray,
) -> *mut aster_runtime::AsterArray {
    wait_all(context, tasks, WaitAllKind::F64)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use aster_runtime::MemoryGovernor;

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

    #[test]
    fn waiting_through_an_incompatible_abi_type_reports_a_controlled_error() {
        let module = aster_compiler::compile("public long Compute() { return 1L; }")
            .expect("source compiles")
            .mir;
        let symbol = module.functions[0].symbol;
        let mut runtime =
            TaskRuntime::new(&Arc::new(module), 1).expect("runtime starts with no tasks yet");
        let handle = runtime.run(symbol).expect("task is accepted");
        let mut context = ExecutionContext::new();
        context.set_task_runtime(std::ptr::from_mut(&mut runtime).cast::<()>());

        let result = aster_task_wait_i32(&raw mut context, handle.to_bits());

        assert_eq!(result, 0);
        assert!(
            context
                .take_error()
                .is_some_and(|error| error.contains("expected an i32-compatible"))
        );
    }

    #[test]
    fn wait_all_allocation_denial_exposes_no_partial_result_and_keeps_handles_valid() {
        let module = aster_compiler::compile("public int Compute() { return 1; }")
            .expect("source compiles")
            .mir;
        let symbol = module.functions[0].symbol;
        let mut runtime =
            TaskRuntime::new(&Arc::new(module), 4).expect("runtime starts with no tasks yet");
        let handles = (0..500)
            .map(|_| runtime.run(symbol).expect("task is accepted"))
            .collect::<Vec<_>>();
        let governor = Arc::new(MemoryGovernor::new(
            ExecutionContext::AARM_MIN_PAGE_CAPACITY_BYTES,
        ));
        let mut context = ExecutionContext::with_memory_governor(Arc::clone(&governor));
        context.set_task_runtime(std::ptr::from_mut(&mut runtime).cast::<()>());
        let tasks = aster_runtime::context::aster_rt_array_new(&raw mut context, 500, 8);
        assert!(!tasks.is_null(), "the input handle array fits one page");
        for (index, handle) in handles.iter().enumerate() {
            let element = aster_runtime::aster_rt_array_element(
                &raw mut context,
                tasks,
                i32::try_from(index).expect("small task index"),
            );
            // SAFETY: the just-created Task<int>[] has initialized eight-byte
            // handle slots owned exclusively by this test context.
            #[allow(unsafe_code)]
            unsafe {
                element.cast::<i64>().write_unaligned(handle.to_bits());
            }
        }

        let result = aster_task_wait_all_i32(&raw mut context, tasks);
        assert!(result.is_null());
        let error = context
            .take_error()
            .expect("allocation denial is controlled");
        assert!(error.contains("execution memory budget"), "{error}");

        assert_eq!(
            aster_task_wait_i32(&raw mut context, handles[0].to_bits()),
            1,
            "WaitAll caches terminal outcomes before caller-array allocation"
        );
        assert_eq!(
            aster_task_wait_i32(&raw mut context, handles[499].to_bits()),
            1,
            "WaitAll reaches the final input before attempting result allocation"
        );
        assert!(context.take_error().is_none());
        drop(context);
        assert_eq!(governor.telemetry().current_capacity_bytes, 0);
    }
}
