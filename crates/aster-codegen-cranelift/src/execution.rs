use super::{
    BackendError, Codegen, ExecutionValue, JITBuilder, JITModule, default_libcall_names, mir,
    module_error, runtime_functions,
};

pub(super) fn execute_resolved(
    module: &mir::Module,
    entry: &mir::Function,
    collect_stats: bool,
) -> Result<(ExecutionValue, super::MemoryStats), BackendError> {
    let mut builder = JITBuilder::new(default_libcall_names()).map_err(module_error)?;
    for function in runtime_functions() {
        builder.symbol(function.name, function.address);
    }
    let jit = JITModule::new(builder);
    let mut codegen = Codegen::new(jit, module)?;
    let function_ids = codegen.declare_functions(module)?;
    codegen.define_interface_tables(module, &function_ids)?;
    for function in &module.functions {
        codegen.define_function(function, &function_ids)?;
    }
    codegen.jit.finalize_definitions().map_err(module_error)?;
    let entry_id = function_ids[&entry.symbol];
    let pointer = codegen.jit.get_finalized_function(entry_id);
    // The JITModule stays alive until after the invocation copies any result
    // (including string payloads) into host-owned memory.
    let mut execution_context = if collect_stats {
        aster_runtime::ExecutionContext::with_stats()
    } else {
        aster_runtime::ExecutionContext::new()
    };
    let value = invoke_finalized(pointer, &entry.return_type, &mut execution_context);
    let runtime_error = execution_context.take_error();
    let stats = execution_context.memory_stats().clone();
    // SAFETY: execution finished and `value` owns copies of any data that
    // lived in the module, so no pointer into the JIT memory survives.
    // Releasing here prevents leaking code and data pages across repeated
    // executions (e.g. `aster watch` rebuilds).
    #[allow(unsafe_code)]
    unsafe {
        codegen.jit.free_memory();
    }
    if let Some(error) = runtime_error {
        Err(BackendError::new(format!("Aster runtime error: {error}")))
    } else {
        value.map(|v| (v, stats))
    }
}

/// This is the only unsafe boundary in the backend. Cranelift returns an untyped
/// pointer after successful finalization. Validation guarantees that the selected
/// entry has no parameters and that its ABI result is exactly `i32`, `u8`, a
/// runtime-ABI string pointer, or void. The `JITModule` remains alive during the
/// call, and string payloads are copied into host memory before it is dropped.
#[allow(unsafe_code)]
fn invoke_finalized(
    pointer: *const u8,
    return_type: &mir::Type,
    context: &mut aster_runtime::ExecutionContext,
) -> Result<ExecutionValue, BackendError> {
    let value = match return_type {
        mir::Type::SByte => {
            // SAFETY: The function was declared and finalized as `() -> i8` above.
            let function: extern "C" fn(*mut aster_runtime::ExecutionContext) -> i8 =
                unsafe { std::mem::transmute(pointer) };
            ExecutionValue::SByte(function(context))
        }
        mir::Type::Byte => {
            // SAFETY: The function was declared and finalized as `() -> i8` above.
            let function: extern "C" fn(*mut aster_runtime::ExecutionContext) -> u8 =
                unsafe { std::mem::transmute(pointer) };
            ExecutionValue::Byte(function(context))
        }
        mir::Type::Short => {
            // SAFETY: The function was declared and finalized as `() -> i16` above.
            let function: extern "C" fn(*mut aster_runtime::ExecutionContext) -> i16 =
                unsafe { std::mem::transmute(pointer) };
            ExecutionValue::Short(function(context))
        }
        mir::Type::UShort => {
            // SAFETY: The function was declared and finalized as `() -> i16` above.
            let function: extern "C" fn(*mut aster_runtime::ExecutionContext) -> u16 =
                unsafe { std::mem::transmute(pointer) };
            ExecutionValue::UShort(function(context))
        }
        mir::Type::UInt => {
            // SAFETY: The function was declared and finalized as `() -> i32` above.
            let function: extern "C" fn(*mut aster_runtime::ExecutionContext) -> u32 =
                unsafe { std::mem::transmute(pointer) };
            ExecutionValue::UInt(function(context))
        }
        mir::Type::ULong => {
            // SAFETY: The function was declared and finalized as `() -> i64` above.
            let function: extern "C" fn(*mut aster_runtime::ExecutionContext) -> u64 =
                unsafe { std::mem::transmute(pointer) };
            ExecutionValue::ULong(function(context))
        }
        mir::Type::Int => {
            // SAFETY: The function was declared and finalized as `() -> i32` above.
            let function: extern "C" fn(*mut aster_runtime::ExecutionContext) -> i32 =
                unsafe { std::mem::transmute(pointer) };
            ExecutionValue::Int(function(context))
        }
        mir::Type::Long => {
            // SAFETY: The function was declared and finalized as `() -> i64` above.
            let function: extern "C" fn(*mut aster_runtime::ExecutionContext) -> i64 =
                unsafe { std::mem::transmute(pointer) };
            ExecutionValue::Long(function(context))
        }
        mir::Type::Float => {
            // SAFETY: The function was declared and finalized as `() -> f32` above.
            let function: extern "C" fn(*mut aster_runtime::ExecutionContext) -> f32 =
                unsafe { std::mem::transmute(pointer) };
            ExecutionValue::float(function(context))
        }
        mir::Type::Double => {
            // SAFETY: The function was declared and finalized as `() -> f64` above.
            let function: extern "C" fn(*mut aster_runtime::ExecutionContext) -> f64 =
                unsafe { std::mem::transmute(pointer) };
            ExecutionValue::double(function(context))
        }
        mir::Type::Char => {
            // SAFETY: The function was declared and finalized as `() -> i32` above,
            // holding a Unicode scalar value.
            let function: extern "C" fn(*mut aster_runtime::ExecutionContext) -> u32 =
                unsafe { std::mem::transmute(pointer) };
            let value = function(context);
            let value = char::from_u32(value).ok_or_else(|| {
                BackendError::new(format!(
                    "Aster function returned invalid Unicode scalar value U+{value:08X} as `char`"
                ))
            })?;
            ExecutionValue::Char(value)
        }
        mir::Type::Bool => {
            // SAFETY: The function was declared and finalized as `() -> i8` above.
            let function: extern "C" fn(*mut aster_runtime::ExecutionContext) -> u8 =
                unsafe { std::mem::transmute(pointer) };
            ExecutionValue::Bool(function(context) != 0)
        }
        mir::Type::String => {
            // SAFETY: The function was declared and finalized as `() -> ptr`,
            // where the pointer follows the runtime string ABI and stays valid
            // while the JIT module is alive.
            let function: extern "C" fn(
                *mut aster_runtime::ExecutionContext,
            ) -> *const aster_runtime::AsterStrHeader = unsafe { std::mem::transmute(pointer) };
            let result = function(context);
            // SAFETY: the returned pointer originates from this module's data
            // section; the module is still alive here and `decode_str` copies
            // the payload before the module can be dropped.
            let value = unsafe { aster_runtime::decode_str(result) };
            match value {
                Some(value) => ExecutionValue::String(value),
                None => ExecutionValue::String(String::from(
                    "<invalid string returned by Aster function>",
                )),
            }
        }
        mir::Type::Void => {
            // SAFETY: The function was declared and finalized as `() -> ()` above.
            let function: extern "C" fn(*mut aster_runtime::ExecutionContext) =
                unsafe { std::mem::transmute(pointer) };
            function(context);
            ExecutionValue::Void
        }
        _ => unreachable!("entry return type was validated before code generation"),
    };
    Ok(value)
}
