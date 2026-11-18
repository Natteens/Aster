//! Cranelift declarations and symbol binding for the async/parallel ABI.
//!
//! The `extern "C"` implementations live in `aster-exec`
//! ([`aster_exec::async_abi`]); this module only declares their signatures on
//! the JIT module and binds their addresses.

use aster_exec::async_abi::async_functions;

use super::{AbiParam, BackendError, Codegen, Linkage, Module, module_error, types};

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
                    signature.params.push(AbiParam::new(pointer)); // copied frame
                    signature.params.push(AbiParam::new(types::I32)); // frame bytes
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
