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

use super::{BackendError, ExecutionValue};

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

pub(super) fn byte_width(kind: i32) -> Result<usize, BackendError> {
    match kind {
        BOOL | SBYTE | BYTE => Ok(1),
        SHORT | USHORT => Ok(2),
        INT | UINT | FLOAT | CHAR => Ok(4),
        LONG | ULONG | DOUBLE => Ok(8),
        _ => Err(BackendError::new(format!("unknown scalar kind tag {kind}"))),
    }
}

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

/// Rebuild the concrete scalar named by `kind` from `bits`. Invalid tags and
/// invalid Unicode scalar values are controlled ABI errors, never silently
/// reinterpreted as another type.
#[allow(clippy::cast_possible_truncation)]
pub(super) fn from_bits(kind: i32, bits: i64) -> Result<ExecutionValue, BackendError> {
    let bits = u64::from_ne_bytes(bits.to_ne_bytes());
    Ok(match kind {
        BOOL => ExecutionValue::Bool(bits != 0),
        SBYTE => ExecutionValue::SByte(i8::from_ne_bytes((bits as u8).to_ne_bytes())),
        BYTE => ExecutionValue::Byte(bits as u8),
        SHORT => ExecutionValue::Short(i16::from_ne_bytes((bits as u16).to_ne_bytes())),
        USHORT => ExecutionValue::UShort(bits as u16),
        INT => ExecutionValue::Int(i32::from_ne_bytes((bits as u32).to_ne_bytes())),
        UINT => ExecutionValue::UInt(bits as u32),
        FLOAT => ExecutionValue::Float(f32::from_bits(bits as u32)),
        DOUBLE => ExecutionValue::Double(f64::from_bits(bits)),
        CHAR => {
            let value = u32::try_from(bits)
                .ok()
                .and_then(char::from_u32)
                .ok_or_else(|| {
                    BackendError::new(format!(
                        "invalid Unicode scalar value 0x{bits:016X} in scalar transport"
                    ))
                })?;
            ExecutionValue::Char(value)
        }
        ULONG => ExecutionValue::ULong(bits),
        LONG => ExecutionValue::Long(i64::from_ne_bytes(bits.to_ne_bytes())),
        _ => return Err(BackendError::new(format!("unknown scalar kind tag {kind}"))),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_char_bits_are_rejected_instead_of_becoming_nul() {
        assert!(from_bits(CHAR, i64::from(u32::MAX)).is_err());
    }

    #[test]
    fn unknown_kind_is_rejected_instead_of_becoming_long() {
        assert!(from_bits(i32::MAX, 7).is_err());
    }

    #[test]
    fn bool_transport_normalizes_every_nonzero_value_to_true() {
        assert_eq!(from_bits(BOOL, 2), Ok(ExecutionValue::Bool(true)));
    }

    #[test]
    fn scalar_widths_match_the_runtime_array_abi() {
        assert_eq!(byte_width(BOOL), Ok(1));
        assert_eq!(byte_width(SHORT), Ok(2));
        assert_eq!(byte_width(FLOAT), Ok(4));
        assert_eq!(byte_width(DOUBLE), Ok(8));
        assert!(byte_width(i32::MAX).is_err());
    }
}
