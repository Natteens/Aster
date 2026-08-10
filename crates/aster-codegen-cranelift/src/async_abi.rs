//! ABI bridge for the async state machine and `Parallel`, between JIT code and
//! the internal [`task_runtime`].
//!
//! These functions are bound into every `PreparedProgram`'s `JITModule` just
//! like `task_abi`'s, but they live here because they touch
//! [`TaskRuntime`], which `aster-runtime` must never depend on. Every one is
//! controlled: a null context, a missing runtime, or an unknown handle is
//! reported through `ExecutionContext::fail` and never panics.
//!
//! `Task<T>` and every scalar cross this boundary as plain integers (a handle,
//! or a `(kind, i64 bits)` pair — see `scalar`), never as a pointer into any
//! arena. The only pointer that crosses is a `Parallel.ForEach`/`Reduce`
//! source array, and it is read and copied entirely on the host before any
//! worker runs (see [`aster_parallel_for_each`]/[`aster_parallel_reduce`]); no
//! array pointer ever reaches a worker. `Parallel.Reduce`'s identity and
//! combined result cross the same way a `Task<T>.Wait` result or async frame
//! slot does: as a `(kind, bits)` pair, never a pointer.

use aster_runtime::{ExecutionContext, aster_rt_array_element, aster_rt_array_length};

use super::task_runtime::{TaskHandleId, TaskRuntime};
use super::{
    AbiParam, BackendError, Codegen, ExecutionValue, Linkage, Module, mir, module_error, scalar,
    types,
};

