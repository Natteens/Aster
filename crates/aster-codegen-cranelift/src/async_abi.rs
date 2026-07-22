//! ABI bridge for the async state machine and `Parallel`, between JIT code and
//! the internal [`task_runtime`].
//!
//! These functions are bound into every `PreparedProgram`'s `JITModule` just
//! like `task_abi`'s, but they live here because they touch
//! [`TaskRuntime`], which `aster-runtime` must never depend on. Every one is
//! controlled: a null context, a missing runtime, or an unknown handle is
//! reported through `ExecutionContext::fail` (or ignored) and never panics.
//!
//! `Task<T>` and every scalar cross this boundary as plain integers (a handle,
//! or a `(kind, i64 bits)` pair — see `scalar`), never as a pointer into any
//! arena. The only pointer that crosses is a `Parallel.ForEach` source array,
//! and it is read and copied entirely on the host before any worker runs (see
//! [`aster_parallel_for_each`]); no array pointer ever reaches a worker.

use aster_runtime::{ExecutionContext, aster_rt_array_element, aster_rt_array_length};

use super::task_runtime::{TaskHandleId, TaskRuntime};
use super::{
    AbiParam, BackendError, Codegen, ExecutionValue, Linkage, Module, mir, module_error, scalar,
    types,
};

/// Name and address of every async/parallel ABI function exported to generated
/// code, alongside `task_abi::task_functions()`.
fn async_functions() -> [(&'static str, *const u8); 10] {
    [
        ("aster_async_spawn", aster_async_spawn as *const u8),
        ("aster_async_state", aster_async_state as *const u8),
        ("aster_async_set_state", aster_async_set_state as *const u8),
        (
            "aster_async_store_slot",
            aster_async_store_slot as *const u8,
        ),
        ("aster_async_load_slot", aster_async_load_slot as *const u8),
        (
            "aster_async_spawn_inner",
            aster_async_spawn_inner as *const u8,
        ),
        (
            "aster_async_await_result",
            aster_async_await_result as *const u8,
        ),
        (
            "aster_async_set_result",
            aster_async_set_result as *const u8,
        ),
        ("aster_parallel_for", aster_parallel_for as *const u8),
        (
            "aster_parallel_for_each",
            aster_parallel_for_each as *const u8,
        ),
    ]
}

impl Codegen {
    /// Declare every [`async_functions`] symbol as an importable function on
    /// `self.jit`, recording its `FuncId` in `self.runtime_ids`.
    pub(super) fn declare_async_functions(&mut self) -> Result<(), BackendError> {
        let pointer = self.pointer_type;
        for (name, _) in async_functions() {
            let mut signature = self.jit.make_signature();
            signature.params.push(AbiParam::new(pointer)); // context
            match name {
                "aster_async_spawn" => {
                    signature.params.push(AbiParam::new(types::I32)); // move_next
                    signature.params.push(AbiParam::new(types::I32)); // slot_count
                    signature.returns.push(AbiParam::new(types::I64)); // handle
                }
                "aster_async_state" => {
                    signature.params.push(AbiParam::new(types::I64)); // handle
                    signature.returns.push(AbiParam::new(types::I32)); // state
                }
                "aster_async_set_state" => {
                    signature.params.push(AbiParam::new(types::I64)); // handle
                    signature.params.push(AbiParam::new(types::I32)); // state
                }
                "aster_async_store_slot" => {
                    signature.params.push(AbiParam::new(types::I64)); // handle
                    signature.params.push(AbiParam::new(types::I32)); // index
                    signature.params.push(AbiParam::new(types::I32)); // kind
                    signature.params.push(AbiParam::new(types::I64)); // bits
                }
                "aster_async_load_slot" => {
                    signature.params.push(AbiParam::new(types::I64)); // handle
                    signature.params.push(AbiParam::new(types::I32)); // index
                    signature.returns.push(AbiParam::new(types::I64)); // bits
                }
                "aster_async_spawn_inner" => {
                    signature.params.push(AbiParam::new(types::I64)); // handle
                    signature.params.push(AbiParam::new(types::I32)); // inner symbol
                }
                "aster_async_await_result" => {
                    signature.params.push(AbiParam::new(types::I64)); // handle
                    signature.returns.push(AbiParam::new(types::I64)); // bits
                }
                "aster_async_set_result" => {
                    signature.params.push(AbiParam::new(types::I64)); // handle
                    signature.params.push(AbiParam::new(types::I32)); // kind
                    signature.params.push(AbiParam::new(types::I64)); // bits
                }
                "aster_parallel_for" => {
                    signature.params.push(AbiParam::new(types::I32)); // start
                    signature.params.push(AbiParam::new(types::I32)); // end
                    signature.params.push(AbiParam::new(types::I32)); // body symbol
                }
                "aster_parallel_for_each" => {
                    signature.params.push(AbiParam::new(pointer)); // array header
                    signature.params.push(AbiParam::new(types::I32)); // body symbol
                    signature.params.push(AbiParam::new(types::I32)); // element kind
                }
                _ => unreachable!("async_functions lists every symbol handled above"),
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

/// Bind every [`async_functions`] address into `builder`'s symbol table.
pub(super) fn bind_async_functions(builder: &mut super::JITBuilder) {
    for (name, address) in async_functions() {
        builder.symbol(name, address);
    }
}

/// Reborrow the host's `TaskRuntime`, or report the controlled "no task
/// support here" error and return `None`.
///
/// # Safety
/// The host that registered the runtime guarantees the pointer is live for the
/// whole call and, per the pump's discipline (see `task_runtime`), is not
/// aliased by any live Rust reference while this call runs.
#[allow(unsafe_code)]
unsafe fn runtime<'a>(context: &mut ExecutionContext) -> Option<&'a mut TaskRuntime> {
    let pointer = context.task_runtime()?;
    // SAFETY: forwarded from this function's own contract.
    Some(unsafe { &mut *pointer.cast::<TaskRuntime>() })
}

/// Get `(context, runtime)` or report a controlled error and return `None`.
#[allow(unsafe_code)]
unsafe fn context_and_runtime<'a>(
    context: *mut ExecutionContext,
) -> Option<(&'a mut ExecutionContext, &'a mut TaskRuntime)> {
    if context.is_null() {
        return None;
    }
    // SAFETY: generated code passes the live, host-owned context.
    let context = unsafe { &mut *context };
    // SAFETY: `runtime`'s own contract.
    let runtime = unsafe { runtime(context) };
    if let Some(runtime) = runtime {
        Some((context, runtime))
    } else {
        context.fail(
            "this concurrency operation requires a task runtime, which is not available from this entry point",
        );
        None
    }
}

extern "C" fn aster_async_spawn(
    context: *mut ExecutionContext,
    move_next: i32,
    slot_count: i32,
) -> i64 {
    #[allow(unsafe_code)]
    let Some((_context, runtime)) = (unsafe { context_and_runtime(context) }) else {
        return 0;
    };
    let symbol = mir::SymbolId(u32::from_ne_bytes(move_next.to_ne_bytes()));
    let slots = usize::try_from(slot_count).unwrap_or(0);
    runtime.async_spawn(symbol, slots).to_bits()
}

extern "C" fn aster_async_state(context: *mut ExecutionContext, handle: i64) -> i32 {
    #[allow(unsafe_code)]
    let Some((_context, runtime)) = (unsafe { context_and_runtime(context) }) else {
        return 0;
    };
    runtime.async_state(TaskHandleId::from_bits(handle))
}

extern "C" fn aster_async_set_state(context: *mut ExecutionContext, handle: i64, state: i32) {
    #[allow(unsafe_code)]
    let Some((_context, runtime)) = (unsafe { context_and_runtime(context) }) else {
        return;
    };
    runtime.async_set_state(TaskHandleId::from_bits(handle), state);
}

extern "C" fn aster_async_store_slot(
    context: *mut ExecutionContext,
    handle: i64,
    index: i32,
    kind: i32,
    bits: i64,
) {
    #[allow(unsafe_code)]
    let Some((_context, runtime)) = (unsafe { context_and_runtime(context) }) else {
        return;
    };
    let Ok(index) = usize::try_from(index) else {
        return;
    };
    runtime.async_store_slot(
        TaskHandleId::from_bits(handle),
        index,
        scalar::from_bits(kind, bits),
    );
}

extern "C" fn aster_async_load_slot(
    context: *mut ExecutionContext,
    handle: i64,
    index: i32,
) -> i64 {
    #[allow(unsafe_code)]
    let Some((_context, runtime)) = (unsafe { context_and_runtime(context) }) else {
        return 0;
    };
    let Ok(index) = usize::try_from(index) else {
        return 0;
    };
    runtime.async_load_slot(TaskHandleId::from_bits(handle), index)
}

extern "C" fn aster_async_spawn_inner(context: *mut ExecutionContext, handle: i64, inner: i32) {
    #[allow(unsafe_code)]
    let Some((_context, runtime)) = (unsafe { context_and_runtime(context) }) else {
        return;
    };
    let inner = mir::SymbolId(u32::from_ne_bytes(inner.to_ne_bytes()));
    runtime.async_spawn_inner(TaskHandleId::from_bits(handle), inner);
}

extern "C" fn aster_async_await_result(context: *mut ExecutionContext, handle: i64) -> i64 {
    #[allow(unsafe_code)]
    let Some((_context, runtime)) = (unsafe { context_and_runtime(context) }) else {
        return 0;
    };
    runtime.async_await_result(TaskHandleId::from_bits(handle))
}

extern "C" fn aster_async_set_result(
    context: *mut ExecutionContext,
    handle: i64,
    kind: i32,
    bits: i64,
) {
    #[allow(unsafe_code)]
    let Some((_context, runtime)) = (unsafe { context_and_runtime(context) }) else {
        return;
    };
    runtime.async_set_result(
        TaskHandleId::from_bits(handle),
        scalar::from_bits(kind, bits),
    );
}

extern "C" fn aster_parallel_for(context: *mut ExecutionContext, start: i32, end: i32, body: i32) {
    #[allow(unsafe_code)]
    let Some((context, runtime)) = (unsafe { context_and_runtime(context) }) else {
        return;
    };
    let body = mir::SymbolId(u32::from_ne_bytes(body.to_ne_bytes()));
    if let Err(error) = runtime.parallel_for(start, end, body) {
        report(context, &error);
    }
}

extern "C" fn aster_parallel_for_each(
    context: *mut ExecutionContext,
    array: *mut u8,
    body: i32,
    kind: i32,
) {
    #[allow(unsafe_code)]
    let Some((context, runtime)) = (unsafe { context_and_runtime(context) }) else {
        return;
    };
    let body = mir::SymbolId(u32::from_ne_bytes(body.to_ne_bytes()));
    // Evaluate the array once and copy its scalar elements into host-owned
    // storage before any worker runs. Only owned scalars reach the pool.
    let values = copy_scalar_array(context, array, kind);
    if let Err(error) = runtime.parallel_for_each(values, body) {
        report(context, &error);
    }
}

/// A `Parallel` failure that already carries the worker's "Aster runtime
/// error: " prefix is reported through `context.fail` without doubling it (the
/// outer top-level invocation adds the same prefix once this returns).
fn report(context: &mut ExecutionContext, error: &BackendError) {
    let message = error
        .message()
        .strip_prefix("Aster runtime error: ")
        .unwrap_or(error.message());
    context.fail(message.to_owned());
}

/// Read every element of the host-side array `array` as a scalar of `kind`
/// into an owned `Vec`. Empty (including a null array, already reported by
/// the runtime's own bounds machinery, or a bounds failure partway through)
/// rather than a panic.
#[allow(unsafe_code, clippy::cast_ptr_alignment)]
fn copy_scalar_array(
    context: &mut ExecutionContext,
    array: *mut u8,
    kind: i32,
) -> Vec<ExecutionValue> {
    if array.is_null() {
        return Vec::new();
    }
    // The arena that produced `array` always allocates `AsterArray` headers
    // at `align_of::<AsterArray>()` (see `aster_runtime::context`), so this
    // pointer is correctly aligned despite the `*mut u8` parameter type.
    let header = array.cast::<aster_runtime::AsterArray>();
    let length = aster_rt_array_length(context, header);
    let length = usize::try_from(length).unwrap_or(0);
    let mut values = Vec::with_capacity(length);
    for index in 0..length {
        let element = aster_rt_array_element(
            context,
            header,
            i32::try_from(index).expect("array index fits i32"),
        );
        if element.is_null() {
            return values;
        }
        // SAFETY: `element` points to one live, `element_size`-byte scalar of
        // the concrete `kind` inside the host context's arena, valid for the
        // duration of this host-thread call.
        values.push(unsafe { read_scalar(element, kind) });
    }
    values
}

/// Read one scalar of `kind` from `pointer` using an unaligned load.
///
/// # Safety
/// `pointer` must point to a live, correctly sized scalar of `kind`.
#[allow(unsafe_code)]
unsafe fn read_scalar(pointer: *const u8, kind: i32) -> ExecutionValue {
    unsafe {
        match kind {
            scalar::BOOL => ExecutionValue::Bool(pointer.read() != 0),
            scalar::SBYTE => ExecutionValue::SByte(pointer.cast::<i8>().read()),
            scalar::BYTE => ExecutionValue::Byte(pointer.read()),
            scalar::SHORT => ExecutionValue::Short(pointer.cast::<i16>().read_unaligned()),
            scalar::USHORT => ExecutionValue::UShort(pointer.cast::<u16>().read_unaligned()),
            scalar::INT => ExecutionValue::Int(pointer.cast::<i32>().read_unaligned()),
            scalar::UINT => ExecutionValue::UInt(pointer.cast::<u32>().read_unaligned()),
            scalar::ULONG => ExecutionValue::ULong(pointer.cast::<u64>().read_unaligned()),
            scalar::FLOAT => ExecutionValue::Float(pointer.cast::<f32>().read_unaligned()),
            scalar::DOUBLE => ExecutionValue::Double(pointer.cast::<f64>().read_unaligned()),
            scalar::CHAR => ExecutionValue::Char(
                char::from_u32(pointer.cast::<u32>().read_unaligned()).unwrap_or('\u{0}'),
            ),
            // `LONG` and any unrecognized kind share this fallback.
            _ => ExecutionValue::Long(pointer.cast::<i64>().read_unaligned()),
        }
    }
}
