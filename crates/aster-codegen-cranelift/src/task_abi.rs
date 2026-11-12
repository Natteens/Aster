//! Cranelift declarations and symbol binding for the task ABI.
//!
//! The `extern "C"` implementations live in `aster-exec`
//! ([`aster_exec::task_abi`]); this module only declares their signatures on
//! the JIT module and binds their addresses, which is the part that needs
//! Cranelift types.

use aster_exec::task_abi::task_functions;
pub(super) use aster_exec::task_abi::{wait_all_symbol_for, wait_symbol_for};

use super::{AbiParam, BackendError, Codegen, Linkage, Module, module_error, types};

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

/// Bind every [`task_functions`] address into `builder`'s symbol table, for
/// use from `PreparedProgram::prepare` alongside `aster_runtime::runtime_functions()`.
pub(super) fn bind_task_functions(builder: &mut super::JITBuilder) {
    for (name, address) in task_functions() {
        builder.symbol(name, address);
    }
}
