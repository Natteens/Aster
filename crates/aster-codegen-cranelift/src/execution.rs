use std::sync::Arc;

use super::task_runtime::{TaskRuntime, module_uses_tasks};
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
        let mut builder = jit_builder()?;
        for function in runtime_functions() {
            builder.symbol(function.name, function.address);
        }
        super::task_abi::bind_task_functions(&mut builder);
        super::async_abi::bind_async_functions(&mut builder);
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
    ///
    /// `task_runtime`, when present, is registered on the fresh
    /// `ExecutionContext` before invocation so `Task.Run` can reach the
    /// host's execution pool (see `aster_runtime::ExecutionContext::set_task_runtime`).
    /// Sequential invocations that never use `Task.Run` pass `None`.
    ///
    /// `console_backend`, when present, is registered on the fresh
    /// `ExecutionContext` before invocation so `aster.io` I/O reaches an
    /// injected backend (e.g. an in-memory one for tests) instead of the
    /// default real stdin/stdout. `filesystem_backend` is the same seam for
    /// `aster.io.ReadAllText`/`WriteAllText`, instead of the default real
    /// filesystem.
    pub(super) fn invoke(
        &self,
        symbol: mir::SymbolId,
        collect_stats: bool,
        task_runtime: Option<*mut ()>,
        console_backend: Option<Box<dyn aster_runtime::ConsoleBackend>>,
        filesystem_backend: Option<Box<dyn aster_runtime::FileSystemBackend>>,
    ) -> Result<(ExecutionValue, super::MemoryStats), BackendError> {
        self.invoke_observed(
            symbol,
            collect_stats,
            task_runtime,
            console_backend,
            filesystem_backend,
        )
        .map(|(value, stats, _)| (value, stats))
    }

    #[cfg(feature = "aarm-telemetry")]
    pub(super) fn invoke_with_aarm_telemetry(
        &self,
        symbol: mir::SymbolId,
        task_runtime: Option<*mut ()>,
    ) -> Result<(ExecutionValue, aster_runtime::AarmMemoryTelemetry), BackendError> {
        self.invoke_observed(symbol, true, task_runtime, None, None)
            .map(|(value, _, telemetry)| {
                (
                    value,
                    telemetry.expect("statistics mode enables AARM telemetry"),
                )
            })
    }

    fn invoke_observed(
        &self,
        symbol: mir::SymbolId,
        collect_stats: bool,
        task_runtime: Option<*mut ()>,
        console_backend: Option<Box<dyn aster_runtime::ConsoleBackend>>,
        filesystem_backend: Option<Box<dyn aster_runtime::FileSystemBackend>>,
    ) -> Result<
        (
            ExecutionValue,
            super::MemoryStats,
            Option<aster_runtime::AarmMemoryTelemetry>,
        ),
        BackendError,
    > {
        let (pointer, return_type) = self
            .entries
            .get(&symbol)
            .ok_or_else(|| BackendError::new(format!("symbol {symbol:?} was not prepared")))?;
        let mut execution_context = if collect_stats {
            aster_runtime::ExecutionContext::with_stats()
        } else {
            aster_runtime::ExecutionContext::new()
        };
        if let Some(pointer) = task_runtime {
            execution_context.set_task_runtime(pointer);
        }
        if let Some(backend) = console_backend {
            execution_context.set_console_backend(backend);
        }
        if let Some(backend) = filesystem_backend {
            execution_context.set_filesystem_backend(backend);
        }
        let value = invoke_finalized(*pointer, return_type, &mut execution_context);
        let runtime_error = execution_context.take_error();
        let stats = execution_context.memory_stats().clone();
        let telemetry = execution_context.aarm_memory_telemetry();
        if let Some(error) = runtime_error {
            Err(BackendError::new(format!("Aster runtime error: {error}")))
        } else {
            value.map(|v| (v, stats, telemetry))
        }
    }

    /// Run one async `MoveNext` step on the host, passing the outer task's
    /// handle as its hidden scalar parameter and registering `task_runtime` on
    /// the fresh `ExecutionContext` so the step's async intrinsics reach the
    /// runtime. Returns the machine status as an `ExecutionValue::Int`, or the
    /// controlled error the step raised (`context.fail` wins over any status).
    pub(super) fn invoke_move_next(
        &self,
        symbol: mir::SymbolId,
        handle: i64,
        task_runtime: *mut (),
    ) -> Result<(ExecutionValue, super::MemoryStats), BackendError> {
        let (pointer, _return_type) = self.entries.get(&symbol).ok_or_else(|| {
            BackendError::new(format!("MoveNext symbol {symbol:?} was not prepared"))
        })?;
        let mut context = aster_runtime::ExecutionContext::new();
        context.set_task_runtime(task_runtime);
        // SAFETY: this symbol was declared and finalized as
        // `(ExecutionContext*, i64) -> i32` (see `mir_lowering::async_machine`
        // and `declarations::signature`); the module stays alive for the call.
        #[allow(unsafe_code)]
        let function: extern "C" fn(*mut aster_runtime::ExecutionContext, i64) -> i32 =
            unsafe { std::mem::transmute(*pointer) };
        let move_next_status = function(&raw mut context, handle);
        let stats = context.memory_stats().clone();
        match context.take_error() {
            Some(error) => Err(BackendError::new(format!("Aster runtime error: {error}"))),
            None => Ok((ExecutionValue::Int(move_next_status), stats)),
        }
    }

    /// Run `Parallel.For`'s `Body(int)` over `[start, end)` on this worker with
    /// one `ExecutionContext` for the whole chunk, resolving the body pointer
    /// once and stopping at the first controlled error (reporting that index).
    pub(super) fn run_for_chunk(
        &self,
        symbol: mir::SymbolId,
        start: i32,
        end: i32,
    ) -> super::worker_pool::ChunkOutcome {
        let Some((pointer, _)) = self.entries.get(&symbol) else {
            return chunk_error(i64::from(start), "Parallel body was not prepared");
        };
        // SAFETY: a `Parallel.For` body is finalized as
        // `(ExecutionContext*, i32) -> ()` (validated shape).
        #[allow(unsafe_code)]
        let body: extern "C" fn(*mut aster_runtime::ExecutionContext, i32) =
            unsafe { std::mem::transmute(*pointer) };
        let mut context = aster_runtime::ExecutionContext::new();
        for index in start..end {
            body(&raw mut context, index);
            if let Some(error) = context.take_error() {
                return chunk_error(i64::from(index), format!("Aster runtime error: {error}"));
            }
        }
        super::worker_pool::ChunkOutcome { first_error: None }
    }

    /// Run `Parallel.ForEach`'s `Body(T)` over the host-owned scalar copies of
    /// this chunk, whose first element is original array position `base`.
    pub(super) fn run_for_each_chunk(
        &self,
        symbol: mir::SymbolId,
        base: usize,
        values: &[ExecutionValue],
    ) -> super::worker_pool::ChunkOutcome {
        let logical = |offset: usize| i64::try_from(base + offset).unwrap_or(i64::MAX);
        let Some((pointer, _)) = self.entries.get(&symbol) else {
            return chunk_error(logical(0), "Parallel body was not prepared");
        };
        let mut context = aster_runtime::ExecutionContext::new();
        for (offset, value) in values.iter().enumerate() {
            invoke_body_scalar(*pointer, &mut context, value);
            if let Some(error) = context.take_error() {
                return chunk_error(logical(offset), format!("Aster runtime error: {error}"));
            }
        }
        super::worker_pool::ChunkOutcome { first_error: None }
    }

    /// Run one `Parallel.Reduce` accumulation chunk: fold `Accumulate` over
    /// `values` in order, starting from an owned copy of `identity`, with one
    /// `ExecutionContext` for the whole chunk. Stops at the first error,
    /// reporting the failing element's logical array position (`base` plus
    /// its offset within this chunk).
    pub(super) fn run_reduce_chunk(
        &self,
        symbol: mir::SymbolId,
        base: usize,
        identity: &ExecutionValue,
        values: &[ExecutionValue],
    ) -> super::worker_pool::ReduceChunkOutcome {
        use super::worker_pool::ReduceChunkOutcome;
        let logical = |offset: usize| i64::try_from(base + offset).unwrap_or(i64::MAX);
        let Some((pointer, _)) = self.entries.get(&symbol) else {
            return ReduceChunkOutcome {
                result: Err((
                    logical(0),
                    BackendError::new("Parallel.Reduce Accumulate was not prepared"),
                )),
            };
        };
        let mut context = aster_runtime::ExecutionContext::new();
        let mut accumulator = identity.clone();
        for (offset, value) in values.iter().enumerate() {
            let next = match invoke_binary_scalar(*pointer, &mut context, &accumulator, value) {
                Ok(next) => next,
                Err(error) => {
                    return ReduceChunkOutcome {
                        result: Err((logical(offset), error)),
                    };
                }
            };
            if let Some(error) = context.take_error() {
                return ReduceChunkOutcome {
                    result: Err((
                        logical(offset),
                        BackendError::new(format!("Aster runtime error: {error}")),
                    )),
                };
            }
            accumulator = next;
        }
        ReduceChunkOutcome {
            result: Ok(accumulator),
        }
    }

    /// Run one `Parallel.Reduce` combine step: `Combine(left, right)` with its
    /// own fresh `ExecutionContext`, never the `ExecutionContext` of any chunk
    /// or of the invocation that started the reduction.
    pub(super) fn run_combine_step(
        &self,
        symbol: mir::SymbolId,
        left: &ExecutionValue,
        right: &ExecutionValue,
    ) -> super::worker_pool::CombineOutcome {
        use super::worker_pool::CombineOutcome;
        let Some((pointer, _)) = self.entries.get(&symbol) else {
            return CombineOutcome {
                result: Err(BackendError::new(
                    "Parallel.Reduce Combine was not prepared",
                )),
            };
        };
        let mut context = aster_runtime::ExecutionContext::new();
        let result = match invoke_binary_scalar(*pointer, &mut context, left, right) {
            Ok(value) => value,
            Err(error) => return CombineOutcome { result: Err(error) },
        };
        if let Some(error) = context.take_error() {
            return CombineOutcome {
                result: Err(BackendError::new(format!("Aster runtime error: {error}"))),
            };
        }
        CombineOutcome { result: Ok(result) }
    }
}

