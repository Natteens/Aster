//! Immutable UTF-8 string views crossing the JIT/runtime ABI.
//!
//! Layout (`docs/compiler/runtime-abi.md`): an Aster `string` value is a single pointer
//! to an 8-byte-aligned allocation containing a [`AsterStrHeader`] (the byte
//! length as a native-endian `usize`) immediately followed by exactly `len`
//! UTF-8 bytes. There is no NUL terminator and none may be assumed.
//!
//! Ownership: literals live in the JIT module data section. Dynamic strings
//! live in the current [`crate::ExecutionContext`]. The runtime only borrows
//! input bytes during a call. No pointer may outlive its JIT session.

use std::ptr;

use crate::ExecutionContext;

/// Header preceding the UTF-8 bytes of an ABI string.
#[repr(C)]
pub struct AsterStrHeader {
    /// Length of the UTF-8 payload in bytes. Not a character count.
    pub len: usize,
}

/// Encode a Rust string into the ABI layout: header followed by UTF-8 bytes.
///
/// The returned buffer must be placed at an 8-byte-aligned address before a
/// pointer to it is handed to Aster code.
#[must_use]
pub fn encode_str(value: &str) -> Vec<u8> {
    let mut buffer = Vec::with_capacity(size_of::<usize>() + value.len());
    buffer.extend_from_slice(&value.len().to_ne_bytes());
    buffer.extend_from_slice(value.as_bytes());
    buffer
}

/// Borrow the UTF-8 payload behind an ABI string pointer.
///
/// Returns `None` for a null pointer or invalid UTF-8 instead of panicking,
/// so malformed input from generated code produces a controlled failure.
///
/// # Safety
///
/// `string`, when non-null, must point to a live, 8-byte-aligned allocation
/// in the documented layout whose payload stays valid and unmodified for the
/// returned lifetime.
#[allow(unsafe_code)]
pub(crate) unsafe fn view<'a>(string: *const AsterStrHeader) -> Option<&'a str> {
    if string.is_null() {
        return None;
    }
    // SAFETY: the caller guarantees a live allocation in the documented
    // layout: a readable header followed by `len` readable payload bytes.
    let bytes = unsafe {
        let len = (*string).len;
        let payload = string.cast::<u8>().add(size_of::<AsterStrHeader>());
        std::slice::from_raw_parts(payload, len)
    };
    std::str::from_utf8(bytes).ok()
}

/// Copy the payload of an ABI string into host-owned memory.
///
/// Returns `None` for a null pointer or invalid UTF-8. Callers use this to
/// preserve a result string before dropping the JIT module that owns it.
///
/// # Safety
///
/// Same contract as [`view`]: `string`, when non-null, must point to a live,
/// 8-byte-aligned allocation in the documented layout.
#[allow(unsafe_code)]
#[must_use]
pub unsafe fn decode_str(string: *const AsterStrHeader) -> Option<String> {
    // SAFETY: forwarded caller contract.
    unsafe { view(string) }.map(str::to_owned)
}

/// Compare two ABI strings by content. Exported to generated code as
/// `aster_rt_string_eq`.
///
/// Null or non-UTF-8 operands compare unequal to everything, including each
/// other, so corrupted input cannot masquerade as a successful comparison.
// Called only from generated code that upholds the ABI contract; marking the
// symbol `unsafe` would not change the JIT call site.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[must_use]
pub extern "C" fn aster_rt_string_eq(
    left: *const AsterStrHeader,
    right: *const AsterStrHeader,
) -> i8 {
    // SAFETY: pointers originate from JIT data created through `encode_str`;
    // the generated code keeps the owning module alive during the call.
    #[allow(unsafe_code)]
    let (left, right) = unsafe { (view(left), view(right)) };
    match (left, right) {
        (Some(left), Some(right)) => i8::from(left == right),
        _ => 0,
    }
}

