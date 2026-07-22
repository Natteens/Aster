//! Immutable UTF-8 string views crossing the JIT/runtime ABI.
//!
//! Layout (`docs/compiler/runtime-abi.md`): an Aster `string` value is a single pointer
//! to an 8-byte-aligned allocation containing a [`AsterStrHeader`] (the byte
//! length as a native-endian `usize`) immediately followed by exactly `len`
//! UTF-8 bytes. There is no NUL terminator and none may be assumed.
//!
//! Ownership: literals live in the JIT module data section. Dynamic strings
//! live in the persistent or temporary arena of the current
//! [`crate::ExecutionContext`]. The runtime only borrows input bytes during a
//! call. No pointer may outlive its JIT session or temporary function scope.

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

/// Concatenate two immutable strings into persistent storage owned by
/// `context`. The result is always a new allocation, including empty operands,
/// so its lifetime is determined only by the selected destination region.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_string_concat(
    context: *mut ExecutionContext,
    left: *const AsterStrHeader,
    right: *const AsterStrHeader,
) -> *const AsterStrHeader {
    string_concat(context, left, right, false)
}

/// Concatenate two immutable strings into the active temporary scope.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_string_concat_temporary(
    context: *mut ExecutionContext,
    left: *const AsterStrHeader,
    right: *const AsterStrHeader,
) -> *const AsterStrHeader {
    string_concat(context, left, right, true)
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
fn string_concat(
    context: *mut ExecutionContext,
    left: *const AsterStrHeader,
    right: *const AsterStrHeader,
    temporary: bool,
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
    if temporary {
        context.allocate_temporary_string_parts(&[left, right])
    } else {
        context.allocate_string_parts(&[left, right])
    }
}

/// Convert a signed integer, already widened to `long`, to a persistent
/// `string`. Backs string interpolation for `sbyte`/`short`/`int`/`long`.
pub extern "C" fn aster_rt_string_from_long(
    context: *mut ExecutionContext,
    value: i64,
) -> *const AsterStrHeader {
    string_from_display(context, value, false)
}

/// Temporary counterpart of [`aster_rt_string_from_long`].
pub extern "C" fn aster_rt_string_from_long_temporary(
    context: *mut ExecutionContext,
    value: i64,
) -> *const AsterStrHeader {
    string_from_display(context, value, true)
}

/// Convert an unsigned integer, already widened to `ulong` (passed as the
/// identical `i64` bit pattern), to a persistent `string`.
pub extern "C" fn aster_rt_string_from_ulong(
    context: *mut ExecutionContext,
    value: i64,
) -> *const AsterStrHeader {
    string_from_display(context, u64::from_ne_bytes(value.to_ne_bytes()), false)
}

/// Temporary counterpart of [`aster_rt_string_from_ulong`].
pub extern "C" fn aster_rt_string_from_ulong_temporary(
    context: *mut ExecutionContext,
    value: i64,
) -> *const AsterStrHeader {
    string_from_display(context, u64::from_ne_bytes(value.to_ne_bytes()), true)
}

/// Convert a `float` (already promoted to `double`) or `double` to a
/// persistent `string`. Formatting is locale independent.
pub extern "C" fn aster_rt_string_from_double(
    context: *mut ExecutionContext,
    value: f64,
) -> *const AsterStrHeader {
    string_from_display(context, value, false)
}

/// Temporary counterpart of [`aster_rt_string_from_double`].
pub extern "C" fn aster_rt_string_from_double_temporary(
    context: *mut ExecutionContext,
    value: f64,
) -> *const AsterStrHeader {
    string_from_display(context, value, true)
}

/// Convert a `bool` (`0`/`1`) to a persistent `"false"`/`"true"` string.
pub extern "C" fn aster_rt_string_from_bool(
    context: *mut ExecutionContext,
    value: i8,
) -> *const AsterStrHeader {
    string_from_display(context, value != 0, false)
}

/// Temporary counterpart of [`aster_rt_string_from_bool`].
pub extern "C" fn aster_rt_string_from_bool_temporary(
    context: *mut ExecutionContext,
    value: i8,
) -> *const AsterStrHeader {
    string_from_display(context, value != 0, true)
}

/// Convert one Unicode scalar value to a persistent one-character string.
/// An invalid scalar value produces a controlled runtime error.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_string_from_char(
    context: *mut ExecutionContext,
    value: u32,
) -> *const AsterStrHeader {
    string_from_char(context, value, false)
}

