use super::{
    BackendError, Codegen, ExecutionValue, HashMap, JITBuilder, JITModule, default_libcall_names,
    mir, module_error, runtime_functions,
};

/// A JIT module compiled and finalized once, ready to invoke any function of
/// `mir::Module` by its resolved [`mir::SymbolId`] any number of times
/// without recompiling. Every invocation allocates a brand new
/// [`aster_runtime::ExecutionContext`]; only the underlying `JITModule` and
/// its finalized machine code are reused across invocations.
///
/// Not `Send`/`Sync`: `cranelift_jit::JITModule` holds interior-mutable
/// symbol state and a memory-provider trait object with no `Send`/`Sync`
/// bound, so it must be built and invoked from a single thread and never
/// shared by reference across threads. The worker pool owns one
/// `PreparedProgram` per worker rather than sharing one.
pub(super) struct PreparedProgram {
    // `Option` so `Drop` can move the module out and call the by-value
    // `JITModule::free_memory`, which `&mut self` cannot do.
    jit: Option<JITModule>,
    entries: HashMap<mir::SymbolId, (*const u8, mir::Type)>,
}

impl PreparedProgram {
    /// Compile every function in `module` and finalize definitions once,
    /// binding every function's resolved symbol to its finalized address and
    /// return type for later invocation.
    pub(super) fn prepare(module: &mir::Module) -> Result<Self, BackendError> {
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
        let entries = module
            .functions
            .iter()
            .map(|function| {
                let id = function_ids[&function.symbol];
                let pointer = codegen.jit.get_finalized_function(id);
                (function.symbol, (pointer, function.return_type.clone()))
            })
            .collect();
        Ok(Self {
            jit: Some(codegen.jit),
            entries,
        })
    }

    /// Run the function bound to `symbol` once against a fresh
    /// `ExecutionContext`. The finalized code and data stay alive for the
    /// duration of this call because `self.jit` is not dropped until `Self`
    /// is. Looks up `symbol` in the map built once by [`Self::prepare`], so
    /// no textual function lookup happens on this path.
    pub(super) fn invoke(
        &self,
        symbol: mir::SymbolId,
        collect_stats: bool,
    ) -> Result<(ExecutionValue, super::MemoryStats), BackendError> {
        let (pointer, return_type) = self
            .entries
            .get(&symbol)
            .ok_or_else(|| BackendError::new(format!("symbol {symbol:?} was not prepared")))?;
        let mut execution_context = if collect_stats {
            aster_runtime::ExecutionContext::with_stats()
        } else {
            aster_runtime::ExecutionContext::new()
        };
        let value = invoke_finalized(*pointer, return_type, &mut execution_context);
        let runtime_error = execution_context.take_error();
        let stats = execution_context.memory_stats().clone();
        if let Some(error) = runtime_error {
            Err(BackendError::new(format!("Aster runtime error: {error}")))
        } else {
            value.map(|v| (v, stats))
        }
    }
}

impl Drop for PreparedProgram {
    fn drop(&mut self) {
        let Some(jit) = self.jit.take() else {
            return;
        };
        // SAFETY: this `PreparedProgram` is being destroyed, so no
        // invocation can still be running, and every prior invocation
        // already copied its result out of JIT memory before returning (see
        // `invoke_finalized`). Releasing here prevents leaking code and data
        // pages across repeated preparations (e.g. `aster watch` rebuilds).
        #[allow(unsafe_code)]
        unsafe {
            jit.free_memory();
        }
    }
}

