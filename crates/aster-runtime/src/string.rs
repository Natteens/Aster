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

/// Convert a `double` to a persistent `string`. Formatting is locale
/// independent.
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

/// Convert a `float` directly to a persistent `string`, without promoting it
/// to `double` first (which would round twice). Formatting is locale
/// independent.
pub extern "C" fn aster_rt_string_from_float(
    context: *mut ExecutionContext,
    value: f32,
) -> *const AsterStrHeader {
    string_from_display(context, value, false)
}

/// Temporary counterpart of [`aster_rt_string_from_float`].
pub extern "C" fn aster_rt_string_from_float_temporary(
    context: *mut ExecutionContext,
    value: f32,
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

/// Returns the exact UTF-8 payload length in bytes -- not a scalar count,
/// unlike [`aster_rt_string_length`]. Used only by `foreach`'s cursor
/// lowering over `string`; never a public Aster API. O(1): reads the
/// header field directly, never walks the payload the way computing a
/// scalar count must.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_string_byte_length(
    context: *mut ExecutionContext,
    value: *const AsterStrHeader,
) -> i32 {
    if context.is_null() || value.is_null() {
        return 0;
    }
    // SAFETY: generated code passes the live context and an ABI string owned
    // by that context or the live JIT module.
    #[allow(unsafe_code)]
    let (context, len) = unsafe { (&mut *context, (*value).len) };
    if let Ok(len) = i32::try_from(len) {
        len
    } else {
        context.fail("string foreach byte length exceeds the supported `int` range");
        0
    }
}

/// Borrow the raw UTF-8 payload bytes behind an ABI string pointer, without
/// validating the whole payload as UTF-8 (unlike [`view`]). Callers that
/// need only a bounded window -- like `foreach`'s per-scalar cursor decode
/// -- validate exactly that window themselves, keeping per-step cost O(1)
/// instead of O(remaining bytes); walking the whole remaining string on
/// every step would make a full iteration quadratic.
///
/// # Safety
///
/// Same contract as [`view`]: `string`, when non-null, must point to a live,
/// 8-byte-aligned allocation in the documented layout.
#[allow(unsafe_code)]
unsafe fn raw_bytes<'a>(string: *const AsterStrHeader) -> Option<&'a [u8]> {
    if string.is_null() {
        return None;
    }
    // SAFETY: forwarded caller contract, identical to `view`'s.
    #[allow(unsafe_code)]
    unsafe {
        let len = (*string).len;
        let payload = string.cast::<u8>().add(size_of::<AsterStrHeader>());
        Some(std::slice::from_raw_parts(payload, len))
    }
}

/// Decodes exactly one Unicode scalar value starting at byte offset `cursor`
/// in `bytes`, returning it together with its UTF-8 width (1-4). Reads and
/// validates at most 4 bytes -- never rescans from the start -- so a full
/// `foreach` iteration is O(total bytes), not O(scalars * string length).
/// Rejects (with a specific message, never a panic): a cursor at or past the
/// end, an invalid leading byte, a truncated sequence, malformed
/// continuation bytes, an overlong encoding, a surrogate code point, and any
/// scalar above `U+10FFFF` -- every one of these is exactly what Rust's own
/// `str::from_utf8` already rejects for a byte slice, applied here to a
/// bounded window instead of the whole remaining payload.
fn decode_scalar_at(bytes: &[u8], cursor: usize) -> Result<(char, usize), &'static str> {
    let len = bytes.len();
    if cursor >= len {
        return Err("string foreach cursor is out of bounds");
    }
    let lead = bytes[cursor];
    let width = if lead & 0x80 == 0 {
        1
    } else if lead & 0xE0 == 0xC0 {
        2
    } else if lead & 0xF0 == 0xE0 {
        3
    } else if lead & 0xF8 == 0xF0 {
        4
    } else {
        return Err("string foreach found an invalid UTF-8 leading byte");
    };
    if len - cursor < width {
        return Err("string foreach found a truncated UTF-8 sequence");
    }
    let slice = &bytes[cursor..cursor + width];
    let text =
        std::str::from_utf8(slice).map_err(|_| "string foreach found an invalid UTF-8 sequence")?;
    let mut chars = text.chars();
    let scalar = chars
        .next()
        .ok_or("string foreach found an invalid UTF-8 sequence")?;
    if chars.next().is_some() {
        return Err("string foreach found an invalid UTF-8 sequence");
    }
    Ok((scalar, width))
}