fn jit_builder() -> Result<JITBuilder, BackendError> {
    JITBuilder::with_flags(&[("opt_level", "speed")], default_libcall_names()).map_err(module_error)
}

fn chunk_error(index: i64, message: impl Into<String>) -> super::worker_pool::ChunkOutcome {
    super::worker_pool::ChunkOutcome {
        first_error: Some((index, BackendError::new(message))),
    }
}

/// Invoke a `Parallel.ForEach` body with one scalar argument, dispatching to
/// the concrete ABI shape by the value's variant so width, signedness, and
/// float bit patterns are all preserved across the call.
#[allow(unsafe_code)]
fn invoke_body_scalar(
    pointer: *const u8,
    context: &mut aster_runtime::ExecutionContext,
    value: &ExecutionValue,
) {
    // SAFETY: the body is finalized as `(ExecutionContext*, T) -> ()` where the
    // Cranelift type of `T` matches the width dispatched on here.
    unsafe {
        match value {
            ExecutionValue::Bool(value) => {
                let body: extern "C" fn(*mut aster_runtime::ExecutionContext, i8) =
                    std::mem::transmute(pointer);
                body(context, i8::from(*value));
            }
            ExecutionValue::SByte(value) => {
                let body: extern "C" fn(*mut aster_runtime::ExecutionContext, i8) =
                    std::mem::transmute(pointer);
                body(context, *value);
            }
            ExecutionValue::Byte(value) => {
                let body: extern "C" fn(*mut aster_runtime::ExecutionContext, u8) =
                    std::mem::transmute(pointer);
                body(context, *value);
            }
            ExecutionValue::Short(value) => {
                let body: extern "C" fn(*mut aster_runtime::ExecutionContext, i16) =
                    std::mem::transmute(pointer);
                body(context, *value);
            }
            ExecutionValue::UShort(value) => {
                let body: extern "C" fn(*mut aster_runtime::ExecutionContext, u16) =
                    std::mem::transmute(pointer);
                body(context, *value);
            }
            ExecutionValue::Int(value) => {
                let body: extern "C" fn(*mut aster_runtime::ExecutionContext, i32) =
                    std::mem::transmute(pointer);
                body(context, *value);
            }
            ExecutionValue::UInt(value) => {
                let body: extern "C" fn(*mut aster_runtime::ExecutionContext, u32) =
                    std::mem::transmute(pointer);
                body(context, *value);
            }
            ExecutionValue::Long(value) => {
                let body: extern "C" fn(*mut aster_runtime::ExecutionContext, i64) =
                    std::mem::transmute(pointer);
                body(context, *value);
            }
            ExecutionValue::ULong(value) => {
                let body: extern "C" fn(*mut aster_runtime::ExecutionContext, u64) =
                    std::mem::transmute(pointer);
                body(context, *value);
            }
            ExecutionValue::Float(value) => {
                let body: extern "C" fn(*mut aster_runtime::ExecutionContext, f32) =
                    std::mem::transmute(pointer);
                body(context, *value);
            }
            ExecutionValue::Double(value) => {
                let body: extern "C" fn(*mut aster_runtime::ExecutionContext, f64) =
                    std::mem::transmute(pointer);
                body(context, *value);
            }
            ExecutionValue::Char(value) => {
                let body: extern "C" fn(*mut aster_runtime::ExecutionContext, u32) =
                    std::mem::transmute(pointer);
                body(context, *value as u32);
            }
            ExecutionValue::String(_) | ExecutionValue::Void => {}
        }
    }
}