/// Temporary counterpart of [`aster_rt_string_from_char`].
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_string_from_char_temporary(
    context: *mut ExecutionContext,
    value: u32,
) -> *const AsterStrHeader {
    string_from_char(context, value, true)
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
fn string_from_char(
    context: *mut ExecutionContext,
    value: u32,
    temporary: bool,
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
    let mut buffer = [0; 4];
    let text = character.encode_utf8(&mut buffer);
    if temporary {
        context.allocate_temporary_string_parts(&[text])
    } else {
        context.allocate_string_parts(&[text])
    }
}

fn string_from_display(
    context: *mut ExecutionContext,
    value: impl std::fmt::Display,
    temporary: bool,
) -> *const AsterStrHeader {
    if context.is_null() {
        return ptr::null();
    }
    // SAFETY: generated code passes its live hidden ExecutionContext.
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    let value = value.to_string();
    if temporary {
        context.allocate_temporary_string_parts(&[&value])
    } else {
        context.allocate_string_parts(&[&value])
    }
}

/// Join every part into one persistent dynamic string.
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
    string_join(context, parts, count, false)
}

/// Join every part into one string owned by the active temporary scope.
///
/// # Safety
///
/// Same contract as [`aster_rt_string_join`].
#[allow(unsafe_code, clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_string_join_temporary(
    context: *mut ExecutionContext,
    parts: *const *const AsterStrHeader,
    count: i32,
) -> *const AsterStrHeader {
    string_join(context, parts, count, true)
}

#[allow(unsafe_code, clippy::not_unsafe_ptr_arg_deref)]
fn string_join(
    context: *mut ExecutionContext,
    parts: *const *const AsterStrHeader,
    count: i32,
    temporary: bool,
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
    if temporary {
        context.allocate_temporary_string_parts(&views)
    } else {
        context.allocate_string_parts(&views)
    }
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

fn string_predicate(
    context: *mut ExecutionContext,
    value: *const AsterStrHeader,
    pattern: *const AsterStrHeader,
    operation: &str,
    predicate: impl FnOnce(&str, &str) -> bool,
) -> i8 {
    if context.is_null() {
        return 0;
    }
    // SAFETY: generated code passes its live context and string references
    // owned by that context or the live JIT module.
    #[allow(unsafe_code)]
    let (context, value, pattern) = unsafe { (&mut *context, view(value), view(pattern)) };
    let (Some(value), Some(pattern)) = (value, pattern) else {
        context.fail(format!(
            "String.{operation} received an invalid UTF-8 string reference"
        ));
        return 0;
    };
    i8::from(predicate(value, pattern))
}

/// Ordinal, case-sensitive substring search without allocation.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[must_use]
pub extern "C" fn aster_rt_string_contains(
    context: *mut ExecutionContext,
    value: *const AsterStrHeader,
    pattern: *const AsterStrHeader,
) -> i8 {
    string_predicate(context, value, pattern, "Contains", |value, pattern| {
        value.contains(pattern)
    })
}

/// Ordinal, case-sensitive prefix test without allocation.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[must_use]
pub extern "C" fn aster_rt_string_starts_with(
    context: *mut ExecutionContext,
    value: *const AsterStrHeader,
    pattern: *const AsterStrHeader,
) -> i8 {
    string_predicate(context, value, pattern, "StartsWith", |value, pattern| {
        value.starts_with(pattern)
    })
}

/// Ordinal, case-sensitive suffix test without allocation.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[must_use]
pub extern "C" fn aster_rt_string_ends_with(
    context: *mut ExecutionContext,
    value: *const AsterStrHeader,
    pattern: *const AsterStrHeader,
) -> i8 {
    string_predicate(context, value, pattern, "EndsWith", |value, pattern| {
        value.ends_with(pattern)
    })
}

/// Return the first occurrence as a Unicode scalar-value index, or `-1`.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[must_use]
pub extern "C" fn aster_rt_string_index_of(
    context: *mut ExecutionContext,
    value: *const AsterStrHeader,
    pattern: *const AsterStrHeader,
) -> i32 {
    if context.is_null() {
        return -1;
    }
    // SAFETY: generated code passes its live context and string references
    // owned by that context or the live JIT module.
    #[allow(unsafe_code)]
    let (context, value, pattern) = unsafe { (&mut *context, view(value), view(pattern)) };
    let (Some(value), Some(pattern)) = (value, pattern) else {
        context.fail("String.IndexOf received an invalid UTF-8 string reference");
        return -1;
    };
    let Some(byte_index) = value.find(pattern) else {
        return -1;
    };
    if let Ok(index) = i32::try_from(value[..byte_index].chars().count()) {
        index
    } else {
        context.fail("String.IndexOf result exceeds the supported `int` range");
        -1
    }
}

/// Copy from `start` (in Unicode scalar values) through the end into the
/// persistent arena.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_string_substring_from(
    context: *mut ExecutionContext,
    value: *const AsterStrHeader,
    start: i32,
) -> *const AsterStrHeader {
    string_substring(context, value, start, None, false)
}