/// Decodes one Unicode scalar value at `cursor` (a byte offset) and writes
/// it, and the resulting next cursor, to the two out-parameters. Returns
/// `1` on success, `0` on a controlled failure (already reported through
/// `context.fail`, which does not unwind on its own -- generated code must
/// branch on this return value itself, never assume the loop should keep
/// going). On failure, neither destination is written -- never a
/// partial/garbage scalar or cursor. Used only by `foreach`'s cursor
/// lowering over `string`; never a public Aster API.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_string_decode_next(
    context: *mut ExecutionContext,
    string: *const AsterStrHeader,
    cursor: i32,
    scalar_destination: *mut i32,
    next_cursor_destination: *mut i32,
) -> i8 {
    if context.is_null() {
        return 0;
    }
    // SAFETY: generated functions receive the live host-owned context as their
    // hidden first parameter, and invocation cannot outlive that context.
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    if scalar_destination.is_null() || next_cursor_destination.is_null() {
        context.fail("string foreach received a null decode destination");
        return 0;
    }
    // SAFETY: generated code passes a live ABI string owned by the live
    // context or JIT module; only the raw bytes are taken here, never the
    // full-string UTF-8 revalidation `view` performs.
    #[allow(unsafe_code)]
    let bytes = unsafe { raw_bytes(string) };
    let Some(bytes) = bytes else {
        context.fail("string foreach received an invalid string reference");
        return 0;
    };
    let Ok(cursor) = usize::try_from(cursor) else {
        context.fail("string foreach cursor is negative");
        return 0;
    };
    let (scalar, width) = match decode_scalar_at(bytes, cursor) {
        Ok(decoded) => decoded,
        Err(message) => {
            context.fail(message);
            return 0;
        }
    };
    let Some(next_cursor) = cursor.checked_add(width) else {
        context.fail("string foreach cursor overflow");
        return 0;
    };
    let Ok(next_cursor) = i32::try_from(next_cursor) else {
        context.fail("string foreach cursor overflow");
        return 0;
    };
    #[allow(clippy::cast_possible_wrap)]
    let scalar_bits = scalar as i32;
    // SAFETY: both destinations were validated non-null above; the caller
    // guarantees each is writable for one `i32`.
    #[allow(unsafe_code)]
    unsafe {
        *scalar_destination = scalar_bits;
        *next_cursor_destination = next_cursor;
    }
    1
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

/// Writes the complete `Option<T>` representation `TryParse*` returns.
///
/// The destination is zeroed first, so a failed parse (`None`) and any
/// trailing enum padding are always fully initialized, never a partial
/// representation; then the tag is written, and, on success, the payload
/// bytes at `payload_offset`. `some_tag`/`none_tag`/`payload_offset` are
/// compiler-computed facts about the concrete `Option<T>` specialization
/// (from the same shared `Layouts` system codegen already uses elsewhere),
/// not assumptions this function makes on its own.
///
/// # Safety
///
/// `destination` must point to at least `total_size` writable bytes, aligned
/// for the concrete `Option<T>` layout, owned exclusively by the caller for
/// the duration of this call.
#[allow(unsafe_code)]
pub(crate) unsafe fn write_option_result<T>(
    destination: *mut u8,
    total_size: usize,
    payload_offset: usize,
    parsed: Option<T>,
    some_tag: i32,
    none_tag: i32,
) {
    // SAFETY: forwarded from the caller. Unaligned reads/writes are used
    // deliberately: `destination` is aligned for the concrete `Option<T>` as
    // a whole, but `*mut u8` carries no alignment guarantee `rustc` can
    // verify statically, and this is not a hot path worth an `#[allow]`.
    unsafe {
        ptr::write_bytes(destination, 0, total_size);
        match parsed {
            Some(value) => {
                ptr::write_unaligned(destination.cast::<i32>(), some_tag);
                ptr::write_unaligned(destination.add(payload_offset).cast::<T>(), value);
            }
            None => ptr::write_unaligned(destination.cast::<i32>(), none_tag),
        }
    }
}

/// Shared entry point for every `TryParse*` runtime symbol: validates the
/// context/receiver/destination (the only checks that guard against ABI
/// corruption; a normal parse failure never reaches `context.fail`), then
/// delegates to `parse` and writes the resulting `Option<T>`.
#[allow(clippy::too_many_arguments)]
fn string_try_parse<T>(
    context: *mut ExecutionContext,
    value: *const AsterStrHeader,
    destination: *mut u8,
    total_size: i32,
    some_tag: i32,
    none_tag: i32,
    payload_offset: i32,
    operation: &str,
    parse: impl FnOnce(&str) -> Option<T>,
) {
    if context.is_null() {
        return;
    }
    // SAFETY: generated code passes its live context and a string reference
    // owned by that context or the live JIT module.
    #[allow(unsafe_code)]
    let (context, text) = unsafe { (&mut *context, view(value)) };
    let Some(text) = text else {
        context.fail(format!(
            "String.{operation} received an invalid UTF-8 string reference"
        ));
        return;
    };
    if destination.is_null() {
        context.fail(format!("String.{operation} received a null destination"));
        return;
    }
    let (Ok(total_size), Ok(payload_offset)) =
        (usize::try_from(total_size), usize::try_from(payload_offset))
    else {
        context.fail(format!(
            "String.{operation} received a malformed Option<T> layout"
        ));
        return;
    };
    if payload_offset > total_size {
        context.fail(format!(
            "String.{operation} destination layout is too small for its payload"
        ));
        return;
    }
    let parsed = parse(text);
    // SAFETY: `destination` is caller-owned for the duration of this call
    // (a stack slot or place address sized by the same `Layouts` system that
    // produced `total_size`/`payload_offset`); `total_size` was just bounds-
    // checked above against `payload_offset`.
    #[allow(unsafe_code)]
    unsafe {
        write_option_result(
            destination,
            total_size,
            payload_offset,
            parsed,
            some_tag,
            none_tag,
        );
    }
}

/// `"true"`/`"false"` only, ASCII case-insensitive, no allocation, no
/// whitespace, no alternate spellings.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn aster_rt_string_try_parse_bool(
    context: *mut ExecutionContext,
    value: *const AsterStrHeader,
    destination: *mut u8,
    total_size: i32,
    some_tag: i32,
    none_tag: i32,
    payload_offset: i32,
) {
    string_try_parse(
        context,
        value,
        destination,
        total_size,
        some_tag,
        none_tag,
        payload_offset,
        "TryParseBool",
        |text| {
            if text.eq_ignore_ascii_case("true") {
                Some(true)
            } else if text.eq_ignore_ascii_case("false") {
                Some(false)
            } else {
                None
            }
        },
    );
}