/// Name and address of every async/parallel ABI function exported to generated
/// code, alongside `task_abi::task_functions()`.
fn async_functions() -> [(&'static str, *const u8); 11] {
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
        ("aster_parallel_reduce", aster_parallel_reduce as *const u8),
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
                "aster_parallel_reduce" => {
                    signature.params.push(AbiParam::new(pointer)); // array header
                    signature.params.push(AbiParam::new(types::I64)); // identity bits
                    signature.params.push(AbiParam::new(types::I32)); // identity (accumulator) kind
                    signature.params.push(AbiParam::new(types::I32)); // element kind
                    signature.params.push(AbiParam::new(types::I32)); // accumulate symbol
                    signature.params.push(AbiParam::new(types::I32)); // combine symbol
                    signature.returns.push(AbiParam::new(types::I64)); // accumulator result bits
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
    let Some((context, runtime)) = (unsafe { context_and_runtime(context) }) else {
        return 0;
    };
    let Ok(slots) = usize::try_from(slot_count) else {
        context.fail(format!(
            "async frame slot count cannot be negative: {slot_count}"
        ));
        return 0;
    };
    let symbol = mir::SymbolId(u32::from_ne_bytes(move_next.to_ne_bytes()));
    match runtime.async_spawn(symbol, slots) {
        Ok(handle) => handle.to_bits(),
        Err(error) => {
            report(context, &error);
            0
        }
    }
}

extern "C" fn aster_async_state(context: *mut ExecutionContext, handle: i64) -> i32 {
    #[allow(unsafe_code)]
    let Some((context, runtime)) = (unsafe { context_and_runtime(context) }) else {
        return 0;
    };
    match runtime.async_state(TaskHandleId::from_bits(handle)) {
        Ok(state) => state,
        Err(error) => {
            report(context, &error);
            0
        }
    }
}

extern "C" fn aster_async_set_state(context: *mut ExecutionContext, handle: i64, state: i32) {
    #[allow(unsafe_code)]
    let Some((context, runtime)) = (unsafe { context_and_runtime(context) }) else {
        return;
    };
    if let Err(error) = runtime.async_set_state(TaskHandleId::from_bits(handle), state) {
        report(context, &error);
    }
}

extern "C" fn aster_async_store_slot(
    context: *mut ExecutionContext,
    handle: i64,
    index: i32,
    kind: i32,
    bits: i64,
) {
    #[allow(unsafe_code)]
    let Some((context, runtime)) = (unsafe { context_and_runtime(context) }) else {
        return;
    };
    let Ok(index) = usize::try_from(index) else {
        context.fail(format!("async frame slot cannot be negative: {index}"));
        return;
    };
    let value = match scalar::from_bits(kind, bits) {
        Ok(value) => value,
        Err(error) => {
            report(context, &error);
            return;
        }
    };
    if let Err(error) = runtime.async_store_slot(TaskHandleId::from_bits(handle), index, value) {
        report(context, &error);
    }
}

extern "C" fn aster_async_load_slot(
    context: *mut ExecutionContext,
    handle: i64,
    index: i32,
) -> i64 {
    #[allow(unsafe_code)]
    let Some((context, runtime)) = (unsafe { context_and_runtime(context) }) else {
        return 0;
    };
    let Ok(index) = usize::try_from(index) else {
        context.fail(format!("async frame slot cannot be negative: {index}"));
        return 0;
    };
    match runtime.async_load_slot(TaskHandleId::from_bits(handle), index) {
        Ok(bits) => bits,
        Err(error) => {
            report(context, &error);
            0
        }
    }
}

extern "C" fn aster_async_spawn_inner(context: *mut ExecutionContext, handle: i64, inner: i32) {
    #[allow(unsafe_code)]
    let Some((context, runtime)) = (unsafe { context_and_runtime(context) }) else {
        return;
    };
    let inner = mir::SymbolId(u32::from_ne_bytes(inner.to_ne_bytes()));
    if let Err(error) = runtime.async_spawn_inner(TaskHandleId::from_bits(handle), inner) {
        report(context, &error);
    }
}

extern "C" fn aster_async_await_result(context: *mut ExecutionContext, handle: i64) -> i64 {
    #[allow(unsafe_code)]
    let Some((context, runtime)) = (unsafe { context_and_runtime(context) }) else {
        return 0;
    };
    match runtime.async_await_result(TaskHandleId::from_bits(handle)) {
        Ok(bits) => bits,
        Err(error) => {
            report(context, &error);
            0
        }
    }
}

extern "C" fn aster_async_set_result(
    context: *mut ExecutionContext,
    handle: i64,
    kind: i32,
    bits: i64,
) {
    #[allow(unsafe_code)]
    let Some((context, runtime)) = (unsafe { context_and_runtime(context) }) else {
        return;
    };
    let value = match scalar::from_bits(kind, bits) {
        Ok(value) => value,
        Err(error) => {
            report(context, &error);
            return;
        }
    };
    if let Err(error) = runtime.async_set_result(TaskHandleId::from_bits(handle), value) {
        report(context, &error);
    }
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
    let values = match copy_scalar_array(context, array, kind) {
        Ok(values) => values,
        Err(error) => {
            report(context, &error);
            return;
        }
    };
    if let Err(error) = runtime.parallel_for_each(values, body) {
        report(context, &error);
    }
}

/// `Parallel.Reduce(values, identity, Accumulate, Combine)`: `values` are
/// copied host-side exactly like [`aster_parallel_for_each`]'s array (no
/// array pointer ever reaches a worker); `identity` crosses as a `(kind,
/// bits)` pair, exactly like an async frame slot, never a pointer. The
/// combined `TAccumulator` result is returned the same way `Task<T>.Wait`
/// returns its scalar: as raw bits the caller narrows back to the concrete
/// type (see `calls::translate_async_intrinsic`).
extern "C" fn aster_parallel_reduce(
    context: *mut ExecutionContext,
    array: *mut u8,
    identity_bits: i64,
    identity_kind: i32,
    element_kind: i32,
    accumulate: i32,
    combine: i32,
) -> i64 {
    #[allow(unsafe_code)]
    let Some((context, runtime)) = (unsafe { context_and_runtime(context) }) else {
        return 0;
    };
    let identity = match scalar::from_bits(identity_kind, identity_bits) {
        Ok(value) => value,
        Err(error) => {
            report(context, &error);
            return 0;
        }
    };
    let values = match copy_scalar_array(context, array, element_kind) {
        Ok(values) => values,
        Err(error) => {
            report(context, &error);
            return 0;
        }
    };
    let accumulate = mir::SymbolId(u32::from_ne_bytes(accumulate.to_ne_bytes()));
    let combine = mir::SymbolId(u32::from_ne_bytes(combine.to_ne_bytes()));
    match runtime.parallel_reduce(values, identity, accumulate, combine) {
        Ok(result) => scalar::to_bits(&result),
        Err(error) => {
            report(context, &error);
            0
        }
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
) -> Result<Vec<ExecutionValue>, BackendError> {
    if array.is_null() {
        return Err(BackendError::new("Parallel.ForEach received a null array"));
    }
    if (array as usize) % std::mem::align_of::<aster_runtime::AsterArray>() != 0 {
        return Err(BackendError::new(
            "Parallel.ForEach received a misaligned array header",
        ));
    }
    // The arena that produced `array` always allocates `AsterArray` headers
    // at `align_of::<AsterArray>()` (see `aster_runtime::context`), so this
    // pointer is correctly aligned despite the `*mut u8` parameter type.
    let header = array.cast::<aster_runtime::AsterArray>();
    let expected_size = scalar::byte_width(kind)?;
    // SAFETY: the pointer is non-null and alignment-checked above. Valid Aster
    // MIR obtains it from this live context's array allocator; no pointer is
    // retained after this host-side copy.
    let actual_size = usize::try_from(unsafe { (&*header).element_size() })
        .map_err(|_| BackendError::new("Parallel.ForEach element size exceeds the platform"))?;
    if actual_size != expected_size {
        return Err(BackendError::new(format!(
            "Parallel.ForEach element size mismatch: header has {actual_size} bytes, scalar kind requires {expected_size}"
        )));
    }
    let length = aster_rt_array_length(context, header);
    let length = usize::try_from(length)
        .map_err(|_| BackendError::new("Parallel.ForEach array has a negative length"))?;
    length.checked_mul(expected_size).ok_or_else(|| {
        BackendError::new("Parallel.ForEach array byte length exceeds the addressable range")
    })?;
    let mut values = Vec::new();
    values.try_reserve_exact(length).map_err(|_| {
        BackendError::new("Parallel.ForEach scalar copy exceeds available host memory")
    })?;
    for index in 0..length {
        let element = aster_rt_array_element(
            context,
            header,
            i32::try_from(index)
                .map_err(|_| BackendError::new("Parallel.ForEach array index exceeds `int`"))?,
        );
        if element.is_null() {
            return Err(BackendError::new(
                "Parallel.ForEach could not read an array element",
            ));
        }
        // SAFETY: `validation::validate_intrinsic_shape` requires the array
        // element type, body parameter, and scalar `kind` metadata to agree.
        // `element` therefore points to one live, correctly sized scalar in
        // the host context's arena for the duration of this call.
        values.push(unsafe { read_scalar(element, kind) }?);
    }
    Ok(values)
}

/// Read one scalar of `kind` from `pointer` using an unaligned load.
///
/// # Safety
/// `pointer` must point to a live, correctly sized scalar of `kind`.
#[allow(unsafe_code)]
unsafe fn read_scalar(pointer: *const u8, kind: i32) -> Result<ExecutionValue, BackendError> {
    unsafe {
        Ok(match kind {
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
            scalar::CHAR => {
                let bits = pointer.cast::<u32>().read_unaligned();
                ExecutionValue::Char(char::from_u32(bits).ok_or_else(|| {
                    BackendError::new(format!(
                        "invalid Unicode scalar value U+{bits:08X} in Parallel.ForEach array"
                    ))
                })?)
            }
            scalar::LONG => ExecutionValue::Long(pointer.cast::<i64>().read_unaligned()),
            _ => return Err(BackendError::new(format!("unknown scalar kind tag {kind}"))),
        })
    }
}
