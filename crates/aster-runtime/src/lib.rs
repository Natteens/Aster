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
pub mod filesystem;
pub mod io;
pub mod io_error;
pub mod log;
mod math;
mod object;
pub mod registry;
pub mod string;

pub use context::{
    AsterArray, AsterList, ExecutionContext, ListRegion, MemoryStats, aster_rt_array_element,
    aster_rt_array_length, aster_rt_list_add, aster_rt_list_get, aster_rt_list_length,
    aster_rt_list_new, aster_rt_list_new_temporary, aster_rt_list_remove_at,
};
pub use filesystem::{
    FailingFileSystemBackend, FileSystemBackend, FileSystemError, MAX_FILE_BYTES, MAX_LIST_FILES,
    MAX_LIST_PATH_BYTES, MemoryFileSystemBackend, PartialWriteFailureFileSystemBackend,
    StdFileSystemBackend, aster_rt_io_list_files, aster_rt_io_list_files_temporary,
    aster_rt_io_read_all_text, aster_rt_io_read_all_text_temporary, aster_rt_io_write_all_text,
};
pub use io::{
    ConsoleBackend, FailingConsoleBackend, MemoryConsoleBackend, StdConsoleBackend,
    aster_rt_io_read_line, aster_rt_io_read_line_temporary, aster_rt_io_write,
    aster_rt_io_write_line,
};
pub use io_error::{PortableIoError, PortableIoErrorKind, classify_io_error};
pub use log::LogLevel;
pub use registry::{RuntimeFunction, RuntimeSignature, RuntimeType, runtime_functions};
pub use string::{AsterStrHeader, decode_str, encode_str};