/// Consumes the entire string as an ASCII, optionally `+`/`-`-signed `int`;
/// `str::parse::<i32>` already implements this contract exactly, including
/// the signed-minimum boundary without an intermediate positive overflow.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn aster_rt_string_try_parse_int(
    context: *mut ExecutionContext,
    value: *const AsterStrHeader,
    destination: *mut u8,
    total_size: i32,
    some_tag: i32,
    none_tag: i32,
    payload_offset: i32,
) {
    string_try_parse(
        context,
        value,
        destination,
        total_size,
        some_tag,
        none_tag,
        payload_offset,
        "TryParseInt",
        |text| text.parse::<i32>().ok(),
    );
}

/// Consumes the entire string as an ASCII, optionally `+`-signed `uint`;
/// `str::parse::<u32>` rejects `-`, including `-0`, and any overflow.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn aster_rt_string_try_parse_uint(
    context: *mut ExecutionContext,
    value: *const AsterStrHeader,
    destination: *mut u8,
    total_size: i32,
    some_tag: i32,
    none_tag: i32,
    payload_offset: i32,
) {
    string_try_parse(
        context,
        value,
        destination,
        total_size,
        some_tag,
        none_tag,
        payload_offset,
        "TryParseUInt",
        |text| text.parse::<u32>().ok(),
    );
}

