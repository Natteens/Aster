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
pub mod descriptor;
pub mod filesystem;
pub mod foreign;
mod host;
pub mod io;
pub mod io_error;
pub mod log;
mod math;
mod memory_governor;
mod object;
#[cfg(feature = "mem-exp-heap")]
pub mod object_heap;
pub mod registry;
pub mod string;
pub mod time;

#[cfg(feature = "mem-exp-rc")]
pub use context::{
    ASTER_ARRAY_COUNT_OFFSET, ASTER_DICTIONARY_COUNT_OFFSET, ASTER_DICTIONARY_DESCRIPTOR_OFFSET,
    ASTER_LIST_COUNT_OFFSET, ASTER_LIST_DESCRIPTOR_OFFSET, aster_rt_array_adopt_elements,
    aster_rt_array_set_element_descriptor, aster_rt_dictionary_set_descriptors,
    aster_rt_list_set_element_descriptor,
};
pub use context::{
    ASTER_ARRAY_DATA_OFFSET, ASTER_ARRAY_LENGTH_OFFSET, ASTER_CALL_DEPTH_LIMIT,
    AarmAllocatorEvents, AarmMemoryTelemetry, AarmRegionTelemetry, AarmRewindTelemetry, AsterArray,
    AsterDictionary, AsterList, AsterStringBuilder, DictionaryKeyKind,
    EXECUTION_CONTEXT_ERROR_FLAG_OFFSET, ExecutionContext, ListRegion, MemoryStats,
    TaskCancellation, aster_rt_array_element, aster_rt_array_length, aster_rt_array_new,
    aster_rt_array_new_temporary, aster_rt_call_enter, aster_rt_call_leave,
    aster_rt_dictionary_add, aster_rt_dictionary_capacity, aster_rt_dictionary_clear,
    aster_rt_dictionary_contains_key, aster_rt_dictionary_ensure_capacity,
    aster_rt_dictionary_entries, aster_rt_dictionary_get_or, aster_rt_dictionary_keys,
    aster_rt_dictionary_length, aster_rt_dictionary_new, aster_rt_dictionary_new_temporary,
    aster_rt_dictionary_remove, aster_rt_dictionary_set, aster_rt_dictionary_try_get,
    aster_rt_dictionary_values, aster_rt_has_error, aster_rt_list_add, aster_rt_list_add_range,
    aster_rt_list_capacity, aster_rt_list_clear, aster_rt_list_ensure_capacity, aster_rt_list_get,
    aster_rt_list_get_range, aster_rt_list_insert, aster_rt_list_length, aster_rt_list_new,
    aster_rt_list_new_temporary, aster_rt_list_remove_at, aster_rt_list_remove_range,
    aster_rt_list_reverse, aster_rt_list_set, aster_rt_list_to_array, aster_rt_list_version,
    aster_rt_list_version_mismatch, aster_rt_string_builder_append, aster_rt_string_builder_clear,
    aster_rt_string_builder_length, aster_rt_string_builder_new,
    aster_rt_string_builder_new_temporary, aster_rt_string_builder_to_string,
    aster_rt_string_builder_to_string_temporary, aster_rt_temporary_scope_enter,
};
#[cfg(feature = "mem-exp-rc")]
pub use context::{EXECUTION_CONTEXT_RELEASES_OFFSET, EXECUTION_CONTEXT_RETAINS_OFFSET};
pub use descriptor::{CLASS_FLAG_ABSTRACT, CLASS_FLAG_SEALED, ClassDescriptor, InterfaceEntry};
pub use filesystem::{
    FailingFileSystemBackend, FileSystemBackend, FileSystemError, MAX_FILE_BYTES, MAX_LIST_FILES,
    MAX_LIST_PATH_BYTES, MemoryFileSystemBackend, PartialWriteFailureFileSystemBackend,
    StdFileSystemBackend, aster_rt_io_append_all_text, aster_rt_io_list_directories,
    aster_rt_io_list_directories_temporary, aster_rt_io_list_files,
    aster_rt_io_list_files_temporary, aster_rt_io_path_bool, aster_rt_io_read_all_text,
    aster_rt_io_read_all_text_temporary, aster_rt_io_write_all_text,
};
pub use foreign::{ForeignRegistry, ForeignRegistryError, ForeignSignature, ForeignType};
pub use host::{
    aster_rt_env_get, aster_rt_env_get_temporary, aster_rt_env_has, aster_rt_io_write_error,
};
pub use io::{
    ConsoleBackend, FailingConsoleBackend, MemoryConsoleBackend, StdConsoleBackend,
    aster_rt_io_read_line, aster_rt_io_read_line_temporary, aster_rt_io_write,
    aster_rt_io_write_line,
};
pub use io_error::{PortableIoError, PortableIoErrorKind, classify_io_error};
pub use log::LogLevel;
pub use math::{aster_rt_random_entropy_seed, aster_rt_random_mix};
pub use memory_governor::{MemoryGovernor, MemoryGovernorTelemetry};
pub use registry::{RuntimeFunction, RuntimeSignature, RuntimeType, runtime_functions};
#[cfg(feature = "mem-exp-rc")]
pub use string::ASTER_STR_COUNT_OFFSET;
pub use string::{
    ASTER_STR_LEN_OFFSET, ASTER_STR_PAYLOAD_OFFSET, AsterStrHeader, decode_str, encode_str,
};
