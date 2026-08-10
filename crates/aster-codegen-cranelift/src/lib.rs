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
pub use aster_runtime::MemoryStats;
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
use layouts::Layouts;
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
