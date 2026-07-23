//! Central registry of every function the runtime exports to generated code.
//!
//! Backends bind these symbols by name and address and translate
//! [`RuntimeType`] into their own value representation. Adding a future
//! runtime module (files, time, windowing, audio, networking, ECS) means
//! adding entries here — never special cases inside a backend.

use crate::context::{
    aster_rt_array_element, aster_rt_array_length, aster_rt_array_new,
    aster_rt_array_new_temporary, aster_rt_list_add, aster_rt_list_get, aster_rt_list_length,
    aster_rt_list_new, aster_rt_list_new_temporary, aster_rt_list_remove_at, aster_rt_list_version,
    aster_rt_list_version_mismatch, aster_rt_temporary_scope_enter, aster_rt_temporary_scope_leave,
};
use crate::filesystem::{
    aster_rt_io_list_files, aster_rt_io_list_files_temporary, aster_rt_io_read_all_text,
    aster_rt_io_read_all_text_temporary, aster_rt_io_write_all_text,
};
use crate::io::{
    aster_rt_io_read_line, aster_rt_io_read_line_temporary, aster_rt_io_write,
    aster_rt_io_write_line,
};
use crate::log::aster_rt_log;
use crate::math::{aster_rt_integer_arithmetic_error, aster_rt_math_domain_error};
use crate::object::{aster_rt_object_new, aster_rt_object_new_temporary};
use crate::string::{
    aster_rt_string_byte_length, aster_rt_string_concat, aster_rt_string_concat_temporary,
    aster_rt_string_contains, aster_rt_string_decode_next, aster_rt_string_ends_with,
    aster_rt_string_eq, aster_rt_string_from_bool, aster_rt_string_from_bool_temporary,
    aster_rt_string_from_char, aster_rt_string_from_char_temporary, aster_rt_string_from_double,
    aster_rt_string_from_double_temporary, aster_rt_string_from_float,
    aster_rt_string_from_float_temporary, aster_rt_string_from_long,
    aster_rt_string_from_long_temporary, aster_rt_string_from_ulong,
    aster_rt_string_from_ulong_temporary, aster_rt_string_index_of, aster_rt_string_join,
    aster_rt_string_join_temporary, aster_rt_string_length, aster_rt_string_starts_with,
    aster_rt_string_substring_from, aster_rt_string_substring_from_temporary,
    aster_rt_string_substring_range, aster_rt_string_substring_range_temporary,
    aster_rt_string_try_parse_bool, aster_rt_string_try_parse_double,
    aster_rt_string_try_parse_float, aster_rt_string_try_parse_int, aster_rt_string_try_parse_long,
    aster_rt_string_try_parse_uint, aster_rt_string_try_parse_ulong,
};

/// Backend-neutral value type used in runtime signatures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeType {
    /// 8-bit integer; Aster `bool` uses `0`/`1`.
    I8,
    /// 32-bit signed integer.
    I32,
    /// 64-bit signed integer; `ulong` crosses the ABI as this same bit pattern.
    I64,
    /// 32-bit floating point.
    F32,
    /// 64-bit floating point.
    F64,
    /// Target-width pointer, e.g. `*const AsterStrHeader` for `string`.
    Pointer,
}

/// The exact `extern "C"` signature of one runtime function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeSignature {
    pub parameters: &'static [RuntimeType],
    pub result: Option<RuntimeType>,
}

/// One function exported by the runtime to generated code.
#[derive(Clone, Copy, Debug)]
pub struct RuntimeFunction {
    /// Stable symbol name used by backends to bind the function.
    pub name: &'static str,
    /// Host address of the `extern "C"` implementation.
    pub address: *const u8,
    pub signature: RuntimeSignature,
}

