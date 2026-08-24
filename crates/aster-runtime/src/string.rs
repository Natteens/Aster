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

use std::{fmt::Write, ptr};

use crate::{ExecutionContext, aster_rt_array_element, aster_rt_array_length};

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
    let Some(value) = display_text(value) else {
        context.fail("scalar formatting exceeded the runtime buffer");
        return ptr::null();
    };
    if temporary {
        context.allocate_temporary_string_parts(&[value.as_str()])
    } else {
        context.allocate_string_parts(&[value.as_str()])
    }
}

/// Canonical locale-independent scalar formatting used by both immutable
/// `ToString` allocation and direct `StringBuilder` append entry points.
pub(crate) fn display_text(value: impl std::fmt::Display) -> Option<DisplayText> {
    let mut text = DisplayText::new();
    write!(&mut text, "{value}").ok()?;
    Some(text)
}

/// Stack-owned scalar display buffer. Rust's canonical finite `f64` display
/// can use more than 300 bytes for extreme subnormals, so the fixed bound is
/// deliberately 384 bytes. Numeric `StringBuilder.Append` calls therefore do
/// not allocate a temporary host `String` even at IEEE boundaries.
pub(crate) struct DisplayText {
    bytes: [u8; 384],
    length: usize,
}

impl DisplayText {
    const fn new() -> Self {
        Self {
            bytes: [0; 384],
            length: 0,
        }
    }

    pub(crate) fn as_str(&self) -> &str {
        // SAFETY: `fmt::Write::write_str` only copies valid UTF-8 `str` bytes.
        #[allow(unsafe_code)]
        unsafe {
            std::str::from_utf8_unchecked(&self.bytes[..self.length])
        }
    }
}

