//! Explicit, host-owned registry for ASTER's minimal native FFI.
//!
//! The registry stores only canonical declaration identities, structural
//! scalar signatures, and opaque wrapper addresses. It performs no dynamic
//! loading and has no process-global state.

use std::{collections::BTreeMap, error::Error, fmt};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ForeignType {
    Void,
    Bool,
    SByte,
    Byte,
    Short,
    UShort,
    Char,
    Int,
    UInt,
    Long,
    ULong,
    Float,
    Double,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ForeignSignature {
    parameters: Vec<ForeignType>,
    result: ForeignType,
}

impl ForeignSignature {
    /// Builds one C-wrapper signature. `void` is valid only as the result.
    ///
    /// # Errors
    ///
    /// Returns an error when a parameter is `void`.
    pub fn new(
        parameters: impl Into<Vec<ForeignType>>,
        result: ForeignType,
    ) -> Result<Self, ForeignRegistryError> {
        let parameters = parameters.into();
        if parameters.contains(&ForeignType::Void) {
            return Err(ForeignRegistryError::new(
                "foreign parameters cannot have type `void`",
            ));
        }
        Ok(Self { parameters, result })
    }

    #[must_use]
    pub fn parameters(&self) -> &[ForeignType] {
        &self.parameters
    }

    #[must_use]
    pub const fn result(&self) -> ForeignType {
        self.result
    }
}

#[derive(Clone, Debug)]
struct ForeignBinding {
    signature: ForeignSignature,
    address: usize,
}

#[derive(Clone, Debug, Default)]
pub struct ForeignRegistry {
    bindings: BTreeMap<String, Vec<ForeignBinding>>,
}

impl ForeignRegistry {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            bindings: BTreeMap::new(),
        }
    }

    /// Registers one long-lived `extern "C"` wrapper under the declaration's
    /// fully linked ASTER identity and exact structural signature.
    ///
    /// # Safety
    ///
    /// `address` must remain executable for every JIT program prepared with
    /// this registry, must implement ASTER's documented status/out-pointer C
    /// wrapper ABI for `signature`, and must not unwind across the boundary.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty identity, null address, or duplicate
    /// registration of the same identity and exact signature.
    #[allow(unsafe_code)]
    pub unsafe fn register(
        &mut self,
        name: impl Into<String>,
        signature: ForeignSignature,
        address: *const (),
    ) -> Result<(), ForeignRegistryError> {
        if address.is_null() {
            return Err(ForeignRegistryError::new(
                "foreign wrapper address cannot be null",
            ));
        }
        let name = name.into();
        if name.is_empty() {
            return Err(ForeignRegistryError::new(
                "foreign declaration identity cannot be empty",
            ));
        }
        let overloads = self.bindings.entry(name.clone()).or_default();
        if overloads
            .iter()
            .any(|binding| binding.signature == signature)
        {
            return Err(ForeignRegistryError::new(format!(
                "duplicate foreign binding for `{name}` with the same signature"
            )));
        }
        overloads.push(ForeignBinding {
            signature,
            address: address as usize,
        });
        overloads.sort_by(|left, right| left.signature.cmp(&right.signature));
        Ok(())
    }

    #[doc(hidden)]
    pub fn resolve_address(
        &self,
        name: &str,
        signature: &ForeignSignature,
    ) -> Result<usize, ForeignRegistryError> {
        let Some(overloads) = self.bindings.get(name) else {
            return Err(ForeignRegistryError::new(format!(
                "missing foreign binding for `{name}`"
            )));
        };
        overloads
            .iter()
            .find(|binding| &binding.signature == signature)
            .map(|binding| binding.address)
            .ok_or_else(|| {
                ForeignRegistryError::new(format!(
                    "foreign binding signature mismatch for `{name}`"
                ))
            })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForeignRegistryError {
    message: String,
}

impl ForeignRegistryError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ForeignRegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for ForeignRegistryError {}

/// Records one validated FFI wrapper failure without unwinding through native
/// code. `kind` is compiler-private: 0=status, 1=invalid bool, 2=invalid char.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_foreign_error(
    context: *mut crate::ExecutionContext,
    kind: i32,
    value: i64,
) {
    if context.is_null() {
        return;
    }
    // SAFETY: generated code supplies its live invocation context and never
    // retains it across the call.
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    let message = match kind {
        0 => format!("foreign call failed with native status {value}"),
        1 => format!("foreign bool result must be 0 or 1, found {value}"),
        2 => format!("foreign char result is not a Unicode scalar: {value}"),
        _ => "invalid foreign runtime state".to_owned(),
    };
    context.fail(message);
}

#[cfg(test)]
mod tests {
    use super::*;

    extern "C" fn wrapper(_value: i32, _out: *mut i32) -> i32 {
        0
    }

    extern "C" fn other_wrapper(_value: i32, _out: *mut i32) -> i32 {
        0
    }

    extern "C" fn void_wrapper() -> i32 {
        0
    }

    #[test]
    #[allow(unsafe_code)]
    fn registry_is_exact_and_rejects_duplicates() {
        let signature = ForeignSignature::new([ForeignType::Int], ForeignType::Int).unwrap();
        let mut registry = ForeignRegistry::new();
        // SAFETY: the wrapper has the exact declared C ABI and static lifetime.
        unsafe {
            registry
                .register("sample::Native", signature.clone(), wrapper as *const ())
                .unwrap();
        }
        assert!(
            registry
                .resolve_address("sample::Native", &signature)
                .is_ok()
        );
        // SAFETY: same valid wrapper; this call intentionally proves duplicate rejection.
        let duplicate =
            unsafe { registry.register("sample::Native", signature.clone(), wrapper as *const ()) };
        assert!(duplicate.unwrap_err().message().contains("duplicate"));

        // SAFETY: `other_wrapper` has the same valid ABI. Bindings are immutable
        // once registered, so a different target cannot silently replace one.
        let replacement = unsafe {
            registry.register(
                "sample::Native",
                signature.clone(),
                other_wrapper as *const (),
            )
        };
        assert!(replacement.unwrap_err().message().contains("duplicate"));

        let overload = ForeignSignature::new([], ForeignType::Void).unwrap();
        // SAFETY: the overload descriptor matches the status-only wrapper ABI.
        unsafe {
            registry
                .register("sample::Native", overload, void_wrapper as *const ())
                .expect("a distinct exact signature is an overload, not a replacement");
        }
    }
}
