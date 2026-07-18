//! Backend-neutral facts and rules for Aster primitive types.
//!
//! This crate deliberately has no dependency on syntax trees, compiler IRs,
//! or a machine-code backend. Those layers adapt their own type representation
//! to [`Primitive`] and share the rules defined here.

/// A primitive Aster value type.
///
/// `void` is not a value type and user-defined types are owned by the
/// compiler's symbol model, so neither belongs in this enum.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Primitive {
    Bool,
    Char,
    SByte,
    Byte,
    Short,
    UShort,
    Int,
    UInt,
    Long,
    ULong,
    Float,
    Double,
    Decimal,
    String,
}

/// Signedness of an integer primitive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signedness {
    Signed,
    Unsigned,
}

impl Primitive {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Bool => "bool",
            Self::Char => "char",
            Self::SByte => "sbyte",
            Self::Byte => "byte",
            Self::Short => "short",
            Self::UShort => "ushort",
            Self::Int => "int",
            Self::UInt => "uint",
            Self::Long => "long",
            Self::ULong => "ulong",
            Self::Float => "float",
            Self::Double => "double",
            Self::Decimal => "decimal",
            Self::String => "string",
        }
    }

    /// Storage width of fixed-width scalar primitives.
    ///
    /// `decimal` returns `None` until its runtime representation is fixed;
    /// `string` is a runtime-managed value whose ABI is not a scalar width.
    #[must_use]
    pub const fn bit_width(self) -> Option<u16> {
        match self {
            Self::Bool | Self::SByte | Self::Byte => Some(8),
            Self::Short | Self::UShort => Some(16),
            Self::Char | Self::Int | Self::UInt | Self::Float => Some(32),
            Self::Long | Self::ULong | Self::Double => Some(64),
            Self::Decimal | Self::String => None,
        }
    }

    #[must_use]
    pub const fn signedness(self) -> Option<Signedness> {
        match self {
            Self::SByte | Self::Short | Self::Int | Self::Long => Some(Signedness::Signed),
            Self::Byte | Self::UShort | Self::UInt | Self::ULong => Some(Signedness::Unsigned),
            _ => None,
        }
    }

    #[must_use]
    pub const fn is_integer(self) -> bool {
        self.signedness().is_some()
    }

    #[must_use]
    pub const fn is_unsigned(self) -> bool {
        matches!(self.signedness(), Some(Signedness::Unsigned))
    }

    #[must_use]
    pub const fn is_float(self) -> bool {
        matches!(self, Self::Float | Self::Double)
    }

    #[must_use]
    pub const fn is_numeric(self) -> bool {
        self.is_integer() || self.is_float() || matches!(self, Self::Decimal)
    }

    /// Inclusive mathematical range of an integer primitive.
    #[must_use]
    pub const fn integer_range(self) -> Option<(i128, i128)> {
        match self {
            Self::SByte => Some((i8::MIN as i128, i8::MAX as i128)),
            Self::Byte => Some((0, u8::MAX as i128)),
            Self::Short => Some((i16::MIN as i128, i16::MAX as i128)),
            Self::UShort => Some((0, u16::MAX as i128)),
            Self::Int => Some((i32::MIN as i128, i32::MAX as i128)),
            Self::UInt => Some((0, u32::MAX as i128)),
            Self::Long => Some((i64::MIN as i128, i64::MAX as i128)),
            Self::ULong => Some((0, u64::MAX as i128)),
            _ => None,
        }
    }
}

/// Whether every value of `from` can be represented exactly by `to`.
#[must_use]
pub fn implicit_converts(from: Primitive, to: Primitive) -> bool {
    use Primitive::{Byte, Decimal, Double, Float, Int, Long, SByte, Short, UInt, ULong, UShort};
    if from == to {
        return true;
    }
    match from {
        SByte => matches!(to, Short | Int | Long | Float | Double | Decimal),
        Byte => matches!(
            to,
            Short | UShort | Int | UInt | Long | ULong | Float | Double | Decimal
        ),
        Short => matches!(to, Int | Long | Float | Double | Decimal),
        UShort => matches!(to, Int | UInt | Long | ULong | Float | Double | Decimal),
        Int => matches!(to, Long | Double | Decimal),
        UInt => matches!(to, Long | ULong | Double | Decimal),
        Long | ULong => matches!(to, Decimal),
        Float => matches!(to, Double),
        _ => false,
    }
}

