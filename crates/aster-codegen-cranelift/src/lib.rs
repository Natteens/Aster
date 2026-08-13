//! Cranelift JIT backend for validated Aster MIR.
//!
//! This crate depends only on `aster-mir` from the Aster compiler pipeline and
//! on `aster-runtime` for the execution ABI. It does not inspect syntax, AST,
//! or HIR, and it exposes no Cranelift types to other crates.

mod async_abi;
mod backend;
mod calls;
mod completion_queue;
mod control_flow;
mod declarations;
mod execution;
mod functions;
#[cfg(feature = "aarm-telemetry")]
mod host_memory;
mod interfaces;
mod layouts;
mod places;
mod scalar;
mod task_abi;
mod task_runtime;
mod validation;
mod values;
mod worker_pool;

use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
    error::Error,
    fmt,
};

use aster_mir as mir;
pub use aster_runtime::{AarmMemoryTelemetry, MemoryStats};
use aster_runtime::{RuntimeType, runtime_functions};
use aster_types::Primitive;
use cranelift_codegen::ir::{
    AbiParam, Block, InstBuilder, MemFlags, Signature, StackSlot, StackSlotData, StackSlotKind,
    TrapCode, Type as ClifType, Value,
    condcodes::{FloatCC, IntCC},
    types,
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module, default_libcall_names};

use backend::module_error;
use declarations::runtime_type;
use execution::execute_resolved;
#[cfg(feature = "aarm-telemetry")]
use execution::execute_resolved_with_aarm_async_governor;
#[cfg(feature = "aarm-telemetry")]
use execution::execute_resolved_with_aarm_parallel_governor;
#[cfg(feature = "aarm-telemetry")]
use execution::execute_resolved_with_aarm_parallel_workers;
#[cfg(feature = "aarm-telemetry")]
use execution::execute_resolved_with_aarm_task_governor;
#[cfg(feature = "aarm-telemetry")]
use execution::execute_resolved_with_aarm_telemetry;
#[cfg(feature = "aarm-telemetry")]
#[doc(hidden)]
pub use host_memory::{
    AarmAutoBudgetError, AarmAutoBudgetTelemetry, AarmAutoGovernor, AarmBudgetPolicy,
    AarmBudgetSource, AarmHostMemoryCapacity, AarmHostMemoryCapacitySource,
    aarm_auto_governor_from_capacity, aarm_explicit_governor, aarm_governor_from_policy,
    discover_aarm_auto_governor, discover_aarm_host_memory_capacity, resolve_aarm_auto_budget,
    resolve_aarm_explicit_budget,
};
use layouts::Layouts;
#[cfg(feature = "aarm-telemetry")]
#[doc(hidden)]
pub use task_runtime::{
    AarmAsyncMemoryDomainTelemetry, AarmParallelPlanningTelemetry, AarmTaskMemoryDomainTelemetry,
    parallel_chunk_budgets,
};
use validation::{select_entry, validate_invocable_entry, validate_module};
use values::{
    cast_value, integer_constant_bits, is_aggregate, primitive, scalar_from_bits, scalar_kind,
    scalar_to_bits, type_name,
};

struct Codegen {
    jit: JITModule,
    pointer_type: ClifType,
    string_data: HashMap<String, DataId>,
    runtime_ids: HashMap<&'static str, FuncId>,
    interface_tables: HashMap<(mir::SymbolId, mir::SymbolId), DataId>,
    interface_methods:
        HashMap<mir::SymbolId, (mir::SymbolId, usize, mir::InterfaceMethodDefinition)>,
    call_depth_guarded: HashSet<mir::SymbolId>,
    runtime_fallible_functions: HashSet<mir::SymbolId>,
    runtime_fallible_interface_methods: HashSet<mir::SymbolId>,
    layouts: Layouts,
}