impl std::fmt::Write for DisplayText {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        let end = self.length.checked_add(text.len()).ok_or(std::fmt::Error)?;
        let destination = self
            .bytes
            .get_mut(self.length..end)
            .ok_or(std::fmt::Error)?;
        destination.copy_from_slice(text.as_bytes());
        self.length = end;
        Ok(())
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
    let mut views = Vec::new();
    if views.try_reserve_exact(count).is_err() {
        context.fail("string interpolation exceeds available host memory");
        return ptr::null();
    }
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

/// Return the last (including overlapping) occurrence as a Unicode scalar
/// index, or `-1`. The empty pattern is found after the final scalar.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[must_use]
pub extern "C" fn aster_rt_string_last_index_of(
    context: *mut ExecutionContext,
    value: *const AsterStrHeader,
    pattern: *const AsterStrHeader,
) -> i32 {
    if context.is_null() {
        return -1;
    }
    #[allow(unsafe_code)]
    let (context, value, pattern) = unsafe { (&mut *context, view(value), view(pattern)) };
    let (Some(value), Some(pattern)) = (value, pattern) else {
        context.fail("String.LastIndexOf received an invalid UTF-8 string reference");
        return -1;
    };
    let byte_index = if pattern.is_empty() {
        value.len()
    } else {
        let mut last = None;
        for (index, _) in value.match_indices(pattern) {
            last = Some(index);
        }
        let Some(index) = last else { return -1 };
        index
    };
    i32::try_from(value[..byte_index].chars().count()).unwrap_or_else(|_| {
        context.fail("String.LastIndexOf result exceeds the supported `int` range");
        -1
    })
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

/// Trim ASTER's fixed Unicode `White_Space` set. This is ordinal and
/// locale-independent; it never consults OS, process culture, or host Unicode
/// tables.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_string_trim(
    context: *mut ExecutionContext,
    value: *const AsterStrHeader,
) -> *const AsterStrHeader {
    string_trim(context, value, false)
}

/// Temporary counterpart of [`aster_rt_string_trim`].
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_string_trim_temporary(
    context: *mut ExecutionContext,
    value: *const AsterStrHeader,
) -> *const AsterStrHeader {
    string_trim(context, value, true)
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
fn string_trim(
    context: *mut ExecutionContext,
    value: *const AsterStrHeader,
    temporary: bool,
) -> *const AsterStrHeader {
    if context.is_null() {
        return ptr::null();
    }
    // SAFETY: generated code supplies a live context and an ABI string.
    #[allow(unsafe_code)]
    let (context, value) = unsafe { (&mut *context, view(value)) };
    let Some(value) = value else {
        context.fail("String.Trim received an invalid UTF-8 string reference");
        return ptr::null();
    };
    let trimmed = value.trim_matches(is_aster_whitespace);
    if temporary {
        context.allocate_temporary_string_parts(&[trimmed])
    } else {
        context.allocate_string_parts(&[trimmed])
    }
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_string_trim_start(
    context: *mut ExecutionContext,
    value: *const AsterStrHeader,
) -> *const AsterStrHeader {
    string_trim_side(context, value, false, true)
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_string_trim_start_temporary(
    context: *mut ExecutionContext,
    value: *const AsterStrHeader,
) -> *const AsterStrHeader {
    string_trim_side(context, value, true, true)
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_string_trim_end(
    context: *mut ExecutionContext,
    value: *const AsterStrHeader,
) -> *const AsterStrHeader {
    string_trim_side(context, value, false, false)
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_string_trim_end_temporary(
    context: *mut ExecutionContext,
    value: *const AsterStrHeader,
) -> *const AsterStrHeader {
    string_trim_side(context, value, true, false)
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
fn string_trim_side(
    context: *mut ExecutionContext,
    value: *const AsterStrHeader,
    temporary: bool,
    start: bool,
) -> *const AsterStrHeader {
    if context.is_null() {
        return ptr::null();
    }
    #[allow(unsafe_code)]
    let (context, value) = unsafe { (&mut *context, view(value)) };
    let Some(value) = value else {
        context.fail("String.Trim received an invalid UTF-8 string reference");
        return ptr::null();
    };
    let result = if start {
        value.trim_start_matches(is_aster_whitespace)
    } else {
        value.trim_end_matches(is_aster_whitespace)
    };
    if temporary {
        context.allocate_temporary_string_parts(&[result])
    } else {
        context.allocate_string_parts(&[result])
    }
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_string_join_array(
    context: *mut ExecutionContext,
    separator: *const AsterStrHeader,
    values: *mut crate::context::AsterArray,
) -> *const AsterStrHeader {
    string_join_array(context, separator, values, false)
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_string_join_array_temporary(
    context: *mut ExecutionContext,
    separator: *const AsterStrHeader,
    values: *mut crate::context::AsterArray,
) -> *const AsterStrHeader {
    string_join_array(context, separator, values, true)
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_string_concat_array(
    context: *mut ExecutionContext,
    values: *mut crate::context::AsterArray,
) -> *const AsterStrHeader {
    let empty = AsterStrHeader { len: 0 };
    string_join_array(context, &raw const empty, values, false)
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_string_concat_array_temporary(
    context: *mut ExecutionContext,
    values: *mut crate::context::AsterArray,
) -> *const AsterStrHeader {
    let empty = AsterStrHeader { len: 0 };
    string_join_array(context, &raw const empty, values, true)
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
fn string_join_array(
    context: *mut ExecutionContext,
    separator: *const AsterStrHeader,
    values: *mut crate::context::AsterArray,
    temporary: bool,
) -> *const AsterStrHeader {
    if context.is_null() {
        return ptr::null();
    }
    #[allow(unsafe_code)]
    let context_ref = unsafe { &mut *context };
    #[allow(unsafe_code)]
    let Some(separator) = (unsafe { view(separator) }) else {
        context_ref.fail("String.Join received an invalid separator");
        return ptr::null();
    };
    let length = aster_rt_array_length(context, values);
    if crate::context::aster_rt_has_error(context) != 0 {
        return ptr::null();
    }
    let mut output_len = separator
        .len()
        .checked_mul(usize::try_from(length.saturating_sub(1)).unwrap_or(0));
    for index in 0..length {
        let slot = aster_rt_array_element(context, values, index);
        if slot.is_null() {
            return ptr::null();
        }
        #[allow(unsafe_code)]
        let item = unsafe { slot.cast::<*const AsterStrHeader>().read_unaligned() };
        #[allow(unsafe_code)]
        let Some(item) = (unsafe { view(item) }) else {
            context_ref.fail("String.Join received an invalid string element");
            return ptr::null();
        };
        output_len = output_len.and_then(|total| total.checked_add(item.len()));
    }
    let Some(output_len) = output_len else {
        context_ref.fail("String.Join result exceeds the addressable range");
        return ptr::null();
    };
    let output = context_ref.allocate_string_storage(output_len, temporary);
    if output.is_null() {
        return ptr::null();
    }
    #[allow(unsafe_code)]
    let bytes = unsafe {
        std::slice::from_raw_parts_mut(
            output.cast::<u8>().add(size_of::<AsterStrHeader>()),
            output_len,
        )
    };
    let mut cursor = 0;
    for index in 0..length {
        if index != 0 {
            bytes[cursor..cursor + separator.len()].copy_from_slice(separator.as_bytes());
            cursor += separator.len();
        }
        let slot = aster_rt_array_element(context, values, index);
        #[allow(unsafe_code)]
        let item =
            unsafe { view(slot.cast::<*const AsterStrHeader>().read_unaligned()).unwrap_or("") };
        bytes[cursor..cursor + item.len()].copy_from_slice(item.as_bytes());
        cursor += item.len();
    }
    output
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_string_repeat(
    context: *mut ExecutionContext,
    value: *const AsterStrHeader,
    count: i32,
) -> *const AsterStrHeader {
    string_repeat(context, value, count, false)
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_string_repeat_temporary(
    context: *mut ExecutionContext,
    value: *const AsterStrHeader,
    count: i32,
) -> *const AsterStrHeader {
    string_repeat(context, value, count, true)
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
fn string_repeat(
    context: *mut ExecutionContext,
    value: *const AsterStrHeader,
    count: i32,
    temporary: bool,
) -> *const AsterStrHeader {
    if context.is_null() {
        return ptr::null();
    }
    #[allow(unsafe_code)]
    let (context, value) = unsafe { (&mut *context, view(value)) };
    let Some(value) = value else {
        context.fail("String.Repeat received an invalid string");
        return ptr::null();
    };
    let Ok(count) = usize::try_from(count) else {
        context.fail("String.Repeat requires a nonnegative count");
        return ptr::null();
    };
    let Some(output_len) = value.len().checked_mul(count) else {
        context.fail("String.Repeat result exceeds the addressable range");
        return ptr::null();
    };
    let output = context.allocate_string_storage(output_len, temporary);
    if output.is_null() {
        return ptr::null();
    }
    #[allow(unsafe_code)]
    let bytes = unsafe {
        std::slice::from_raw_parts_mut(
            output.cast::<u8>().add(size_of::<AsterStrHeader>()),
            output_len,
        )
    };
    for chunk in bytes.chunks_exact_mut(value.len().max(1)) {
        if !value.is_empty() {
            chunk.copy_from_slice(value.as_bytes());
        }
    }
    output
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_string_to_chars(
    context: *mut ExecutionContext,
    value: *const AsterStrHeader,
) -> *mut crate::context::AsterArray {
    string_to_chars(context, value, false)
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_string_to_chars_temporary(
    context: *mut ExecutionContext,
    value: *const AsterStrHeader,
) -> *mut crate::context::AsterArray {
    string_to_chars(context, value, true)
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
fn string_to_chars(
    context: *mut ExecutionContext,
    value: *const AsterStrHeader,
    temporary: bool,
) -> *mut crate::context::AsterArray {
    if context.is_null() {
        return ptr::null_mut();
    }
    #[allow(unsafe_code)]
    let (context_ref, value) = unsafe { (&mut *context, view(value)) };
    let Some(value) = value else {
        context_ref.fail("String.ToChars received an invalid string");
        return ptr::null_mut();
    };
    let Ok(length) = i32::try_from(value.chars().count()) else {
        context_ref.fail("String.ToChars exceeds the supported array length");
        return ptr::null_mut();
    };
    let output = if temporary {
        context_ref.allocate_temporary_array(length, 4)
    } else {
        context_ref.allocate_array(length, 4)
    };
    if output.is_null() {
        return ptr::null_mut();
    }
    for (index, character) in value.chars().enumerate() {
        let slot =
            aster_rt_array_element(context, output, i32::try_from(index).unwrap_or(i32::MAX));
        if slot.is_null() {
            return ptr::null_mut();
        }
        #[allow(unsafe_code)]
        unsafe {
            slot.cast::<u32>().write_unaligned(u32::from(character));
        }
    }
    output
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_string_from_chars(
    context: *mut ExecutionContext,
    values: *mut crate::context::AsterArray,
) -> *const AsterStrHeader {
    string_from_chars(context, values, false)
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_string_from_chars_temporary(
    context: *mut ExecutionContext,
    values: *mut crate::context::AsterArray,
) -> *const AsterStrHeader {
    string_from_chars(context, values, true)
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
fn string_from_chars(
    context: *mut ExecutionContext,
    values: *mut crate::context::AsterArray,
    temporary: bool,
) -> *const AsterStrHeader {
    if context.is_null() {
        return ptr::null();
    }
    #[allow(unsafe_code)]
    let context_ref = unsafe { &mut *context };
    let length = aster_rt_array_length(context, values);
    if crate::context::aster_rt_has_error(context) != 0 {
        return ptr::null();
    }
    let mut output_len = Some(0_usize);
    for index in 0..length {
        let slot = aster_rt_array_element(context, values, index);
        if slot.is_null() {
            return ptr::null();
        }
        #[allow(unsafe_code)]
        let scalar = unsafe { slot.cast::<u32>().read_unaligned() };
        let Some(character) = char::from_u32(scalar) else {
            context_ref.fail("String.FromChars received an invalid Unicode scalar");
            return ptr::null();
        };
        output_len = output_len.and_then(|total| total.checked_add(character.len_utf8()));
    }
    let Some(output_len) = output_len else {
        context_ref.fail("String.FromChars result exceeds the addressable range");
        return ptr::null();
    };
    let output = context_ref.allocate_string_storage(output_len, temporary);
    if output.is_null() {
        return ptr::null();
    }
    #[allow(unsafe_code)]
    let bytes = unsafe {
        std::slice::from_raw_parts_mut(
            output.cast::<u8>().add(size_of::<AsterStrHeader>()),
            output_len,
        )
    };
    let mut cursor = 0;
    for index in 0..length {
        let slot = aster_rt_array_element(context, values, index);
        #[allow(unsafe_code)]
        let scalar = unsafe { slot.cast::<u32>().read_unaligned() };
        let character = char::from_u32(scalar).unwrap_or('\u{FFFD}');
        cursor += character.encode_utf8(&mut bytes[cursor..]).len();
    }
    output
}

/// Replace exact, non-overlapping matches from left to right.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_string_replace(
    context: *mut ExecutionContext,
    value: *const AsterStrHeader,
    old_value: *const AsterStrHeader,
    new_value: *const AsterStrHeader,
) -> *const AsterStrHeader {
    string_replace(context, value, old_value, new_value, false)
}

/// Temporary counterpart of [`aster_rt_string_replace`].
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_string_replace_temporary(
    context: *mut ExecutionContext,
    value: *const AsterStrHeader,
    old_value: *const AsterStrHeader,
    new_value: *const AsterStrHeader,
) -> *const AsterStrHeader {
    string_replace(context, value, old_value, new_value, true)
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
fn string_replace(
    context: *mut ExecutionContext,
    value: *const AsterStrHeader,
    old_value: *const AsterStrHeader,
    new_value: *const AsterStrHeader,
    temporary: bool,
) -> *const AsterStrHeader {
    if context.is_null() {
        return ptr::null();
    }
    // SAFETY: generated code supplies live ABI strings owned by the current
    // execution or JIT module.
    #[allow(unsafe_code)]
    let (context, value, old_value, new_value) =
        unsafe { (&mut *context, view(value), view(old_value), view(new_value)) };
    let (Some(value), Some(old_value), Some(new_value)) = (value, old_value, new_value) else {
        context.fail("String.Replace received an invalid UTF-8 string reference");
        return ptr::null();
    };
    if old_value.is_empty() {
        context.fail("String.Replace requires a non-empty oldValue");
        return ptr::null();
    }
    let matches = value.match_indices(old_value).count();
    let Some(removed) = old_value.len().checked_mul(matches) else {
        context.fail("String.Replace result exceeds the addressable range");
        return ptr::null();
    };
    let Some(added) = new_value.len().checked_mul(matches) else {
        context.fail("String.Replace result exceeds the addressable range");
        return ptr::null();
    };
    let Some(output_len) = value
        .len()
        .checked_sub(removed)
        .and_then(|length| length.checked_add(added))
    else {
        context.fail("String.Replace result exceeds the addressable range");
        return ptr::null();
    };
    let output = context.allocate_string_storage(output_len, temporary);
    if output.is_null() {
        return ptr::null();
    }
    // SAFETY: the allocated payload has exactly `output_len` writable bytes.
    #[allow(unsafe_code)]
    let bytes = unsafe {
        std::slice::from_raw_parts_mut(
            output.cast::<u8>().add(size_of::<AsterStrHeader>()),
            output_len,
        )
    };
    let mut source_cursor = 0;
    let mut output_cursor = 0;
    for (index, _) in value.match_indices(old_value) {
        let prefix = &value[source_cursor..index];
        bytes[output_cursor..output_cursor + prefix.len()].copy_from_slice(prefix.as_bytes());
        output_cursor += prefix.len();
        bytes[output_cursor..output_cursor + new_value.len()].copy_from_slice(new_value.as_bytes());
        output_cursor += new_value.len();
        source_cursor = index + old_value.len();
    }
    let suffix = &value[source_cursor..];
    bytes[output_cursor..].copy_from_slice(suffix.as_bytes());
    output
}

/// Split on an exact, non-empty delimiter, preserving empty segments.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_string_split(
    context: *mut ExecutionContext,
    value: *const AsterStrHeader,
    separator: *const AsterStrHeader,
) -> *mut crate::context::AsterArray {
    string_split(context, value, separator, false)
}

/// Temporary counterpart of [`aster_rt_string_split`].
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_string_split_temporary(
    context: *mut ExecutionContext,
    value: *const AsterStrHeader,
    separator: *const AsterStrHeader,
) -> *mut crate::context::AsterArray {
    string_split(context, value, separator, true)
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
fn string_split(
    context: *mut ExecutionContext,
    value: *const AsterStrHeader,
    separator: *const AsterStrHeader,
    temporary: bool,
) -> *mut crate::context::AsterArray {
    if context.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: generated code supplies live ABI strings owned by the current
    // execution or JIT module.
    #[allow(unsafe_code)]
    let (context, value, separator) = unsafe { (&mut *context, view(value), view(separator)) };
    let (Some(value), Some(separator)) = (value, separator) else {
        context.fail("String.Split received an invalid UTF-8 string reference");
        return ptr::null_mut();
    };
    if separator.is_empty() {
        context.fail("String.Split requires a non-empty separator");
        return ptr::null_mut();
    }
    let Some(length) = value.match_indices(separator).count().checked_add(1) else {
        context.fail("String.Split result exceeds the supported array length");
        return ptr::null_mut();
    };
    let Ok(length) = i32::try_from(length) else {
        context.fail("String.Split result exceeds the supported array length");
        return ptr::null_mut();
    };
    let element_size = u32::try_from(size_of::<*const AsterStrHeader>()).unwrap_or(8);
    let output = if temporary {
        context.allocate_temporary_array(length, element_size)
    } else {
        context.allocate_array(length, element_size)
    };
    if output.is_null() {
        return ptr::null_mut();
    }
    for (index, segment) in value.split(separator).enumerate() {
        let item = if temporary {
            context.allocate_temporary_string_parts(&[segment])
        } else {
            context.allocate_string_parts(&[segment])
        };
        if item.is_null() {
            return ptr::null_mut();
        }
        let Ok(index) = i32::try_from(index) else {
            context.fail("String.Split result exceeds the supported array length");
            return ptr::null_mut();
        };
        let destination = aster_rt_array_element(std::ptr::from_mut(context), output, index);
        if destination.is_null() {
            return ptr::null_mut();
        }
        // SAFETY: the runtime allocated `output` for pointer-sized string
        // elements and `destination` points at its in-bounds slot.
        #[allow(unsafe_code)]
        unsafe {
            destination
                .cast::<*const AsterStrHeader>()
                .write_unaligned(item);
        };
    }
    output
}

fn scalar_boundary(value: &str, index: usize) -> Option<usize> {
    if index == value.chars().count() {
        Some(value.len())
    } else {
        value.char_indices().nth(index).map(|(byte, _)| byte)
    }
}

fn is_aster_whitespace(value: char) -> bool {
    matches!(
        value,
        '\u{0009}'..='\u{000d}'
            | '\u{0020}'
            | '\u{0085}'
            | '\u{00a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
    )
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
    let Some(payload_end) = payload_offset.checked_add(std::mem::size_of::<T>()) else {
        context.fail(format!(
            "String.{operation} Option<T> payload layout overflow"
        ));
        return;
    };
    if total_size < std::mem::size_of::<i32>() || payload_end > total_size {
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

/// Parses exactly one Unicode scalar value. Empty and multi-scalar strings
/// produce `None`; valid ASTER strings are already guaranteed UTF-8.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn aster_rt_string_try_parse_char(
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
        "TryParseChar",
        |text| {
            let mut chars = text.chars();
            let value = chars.next()?;
            chars.next().is_none().then_some(value)
        },
    );
}

macro_rules! define_narrow_integer_parser {
    ($name:ident, $label:literal, $type:ty) => {
        #[allow(clippy::not_unsafe_ptr_arg_deref)]
        #[allow(clippy::too_many_arguments)]
        pub extern "C" fn $name(
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
                $label,
                |text| text.parse::<$type>().ok(),
            );
        }
    };
}

define_narrow_integer_parser!(aster_rt_string_try_parse_sbyte, "TryParseSByte", i8);
define_narrow_integer_parser!(aster_rt_string_try_parse_byte, "TryParseByte", u8);
define_narrow_integer_parser!(aster_rt_string_try_parse_short, "TryParseShort", i16);
define_narrow_integer_parser!(aster_rt_string_try_parse_ushort, "TryParseUShort", u16);

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
    use std::sync::Arc;

    use super::{
        AsterStrHeader, aster_rt_string_concat, aster_rt_string_contains,
        aster_rt_string_ends_with, aster_rt_string_eq, aster_rt_string_from_double,
        aster_rt_string_from_float, aster_rt_string_index_of, aster_rt_string_length,
        aster_rt_string_replace, aster_rt_string_split, aster_rt_string_starts_with,
        aster_rt_string_substring_from, aster_rt_string_substring_range,
        aster_rt_string_substring_range_temporary, aster_rt_string_trim,
        aster_rt_string_try_parse_bool, aster_rt_string_try_parse_byte,
        aster_rt_string_try_parse_char, aster_rt_string_try_parse_double,
        aster_rt_string_try_parse_float, aster_rt_string_try_parse_int,
        aster_rt_string_try_parse_long, aster_rt_string_try_parse_short,
        aster_rt_string_try_parse_ulong, encode_str, is_valid_float_grammar, view,
    };
    use crate::{
        ExecutionContext, MemoryGovernor, aster_rt_array_element, aster_rt_array_length,
        context::{AsterArray, aster_rt_temporary_scope_enter, aster_rt_temporary_scope_leave},
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

    fn limited_context() -> ExecutionContext {
        ExecutionContext::with_memory_budget(
            Arc::new(MemoryGovernor::new(crate::arena::MIN_PAGE_SIZE * 4)),
            crate::arena::MIN_PAGE_SIZE,
        )
    }

    fn array_strings(context: &mut ExecutionContext, array: *mut AsterArray) -> Vec<String> {
        let context_pointer = std::ptr::from_mut(context);
        let length = aster_rt_array_length(context_pointer, array);
        (0..length)
            .map(|index| {
                let slot = aster_rt_array_element(context_pointer, array, index);
                assert!(!slot.is_null());
                // SAFETY: Split privately initialized every pointer-sized slot
                // before publishing the array.
                #[allow(unsafe_code)]
                let value = unsafe { slot.cast::<*const AsterStrHeader>().read_unaligned() };
                // SAFETY: each slot is a live ASTER string in this context.
                #[allow(unsafe_code)]
                unsafe { view(value) }
                    .expect("valid split string")
                    .to_owned()
            })
            .collect()
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
            (
                i32::MAX,
                Some(i32::MAX),
                "start 2147483647, length 2147483647",
            ),
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
    fn trim_uses_the_fixed_unicode_white_space_set() {
        let value = aligned("\t\u{00a0}\u{2003}aáβ🙂z\u{3000}\r");
        let zero_width = aligned("\u{200b}a\u{200b}");
        let mut context = ExecutionContext::new();

        let trimmed = aster_rt_string_trim(&raw mut context, pointer(&value));
        // SAFETY: the result is owned by the live context.
        #[allow(unsafe_code)]
        let trimmed = unsafe { view(trimmed) };
        assert_eq!(trimmed, Some("aáβ🙂z"));

        let unchanged = aster_rt_string_trim(&raw mut context, pointer(&zero_width));
        assert_ne!(unchanged, pointer(&zero_width));
        // SAFETY: the result is owned by the live context.
        #[allow(unsafe_code)]
        let unchanged = unsafe { view(unchanged) };
        assert_eq!(unchanged, Some("\u{200b}a\u{200b}"));
        assert!(context.take_error().is_none());
    }

    #[test]
    fn replace_is_left_to_right_non_overlapping_and_always_publishes_its_own_string() {
        let value = aligned("aaaa");
        let old_value = aligned("aa");
        let replacement = aligned("aaa");
        let absent = aligned("z");
        let mut context = ExecutionContext::new();

        let replaced = aster_rt_string_replace(
            &raw mut context,
            pointer(&value),
            pointer(&old_value),
            pointer(&replacement),
        );
        // SAFETY: the result is owned by the live context.
        #[allow(unsafe_code)]
        let replaced = unsafe { view(replaced) };
        assert_eq!(replaced, Some("aaaaaa"));

        let copied = aster_rt_string_replace(
            &raw mut context,
            pointer(&value),
            pointer(&absent),
            pointer(&replacement),
        );
        assert_ne!(copied, pointer(&value));
        // SAFETY: the result is owned by the live context.
        #[allow(unsafe_code)]
        let copied = unsafe { view(copied) };
        assert_eq!(copied, Some("aaaa"));
        assert!(context.take_error().is_none());
    }

    #[test]
    fn split_preserves_empty_segments_and_initializes_every_string_slot() {
        let value = aligned("a,,β🙂,");
        let separator = aligned(",");
        let empty = aligned("");
        let mut context = ExecutionContext::new();

        let parts = aster_rt_string_split(&raw mut context, pointer(&value), pointer(&separator));
        assert_eq!(array_strings(&mut context, parts), ["a", "", "β🙂", ""]);

        let one_empty =
            aster_rt_string_split(&raw mut context, pointer(&empty), pointer(&separator));
        assert_eq!(array_strings(&mut context, one_empty), [""]);
        assert!(context.take_error().is_none());
    }

    #[test]
    fn allocating_text_helpers_fail_controlled_without_invalidating_the_source() {
        let large_text = "x".repeat(crate::arena::MIN_PAGE_SIZE + 1);
        let value = aligned(&large_text);
        let absent = aligned("z");
        let mut substring_context = limited_context();
        assert!(
            aster_rt_string_substring_from(&raw mut substring_context, pointer(&value), 0,)
                .is_null()
        );
        let first_error = substring_context
            .take_error()
            .expect("substring allocation denial");
        assert!(first_error.contains("execution memory limit"));

        let mut replace_context = limited_context();
        assert!(
            aster_rt_string_replace(
                &raw mut replace_context,
                pointer(&value),
                pointer(&absent),
                pointer(&absent),
            )
            .is_null()
        );
        let empty = aligned("");
        assert!(
            aster_rt_string_split(&raw mut replace_context, pointer(&value), pointer(&empty),)
                .is_null()
        );
        let first_error = replace_context
            .take_error()
            .expect("replace allocation denial");
        assert!(first_error.contains("execution memory limit"));

        let many_segments = aligned(&"x,".repeat(10_000));
        let separator = aligned(",");
        let mut split_context = limited_context();
        assert!(
            aster_rt_string_split(
                &raw mut split_context,
                pointer(&many_segments),
                pointer(&separator),
            )
            .is_null()
        );
        let first_error = split_context.take_error().expect("split allocation denial");
        assert!(first_error.contains("execution memory limit"));

        // The source lives outside the denied contexts and remains valid.
        // SAFETY: `value` is the unchanged aligned test allocation.
        #[allow(unsafe_code)]
        let source = unsafe { view(pointer(&value)) };
        assert_eq!(source, Some(large_text.as_str()));
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
    fn try_parse_rejects_out_of_bounds_option_layouts_without_touching_destination() {
        let value = aligned("42");
        for (total_size, payload_offset) in [(3, 0), (8, 7), (16, i32::MAX)] {
            let mut destination = vec![0xaaaa_aaaa_aaaa_aaaa_u64; 3];
            let expected = destination.clone();
            let mut context = ExecutionContext::new();
            aster_rt_string_try_parse_int(
                &raw mut context,
                pointer(&value),
                destination.as_mut_ptr().cast::<u8>(),
                total_size,
                1,
                0,
                payload_offset,
            );
            assert!(
                context
                    .take_error()
                    .is_some_and(|error| error.contains("layout")),
                "layout total={total_size} payload={payload_offset} must fail"
            );
            assert_eq!(destination, expected, "malformed layout must not write");
        }
    }

    #[test]
    fn try_parse_payload_bounds_follow_each_scalar_width() {
        type Parser = extern "C" fn(
            *mut ExecutionContext,
            *const AsterStrHeader,
            *mut u8,
            i32,
            i32,
            i32,
            i32,
        );
        let parsers: [(&str, Parser, i32, i32); 9] = [
            ("Bool", aster_rt_string_try_parse_bool, 8, 8),
            ("Byte", aster_rt_string_try_parse_byte, 8, 8),
            ("Short", aster_rt_string_try_parse_short, 8, 7),
            ("Char", aster_rt_string_try_parse_char, 8, 5),
            ("Int", aster_rt_string_try_parse_int, 8, 5),
            ("Long", aster_rt_string_try_parse_long, 16, 9),
            ("ULong", aster_rt_string_try_parse_ulong, 16, 9),
            ("Float", aster_rt_string_try_parse_float, 8, 5),
            ("Double", aster_rt_string_try_parse_double, 16, 9),
        ];
        let value = aligned("1");
        for (name, parser, total_size, payload_offset) in parsers {
            let mut destination = vec![0xaaaa_aaaa_aaaa_aaaa_u64; 3];
            let expected = destination.clone();
            let mut context = ExecutionContext::new();
            parser(
                &raw mut context,
                pointer(&value),
                destination.as_mut_ptr().cast::<u8>(),
                total_size,
                1,
                0,
                payload_offset,
            );
            assert!(
                context
                    .take_error()
                    .is_some_and(|error| error.contains(name) && error.contains("layout")),
                "TryParse{name} malformed payload must fail"
            );
            assert_eq!(
                destination, expected,
                "TryParse{name} must not publish a malformed Option"
            );
        }
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