/// Common type for a binary numeric operation.
///
/// Small integers always promote to `int`. Other pairs receive a common type
/// only when both operands convert to it without losing range, precision, or
/// signedness. Integer pairs do not jump to `decimal` merely to bridge an
/// incompatible signed/unsigned pair.
#[must_use]
pub fn promote(left: Primitive, right: Primitive) -> Option<Primitive> {
    use Primitive::{Decimal, Double, Float, Int, Long, UInt, ULong};
    if !left.is_numeric() || !right.is_numeric() {
        return None;
    }
    if left == Decimal || right == Decimal {
        let other = if left == Decimal { right } else { left };
        return (other == Decimal || other.is_integer()).then_some(Decimal);
    }
    if left.is_integer() && right.is_integer() {
        if is_small_integer(left) && is_small_integer(right) {
            return Some(Int);
        }
        return [Int, UInt, Long, ULong].into_iter().find(|candidate| {
            implicit_converts(left, *candidate) && implicit_converts(right, *candidate)
        });
    }
    if left == right {
        return Some(left);
    }
    [Float, Double].into_iter().find(|candidate| {
        implicit_converts(left, *candidate) && implicit_converts(right, *candidate)
    })
}

const fn is_small_integer(primitive: Primitive) -> bool {
    matches!(
        primitive,
        Primitive::SByte | Primitive::Byte | Primitive::Short | Primitive::UShort
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntegerFit {
    Int,
    Long,
}

#[must_use]
pub fn classify_integer(text: &str) -> Option<IntegerFit> {
    if text.parse::<i32>().is_ok() {
        Some(IntegerFit::Int)
    } else if text.parse::<i64>().is_ok() {
        Some(IntegerFit::Long)
    } else {
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnsignedFit {
    UInt,
    ULong,
}

#[must_use]
pub fn classify_unsigned(text: &str) -> Option<UnsignedFit> {
    if text.parse::<u32>().is_ok() {
        Some(UnsignedFit::UInt)
    } else if text.parse::<u64>().is_ok() {
        Some(UnsignedFit::ULong)
    } else {
        None
    }
}

#[must_use]
pub fn fits_long(text: &str) -> bool {
    text.parse::<i64>().is_ok()
}

#[must_use]
pub fn fits_ulong(text: &str) -> bool {
    text.parse::<u64>().is_ok()
}

#[must_use]
pub fn from_name(name: &str) -> Option<Primitive> {
    Some(match name {
        "bool" => Primitive::Bool,
        "char" => Primitive::Char,
        "sbyte" => Primitive::SByte,
        "byte" => Primitive::Byte,
        "short" => Primitive::Short,
        "ushort" => Primitive::UShort,
        "int" => Primitive::Int,
        "uint" => Primitive::UInt,
        "long" => Primitive::Long,
        "ulong" => Primitive::ULong,
        "float" => Primitive::Float,
        "double" => Primitive::Double,
        "decimal" => Primitive::Decimal,
        "string" => Primitive::String,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_scalar_metadata() {
        assert_eq!(Primitive::SByte.bit_width(), Some(8));
        assert_eq!(Primitive::Double.bit_width(), Some(64));
        assert_eq!(Primitive::Decimal.bit_width(), None);
        assert_eq!(Primitive::ULong.signedness(), Some(Signedness::Unsigned));
        assert_eq!(
            Primitive::Long.integer_range(),
            Some((i128::from(i64::MIN), i128::from(i64::MAX)))
        );
    }

    #[test]
    fn classifies_integer_literals_by_range() {
        assert_eq!(classify_integer("2147483647"), Some(IntegerFit::Int));
        assert_eq!(classify_integer("2147483648"), Some(IntegerFit::Long));
        assert_eq!(classify_integer("9223372036854775808"), None);
        assert_eq!(classify_unsigned("4294967295"), Some(UnsignedFit::UInt));
        assert_eq!(classify_unsigned("4294967296"), Some(UnsignedFit::ULong));
        assert_eq!(classify_unsigned("18446744073709551616"), None);
    }

    #[test]
    fn implicit_conversions_preserve_every_value_exactly() {
        assert!(implicit_converts(Primitive::Byte, Primitive::Float));
        assert!(implicit_converts(Primitive::Int, Primitive::Double));
        assert!(implicit_converts(Primitive::Float, Primitive::Double));
        assert!(!implicit_converts(Primitive::Int, Primitive::Float));
        assert!(!implicit_converts(Primitive::Long, Primitive::Double));
        assert!(!implicit_converts(Primitive::ULong, Primitive::Double));
    }

    #[test]
    fn promotion_uses_only_safe_common_types() {
        assert_eq!(
            promote(Primitive::Byte, Primitive::Byte),
            Some(Primitive::Int)
        );
        assert_eq!(
            promote(Primitive::Short, Primitive::UShort),
            Some(Primitive::Int)
        );
        assert_eq!(
            promote(Primitive::Int, Primitive::UInt),
            Some(Primitive::Long)
        );
        assert_eq!(
            promote(Primitive::Int, Primitive::Float),
            Some(Primitive::Double)
        );
        assert_eq!(promote(Primitive::Long, Primitive::Float), None);
        assert_eq!(promote(Primitive::Long, Primitive::Double), None);
        assert_eq!(promote(Primitive::ULong, Primitive::Long), None);
        assert_eq!(
            promote(Primitive::Decimal, Primitive::ULong),
            Some(Primitive::Decimal)
        );
    }
}