struct FunctionState {
    slots: HashMap<mir::LocalId, StackSlot>,
    execution_context: Option<Value>,
    hidden_return: Option<Value>,
    temporary_scope: bool,
    call_depth_guarded: bool,
    runtime_failure: Block,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExecutionValue {
    SByte(i8),
    Byte(u8),
    Short(i16),
    UShort(u16),
    Int(i32),
    UInt(u32),
    Long(i64),
    ULong(u64),
    Float(f32),
    Double(f64),
    Bool(bool),
    Char(char),
    String(String),
    Void,
}

impl ExecutionValue {
    #[must_use]
    pub fn float(value: f32) -> Self {
        Self::Float(value)
    }

    #[must_use]
    pub fn double(value: f64) -> Self {
        Self::Double(value)
    }
}

impl fmt::Display for ExecutionValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SByte(value) => write!(formatter, "{value}"),
            Self::Byte(value) => write!(formatter, "{value}"),
            Self::Short(value) => write!(formatter, "{value}"),
            Self::UShort(value) => write!(formatter, "{value}"),
            Self::Int(value) => write!(formatter, "{value}"),
            Self::UInt(value) => write!(formatter, "{value}"),
            Self::Long(value) => write!(formatter, "{value}"),
            Self::ULong(value) => write!(formatter, "{value}"),
            Self::Float(value) => write!(formatter, "{value}"),
            Self::Double(value) => write!(formatter, "{value}"),
            Self::Bool(value) => write!(formatter, "{value}"),
            Self::Char(value) => write!(formatter, "{value}"),
            Self::String(value) => formatter.write_str(value),
            Self::Void => formatter.write_str("function completed successfully (void)"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendError {
    message: String,
}

impl BackendError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for BackendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for BackendError {}

/// Release-benchmark seam that prepares sequential machine code once and
/// invokes it repeatedly against fresh execution contexts.
///
/// Normal ASTER execution continues to use [`execute`]. This type exists so
/// research measurements can exclude MIR validation, Cranelift compilation,
/// and JIT finalization from the timed execution interval.
#[cfg(feature = "aarm-telemetry")]
#[doc(hidden)]
pub struct PreparedSequentialExecution {
    program: execution::PreparedProgram,
    entry: mir::SymbolId,
}

#[cfg(feature = "aarm-telemetry")]
impl PreparedSequentialExecution {
    /// Validate and finalize one sequential entry before measurement.
    ///
    /// # Errors
    ///
    /// Returns a controlled error for invalid MIR, an invalid entry, task
    /// operations, or Cranelift preparation failure.
    pub fn prepare(module: &mir::Module, function_name: &str) -> Result<Self, BackendError> {
        validate_module(module)?;
        if task_runtime::module_uses_tasks(module) {
            return Err(BackendError::new(
                "prepared sequential research execution does not support Task.Run",
            ));
        }
        let entry = select_entry(module, function_name)?.symbol;
        let program = execution::PreparedProgram::prepare(module)?;
        Ok(Self { program, entry })
    }

    /// Invoke finalized code once without collecting memory statistics.
    ///
    /// # Errors
    ///
    /// Returns the same controlled runtime and entry errors as [`execute`].
    pub fn invoke(&self) -> Result<ExecutionValue, BackendError> {
        self.program
            .invoke(self.entry, false, None, None, None)
            .map(|(value, _)| value)
    }

    /// Invoke finalized code once and return stable runtime memory statistics.
    ///
    /// # Errors
    ///
    /// Returns the same controlled runtime and entry errors as [`execute_with_stats`].
    pub fn invoke_with_stats(&self) -> Result<(ExecutionValue, MemoryStats), BackendError> {
        self.program.invoke(self.entry, true, None, None, None)
    }

    /// Invoke finalized code once with experimental allocator telemetry.
    ///
    /// # Errors
    ///
    /// Returns the same controlled runtime and entry errors as
    /// [`execute_with_aarm_telemetry`].
    #[cfg(feature = "aarm-telemetry")]
    pub fn invoke_with_aarm_telemetry(
        &self,
    ) -> Result<(ExecutionValue, AarmMemoryTelemetry), BackendError> {
        self.program.invoke_with_aarm_telemetry(self.entry, None)
    }
}

/// Compile a validated MIR module in memory and invoke one explicitly selected function.
///
/// If, and only if, `module` contains `aster.core.Task.Run`/`Task<T>.Wait`
/// anywhere (see `task_runtime::module_uses_tasks`), this call also creates
/// an internal task runtime (one `worker_pool::ExecutionPool` plus task
/// entries) for the duration of this call only, then fully shuts it down
/// before returning — never a global, never a singleton, and never created
/// at all for a module that never uses tasks. Every entry point in this
/// file shares this same behavior through `execution::execute_resolved`;
/// there is exactly one way to run an Aster program.
///
/// # Errors
///
/// Returns a controlled error for an invalid entry selection, unsupported MIR, or a
/// Cranelift declaration/compilation/finalization failure.
pub fn execute(module: &mir::Module, function_name: &str) -> Result<ExecutionValue, BackendError> {
    validate_module(module)?;
    let entry = select_entry(module, function_name)?;
    execute_resolved(module, entry, false, None, None).map(|(value, _)| value)
}

/// Like [`execute`], but also returns runtime allocation metrics.
///
/// # Errors
///
/// Returns a controlled error for an invalid entry selection, unsupported MIR, or a
/// Cranelift declaration/compilation/finalization failure.
pub fn execute_with_stats(
    module: &mir::Module,
    function_name: &str,
) -> Result<(ExecutionValue, MemoryStats), BackendError> {
    validate_module(module)?;
    let entry = select_entry(module, function_name)?;
    execute_resolved(module, entry, true, None, None)
}

/// Execute one entry with the opt-in experimental AARM allocator telemetry.
/// Stable CLI memory-statistics output remains unchanged.
///
/// # Errors
///
/// Returns the same controlled validation, preparation, and runtime errors as
/// [`execute_with_stats`].
#[doc(hidden)]
#[cfg(feature = "aarm-telemetry")]
pub fn execute_with_aarm_telemetry(
    module: &mir::Module,
    function_name: &str,
) -> Result<(ExecutionValue, AarmMemoryTelemetry), BackendError> {
    validate_module(module)?;
    let entry = select_entry(module, function_name)?;
    execute_resolved_with_aarm_telemetry(module, entry)
}

/// Execute with the experimental shared governor applied to the main context
/// and deterministic logical Parallel chunk partitions.
///
/// Ordinary `Task.Run` and async worker contexts remain ungoverned.
///
/// # Errors
///
/// Returns the same controlled validation, preparation, and runtime errors as
/// [`execute_with_aarm_telemetry`].
#[doc(hidden)]
#[cfg(feature = "aarm-telemetry")]
pub fn execute_with_aarm_parallel_governor(
    module: &mir::Module,
    function_name: &str,
    worker_count: usize,
    governor: std::sync::Arc<aster_runtime::MemoryGovernor>,
) -> Result<
    (
        ExecutionValue,
        AarmMemoryTelemetry,
        Vec<AarmParallelPlanningTelemetry>,
        Vec<AarmMemoryTelemetry>,
    ),
    BackendError,
> {
    validate_module(module)?;
    let entry = select_entry(module, function_name)?;
    execute_resolved_with_aarm_parallel_governor(module, entry, worker_count, governor)
}

/// Execute the ordinary ungoverned Parallel runtime with an explicit worker
/// count for AARM comparison measurements.
///
/// # Errors
///
/// Returns the same controlled validation, preparation, and runtime errors as
/// [`execute_with_aarm_telemetry`].
#[doc(hidden)]
#[cfg(feature = "aarm-telemetry")]
pub fn execute_with_aarm_parallel_workers(
    module: &mir::Module,
    function_name: &str,
    worker_count: usize,
) -> Result<(ExecutionValue, AarmMemoryTelemetry), BackendError> {
    validate_module(module)?;
    let entry = select_entry(module, function_name)?;
    execute_resolved_with_aarm_parallel_workers(module, entry, worker_count)
}

/// Execute with the experimental governor applied to Main and plain
/// `Task.Run` through one frozen deterministic task memory domain.
///
/// Async and Parallel execution are rejected by this research-only entry
/// point until their memory domains are integrated explicitly.
///
/// # Errors
///
/// Returns controlled validation, unsupported-domain, preparation, and
/// runtime errors.
#[doc(hidden)]
#[cfg(feature = "aarm-telemetry")]
pub fn execute_with_aarm_task_governor(
    module: &mir::Module,
    function_name: &str,
    worker_count: usize,
    governor: std::sync::Arc<aster_runtime::MemoryGovernor>,
) -> Result<
    (
        ExecutionValue,
        AarmMemoryTelemetry,
        Option<AarmTaskMemoryDomainTelemetry>,
    ),
    BackendError,
> {
    validate_module(module)?;
    let entry = select_entry(module, function_name)?;
    execute_resolved_with_aarm_task_governor(module, entry, worker_count, governor)
}

/// Execute with one experimental governor shared by Main, async `MoveNext`
/// contexts, and awaited-inner worker contexts through a frozen async domain.
/// Independent plain `Task.Run` and Parallel operations are rejected.
///
/// # Errors
///
/// Returns controlled validation, unsupported-domain, preparation, and
/// runtime errors.
#[doc(hidden)]
#[cfg(feature = "aarm-telemetry")]
pub fn execute_with_aarm_async_governor(
    module: &mir::Module,
    function_name: &str,
    worker_count: usize,
    governor: std::sync::Arc<aster_runtime::MemoryGovernor>,
) -> Result<
    (
        ExecutionValue,
        AarmMemoryTelemetry,
        Option<AarmAsyncMemoryDomainTelemetry>,
    ),
    BackendError,
> {
    validate_module(module)?;
    let entry = select_entry(module, function_name)?;
    execute_resolved_with_aarm_async_governor(module, entry, worker_count, governor)
}

/// Resolve one frozen experimental Auto budget and apply its one shared
/// governor to deterministic Parallel execution.
#[doc(hidden)]
#[cfg(feature = "aarm-telemetry")]
pub type AarmAutoParallelExecution = (
    ExecutionValue,
    AarmMemoryTelemetry,
    Vec<AarmParallelPlanningTelemetry>,
    Vec<AarmMemoryTelemetry>,
    AarmAutoBudgetTelemetry,
);

/// Resolve one frozen experimental Auto budget and apply its one shared
/// governor to deterministic Parallel execution.
#[doc(hidden)]
#[cfg(feature = "aarm-telemetry")]
pub fn execute_with_aarm_auto_parallel_governor(
    module: &mir::Module,
    function_name: &str,
    worker_count: usize,
) -> Result<AarmAutoParallelExecution, BackendError> {
    let auto =
        discover_aarm_auto_governor().map_err(|error| BackendError::new(error.to_string()))?;
    let telemetry = auto.telemetry();
    execute_with_aarm_parallel_governor(module, function_name, worker_count, auto.governor())
        .map(|(value, main, plans, workers)| (value, main, plans, workers, telemetry))
}

/// Resolve one frozen experimental Auto budget and apply its one shared
/// governor to deterministic plain `Task.Run` execution.
#[doc(hidden)]
#[cfg(feature = "aarm-telemetry")]
pub fn execute_with_aarm_auto_task_governor(
    module: &mir::Module,
    function_name: &str,
    worker_count: usize,
) -> Result<
    (
        ExecutionValue,
        AarmMemoryTelemetry,
        Option<AarmTaskMemoryDomainTelemetry>,
        AarmAutoBudgetTelemetry,
    ),
    BackendError,
> {
    let auto =
        discover_aarm_auto_governor().map_err(|error| BackendError::new(error.to_string()))?;
    let telemetry = auto.telemetry();
    execute_with_aarm_task_governor(module, function_name, worker_count, auto.governor())
        .map(|(value, main, domain)| (value, main, domain, telemetry))
}

/// Resolve one frozen experimental Auto budget and apply its one shared
/// governor to deterministic governed async execution.
#[doc(hidden)]
#[cfg(feature = "aarm-telemetry")]
pub fn execute_with_aarm_auto_async_governor(
    module: &mir::Module,
    function_name: &str,
    worker_count: usize,
) -> Result<
    (
        ExecutionValue,
        AarmMemoryTelemetry,
        Option<AarmAsyncMemoryDomainTelemetry>,
        AarmAutoBudgetTelemetry,
    ),
    BackendError,
> {
    let auto =
        discover_aarm_auto_governor().map_err(|error| BackendError::new(error.to_string()))?;
    let telemetry = auto.telemetry();
    execute_with_aarm_async_governor(module, function_name, worker_count, auto.governor())
        .map(|(value, main, domain)| (value, main, domain, telemetry))
}

/// Resolve one exact experimental budget and apply its one shared governor to
/// deterministic Parallel execution without host-capacity discovery.
#[doc(hidden)]
#[cfg(feature = "aarm-telemetry")]
pub fn execute_with_aarm_exact_parallel_governor(
    module: &mir::Module,
    function_name: &str,
    worker_count: usize,
    requested_bytes: u64,
) -> Result<AarmAutoParallelExecution, BackendError> {
    let explicit = aarm_explicit_governor(requested_bytes)
        .map_err(|error| BackendError::new(error.to_string()))?;
    let telemetry = explicit.telemetry();
    execute_with_aarm_parallel_governor(module, function_name, worker_count, explicit.governor())
        .map(|(value, main, plans, workers)| (value, main, plans, workers, telemetry))
}

/// Resolve one exact experimental budget and apply its one shared governor to
/// deterministic plain `Task.Run` execution without host-capacity discovery.
#[doc(hidden)]
#[cfg(feature = "aarm-telemetry")]
pub fn execute_with_aarm_exact_task_governor(
    module: &mir::Module,
    function_name: &str,
    worker_count: usize,
    requested_bytes: u64,
) -> Result<
    (
        ExecutionValue,
        AarmMemoryTelemetry,
        Option<AarmTaskMemoryDomainTelemetry>,
        AarmAutoBudgetTelemetry,
    ),
    BackendError,
> {
    let explicit = aarm_explicit_governor(requested_bytes)
        .map_err(|error| BackendError::new(error.to_string()))?;
    let telemetry = explicit.telemetry();
    execute_with_aarm_task_governor(module, function_name, worker_count, explicit.governor())
        .map(|(value, main, domain)| (value, main, domain, telemetry))
}

/// Resolve one exact experimental budget and apply its one shared governor to
/// deterministic governed async execution without host-capacity discovery.
#[doc(hidden)]
#[cfg(feature = "aarm-telemetry")]
pub fn execute_with_aarm_exact_async_governor(
    module: &mir::Module,
    function_name: &str,
    worker_count: usize,
    requested_bytes: u64,
) -> Result<
    (
        ExecutionValue,
        AarmMemoryTelemetry,
        Option<AarmAsyncMemoryDomainTelemetry>,
        AarmAutoBudgetTelemetry,
    ),
    BackendError,
> {
    let explicit = aarm_explicit_governor(requested_bytes)
        .map_err(|error| BackendError::new(error.to_string()))?;
    let telemetry = explicit.telemetry();
    execute_with_aarm_async_governor(module, function_name, worker_count, explicit.governor())
        .map(|(value, main, domain)| (value, main, domain, telemetry))
}

/// Like [`execute`], but injects `console_backend` for `aster.io.Write`/
/// `WriteLine`/`ReadLine` instead of the default real stdin/stdout. Intended
/// for tests: pass an in-memory backend to observe output and supply input
/// without touching the developer's or CI's real terminal.
///
/// # Errors
///
/// Returns a controlled error for an invalid entry selection, unsupported MIR, or a
/// Cranelift declaration/compilation/finalization failure.
pub fn execute_with_console(
    module: &mir::Module,
    function_name: &str,
    console_backend: Box<dyn aster_runtime::ConsoleBackend>,
) -> Result<ExecutionValue, BackendError> {
    validate_module(module)?;
    let entry = select_entry(module, function_name)?;
    execute_resolved(module, entry, false, Some(console_backend), None).map(|(value, _)| value)
}

/// Like [`execute_with_console`], but also returns runtime allocation
/// metrics.
///
/// # Errors
///
/// Returns a controlled error for an invalid entry selection, unsupported MIR, or a
/// Cranelift declaration/compilation/finalization failure.
pub fn execute_with_console_and_stats(
    module: &mir::Module,
    function_name: &str,
    console_backend: Box<dyn aster_runtime::ConsoleBackend>,
) -> Result<(ExecutionValue, MemoryStats), BackendError> {
    validate_module(module)?;
    let entry = select_entry(module, function_name)?;
    execute_resolved(module, entry, true, Some(console_backend), None)
}

/// Like [`execute`], but injects `filesystem_backend` for `aster.io.
/// ReadAllText`/`WriteAllText` instead of the default real filesystem.
/// Intended for tests: pass an in-memory backend so file I/O never touches
/// the developer's or CI's real filesystem.
///
/// # Errors
///
/// Returns a controlled error for an invalid entry selection, unsupported MIR, or a
/// Cranelift declaration/compilation/finalization failure.
pub fn execute_with_filesystem(
    module: &mir::Module,
    function_name: &str,
    filesystem_backend: Box<dyn aster_runtime::FileSystemBackend>,
) -> Result<ExecutionValue, BackendError> {
    validate_module(module)?;
    let entry = select_entry(module, function_name)?;
    execute_resolved(module, entry, false, None, Some(filesystem_backend)).map(|(value, _)| value)
}

/// Like [`execute_with_filesystem`], but also returns runtime allocation
/// metrics.
///
/// # Errors
///
/// Returns a controlled error for an invalid entry selection, unsupported MIR, or a
/// Cranelift declaration/compilation/finalization failure.
pub fn execute_with_filesystem_and_stats(
    module: &mir::Module,
    function_name: &str,
    filesystem_backend: Box<dyn aster_runtime::FileSystemBackend>,
) -> Result<(ExecutionValue, MemoryStats), BackendError> {
    validate_module(module)?;
    let entry = select_entry(module, function_name)?;
    execute_resolved(module, entry, true, None, Some(filesystem_backend))
}

/// Compile validated MIR and invoke the concrete function selected by the
/// compiler's application-entry layer.
///
/// # Errors
///
/// Returns a controlled error if the symbol is missing or cannot use the
/// zero-parameter host invocation ABI.
pub fn execute_symbol(
    module: &mir::Module,
    symbol: mir::SymbolId,
) -> Result<ExecutionValue, BackendError> {
    validate_module(module)?;
    let entry = module
        .functions
        .iter()
        .find(|function| function.symbol == symbol)
        .ok_or_else(|| BackendError::new(format!("entry symbol {symbol:?} was not found")))?;
    validate_invocable_entry(entry, &entry.name)?;
    execute_resolved(module, entry, false, None, None).map(|(value, _)| value)
}

/// Like [`execute_symbol`], but also returns runtime allocation metrics.
///
/// # Errors
///
/// Returns a controlled error if the symbol is missing or cannot use the
/// zero-parameter host invocation ABI.
pub fn execute_symbol_with_stats(
    module: &mir::Module,
    symbol: mir::SymbolId,
) -> Result<(ExecutionValue, MemoryStats), BackendError> {
    validate_module(module)?;
    let entry = module
        .functions
        .iter()
        .find(|function| function.symbol == symbol)
        .ok_or_else(|| BackendError::new(format!("entry symbol {symbol:?} was not found")))?;
    validate_invocable_entry(entry, &entry.name)?;
    execute_resolved(module, entry, true, None, None)
}

/// Runs the same structural MIR validation every `execute*` function performs
/// (shapes, symbols, regions, worker-body restrictions like console I/O in a
/// `Task.Run`/`Parallel.*` body) without finalizing or running any code. Lets
/// verification-only tools (`aster check`) reject invalid MIR before
/// execution, reusing the one validator/call-graph analysis `execute*`
/// already runs instead of a second, divergent check.
///
/// # Errors
///
/// Returns a controlled error describing the first structural violation found.
pub fn validate(module: &mir::Module) -> Result<(), BackendError> {
    validate_module(module)
}