/// Consumes the entire string as an ASCII, optionally `+`/`-`-signed `long`.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn aster_rt_string_try_parse_long(
    context: *mut ExecutionContext,
    value: *const AsterStrHeader,
    destination: *mut u8,
    total_size: i32,
    some_tag: i32,
    none_tag: i32,
    payload_offset: i32,
) {
    string_try_parse(
        context,
        value,
        destination,
        total_size,
        some_tag,
        none_tag,
        payload_offset,
        "TryParseLong",
        |text| text.parse::<i64>().ok(),
    );
}

/// Consumes the entire string as an ASCII, optionally `+`-signed `ulong`.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn aster_rt_string_try_parse_ulong(
    context: *mut ExecutionContext,
    value: *const AsterStrHeader,
    destination: *mut u8,
    total_size: i32,
    some_tag: i32,
    none_tag: i32,
    payload_offset: i32,
) {
    string_try_parse(
        context,
        value,
        destination,
        total_size,
        some_tag,
        none_tag,
        payload_offset,
        "TryParseULong",
        |text| text.parse::<u64>().ok(),
    );
}

/// Whether `text` matches the ASCII, culture-invariant decimal grammar
/// `TryParseFloat`/`TryParseDouble` accept: `sign? significand exponent?`,
/// where `significand` is `digits ("." digits?)? | "." digits` (at least one
/// digit somewhere in the significand) and `exponent` is `("e"|"E") sign?
/// digits`. The whole string must be consumed.
///
/// This exists specifically because `str::parse::<f32/f64>()` accepts
/// several forms this grammar does not (`"NaN"`, `"inf"`, `"Infinity"`, and
/// their sign/case variants): every one of those has no digit in the
/// position this grammar requires, so scanning for the grammar first, then
/// only calling `parse` once it matches, rejects them without checking any
/// literal spelling.
fn is_valid_float_grammar(text: &str) -> bool {
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    if i < len && (bytes[i] == b'+' || bytes[i] == b'-') {
        i += 1;
    }
    let mut has_digits = false;
    while i < len && bytes[i].is_ascii_digit() {
        i += 1;
        has_digits = true;
    }
    if i < len && bytes[i] == b'.' {
        i += 1;
        while i < len && bytes[i].is_ascii_digit() {
            i += 1;
            has_digits = true;
        }
    }
    if !has_digits {
        return false;
    }
    if i < len && (bytes[i] == b'e' || bytes[i] == b'E') {
        i += 1;
        if i < len && (bytes[i] == b'+' || bytes[i] == b'-') {
            i += 1;
        }
        let mut has_exponent_digits = false;
        while i < len && bytes[i].is_ascii_digit() {
            i += 1;
            has_exponent_digits = true;
        }
        if !has_exponent_digits {
            return false;
        }
    }
    i == len
}

/// Consumes the entire string as the ASCII decimal grammar above and parses
/// directly into `f32`; overflow past `f32::MAX`/`f32::MIN` is `None`.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn aster_rt_string_try_parse_float(
    context: *mut ExecutionContext,
    value: *const AsterStrHeader,
    destination: *mut u8,
    total_size: i32,
    some_tag: i32,
    none_tag: i32,
    payload_offset: i32,
) {
    string_try_parse(
        context,
        value,
        destination,
        total_size,
        some_tag,
        none_tag,
        payload_offset,
        "TryParseFloat",
        |text| {
            if !is_valid_float_grammar(text) {
                return None;
            }
            let value = text.parse::<f32>().ok()?;
            value.is_finite().then_some(value)
        },
    );
}

