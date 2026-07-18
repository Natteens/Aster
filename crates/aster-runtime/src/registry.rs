//! Central registry of every function the runtime exports to generated code.
//!
//! Backends bind these symbols by name and address and translate
//! [`RuntimeType`] into their own value representation. Adding a future
//! runtime module (files, time, windowing, audio, networking, ECS) means
//! adding entries here — never special cases inside a backend.

use crate::context::{aster_rt_array_element, aster_rt_array_length, aster_rt_array_new};
use crate::log::aster_rt_log;
use crate::math::aster_rt_math_domain_error;
use crate::object::aster_rt_object_new;
use crate::string::{aster_rt_string_concat, aster_rt_string_eq, aster_rt_string_length};

/// Backend-neutral value type used in runtime signatures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeType {
    /// 8-bit integer; Aster `bool` uses `0`/`1`.
    I8,
    /// 32-bit signed integer.
    I32,
    /// 64-bit signed integer.
    I64,
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
            name: "aster_rt_object_new",
            address: aster_rt_object_new as *const u8,
            signature: RuntimeSignature {
                parameters: &[RuntimeType::Pointer, RuntimeType::I32],
                result: Some(RuntimeType::Pointer),
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
            name: "aster_rt_string_length",
            address: aster_rt_string_length as *const u8,
            signature: RuntimeSignature {
                parameters: &[RuntimeType::Pointer, RuntimeType::Pointer],
                result: Some(RuntimeType::I32),
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
}