pub(super) fn execute_resolved(
    module: &mir::Module,
    entry: &mir::Function,
    collect_stats: bool,
) -> Result<(ExecutionValue, super::MemoryStats), BackendError> {
    let prepared = PreparedProgram::prepare(module)?;
    prepared.invoke(entry.symbol, collect_stats)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn compile(source: &str) -> mir::Module {
        aster_compiler::compile(source)
            .expect("source compiles")
            .mir
    }

    fn prepare(module: &mir::Module, function_name: &str) -> (PreparedProgram, mir::SymbolId) {
        let entry = crate::select_entry(module, function_name).expect("entry resolves");
        let symbol = entry.symbol;
        let program = PreparedProgram::prepare(module).expect("module compiles and finalizes");
        (program, symbol)
    }

    #[test]
    fn prepared_program_runs_the_same_entry_repeatedly_with_matching_results() {
        let module = compile("public int Run() { return 40 + 2; }");
        let (prepared, run) = prepare(&module, "Run");

        let (first, _) = prepared
            .invoke(run, false)
            .expect("first invocation succeeds");
        let (second, _) = prepared
            .invoke(run, false)
            .expect("second invocation succeeds");
        let (third, _) = prepared
            .invoke(run, false)
            .expect("third invocation succeeds");

        assert_eq!(first, ExecutionValue::Int(42));
        assert_eq!(second, ExecutionValue::Int(42));
        assert_eq!(third, ExecutionValue::Int(42));
    }

    #[test]
    fn prepared_program_resolves_multiple_functions_of_the_same_module() {
        let module = compile(
            "public int Answer() { return 42; } public int Double(int value) { return value * 2; } public int Run() { return Double(Answer()); }",
        );
        let (prepared, answer) = prepare(&module, "Answer");
        let run = crate::select_entry(&module, "Run")
            .expect("entry resolves")
            .symbol;

        let (answer_value, _) = prepared.invoke(answer, false).expect("Answer succeeds");
        let (run_value, _) = prepared.invoke(run, false).expect("Run succeeds");

        assert_eq!(answer_value, ExecutionValue::Int(42));
        assert_eq!(run_value, ExecutionValue::Int(84));
    }

    #[test]
    fn each_invocation_reports_independent_non_accumulating_metrics() {
        let module =
            compile("public int Run() { int[] values = [20, 22]; return values[0] + values[1]; }");
        let (prepared, run) = prepare(&module, "Run");

        for _ in 0..3 {
            let (value, stats) = prepared.invoke(run, true).expect("invocation succeeds");
            assert_eq!(value, ExecutionValue::Int(42));
            assert_eq!(stats.total_allocations, 1);
            assert_eq!(stats.array_allocations, 1);
            // The temporary array is rewound before `Run` returns, and each
            // invocation starts from a brand new `ExecutionContext`, so
            // nothing from a previous invocation can accumulate here.
            assert_eq!(stats.used_bytes, 0);
        }
    }

    #[test]
    fn a_controlled_runtime_error_in_one_invocation_does_not_contaminate_the_next() {
        let module = compile("public int Run() { int[] values = new int[1]; return values[5]; }");
        let (prepared, run) = prepare(&module, "Run");

        // Each invocation gets a fresh `ExecutionContext`, so a controlled
        // out-of-bounds error must be reported identically every time, never
        // suppressed, duplicated, or reworded by leftover state from a
        // previous invocation's `ExecutionContext.error` field.
        let first = prepared
            .invoke(run, false)
            .expect_err("first invocation fails");
        let second = prepared
            .invoke(run, false)
            .expect_err("second invocation fails");
        let third = prepared
            .invoke(run, false)
            .expect_err("third invocation fails");

        assert_eq!(first, second);
        assert_eq!(second, third);
        assert!(first.message().contains("array index 5"));
    }

    #[test]
    fn prepare_then_invoke_matches_the_public_sequential_execute_path() {
        let module = compile("public int Run() { return 40 + 2; }");
        let (prepared, run) = prepare(&module, "Run");

        let prepared_result = prepared.invoke(run, false).map(|(value, _)| value);

        let entry = crate::select_entry(&module, "Run").expect("entry resolves");
        let sequential_result = execute_resolved(&module, entry, false).map(|(value, _)| value);

        assert_eq!(prepared_result, sequential_result);
    }

    #[test]
    fn dropping_a_prepared_program_after_use_frees_jit_memory_exactly_once() {
        let module = compile("public int Run() { return 1; }");
        let (prepared, run) = prepare(&module, "Run");
        prepared.invoke(run, false).expect("invocation succeeds");
        // `Drop` moves `jit` out of the `Option` before freeing it, so this
        // cannot double-free even if `drop` were somehow reachable twice.
        drop(prepared);
    }

    #[test]
    fn public_execution_types_are_thread_transferable() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}

        assert_send::<mir::Module>();
        assert_sync::<mir::Module>();
        assert_send::<ExecutionValue>();
        assert_send::<crate::MemoryStats>();
    }
}