/// Every runtime function, in a stable order.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn runtime_functions() -> Vec<RuntimeFunction> {
    vec![
        RuntimeFunction {
            name: "aster_rt_log",
            address: aster_rt_log as *const u8,
            signature: RuntimeSignature {
                parameters: &[RuntimeType::I32, RuntimeType::Pointer],
                result: None,
            },
        },
        RuntimeFunction {
            name: "aster_rt_array_new",
            address: aster_rt_array_new as *const u8,
            signature: RuntimeSignature {
                parameters: &[RuntimeType::Pointer, RuntimeType::I32, RuntimeType::I32],
                result: Some(RuntimeType::Pointer),
            },
        },
        RuntimeFunction {
            name: "aster_rt_array_new_temporary",
            address: aster_rt_array_new_temporary as *const u8,
            signature: RuntimeSignature {
                parameters: &[RuntimeType::Pointer, RuntimeType::I32, RuntimeType::I32],
                result: Some(RuntimeType::Pointer),
            },
        },
        RuntimeFunction {
            name: "aster_rt_object_new",
            address: aster_rt_object_new as *const u8,
            signature: RuntimeSignature {
                parameters: &[RuntimeType::Pointer, RuntimeType::I32],
                result: Some(RuntimeType::Pointer),
            },
        },
        RuntimeFunction {
            name: "aster_rt_object_new_temporary",
            address: aster_rt_object_new_temporary as *const u8,
            signature: RuntimeSignature {
                parameters: &[RuntimeType::Pointer, RuntimeType::I32],
                result: Some(RuntimeType::Pointer),
            },
        },
        RuntimeFunction {
            name: "aster_rt_temporary_scope_enter",
            address: aster_rt_temporary_scope_enter as *const u8,
            signature: RuntimeSignature {
                parameters: &[RuntimeType::Pointer],
                result: None,
            },
        },
        RuntimeFunction {
            name: "aster_rt_temporary_scope_leave",
            address: aster_rt_temporary_scope_leave as *const u8,
            signature: RuntimeSignature {
                parameters: &[RuntimeType::Pointer],
                result: None,
            },
        },
        RuntimeFunction {
            name: "aster_rt_array_element",
            address: aster_rt_array_element as *const u8,
            signature: RuntimeSignature {
                parameters: &[RuntimeType::Pointer, RuntimeType::Pointer, RuntimeType::I32],
                result: Some(RuntimeType::Pointer),
            },
        },
        RuntimeFunction {
            name: "aster_rt_array_length",
            address: aster_rt_array_length as *const u8,
            signature: RuntimeSignature {
                parameters: &[RuntimeType::Pointer, RuntimeType::Pointer],
                result: Some(RuntimeType::I32),
            },
        },
        RuntimeFunction {
            name: "aster_rt_list_new",
            address: aster_rt_list_new as *const u8,
            signature: RuntimeSignature {
                parameters: &[
                    RuntimeType::Pointer,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I64,
                ],
                result: Some(RuntimeType::Pointer),
            },
        },
        RuntimeFunction {
            name: "aster_rt_list_new_temporary",
            address: aster_rt_list_new_temporary as *const u8,
            signature: RuntimeSignature {
                parameters: &[
                    RuntimeType::Pointer,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I64,
                ],
                result: Some(RuntimeType::Pointer),
            },
        },
        RuntimeFunction {
            name: "aster_rt_list_length",
            address: aster_rt_list_length as *const u8,
            signature: RuntimeSignature {
                parameters: &[RuntimeType::Pointer, RuntimeType::Pointer],
                result: Some(RuntimeType::I32),
            },
        },
        RuntimeFunction {
            name: "aster_rt_list_version",
            address: aster_rt_list_version as *const u8,
            signature: RuntimeSignature {
                parameters: &[RuntimeType::Pointer, RuntimeType::Pointer],
                result: Some(RuntimeType::I64),
            },
        },
        RuntimeFunction {
            name: "aster_rt_list_version_mismatch",
            address: aster_rt_list_version_mismatch as *const u8,
            signature: RuntimeSignature {
                parameters: &[RuntimeType::Pointer],
                result: None,
            },
        },
        RuntimeFunction {
            name: "aster_rt_list_add",
            address: aster_rt_list_add as *const u8,
            signature: RuntimeSignature {
                parameters: &[
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I64,
                    RuntimeType::Pointer,
                ],
                result: None,
            },
        },
        RuntimeFunction {
            name: "aster_rt_list_get",
            address: aster_rt_list_get as *const u8,
            signature: RuntimeSignature {
                parameters: &[
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I64,
                    RuntimeType::I32,
                    RuntimeType::Pointer,
                ],
                result: None,
            },
        },
        RuntimeFunction {
            name: "aster_rt_list_remove_at",
            address: aster_rt_list_remove_at as *const u8,
            signature: RuntimeSignature {
                parameters: &[
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I64,
                    RuntimeType::I32,
                ],
                result: None,
            },
        },
        RuntimeFunction {
            name: "aster_rt_string_eq",
            address: aster_rt_string_eq as *const u8,
            signature: RuntimeSignature {
                parameters: &[RuntimeType::Pointer, RuntimeType::Pointer],
                result: Some(RuntimeType::I8),
            },
        },
        RuntimeFunction {
            name: "aster_rt_string_concat",
            address: aster_rt_string_concat as *const u8,
            signature: RuntimeSignature {
                parameters: &[
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                ],
                result: Some(RuntimeType::Pointer),
            },
        },
        RuntimeFunction {
            name: "aster_rt_string_concat_temporary",
            address: aster_rt_string_concat_temporary as *const u8,
            signature: RuntimeSignature {
                parameters: &[
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                ],
                result: Some(RuntimeType::Pointer),
            },
        },
        RuntimeFunction {
            name: "aster_rt_string_length",
            address: aster_rt_string_length as *const u8,
            signature: RuntimeSignature {
                parameters: &[RuntimeType::Pointer, RuntimeType::Pointer],
                result: Some(RuntimeType::I32),
            },
        },
        RuntimeFunction {
            name: "aster_rt_string_byte_length",
            address: aster_rt_string_byte_length as *const u8,
            signature: RuntimeSignature {
                parameters: &[RuntimeType::Pointer, RuntimeType::Pointer],
                result: Some(RuntimeType::I32),
            },
        },
        RuntimeFunction {
            name: "aster_rt_string_decode_next",
            address: aster_rt_string_decode_next as *const u8,
            signature: RuntimeSignature {
                parameters: &[
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                    RuntimeType::I32,
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                ],
                result: Some(RuntimeType::I8),
            },
        },
        RuntimeFunction {
            name: "aster_rt_string_contains",
            address: aster_rt_string_contains as *const u8,
            signature: RuntimeSignature {
                parameters: &[
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                ],
                result: Some(RuntimeType::I8),
            },
        },
        RuntimeFunction {
            name: "aster_rt_string_starts_with",
            address: aster_rt_string_starts_with as *const u8,
            signature: RuntimeSignature {
                parameters: &[
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                ],
                result: Some(RuntimeType::I8),
            },
        },
        RuntimeFunction {
            name: "aster_rt_string_ends_with",
            address: aster_rt_string_ends_with as *const u8,
            signature: RuntimeSignature {
                parameters: &[
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                ],
                result: Some(RuntimeType::I8),
            },
        },
        RuntimeFunction {
            name: "aster_rt_string_index_of",
            address: aster_rt_string_index_of as *const u8,
            signature: RuntimeSignature {
                parameters: &[
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                ],
                result: Some(RuntimeType::I32),
            },
        },
        RuntimeFunction {
            name: "aster_rt_string_substring_from",
            address: aster_rt_string_substring_from as *const u8,
            signature: RuntimeSignature {
                parameters: &[RuntimeType::Pointer, RuntimeType::Pointer, RuntimeType::I32],
                result: Some(RuntimeType::Pointer),
            },
        },
        RuntimeFunction {
            name: "aster_rt_string_substring_from_temporary",
            address: aster_rt_string_substring_from_temporary as *const u8,
            signature: RuntimeSignature {
                parameters: &[RuntimeType::Pointer, RuntimeType::Pointer, RuntimeType::I32],
                result: Some(RuntimeType::Pointer),
            },
        },
        RuntimeFunction {
            name: "aster_rt_string_substring_range",
            address: aster_rt_string_substring_range as *const u8,
            signature: RuntimeSignature {
                parameters: &[
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                    RuntimeType::I32,
                    RuntimeType::I32,
                ],
                result: Some(RuntimeType::Pointer),
            },
        },
        RuntimeFunction {
            name: "aster_rt_string_substring_range_temporary",
            address: aster_rt_string_substring_range_temporary as *const u8,
            signature: RuntimeSignature {
                parameters: &[
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                    RuntimeType::I32,
                    RuntimeType::I32,
                ],
                result: Some(RuntimeType::Pointer),
            },
        },
        RuntimeFunction {
            name: "aster_rt_string_from_long",
            address: aster_rt_string_from_long as *const u8,
            signature: RuntimeSignature {
                parameters: &[RuntimeType::Pointer, RuntimeType::I64],
                result: Some(RuntimeType::Pointer),
            },
        },
        RuntimeFunction {
            name: "aster_rt_string_from_long_temporary",
            address: aster_rt_string_from_long_temporary as *const u8,
            signature: RuntimeSignature {
                parameters: &[RuntimeType::Pointer, RuntimeType::I64],
                result: Some(RuntimeType::Pointer),
            },
        },
        RuntimeFunction {
            name: "aster_rt_string_from_ulong",
            address: aster_rt_string_from_ulong as *const u8,
            signature: RuntimeSignature {
                parameters: &[RuntimeType::Pointer, RuntimeType::I64],
                result: Some(RuntimeType::Pointer),
            },
        },
        RuntimeFunction {
            name: "aster_rt_string_from_ulong_temporary",
            address: aster_rt_string_from_ulong_temporary as *const u8,
            signature: RuntimeSignature {
                parameters: &[RuntimeType::Pointer, RuntimeType::I64],
                result: Some(RuntimeType::Pointer),
            },
        },
        RuntimeFunction {
            name: "aster_rt_string_from_double",
            address: aster_rt_string_from_double as *const u8,
            signature: RuntimeSignature {
                parameters: &[RuntimeType::Pointer, RuntimeType::F64],
                result: Some(RuntimeType::Pointer),
            },
        },
        RuntimeFunction {
            name: "aster_rt_string_from_double_temporary",
            address: aster_rt_string_from_double_temporary as *const u8,
            signature: RuntimeSignature {
                parameters: &[RuntimeType::Pointer, RuntimeType::F64],
                result: Some(RuntimeType::Pointer),
            },
        },
        RuntimeFunction {
            name: "aster_rt_string_from_float",
            address: aster_rt_string_from_float as *const u8,
            signature: RuntimeSignature {
                parameters: &[RuntimeType::Pointer, RuntimeType::F32],
                result: Some(RuntimeType::Pointer),
            },
        },
        RuntimeFunction {
            name: "aster_rt_string_from_float_temporary",
            address: aster_rt_string_from_float_temporary as *const u8,
            signature: RuntimeSignature {
                parameters: &[RuntimeType::Pointer, RuntimeType::F32],
                result: Some(RuntimeType::Pointer),
            },
        },
        RuntimeFunction {
            name: "aster_rt_string_from_bool",
            address: aster_rt_string_from_bool as *const u8,
            signature: RuntimeSignature {
                parameters: &[RuntimeType::Pointer, RuntimeType::I8],
                result: Some(RuntimeType::Pointer),
            },
        },
        RuntimeFunction {
            name: "aster_rt_string_from_bool_temporary",
            address: aster_rt_string_from_bool_temporary as *const u8,
            signature: RuntimeSignature {
                parameters: &[RuntimeType::Pointer, RuntimeType::I8],
                result: Some(RuntimeType::Pointer),
            },
        },
        RuntimeFunction {
            name: "aster_rt_string_from_char",
            address: aster_rt_string_from_char as *const u8,
            signature: RuntimeSignature {
                parameters: &[RuntimeType::Pointer, RuntimeType::I32],
                result: Some(RuntimeType::Pointer),
            },
        },
        RuntimeFunction {
            name: "aster_rt_string_from_char_temporary",
            address: aster_rt_string_from_char_temporary as *const u8,
            signature: RuntimeSignature {
                parameters: &[RuntimeType::Pointer, RuntimeType::I32],
                result: Some(RuntimeType::Pointer),
            },
        },
        RuntimeFunction {
            name: "aster_rt_string_join",
            address: aster_rt_string_join as *const u8,
            signature: RuntimeSignature {
                parameters: &[RuntimeType::Pointer, RuntimeType::Pointer, RuntimeType::I32],
                result: Some(RuntimeType::Pointer),
            },
        },
        RuntimeFunction {
            name: "aster_rt_string_join_temporary",
            address: aster_rt_string_join_temporary as *const u8,
            signature: RuntimeSignature {
                parameters: &[RuntimeType::Pointer, RuntimeType::Pointer, RuntimeType::I32],
                result: Some(RuntimeType::Pointer),
            },
        },
        RuntimeFunction {
            name: "aster_rt_string_try_parse_bool",
            address: aster_rt_string_try_parse_bool as *const u8,
            signature: RuntimeSignature {
                parameters: &[
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I32,
                ],
                result: None,
            },
        },
        RuntimeFunction {
            name: "aster_rt_string_try_parse_int",
            address: aster_rt_string_try_parse_int as *const u8,
            signature: RuntimeSignature {
                parameters: &[
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I32,
                ],
                result: None,
            },
        },
        RuntimeFunction {
            name: "aster_rt_string_try_parse_uint",
            address: aster_rt_string_try_parse_uint as *const u8,
            signature: RuntimeSignature {
                parameters: &[
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I32,
                ],
                result: None,
            },
        },
        RuntimeFunction {
            name: "aster_rt_string_try_parse_long",
            address: aster_rt_string_try_parse_long as *const u8,
            signature: RuntimeSignature {
                parameters: &[
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I32,
                ],
                result: None,
            },
        },
        RuntimeFunction {
            name: "aster_rt_string_try_parse_ulong",
            address: aster_rt_string_try_parse_ulong as *const u8,
            signature: RuntimeSignature {
                parameters: &[
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I32,
                ],
                result: None,
            },
        },
        RuntimeFunction {
            name: "aster_rt_string_try_parse_float",
            address: aster_rt_string_try_parse_float as *const u8,
            signature: RuntimeSignature {
                parameters: &[
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I32,
                ],
                result: None,
            },
        },
        RuntimeFunction {
            name: "aster_rt_string_try_parse_double",
            address: aster_rt_string_try_parse_double as *const u8,
            signature: RuntimeSignature {
                parameters: &[
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I32,
                ],
                result: None,
            },
        },
        RuntimeFunction {
            name: "aster_rt_io_write",
            address: aster_rt_io_write as *const u8,
            signature: RuntimeSignature {
                parameters: &[RuntimeType::Pointer, RuntimeType::Pointer],
                result: None,
            },
        },
        RuntimeFunction {
            name: "aster_rt_io_write_line",
            address: aster_rt_io_write_line as *const u8,
            signature: RuntimeSignature {
                parameters: &[RuntimeType::Pointer, RuntimeType::Pointer],
                result: None,
            },
        },
        RuntimeFunction {
            name: "aster_rt_io_read_line",
            address: aster_rt_io_read_line as *const u8,
            signature: RuntimeSignature {
                parameters: &[
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I32,
                ],
                result: None,
            },
        },
        RuntimeFunction {
            name: "aster_rt_io_read_line_temporary",
            address: aster_rt_io_read_line_temporary as *const u8,
            signature: RuntimeSignature {
                parameters: &[
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I32,
                ],
                result: None,
            },
        },
        RuntimeFunction {
            name: "aster_rt_io_read_all_text",
            address: aster_rt_io_read_all_text as *const u8,
            signature: RuntimeSignature {
                parameters: &[
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::Pointer,
                ],
                result: None,
            },
        },
        RuntimeFunction {
            name: "aster_rt_io_read_all_text_temporary",
            address: aster_rt_io_read_all_text_temporary as *const u8,
            signature: RuntimeSignature {
                parameters: &[
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::Pointer,
                ],
                result: None,
            },
        },
        RuntimeFunction {
            name: "aster_rt_io_write_all_text",
            address: aster_rt_io_write_all_text as *const u8,
            signature: RuntimeSignature {
                parameters: &[
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::Pointer,
                ],
                result: None,
            },
        },
        RuntimeFunction {
            name: "aster_rt_io_list_files",
            address: aster_rt_io_list_files as *const u8,
            signature: RuntimeSignature {
                parameters: &[
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::Pointer,
                ],
                result: None,
            },
        },
        RuntimeFunction {
            name: "aster_rt_io_list_files_temporary",
            address: aster_rt_io_list_files_temporary as *const u8,
            signature: RuntimeSignature {
                parameters: &[
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::I32,
                    RuntimeType::Pointer,
                ],
                result: None,
            },
        },
        RuntimeFunction {
            name: "aster_rt_math_domain_error",
            address: aster_rt_math_domain_error as *const u8,
            signature: RuntimeSignature {
                parameters: &[RuntimeType::Pointer, RuntimeType::I32],
                result: None,
            },
        },
        RuntimeFunction {
            name: "aster_rt_integer_arithmetic_error",
            address: aster_rt_integer_arithmetic_error as *const u8,
            signature: RuntimeSignature {
                parameters: &[RuntimeType::Pointer, RuntimeType::I32],
                result: None,
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::{RuntimeType, runtime_functions};

    #[test]
    fn registry_has_unique_names_and_valid_addresses() {
        let functions = runtime_functions();
        let mut names = functions.iter().map(|f| f.name).collect::<Vec<_>>();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), functions.len());
        assert!(functions.iter().all(|f| !f.address.is_null()));
    }

    #[test]
    fn logging_signature_matches_abi() {
        let functions = runtime_functions();
        let log = functions.iter().find(|f| f.name == "aster_rt_log").unwrap();
        assert_eq!(
            log.signature.parameters,
            &[RuntimeType::I32, RuntimeType::Pointer]
        );
        assert_eq!(log.signature.result, None);
    }

    #[test]
    fn string_search_and_substring_signatures_match_the_abi() {
        let functions = runtime_functions();
        for name in [
            "aster_rt_string_contains",
            "aster_rt_string_starts_with",
            "aster_rt_string_ends_with",
        ] {
            let function = functions
                .iter()
                .find(|function| function.name == name)
                .unwrap_or_else(|| panic!("missing runtime function `{name}`"));
            assert_eq!(
                function.signature.parameters,
                &[
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                ]
            );
            assert_eq!(function.signature.result, Some(RuntimeType::I8));
        }
        let index = functions
            .iter()
            .find(|function| function.name == "aster_rt_string_index_of")
            .expect("IndexOf binding");
        assert_eq!(
            index.signature.parameters,
            &[
                RuntimeType::Pointer,
                RuntimeType::Pointer,
                RuntimeType::Pointer,
            ]
        );
        assert_eq!(index.signature.result, Some(RuntimeType::I32));

        for name in [
            "aster_rt_string_substring_from",
            "aster_rt_string_substring_from_temporary",
        ] {
            let function = functions
                .iter()
                .find(|function| function.name == name)
                .unwrap_or_else(|| panic!("missing runtime function `{name}`"));
            assert_eq!(
                function.signature.parameters,
                &[RuntimeType::Pointer, RuntimeType::Pointer, RuntimeType::I32]
            );
            assert_eq!(function.signature.result, Some(RuntimeType::Pointer));
        }
        for name in [
            "aster_rt_string_substring_range",
            "aster_rt_string_substring_range_temporary",
        ] {
            let function = functions
                .iter()
                .find(|function| function.name == name)
                .unwrap_or_else(|| panic!("missing runtime function `{name}`"));
            assert_eq!(
                function.signature.parameters,
                &[
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                    RuntimeType::I32,
                    RuntimeType::I32,
                ]
            );
            assert_eq!(function.signature.result, Some(RuntimeType::Pointer));
        }
    }

    #[test]
    fn integer_arithmetic_error_signature_matches_abi() {
        let functions = runtime_functions();
        let reporter = functions
            .iter()
            .find(|function| function.name == "aster_rt_integer_arithmetic_error")
            .unwrap();
        assert_eq!(
            reporter.signature.parameters,
            &[RuntimeType::Pointer, RuntimeType::I32]
        );
        assert_eq!(reporter.signature.result, None);
    }

    #[test]
    fn temporary_allocation_signatures_match_the_abi() {
        let functions = runtime_functions();

        for (name, parameters) in [
            (
                "aster_rt_object_new_temporary",
                &[RuntimeType::Pointer, RuntimeType::I32][..],
            ),
            (
                "aster_rt_array_new_temporary",
                &[RuntimeType::Pointer, RuntimeType::I32, RuntimeType::I32][..],
            ),
            (
                "aster_rt_string_concat_temporary",
                &[
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                    RuntimeType::Pointer,
                ][..],
            ),
            (
                "aster_rt_string_join_temporary",
                &[RuntimeType::Pointer, RuntimeType::Pointer, RuntimeType::I32][..],
            ),
        ] {
            let function = functions
                .iter()
                .find(|function| function.name == name)
                .unwrap_or_else(|| panic!("missing runtime function `{name}`"));
            assert_eq!(function.signature.parameters, parameters);
            assert_eq!(function.signature.result, Some(RuntimeType::Pointer));
        }

        for name in [
            "aster_rt_string_from_long_temporary",
            "aster_rt_string_from_ulong_temporary",
        ] {
            let function = functions
                .iter()
                .find(|function| function.name == name)
                .unwrap_or_else(|| panic!("missing runtime function `{name}`"));
            assert_eq!(
                function.signature.parameters,
                &[RuntimeType::Pointer, RuntimeType::I64]
            );
            assert_eq!(function.signature.result, Some(RuntimeType::Pointer));
        }

        for name in [
            "aster_rt_temporary_scope_enter",
            "aster_rt_temporary_scope_leave",
        ] {
            let function = functions
                .iter()
                .find(|function| function.name == name)
                .unwrap_or_else(|| panic!("missing runtime function `{name}`"));
            assert_eq!(function.signature.parameters, &[RuntimeType::Pointer]);
            assert_eq!(function.signature.result, None);
        }
    }
}