/// Temporary counterpart of [`aster_rt_string_substring_from`].
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_string_substring_from_temporary(
    context: *mut ExecutionContext,
    value: *const AsterStrHeader,
    start: i32,
) -> *const AsterStrHeader {
    string_substring(context, value, start, None, true)
}

/// Copy a scalar-indexed range into the persistent arena.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_string_substring_range(
    context: *mut ExecutionContext,
    value: *const AsterStrHeader,
    start: i32,
    length: i32,
) -> *const AsterStrHeader {
    string_substring(context, value, start, Some(length), false)
}

/// Temporary counterpart of [`aster_rt_string_substring_range`].
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_string_substring_range_temporary(
    context: *mut ExecutionContext,
    value: *const AsterStrHeader,
    start: i32,
    length: i32,
) -> *const AsterStrHeader {
    string_substring(context, value, start, Some(length), true)
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
fn string_substring(
    context: *mut ExecutionContext,
    value: *const AsterStrHeader,
    start: i32,
    length: Option<i32>,
    temporary: bool,
) -> *const AsterStrHeader {
    if context.is_null() {
        return ptr::null();
    }
    // SAFETY: generated code passes its live context and an ABI string owned
    // by that context or the live JIT module.
    #[allow(unsafe_code)]
    let (context, value) = unsafe { (&mut *context, view(value)) };
    let Some(value) = value else {
        context.fail("String.Substring received an invalid UTF-8 string reference");
        return ptr::null();
    };
    let scalar_length = value.chars().count();
    let Ok(start_usize) = usize::try_from(start) else {
        substring_bounds_error(context, start, length, scalar_length, false);
        return ptr::null();
    };
    let end = if let Some(length) = length {
        let Ok(length_usize) = usize::try_from(length) else {
            substring_bounds_error(context, start, Some(length), scalar_length, false);
            return ptr::null();
        };
        let Some(end) = start.checked_add(length) else {
            substring_bounds_error(context, start, Some(length), scalar_length, true);
            return ptr::null();
        };
        let end = usize::try_from(end).unwrap_or(usize::MAX);
        if start_usize > scalar_length
            || length_usize > scalar_length - start_usize
            || end > scalar_length
        {
            substring_bounds_error(context, start, Some(length), scalar_length, false);
            return ptr::null();
        }
        end
    } else {
        if start_usize > scalar_length {
            substring_bounds_error(context, start, None, scalar_length, false);
            return ptr::null();
        }
        scalar_length
    };

    let Some(start_byte) = scalar_boundary(value, start_usize) else {
        substring_boundary_error(context, start, length, scalar_length);
        return ptr::null();
    };
    let Some(end_byte) = scalar_boundary(value, end) else {
        substring_boundary_error(context, start, length, scalar_length);
        return ptr::null();
    };
    let Some(result) = value.get(start_byte..end_byte) else {
        substring_boundary_error(context, start, length, scalar_length);
        return ptr::null();
    };
    if temporary {
        context.allocate_temporary_string_parts(&[result])
    } else {
        context.allocate_string_parts(&[result])
    }
}

fn scalar_boundary(value: &str, index: usize) -> Option<usize> {
    if index == value.chars().count() {
        Some(value.len())
    } else {
        value.char_indices().nth(index).map(|(byte, _)| byte)
    }
}

fn substring_bounds_error(
    context: &mut ExecutionContext,
    start: i32,
    length: Option<i32>,
    current: usize,
    overflow: bool,
) {
    match length {
        Some(length) if overflow => context.fail(format!(
            "String.Substring start {start}, length {length} overflows for current length {current}"
        )),
        Some(length) => context.fail(format!(
            "String.Substring start {start}, length {length} is outside current length {current}"
        )),
        None => context.fail(format!(
            "String.Substring start {start} is outside current length {current}"
        )),
    }
}

fn substring_boundary_error(
    context: &mut ExecutionContext,
    start: i32,
    length: Option<i32>,
    current: usize,
) {
    match length {
        Some(length) => context.fail(format!(
            "String.Substring start {start}, length {length} does not identify valid UTF-8 boundaries for current length {current}"
        )),
        None => context.fail(format!(
            "String.Substring start {start} does not identify a valid UTF-8 boundary for current length {current}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AsterStrHeader, aster_rt_string_concat, aster_rt_string_contains,
        aster_rt_string_ends_with, aster_rt_string_eq, aster_rt_string_index_of,
        aster_rt_string_length, aster_rt_string_starts_with, aster_rt_string_substring_from,
        aster_rt_string_substring_range, aster_rt_string_substring_range_temporary, encode_str,
        view,
    };
    use crate::{
        ExecutionContext,
        context::{aster_rt_temporary_scope_enter, aster_rt_temporary_scope_leave},
    };

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
    fn concatenates_into_context_and_copies_empty_operands() {
        let left = aligned("Olá, ");
        let right = aligned("Aster!");
        let empty = aligned("");
        let mut context = ExecutionContext::new();
        let result = aster_rt_string_concat(&raw mut context, pointer(&left), pointer(&right));
        // SAFETY: the result is owned by the still-live context.
        #[allow(unsafe_code)]
        let result = unsafe { view(result) };
        assert_eq!(result, Some("Olá, Aster!"));
        let copied = aster_rt_string_concat(&raw mut context, pointer(&empty), pointer(&right));
        assert_ne!(copied, pointer(&right));
        // SAFETY: the copied result is owned by the still-live context.
        #[allow(unsafe_code)]
        let copied_text = unsafe { view(copied) };
        assert_eq!(copied_text, Some("Aster!"));
        assert!(context.take_error().is_none());
    }

    #[test]
    fn length_counts_unicode_scalars_not_utf8_bytes() {
        let text = aligned("Olá, Mundo!");
        let mut context = ExecutionContext::new();
        assert_eq!(aster_rt_string_length(&raw mut context, pointer(&text)), 11);
        assert!(context.take_error().is_none());
    }

    #[test]
    fn ordinal_search_handles_unicode_empty_and_absent_patterns_without_allocating() {
        let value = aligned("aéβ🙂z");
        let accent = aligned("é");
        let emoji = aligned("🙂");
        let empty = aligned("");
        let absent = aligned("ASTER");
        let mut context = ExecutionContext::with_stats();

        assert_eq!(
            aster_rt_string_contains(&raw mut context, pointer(&value), pointer(&accent)),
            1
        );
        assert_eq!(
            aster_rt_string_starts_with(&raw mut context, pointer(&value), pointer(&empty)),
            1
        );
        assert_eq!(
            aster_rt_string_ends_with(&raw mut context, pointer(&value), pointer(&emoji)),
            0
        );
        assert_eq!(
            aster_rt_string_contains(&raw mut context, pointer(&value), pointer(&absent)),
            0
        );
        assert_eq!(
            aster_rt_string_index_of(&raw mut context, pointer(&value), pointer(&accent)),
            1
        );
        assert_eq!(
            aster_rt_string_index_of(&raw mut context, pointer(&value), pointer(&emoji)),
            3
        );
        assert_eq!(
            aster_rt_string_index_of(&raw mut context, pointer(&value), pointer(&empty)),
            0
        );
        for _ in 0..10_000 {
            assert_eq!(
                aster_rt_string_contains(&raw mut context, pointer(&value), pointer(&accent)),
                1
            );
        }
        assert_eq!(context.memory_stats().total_allocations, 0);
        assert_eq!(context.memory_stats().used_bytes, 0);
        assert!(context.take_error().is_none());
    }

    #[test]
    fn substring_uses_scalar_indices_and_allocates_in_the_selected_region() {
        let value = aligned("aéβ🙂z");
        let mut context = ExecutionContext::with_stats();
        let persistent = aster_rt_string_substring_range(&raw mut context, pointer(&value), 1, 3);
        // SAFETY: the result is owned by the live persistent arena.
        #[allow(unsafe_code)]
        let persistent = unsafe { view(persistent) };
        assert_eq!(persistent, Some("éβ🙂"));
        assert_eq!(context.memory_stats().string_allocations, 1);
        let persistent_used = context.memory_stats().used_bytes;

        aster_rt_temporary_scope_enter(&raw mut context);
        let temporary =
            aster_rt_string_substring_range_temporary(&raw mut context, pointer(&value), 5, 0);
        // SAFETY: the result remains live until the temporary scope leaves.
        #[allow(unsafe_code)]
        let temporary = unsafe { view(temporary) };
        assert_eq!(temporary, Some(""));
        assert!(context.memory_stats().used_bytes > persistent_used);
        aster_rt_temporary_scope_leave(&raw mut context);
        assert_eq!(context.memory_stats().string_allocations, 2);
        assert_eq!(context.memory_stats().used_bytes, persistent_used);
        assert!(context.take_error().is_none());
    }

    #[test]
    fn substring_reports_every_invalid_range_without_publishing_a_string() {
        let value = aligned("abc");
        for (start, length, expected) in [
            (-1, Some(1), "start -1, length 1"),
            (0, Some(-1), "start 0, length -1"),
            (4, None, "start 4"),
            (2, Some(2), "start 2, length 2"),
            (i32::MAX, Some(1), "start 2147483647, length 1"),
        ] {
            let mut context = ExecutionContext::new();
            let result = match length {
                Some(length) => aster_rt_string_substring_range(
                    &raw mut context,
                    pointer(&value),
                    start,
                    length,
                ),
                None => aster_rt_string_substring_from(&raw mut context, pointer(&value), start),
            };
            assert!(result.is_null());
            let error = context.take_error().expect("range error is recorded");
            assert!(error.contains("String.Substring"));
            assert!(error.contains(expected));
            assert!(error.contains("current length 3"));
        }
    }

    #[test]
    fn string_methods_reject_invalid_utf8_in_a_valid_sized_buffer() {
        let mut invalid = aligned("x");
        // SAFETY: `invalid` owns a header plus one payload byte. Only that
        // payload byte is changed, preserving the allocation and header size
        // while deliberately making the UTF-8 metadata invalid.
        #[allow(unsafe_code)]
        unsafe {
            invalid
                .as_mut_ptr()
                .cast::<u8>()
                .add(size_of::<AsterStrHeader>())
                .write(0xff);
        }
        let pattern = aligned("x");
        let mut context = ExecutionContext::new();
        assert_eq!(
            aster_rt_string_contains(&raw mut context, pointer(&invalid), pointer(&pattern),),
            0
        );
        assert!(
            context
                .take_error()
                .is_some_and(|error| error.contains("String.Contains") && error.contains("UTF-8"))
        );
    }
}
