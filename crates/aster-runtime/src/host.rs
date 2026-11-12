//! Host process services behind `aster.io.WriteError` and `Environment`.
//!
//! Standard error goes through the context's console backend, so tests can
//! capture it; environment access reads the real process environment (the
//! embedding host decides whether to expose one by running the program at all).

use std::ptr;

use crate::{
    ExecutionContext,
    string::{AsterStrHeader, view},
};

/// Whether `name` can name an environment variable on every supported host.
/// An empty name, a name containing `=`, or one containing NUL never exists.
fn valid_name(name: &str) -> bool {
    !name.is_empty() && !name.contains('=') && !name.contains('\0')
}

/// Look up `name`. A value that is not valid Unicode is decoded lossily, the
/// same way program arguments are (invalid sequences become U+FFFD).
fn lookup(name: &str) -> Option<String> {
    if !valid_name(name) {
        return None;
    }
    std::env::var_os(name).map(|value| value.to_string_lossy().into_owned())
}

/// Emit `value`'s UTF-8 bytes verbatim to standard error, then flush.
/// Exported to generated code as `aster_rt_io_write_error`.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_io_write_error(
    context: *mut ExecutionContext,
    value: *const AsterStrHeader,
) {
    if context.is_null() {
        return;
    }
    // SAFETY: generated code passes its live hidden ExecutionContext and a
    // string reference owned by that context or the live JIT module.
    #[allow(unsafe_code)]
    let (context, text) = unsafe { (&mut *context, view(value)) };
    let Some(text) = text else {
        context.fail("aster.io.WriteError received an invalid UTF-8 string reference");
        return;
    };
    let backend = context.console_backend();
    if let Err(error) = backend.write_error(text.as_bytes()) {
        context.fail(format!("aster.io.WriteError failed: {error}"));
        return;
    }
    if let Err(error) = backend.flush_error() {
        context.fail(format!("aster.io.WriteError failed to flush: {error}"));
    }
}

/// Whether the environment defines the named variable. Exported to generated
/// code as `aster_rt_env_has`.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_env_has(
    context: *mut ExecutionContext,
    name: *const AsterStrHeader,
) -> i8 {
    if context.is_null() {
        return 0;
    }
    // SAFETY: generated code passes its live context and an ABI string.
    #[allow(unsafe_code)]
    let (context, name) = unsafe { (&mut *context, view(name)) };
    let Some(name) = name else {
        context.fail("Environment.GetVariable received an invalid UTF-8 string reference");
        return 0;
    };
    i8::from(lookup(name).is_some())
}

fn env_get(
    context: *mut ExecutionContext,
    name: *const AsterStrHeader,
    temporary: bool,
) -> *const AsterStrHeader {
    if context.is_null() {
        return ptr::null();
    }
    // SAFETY: generated code passes its live context and an ABI string.
    #[allow(unsafe_code)]
    let (context, name) = unsafe { (&mut *context, view(name)) };
    let Some(name) = name else {
        context.fail("Environment.GetVariable received an invalid UTF-8 string reference");
        return ptr::null();
    };
    let value = lookup(name).unwrap_or_default();
    if temporary {
        context.allocate_temporary_string_parts(&[value.as_str()])
    } else {
        context.allocate_string_parts(&[value.as_str()])
    }
}

/// The variable's value as a persistent string (empty when it is not defined;
/// callers test [`aster_rt_env_has`] first). Exported as `aster_rt_env_get`.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_env_get(
    context: *mut ExecutionContext,
    name: *const AsterStrHeader,
) -> *const AsterStrHeader {
    env_get(context, name, false)
}

/// Temporary-arena counterpart of [`aster_rt_env_get`].
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_env_get_temporary(
    context: *mut ExecutionContext,
    name: *const AsterStrHeader,
) -> *const AsterStrHeader {
    env_get(context, name, true)
}

#[cfg(test)]
mod tests {
    use super::{lookup, valid_name};
    use crate::{
        ExecutionContext, aster_rt_array_element, aster_rt_array_length,
        string::{AsterStrHeader, decode_str},
    };

    #[test]
    #[allow(unsafe_code)]
    fn string_array_holds_the_given_arguments_in_order() {
        let mut context = ExecutionContext::new();
        let values = vec!["a".to_owned(), "\u{65e5}\u{672c}".to_owned(), String::new()];
        let array = context.allocate_string_array(&values);
        assert!(!array.is_null());
        assert_eq!(aster_rt_array_length(&raw mut context, array), 3);
        for (index, expected) in values.iter().enumerate() {
            let index = i32::try_from(index).expect("small index");
            let element = aster_rt_array_element(&raw mut context, array, index);
            // SAFETY: the array holds pointer-sized string elements it owns.
            let string =
                unsafe { std::ptr::read_unaligned(element.cast::<*const AsterStrHeader>()) };
            // SAFETY: the element was just written by `allocate_string_array`.
            let decoded = unsafe { decode_str(string) };
            assert_eq!(decoded.as_deref(), Some(expected.as_str()));
        }
        assert!(context.take_error().is_none());
    }

    #[test]
    fn empty_argument_list_allocates_an_empty_array() {
        let mut context = ExecutionContext::new();
        let array = context.allocate_string_array(&[]);
        assert!(!array.is_null());
        assert_eq!(aster_rt_array_length(&raw mut context, array), 0);
    }

    #[test]
    fn rejects_names_no_host_can_define() {
        assert!(!valid_name(""));
        assert!(!valid_name("A=B"));
        assert!(!valid_name("A\0B"));
        assert!(valid_name("PATH"));
    }

    #[test]
    fn lookup_distinguishes_present_from_absent() {
        assert!(lookup("ASTER_SURELY_UNDEFINED_VARIABLE_8F2A").is_none());
        assert!(lookup("").is_none());
        // Cargo always exports its own manifest directory to test binaries.
        assert!(lookup("CARGO_MANIFEST_DIR").is_some());
    }
}