/// Concatenate two immutable strings into storage owned by `context`.
/// Empty operands reuse the other reference without allocating.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_string_concat(
    context: *mut ExecutionContext,
    left: *const AsterStrHeader,
    right: *const AsterStrHeader,
) -> *const AsterStrHeader {
    if context.is_null() {
        return ptr::null();
    }
    // SAFETY: generated code passes its live hidden ExecutionContext and
    // string references owned by that context or the live JIT module.
    #[allow(unsafe_code)]
    let (context, left, right) = unsafe { (&mut *context, view(left), view(right)) };
    let (Some(left), Some(right)) = (left, right) else {
        context.fail("string concatenation received an invalid UTF-8 string reference");
        return ptr::null();
    };
    if left.is_empty() {
        return right
            .as_ptr()
            .wrapping_sub(size_of::<AsterStrHeader>())
            .cast();
    }
    if right.is_empty() {
        return left
            .as_ptr()
            .wrapping_sub(size_of::<AsterStrHeader>())
            .cast();
    }
    context.allocate_string_parts(&[left, right])
}

/// Convert a signed integer, already widened to `long`, to a `string` owned
/// by `context`. Backs string interpolation for `sbyte`/`short`/`int`/`long`.
pub extern "C" fn aster_rt_string_from_long(
    context: *mut ExecutionContext,
    value: i64,
) -> *const AsterStrHeader {
    string_from_display(context, value)
}

/// Convert an unsigned integer, already widened to `ulong` (passed as the
/// identical `i64` bit pattern), to a `string` owned by `context`. Backs
/// string interpolation for `byte`/`ushort`/`uint`/`ulong`.
pub extern "C" fn aster_rt_string_from_ulong(
    context: *mut ExecutionContext,
    value: i64,
) -> *const AsterStrHeader {
    string_from_display(context, u64::from_ne_bytes(value.to_ne_bytes()))
}

/// Convert a `float` (already promoted to `double`) or `double` to a
/// `string` owned by `context`. Uses Rust's locale-independent `Display` for
/// `f64`, so the decimal separator is always `.` regardless of the host
/// system's regional settings.
pub extern "C" fn aster_rt_string_from_double(
    context: *mut ExecutionContext,
    value: f64,
) -> *const AsterStrHeader {
    string_from_display(context, value)
}

/// Convert a `bool` (`0`/`1`) to `"false"`/`"true"`, owned by `context`.
pub extern "C" fn aster_rt_string_from_bool(
    context: *mut ExecutionContext,
    value: i8,
) -> *const AsterStrHeader {
    string_from_display(context, value != 0)
}

/// Convert one Unicode scalar value to its one-character `string`, owned by
/// `context`. An invalid scalar value produces a controlled runtime error.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_string_from_char(
    context: *mut ExecutionContext,
    value: u32,
) -> *const AsterStrHeader {
    if context.is_null() {
        return ptr::null();
    }
    // SAFETY: generated code passes its live hidden ExecutionContext.
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    let Some(character) = char::from_u32(value) else {
        context.fail("string interpolation received an invalid `char` value");
        return ptr::null();
    };
    context.allocate_string_parts(&[character.encode_utf8(&mut [0; 4])])
}

fn string_from_display(
    context: *mut ExecutionContext,
    value: impl std::fmt::Display,
) -> *const AsterStrHeader {
    if context.is_null() {
        return ptr::null();
    }
    // SAFETY: generated code passes its live hidden ExecutionContext.
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    context.allocate_string_parts(&[&value.to_string()])
}

/// Join every part, each already a valid ABI string, into one new `string`
/// owned by `context`. Computes the combined length and copies every part's
/// bytes exactly once, so this is a single allocation regardless of how many
/// parts are joined. Backs string interpolation's final concatenation.
///
/// # Safety
///
/// `parts` must point to `count` readable, correctly-aligned
/// `*const AsterStrHeader` pointers, each valid per [`view`]'s contract.
#[allow(unsafe_code, clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_string_join(
    context: *mut ExecutionContext,
    parts: *const *const AsterStrHeader,
    count: i32,
) -> *const AsterStrHeader {
    if context.is_null() || parts.is_null() || count < 0 {
        return ptr::null();
    }
    let context = unsafe { &mut *context };
    let count = usize::try_from(count).unwrap_or(0);
    // SAFETY: caller (generated code) provides `count` live pointers, as
    // documented above.
    let headers = unsafe { std::slice::from_raw_parts(parts, count) };
    let mut views = Vec::with_capacity(count);
    for &header in headers {
        // SAFETY: each pointer is owned by the live context or JIT module.
        let Some(text) = (unsafe { view(header) }) else {
            context.fail("string interpolation received an invalid UTF-8 string reference");
            return ptr::null();
        };
        views.push(text);
    }
    context.allocate_string_parts(&views)
}