/// Invoke a `Parallel.Reduce` `Accumulate(TAccumulator, TElement) ->
/// TAccumulator` or `Combine(TAccumulator, TAccumulator) -> TAccumulator`
/// body with two scalar arguments, dispatching to the concrete ABI shape by
/// each value's variant so width, signedness, and float bit patterns are
/// preserved. `accumulator` and `element` may be different concrete scalar
/// kinds (e.g. `Accumulate(int, long)`); `Combine` simply calls this with
/// both arguments of the same kind. Never a panic: an invalid `char` result
/// or a validated-impossible non-scalar variant is a controlled error,
/// exactly like `execution::invoke_finalized`'s own `char` handling.
#[allow(unsafe_code, clippy::too_many_lines)]
fn invoke_binary_scalar(
    pointer: *const u8,
    context: &mut aster_runtime::ExecutionContext,
    accumulator: &ExecutionValue,
    element: &ExecutionValue,
) -> Result<ExecutionValue, BackendError> {
    // SAFETY: the body is finalized as `(ExecutionContext*, TAccumulator,
    // TElement) -> TAccumulator` where the Cranelift types match the widths
    // dispatched on here (validated shape: `validation::is_worker_transferable`
    // on both the accumulator and element types).
    macro_rules! call_with_element {
        ($accumulator_native:ty, $accumulator_value:expr) => {
            match element {
                ExecutionValue::Bool(value) => {
                    let function: extern "C" fn(
                        *mut aster_runtime::ExecutionContext,
                        $accumulator_native,
                        i8,
                    ) -> $accumulator_native = unsafe { std::mem::transmute(pointer) };
                    function(context, $accumulator_value, i8::from(*value))
                }
                ExecutionValue::SByte(value) => {
                    let function: extern "C" fn(
                        *mut aster_runtime::ExecutionContext,
                        $accumulator_native,
                        i8,
                    ) -> $accumulator_native = unsafe { std::mem::transmute(pointer) };
                    function(context, $accumulator_value, *value)
                }
                ExecutionValue::Byte(value) => {
                    let function: extern "C" fn(
                        *mut aster_runtime::ExecutionContext,
                        $accumulator_native,
                        u8,
                    ) -> $accumulator_native = unsafe { std::mem::transmute(pointer) };
                    function(context, $accumulator_value, *value)
                }
                ExecutionValue::Short(value) => {
                    let function: extern "C" fn(
                        *mut aster_runtime::ExecutionContext,
                        $accumulator_native,
                        i16,
                    ) -> $accumulator_native = unsafe { std::mem::transmute(pointer) };
                    function(context, $accumulator_value, *value)
                }
                ExecutionValue::UShort(value) => {
                    let function: extern "C" fn(
                        *mut aster_runtime::ExecutionContext,
                        $accumulator_native,
                        u16,
                    ) -> $accumulator_native = unsafe { std::mem::transmute(pointer) };
                    function(context, $accumulator_value, *value)
                }
                ExecutionValue::Int(value) => {
                    let function: extern "C" fn(
                        *mut aster_runtime::ExecutionContext,
                        $accumulator_native,
                        i32,
                    ) -> $accumulator_native = unsafe { std::mem::transmute(pointer) };
                    function(context, $accumulator_value, *value)
                }
                ExecutionValue::UInt(value) => {
                    let function: extern "C" fn(
                        *mut aster_runtime::ExecutionContext,
                        $accumulator_native,
                        u32,
                    ) -> $accumulator_native = unsafe { std::mem::transmute(pointer) };
                    function(context, $accumulator_value, *value)
                }
                ExecutionValue::Long(value) => {
                    let function: extern "C" fn(
                        *mut aster_runtime::ExecutionContext,
                        $accumulator_native,
                        i64,
                    ) -> $accumulator_native = unsafe { std::mem::transmute(pointer) };
                    function(context, $accumulator_value, *value)
                }
                ExecutionValue::ULong(value) => {
                    let function: extern "C" fn(
                        *mut aster_runtime::ExecutionContext,
                        $accumulator_native,
                        u64,
                    ) -> $accumulator_native = unsafe { std::mem::transmute(pointer) };
                    function(context, $accumulator_value, *value)
                }
                ExecutionValue::Float(value) => {
                    let function: extern "C" fn(
                        *mut aster_runtime::ExecutionContext,
                        $accumulator_native,
                        f32,
                    ) -> $accumulator_native = unsafe { std::mem::transmute(pointer) };
                    function(context, $accumulator_value, *value)
                }
                ExecutionValue::Double(value) => {
                    let function: extern "C" fn(
                        *mut aster_runtime::ExecutionContext,
                        $accumulator_native,
                        f64,
                    ) -> $accumulator_native = unsafe { std::mem::transmute(pointer) };
                    function(context, $accumulator_value, *value)
                }
                ExecutionValue::Char(value) => {
                    let function: extern "C" fn(
                        *mut aster_runtime::ExecutionContext,
                        $accumulator_native,
                        u32,
                    ) -> $accumulator_native = unsafe { std::mem::transmute(pointer) };
                    function(context, $accumulator_value, *value as u32)
                }
                ExecutionValue::String(_) | ExecutionValue::Void => {
                    return Err(BackendError::new(
                        "Parallel.Reduce received a non-scalar element",
                    ));
                }
            }
        };
    }

    let result = match accumulator {
        ExecutionValue::Bool(value) => {
            let raw: i8 = call_with_element!(i8, i8::from(*value));
            ExecutionValue::Bool(raw != 0)
        }
        ExecutionValue::SByte(value) => ExecutionValue::SByte(call_with_element!(i8, *value)),
        ExecutionValue::Byte(value) => ExecutionValue::Byte(call_with_element!(u8, *value)),
        ExecutionValue::Short(value) => ExecutionValue::Short(call_with_element!(i16, *value)),
        ExecutionValue::UShort(value) => ExecutionValue::UShort(call_with_element!(u16, *value)),
        ExecutionValue::Int(value) => ExecutionValue::Int(call_with_element!(i32, *value)),
        ExecutionValue::UInt(value) => ExecutionValue::UInt(call_with_element!(u32, *value)),
        ExecutionValue::Long(value) => ExecutionValue::Long(call_with_element!(i64, *value)),
        ExecutionValue::ULong(value) => ExecutionValue::ULong(call_with_element!(u64, *value)),
        ExecutionValue::Float(value) => ExecutionValue::float(call_with_element!(f32, *value)),
        ExecutionValue::Double(value) => ExecutionValue::double(call_with_element!(f64, *value)),
        ExecutionValue::Char(value) => {
            let raw: u32 = call_with_element!(u32, *value as u32);
            return char::from_u32(raw).map(ExecutionValue::Char).ok_or_else(|| {
                BackendError::new(format!(
                    "Parallel.Reduce Accumulate/Combine returned invalid Unicode scalar value U+{raw:08X} as `char`"
                ))
            });
        }
        ExecutionValue::String(_) | ExecutionValue::Void => {
            return Err(BackendError::new(
                "Parallel.Reduce received a non-scalar accumulator",
            ));
        }
    };
    Ok(result)
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

/// Shared implementation behind every public `execute*` function. Detects
/// whether `module` uses `Task.Run`/`Wait` anywhere and, only then, creates
/// a [`TaskRuntime`] for the duration of this call: a module that never
/// uses tasks never pays for one, and there is no second, task-aware
/// entry point to choose instead of this one.
pub(super) fn execute_resolved(
    module: &mir::Module,
    entry: &mir::Function,
    collect_stats: bool,
    console_backend: Option<Box<dyn aster_runtime::ConsoleBackend>>,
    filesystem_backend: Option<Box<dyn aster_runtime::FileSystemBackend>>,
) -> Result<(ExecutionValue, super::MemoryStats), BackendError> {
    let prepared = PreparedProgram::prepare(module)?;
    if !module_uses_tasks(module) {
        return prepared.invoke(
            entry.symbol,
            collect_stats,
            None,
            console_backend,
            filesystem_backend,
        );
    }
    let worker_count = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let mut runtime = TaskRuntime::new(&Arc::new(module.clone()), worker_count)?;
    // SAFETY-relevant only in spirit: this is a plain pointer value, not a
    // live borrow held across the call. `runtime` is not read or written by
    // name again until after `invoke` returns, so nothing aliases it while
    // `task_abi`'s ABI functions dereference the pointer on this same
    // thread. `runtime` outlives the call and is dropped only afterward,
    // which shuts its pool down and releases every task entry.
    let pointer = std::ptr::from_mut(&mut runtime).cast::<()>();
    prepared.invoke(
        entry.symbol,
        collect_stats,
        Some(pointer),
        console_backend,
        filesystem_backend,
    )
}

#[cfg(feature = "aarm-telemetry")]
pub(super) fn execute_resolved_with_aarm_telemetry(
    module: &mir::Module,
    entry: &mir::Function,
) -> Result<(ExecutionValue, aster_runtime::AarmMemoryTelemetry), BackendError> {
    let prepared = PreparedProgram::prepare(module)?;
    if !module_uses_tasks(module) {
        return prepared.invoke_with_aarm_telemetry(entry.symbol, None);
    }
    let worker_count = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let mut runtime = TaskRuntime::new(&Arc::new(module.clone()), worker_count)?;
    let pointer = std::ptr::from_mut(&mut runtime).cast::<()>();
    prepared.invoke_with_aarm_telemetry(entry.symbol, Some(pointer))
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
        mir::Type::Task(_) => {
            // SAFETY: The function was declared and finalized as `() -> i64`
            // above (`Task<T>` is always a plain handle, never a pointer).
            // `execute`/`execute_symbol` themselves never reach this arm
            // (`validate_invocable_entry` rejects a `Task<T>` entry with a
            // controlled error first); this stays defensive for any other
            // caller of `PreparedProgram::invoke`, such as internal tests
            // that call an async wrapper directly.
            let function: extern "C" fn(*mut aster_runtime::ExecutionContext) -> i64 =
                unsafe { std::mem::transmute(pointer) };
            ExecutionValue::Long(function(context))
        }
        _ => unreachable!("entry return type was validated before code generation"),
    };
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cranelift_codegen::settings::OptLevel;
    use cranelift_module::Module;

    #[test]
    fn jit_uses_cranelift_speed_optimizations() {
        let module = JITModule::new(jit_builder().expect("native JIT builder"));
        assert_eq!(module.isa().flags().opt_level(), OptLevel::Speed);
    }

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
            .invoke(run, false, None, None, None)
            .expect("first invocation succeeds");
        let (second, _) = prepared
            .invoke(run, false, None, None, None)
            .expect("second invocation succeeds");
        let (third, _) = prepared
            .invoke(run, false, None, None, None)
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

        let (answer_value, _) = prepared
            .invoke(answer, false, None, None, None)
            .expect("Answer succeeds");
        let (run_value, _) = prepared
            .invoke(run, false, None, None, None)
            .expect("Run succeeds");

        assert_eq!(answer_value, ExecutionValue::Int(42));
        assert_eq!(run_value, ExecutionValue::Int(84));
    }

    #[test]
    fn each_invocation_reports_independent_non_accumulating_metrics() {
        let module =
            compile("public int Run() { int[] values = [20, 22]; return values[0] + values[1]; }");
        let (prepared, run) = prepare(&module, "Run");

        for _ in 0..3 {
            let (value, stats) = prepared
                .invoke(run, true, None, None, None)
                .expect("invocation succeeds");
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
            .invoke(run, false, None, None, None)
            .expect_err("first invocation fails");
        let second = prepared
            .invoke(run, false, None, None, None)
            .expect_err("second invocation fails");
        let third = prepared
            .invoke(run, false, None, None, None)
            .expect_err("third invocation fails");

        assert_eq!(first, second);
        assert_eq!(second, third);
        assert!(first.message().contains("array index 5"));
    }

    #[test]
    fn prepare_then_invoke_matches_the_public_sequential_execute_path() {
        let module = compile("public int Run() { return 40 + 2; }");
        let (prepared, run) = prepare(&module, "Run");

        let prepared_result = prepared
            .invoke(run, false, None, None, None)
            .map(|(value, _)| value);

        let entry = crate::select_entry(&module, "Run").expect("entry resolves");
        let sequential_result =
            execute_resolved(&module, entry, false, None, None).map(|(value, _)| value);

        assert_eq!(prepared_result, sequential_result);
    }

    #[test]
    fn dropping_a_prepared_program_after_use_frees_jit_memory_exactly_once() {
        let module = compile("public int Run() { return 1; }");
        let (prepared, run) = prepare(&module, "Run");
        prepared
            .invoke(run, false, None, None, None)
            .expect("invocation succeeds");
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
