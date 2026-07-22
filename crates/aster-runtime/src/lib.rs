//! Execution runtime for JIT-compiled Aster programs.
//!
//! This crate defines the boundary between generated Aster code and functions
//! provided by the host. It deliberately depends on no other Aster crate: it
//! does not know about the AST, HIR, MIR, the parser, or Cranelift. Backends
//! consume [`registry::runtime_functions`] to bind the exported symbols and
//! translate [`registry::RuntimeType`] into their own value types.
//!
//! The string ABI is documented in `docs/compiler/runtime-abi.md` and implemented in
//! [`string`]. No pointer handed across this boundary may outlive the JIT
//! module or session that produced it.

mod arena;
pub mod context;
pub mod log;
mod math;
mod object;
pub mod registry;
pub mod string;

pub use context::{
    AsterArray, ExecutionContext, MemoryStats, aster_rt_array_element, aster_rt_array_length,
};
pub use log::LogLevel;
pub use registry::{RuntimeFunction, RuntimeSignature, RuntimeType, runtime_functions};
pub use string::{AsterStrHeader, decode_str, encode_str};