/// Return the number of Unicode scalar values in one immutable string.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_string_length(
    context: *mut ExecutionContext,
    value: *const AsterStrHeader,
) -> i32 {
    if context.is_null() {
        return 0;
    }
    // SAFETY: generated code passes the live context and an ABI string owned
    // by that context or the live JIT module.
    #[allow(unsafe_code)]
    let (context, value) = unsafe { (&mut *context, view(value)) };
    let Some(value) = value else {
        context.fail("string Length received an invalid UTF-8 string reference");
        return 0;
    };
    if let Ok(length) = i32::try_from(value.chars().count()) {
        length
    } else {
        context.fail("string Length exceeds the supported `int` range");
        0
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AsterStrHeader, aster_rt_string_concat, aster_rt_string_eq, aster_rt_string_length,
        encode_str, view,
    };
    use crate::ExecutionContext;

    /// 8-byte-aligned backing store for test strings.
    fn aligned(value: &str) -> Vec<u64> {
        let bytes = encode_str(value);
        let mut buffer = vec![0_u64; bytes.len().div_ceil(8)];
        // SAFETY: u64 slices are validly viewable as bytes.
        #[allow(unsafe_code)]
        let target = unsafe {
            std::slice::from_raw_parts_mut(buffer.as_mut_ptr().cast::<u8>(), bytes.len())
        };
        target.copy_from_slice(&bytes);
        buffer
    }

    fn pointer(buffer: &[u64]) -> *const AsterStrHeader {
        buffer.as_ptr().cast()
    }

    #[test]
    fn encodes_length_and_payload() {
        let encoded = encode_str("héllo");
        assert_eq!(encoded[..size_of::<usize>()], 6_usize.to_ne_bytes());
        assert_eq!(&encoded[size_of::<usize>()..], "héllo".as_bytes());
    }

    #[test]
    fn views_utf8_payload() {
        let buffer = aligned("Aster ✓");
        // SAFETY: `aligned` produces the documented layout.
        #[allow(unsafe_code)]
        let text = unsafe { view(pointer(&buffer)) };
        assert_eq!(text, Some("Aster ✓"));
    }

    #[test]
    fn rejects_null_pointer() {
        // SAFETY: null is explicitly handled.
        #[allow(unsafe_code)]
        let text = unsafe { view(std::ptr::null()) };
        assert_eq!(text, None);
    }

    #[test]
    fn compares_by_content() {
        let left = aligned("same");
        let right = aligned("same");
        let other = aligned("other");
        assert_eq!(aster_rt_string_eq(pointer(&left), pointer(&right)), 1);
        assert_eq!(aster_rt_string_eq(pointer(&left), pointer(&other)), 0);
        assert_eq!(aster_rt_string_eq(std::ptr::null(), std::ptr::null()), 0);
    }

    #[test]
    fn concatenates_into_context_and_reuses_empty_operands() {
        let left = aligned("Olá, ");
        let right = aligned("Natte!");
        let empty = aligned("");
        let mut context = ExecutionContext::new();
        let result = aster_rt_string_concat(&raw mut context, pointer(&left), pointer(&right));
        // SAFETY: the result is owned by the still-live context.
        #[allow(unsafe_code)]
        let result = unsafe { view(result) };
        assert_eq!(result, Some("Olá, Natte!"));
        assert_eq!(
            aster_rt_string_concat(&raw mut context, pointer(&empty), pointer(&right)),
            pointer(&right)
        );
        assert!(context.take_error().is_none());
    }

    #[test]
    fn length_counts_unicode_scalars_not_utf8_bytes() {
        let text = aligned("Olá, Natte!");
        let mut context = ExecutionContext::new();
        assert_eq!(aster_rt_string_length(&raw mut context, pointer(&text)), 11);
        assert!(context.take_error().is_none());
    }
}
