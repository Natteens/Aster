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
    ASTER_ARRAY_DATA_OFFSET, ASTER_ARRAY_LENGTH_OFFSET, ASTER_CALL_DEPTH_LIMIT, AsterArray,
    AsterDictionary, AsterList, AsterStringBuilder, DictionaryKeyKind, ExecutionContext,
    ListRegion, MemoryStats, aster_rt_array_element, aster_rt_array_length, aster_rt_call_enter,
    aster_rt_call_leave, aster_rt_dictionary_add, aster_rt_dictionary_contains_key,
    aster_rt_dictionary_entries, aster_rt_dictionary_length, aster_rt_dictionary_new,
    aster_rt_dictionary_new_temporary, aster_rt_dictionary_remove, aster_rt_dictionary_set,
    aster_rt_dictionary_try_get, aster_rt_has_error, aster_rt_list_add, aster_rt_list_get,
    aster_rt_list_length, aster_rt_list_new, aster_rt_list_new_temporary, aster_rt_list_remove_at,
    aster_rt_list_version, aster_rt_list_version_mismatch, aster_rt_string_builder_append,
    aster_rt_string_builder_new, aster_rt_string_builder_new_temporary,
    aster_rt_string_builder_to_string, aster_rt_string_builder_to_string_temporary,
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
