//! Central registry of every function the runtime exports to generated code.
//!
//! Backends bind these symbols by name and address and translate
//! [`RuntimeType`] into their own value representation. Adding a future
//! runtime module (files, time, windowing, audio, networking, ECS) means
//! adding entries here — never special cases inside a backend.

use crate::context::{
    aster_rt_array_element, aster_rt_array_length, aster_rt_array_new,
    aster_rt_array_new_temporary, aster_rt_temporary_scope_enter, aster_rt_temporary_scope_leave,
};
use crate::log::aster_rt_log;
use crate::math::aster_rt_math_domain_error;
use crate::object::{aster_rt_object_new, aster_rt_object_new_temporary};
use crate::string::{
    aster_rt_string_concat, aster_rt_string_concat_temporary, aster_rt_string_eq,
    aster_rt_string_from_bool, aster_rt_string_from_bool_temporary, aster_rt_string_from_char,
    aster_rt_string_from_char_temporary, aster_rt_string_from_double,
    aster_rt_string_from_double_temporary, aster_rt_string_from_long,
    aster_rt_string_from_long_temporary, aster_rt_string_from_ulong,
    aster_rt_string_from_ulong_temporary, aster_rt_string_join, aster_rt_string_join_temporary,
    aster_rt_string_length,
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
            name: "aster_rt_math_domain_error",
            address: aster_rt_math_domain_error as *const u8,
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
