//! Typed transport for one scalar value across the async/parallel ABI.
//!
//! Async frame slots and results move between JIT-generated code and the host
//! runtime as a `(kind, i64 bit pattern)` pair, never as an untyped integer:
//! [`from_bits`] rebuilds the exact [`ExecutionValue`] variant (preserving
//! width, signedness, `bool`, `char`, and float bit patterns), and [`to_bits`]
//! is its inverse. Generated code widens the scalar to the 64-bit carrier and
//! narrows it back using the local's own concrete type (see
//! `calls::scalar_to_bits`/`scalar_from_bits`), so the kind tag and the bit
//! pattern together always name a fully concrete value.

use super::ExecutionValue;

// Kind tags shared by codegen (`calls::scalar_kind`) and the runtime ABI.
pub(super) const BOOL: i32 = 0;
pub(super) const SBYTE: i32 = 1;
pub(super) const BYTE: i32 = 2;
pub(super) const SHORT: i32 = 3;
pub(super) const USHORT: i32 = 4;
pub(super) const INT: i32 = 5;
pub(super) const UINT: i32 = 6;
pub(super) const LONG: i32 = 7;
pub(super) const ULONG: i32 = 8;
pub(super) const FLOAT: i32 = 9;
pub(super) const DOUBLE: i32 = 10;
pub(super) const CHAR: i32 = 11;

/// The 64-bit carrier for `value`'s raw bits (zero-extended for narrow
/// integers, IEEE bit pattern for floats).
pub(super) fn to_bits(value: &ExecutionValue) -> i64 {
    let bits: u64 = match value {
        ExecutionValue::Bool(value) => u64::from(*value),
        ExecutionValue::SByte(value) => u64::from(u8::from_ne_bytes(value.to_ne_bytes())),
        ExecutionValue::Byte(value) => u64::from(*value),
        ExecutionValue::Short(value) => u64::from(u16::from_ne_bytes(value.to_ne_bytes())),
        ExecutionValue::UShort(value) => u64::from(*value),
        ExecutionValue::Int(value) => u64::from(u32::from_ne_bytes(value.to_ne_bytes())),
        ExecutionValue::UInt(value) => u64::from(*value),
        ExecutionValue::Long(value) => u64::from_ne_bytes(value.to_ne_bytes()),
        ExecutionValue::ULong(value) => *value,
        ExecutionValue::Float(value) => u64::from(value.to_bits()),
        ExecutionValue::Double(value) => value.to_bits(),
        ExecutionValue::Char(value) => u64::from(*value as u32),
        ExecutionValue::String(_) | ExecutionValue::Void => 0,
    };
    i64::from_ne_bytes(bits.to_ne_bytes())
}

/// Rebuild the concrete scalar named by `kind` from `bits`. An unknown kind or
/// an invalid `char` scalar becomes a well-defined fallback (the same one as
/// `LONG`) rather than a panic, keeping every ABI path controlled.
#[allow(clippy::cast_possible_truncation)]
pub(super) fn from_bits(kind: i32, bits: i64) -> ExecutionValue {
    let bits = u64::from_ne_bytes(bits.to_ne_bytes());
    match kind {
        BOOL => ExecutionValue::Bool(bits & 1 != 0),
        SBYTE => ExecutionValue::SByte(i8::from_ne_bytes((bits as u8).to_ne_bytes())),
        BYTE => ExecutionValue::Byte(bits as u8),
        SHORT => ExecutionValue::Short(i16::from_ne_bytes((bits as u16).to_ne_bytes())),
        USHORT => ExecutionValue::UShort(bits as u16),
        INT => ExecutionValue::Int(i32::from_ne_bytes((bits as u32).to_ne_bytes())),
        UINT => ExecutionValue::UInt(bits as u32),
        FLOAT => ExecutionValue::Float(f32::from_bits(bits as u32)),
        DOUBLE => ExecutionValue::Double(f64::from_bits(bits)),
        CHAR => ExecutionValue::Char(char::from_u32(bits as u32).unwrap_or('\u{0}')),
        ULONG => ExecutionValue::ULong(bits),
        // `LONG` and any unrecognized kind share this fallback.
        _ => ExecutionValue::Long(i64::from_ne_bytes(bits.to_ne_bytes())),
    }
}