/// Consumes the entire string as the ASCII decimal grammar above and parses
/// directly into `f64`; overflow past `f64::MAX`/`f64::MIN` is `None`.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn aster_rt_string_try_parse_double(
    context: *mut ExecutionContext,
    value: *const AsterStrHeader,
    destination: *mut u8,
    total_size: i32,
    some_tag: i32,
    none_tag: i32,
    payload_offset: i32,
) {
    string_try_parse(
        context,
        value,
        destination,
        total_size,
        some_tag,
        none_tag,
        payload_offset,
        "TryParseDouble",
        |text| {
            if !is_valid_float_grammar(text) {
                return None;
            }
            let value = text.parse::<f64>().ok()?;
            value.is_finite().then_some(value)
        },
    );
}

#[cfg(test)]
mod tests {
    use super::{
        AsterStrHeader, aster_rt_string_concat, aster_rt_string_contains,
        aster_rt_string_ends_with, aster_rt_string_eq, aster_rt_string_from_double,
        aster_rt_string_from_float, aster_rt_string_index_of, aster_rt_string_length,
        aster_rt_string_starts_with, aster_rt_string_substring_from,
        aster_rt_string_substring_range, aster_rt_string_substring_range_temporary,
        aster_rt_string_try_parse_bool, aster_rt_string_try_parse_double,
        aster_rt_string_try_parse_float, aster_rt_string_try_parse_int, encode_str,
        is_valid_float_grammar, view,
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

    /// A destination buffer large enough for any `Option<T>` this task
    /// targets: a 4-byte tag plus an 8-byte payload, rounded up.
    fn option_destination() -> Vec<u64> {
        vec![0_u64; 2]
    }

    #[test]
    fn try_parse_rejects_invalid_utf8_in_a_controlled_buffer_without_touching_destination() {
        let mut invalid = aligned("1");
        // SAFETY: same technique as `string_methods_reject_invalid_utf8_in_a_valid_sized_buffer`.
        #[allow(unsafe_code)]
        unsafe {
            invalid
                .as_mut_ptr()
                .cast::<u8>()
                .add(size_of::<AsterStrHeader>())
                .write(0xff);
        }
        let mut destination = option_destination();
        let destination_ptr = destination.as_mut_ptr().cast::<u8>();
        let mut context = ExecutionContext::new();
        aster_rt_string_try_parse_int(
            &raw mut context,
            pointer(&invalid),
            destination_ptr,
            16,
            1,
            0,
            8,
        );
        assert!(
            context
                .take_error()
                .is_some_and(|error| error.contains("TryParseInt") && error.contains("UTF-8"))
        );
        // The destination is never touched on this ABI-corruption path; every
        // byte stays exactly as it started (zeroed), matching `Option<T>`
        // never being partially initialized from a call that never publishes
        // a result.
        assert_eq!(destination, option_destination());
    }

    #[test]
    fn try_parse_error_does_not_contaminate_a_later_valid_call() {
        let mut invalid = aligned("1");
        // SAFETY: same technique as above.
        #[allow(unsafe_code)]
        unsafe {
            invalid
                .as_mut_ptr()
                .cast::<u8>()
                .add(size_of::<AsterStrHeader>())
                .write(0xff);
        }
        let mut context = ExecutionContext::new();
        let mut destination = option_destination();
        let destination_ptr = destination.as_mut_ptr().cast::<u8>();
        aster_rt_string_try_parse_int(
            &raw mut context,
            pointer(&invalid),
            destination_ptr,
            16,
            1,
            0,
            8,
        );
        assert!(context.take_error().is_some());

        let valid = aligned("42");
        aster_rt_string_try_parse_int(
            &raw mut context,
            pointer(&valid),
            destination_ptr,
            16,
            1,
            0,
            8,
        );
        assert!(context.take_error().is_none());
        // SAFETY: `destination` was just written by the call above.
        #[allow(unsafe_code)]
        unsafe {
            assert_eq!(std::ptr::read_unaligned(destination_ptr.cast::<i32>()), 1);
            assert_eq!(
                std::ptr::read_unaligned(destination_ptr.add(8).cast::<i32>()),
                42
            );
        }
    }

    #[test]
    fn try_parse_writes_the_complete_option_representation_on_success_and_failure() {
        let mut context = ExecutionContext::new();
        let mut destination = option_destination();
        let destination_ptr = destination.as_mut_ptr().cast::<u8>();

        let some_text = aligned("123");
        aster_rt_string_try_parse_int(
            &raw mut context,
            pointer(&some_text),
            destination_ptr,
            16,
            1,
            0,
            8,
        );
        assert!(context.take_error().is_none());
        // SAFETY: just written above.
        #[allow(unsafe_code)]
        unsafe {
            assert_eq!(std::ptr::read_unaligned(destination_ptr.cast::<i32>()), 1);
            assert_eq!(
                std::ptr::read_unaligned(destination_ptr.add(8).cast::<i32>()),
                123
            );
        }

        let none_text = aligned("not a number");
        aster_rt_string_try_parse_int(
            &raw mut context,
            pointer(&none_text),
            destination_ptr,
            16,
            1,
            0,
            8,
        );
        assert!(context.take_error().is_none());
        // SAFETY: just written above; every byte (including the stale
        // payload from the previous `Some`) must be zeroed, not just the
        // tag, so `None`'s representation is never partial.
        #[allow(unsafe_code)]
        unsafe {
            assert_eq!(std::ptr::read_unaligned(destination_ptr.cast::<i32>()), 0);
            assert_eq!(
                std::ptr::read_unaligned(destination_ptr.add(8).cast::<i32>()),
                0
            );
        }
    }

    #[test]
    fn try_parse_bool_accepts_case_insensitive_ascii_without_allocating_a_lowercase_copy() {
        let mut context = ExecutionContext::with_stats();
        let mut destination = option_destination();
        let destination_ptr = destination.as_mut_ptr().cast::<u8>();
        let text = aligned("fAlSe");
        aster_rt_string_try_parse_bool(
            &raw mut context,
            pointer(&text),
            destination_ptr,
            16,
            1,
            0,
            4,
        );
        assert!(context.take_error().is_none());
        assert_eq!(context.memory_stats().string_allocations, 0);
        // SAFETY: just written above.
        #[allow(unsafe_code)]
        unsafe {
            assert_eq!(std::ptr::read_unaligned(destination_ptr.cast::<i32>()), 1);
            assert_eq!(std::ptr::read(destination_ptr.add(4).cast::<u8>()), 0);
        }
    }

    #[test]
    fn float_grammar_accepts_every_required_valid_form() {
        for text in [
            "0",
            "+0",
            "-0",
            "1",
            "-1",
            "0001",
            "1.0",
            "1.",
            ".5",
            "-.5",
            "+0.25",
            "1e3",
            "1E3",
            "1e+3",
            "1e-3",
            "-1.25e+10",
        ] {
            assert!(is_valid_float_grammar(text), "{text:?} should be valid");
        }
    }

    #[test]
    fn float_grammar_rejects_every_required_invalid_form() {
        for text in [
            "",
            "+",
            "-",
            ".",
            "+.",
            "-.",
            "e10",
            "1e",
            "1e+",
            "1e-",
            "1.2.3",
            "1e2e3",
            " 1.0",
            "1.0 ",
            "1_000.0",
            "1,5",
            "\u{FF11}\u{FF12}.\u{FF15}",
            "0x1.0",
            "1f",
            "1d",
            "NaN",
            "nan",
            "inf",
            "-INF",
            "Infinity",
            "+Infinity",
            "-Infinity",
        ] {
            assert!(!is_valid_float_grammar(text), "{text:?} should be invalid");
        }
    }

    #[test]
    fn try_parse_double_preserves_negative_zero_bit_pattern() {
        // `0.0 == -0.0` in IEEE-754, so only a bit-level comparison actually
        // distinguishes them; `Option<double>`'s payload must carry the
        // exact bits `str::parse` produced, not merely an equal-by-value
        // double.
        for (text, expected_bits) in [
            ("-0", 0x8000_0000_0000_0000_u64),
            ("-0.0", 0x8000_0000_0000_0000_u64),
            ("0", 0),
            ("0.0", 0),
        ] {
            let mut context = ExecutionContext::new();
            let mut destination = option_destination();
            let destination_ptr = destination.as_mut_ptr().cast::<u8>();
            let input = aligned(text);
            aster_rt_string_try_parse_double(
                &raw mut context,
                pointer(&input),
                destination_ptr,
                16,
                1,
                0,
                8,
            );
            assert!(context.take_error().is_none());
            // SAFETY: just written above.
            #[allow(unsafe_code)]
            unsafe {
                assert_eq!(std::ptr::read_unaligned(destination_ptr.cast::<i32>()), 1);
                let bits = std::ptr::read_unaligned(destination_ptr.add(8).cast::<u64>());
                assert_eq!(bits, expected_bits, "{text:?} produced the wrong sign bit");
            }
        }
    }

    #[test]
    fn try_parse_float_preserves_negative_zero_bit_pattern() {
        for (text, expected_bits) in [
            ("-0", 0x8000_0000_u32),
            ("-0.0", 0x8000_0000_u32),
            ("0", 0),
            ("0.0", 0),
        ] {
            let mut context = ExecutionContext::new();
            let mut destination = option_destination();
            let destination_ptr = destination.as_mut_ptr().cast::<u8>();
            let input = aligned(text);
            aster_rt_string_try_parse_float(
                &raw mut context,
                pointer(&input),
                destination_ptr,
                8,
                1,
                0,
                4,
            );
            assert!(context.take_error().is_none());
            // SAFETY: just written above.
            #[allow(unsafe_code)]
            unsafe {
                assert_eq!(std::ptr::read_unaligned(destination_ptr.cast::<i32>()), 1);
                let bits = std::ptr::read_unaligned(destination_ptr.add(4).cast::<u32>());
                assert_eq!(bits, expected_bits, "{text:?} produced the wrong sign bit");
            }
        }
    }

    #[test]
    fn try_parse_double_rejects_nan_and_infinity_text_and_overflow() {
        let mut context = ExecutionContext::new();
        let mut destination = option_destination();
        let destination_ptr = destination.as_mut_ptr().cast::<u8>();
        for text in ["NaN", "inf", "-Infinity", "1.7976931348623159e308"] {
            let input = aligned(text);
            aster_rt_string_try_parse_double(
                &raw mut context,
                pointer(&input),
                destination_ptr,
                16,
                1,
                0,
                8,
            );
            assert!(context.take_error().is_none());
            // SAFETY: just written above.
            #[allow(unsafe_code)]
            unsafe {
                assert_eq!(
                    std::ptr::read_unaligned(destination_ptr.cast::<i32>()),
                    0,
                    "{text:?} should be None"
                );
            }
        }
    }

    #[test]
    fn from_float_and_from_double_preserve_negative_zero_sign_in_text() {
        let mut context = ExecutionContext::new();
        // SAFETY: `aster_rt_string_from_float`/`_double` return a freshly
        // allocated, fully initialized persistent string.
        #[allow(unsafe_code)]
        unsafe {
            let text = view(aster_rt_string_from_float(&raw mut context, -0.0_f32))
                .expect("valid utf8 string");
            assert_eq!(text, "-0");
            let text = view(aster_rt_string_from_float(&raw mut context, 0.0_f32))
                .expect("valid utf8 string");
            assert_eq!(text, "0");
            let text = view(aster_rt_string_from_double(&raw mut context, -0.0_f64))
                .expect("valid utf8 string");
            assert_eq!(text, "-0");
            let text = view(aster_rt_string_from_double(&raw mut context, 0.0_f64))
                .expect("valid utf8 string");
            assert_eq!(text, "0");
        }
    }

    #[test]
    fn from_float_never_formats_through_a_widened_double() {
        // `0.1_f32` widened to `f64` prints `0.10000000149011612` (the exact
        // bits of the nearest `f64` to that `f32`, per IEEE-754 widening);
        // formatting `f32` directly must not go through that intermediate
        // step or every non-exactly-representable `float` would gain spurious
        // digits.
        let mut context = ExecutionContext::new();
        // SAFETY: as above.
        #[allow(unsafe_code)]
        let text =
            unsafe { view(aster_rt_string_from_float(&raw mut context, 0.1_f32)) }.expect("utf8");
        assert_eq!(text, "0.1");
    }

    #[test]
    fn from_float_and_from_double_produce_a_stable_special_value_text() {
        let mut context = ExecutionContext::new();
        // SAFETY: as above.
        #[allow(unsafe_code)]
        unsafe {
            assert_eq!(
                view(aster_rt_string_from_float(&raw mut context, f32::NAN)).expect("utf8"),
                "NaN"
            );
            assert_eq!(
                view(aster_rt_string_from_float(&raw mut context, f32::INFINITY)).expect("utf8"),
                "inf"
            );
            assert_eq!(
                view(aster_rt_string_from_float(
                    &raw mut context,
                    f32::NEG_INFINITY
                ))
                .expect("utf8"),
                "-inf"
            );
            assert_eq!(
                view(aster_rt_string_from_double(&raw mut context, f64::NAN)).expect("utf8"),
                "NaN"
            );
            assert_eq!(
                view(aster_rt_string_from_double(&raw mut context, f64::INFINITY)).expect("utf8"),
                "inf"
            );
            assert_eq!(
                view(aster_rt_string_from_double(
                    &raw mut context,
                    f64::NEG_INFINITY
                ))
                .expect("utf8"),
                "-inf"
            );
        }
        // The parsing side deliberately keeps rejecting these textual forms.
        let mut destination = option_destination();
        let destination_ptr = destination.as_mut_ptr().cast::<u8>();
        let input = aligned("NaN");
        aster_rt_string_try_parse_double(
            &raw mut context,
            pointer(&input),
            destination_ptr,
            16,
            1,
            0,
            8,
        );
        // SAFETY: just written above.
        #[allow(unsafe_code)]
        unsafe {
            assert_eq!(std::ptr::read_unaligned(destination_ptr.cast::<i32>()), 0);
        }
    }

    #[test]
    fn try_parse_float_rejects_invalid_utf8_in_a_controlled_buffer_without_touching_destination() {
        let mut invalid = aligned("1.0");
        // SAFETY: same technique as the other invalid-UTF-8 tests above:
        // only the payload byte is changed, preserving the allocation shape.
        #[allow(unsafe_code)]
        unsafe {
            invalid
                .as_mut_ptr()
                .cast::<u8>()
                .add(size_of::<AsterStrHeader>())
                .write(0xff);
        }
        let mut destination = option_destination();
        let destination_ptr = destination.as_mut_ptr().cast::<u8>();
        let mut context = ExecutionContext::new();
        aster_rt_string_try_parse_float(
            &raw mut context,
            pointer(&invalid),
            destination_ptr,
            8,
            1,
            0,
            4,
        );
        assert!(
            context
                .take_error()
                .is_some_and(|error| error.contains("TryParseFloat") && error.contains("UTF-8"))
        );
        assert_eq!(destination, option_destination());
    }

    #[test]
    fn try_parse_float_error_does_not_contaminate_a_later_valid_call() {
        let mut invalid = aligned("1.0");
        // SAFETY: same technique as above.
        #[allow(unsafe_code)]
        unsafe {
            invalid
                .as_mut_ptr()
                .cast::<u8>()
                .add(size_of::<AsterStrHeader>())
                .write(0xff);
        }
        let mut context = ExecutionContext::new();
        let mut destination = option_destination();
        let destination_ptr = destination.as_mut_ptr().cast::<u8>();
        aster_rt_string_try_parse_float(
            &raw mut context,
            pointer(&invalid),
            destination_ptr,
            8,
            1,
            0,
            4,
        );
        assert!(context.take_error().is_some());

        let valid = aligned("2.5");
        aster_rt_string_try_parse_float(
            &raw mut context,
            pointer(&valid),
            destination_ptr,
            8,
            1,
            0,
            4,
        );
        assert!(context.take_error().is_none());
        // SAFETY: `destination` was just written by the call above.
        #[allow(unsafe_code)]
        unsafe {
            assert_eq!(std::ptr::read_unaligned(destination_ptr.cast::<i32>()), 1);
            let value = std::ptr::read_unaligned(destination_ptr.add(4).cast::<f32>());
            assert!((value - 2.5).abs() < f32::EPSILON);
        }
    }
}
