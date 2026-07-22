//! Compiler adapters for the backend-neutral primitive type model.

pub(crate) use aster_types::{
    IntegerFit, Primitive, UnsignedFit, classify_integer, classify_unsigned, fits_long, fits_ulong,
    from_name, implicit_converts, promote,
};

/// Map an HIR type to its primitive, when it has one.
pub(crate) fn of_hir(type_: &aster_hir::Type) -> Option<Primitive> {
    use aster_hir::Type;
    Some(match type_ {
        Type::Bool => Primitive::Bool,
        Type::Char => Primitive::Char,
        Type::SByte => Primitive::SByte,
        Type::Byte => Primitive::Byte,
        Type::Short => Primitive::Short,
        Type::UShort => Primitive::UShort,
        Type::Int => Primitive::Int,
        Type::UInt => Primitive::UInt,
        Type::Long => Primitive::Long,
        Type::ULong => Primitive::ULong,
        Type::Float => Primitive::Float,
        Type::Double => Primitive::Double,
        Type::Decimal => Primitive::Decimal,
        Type::String => Primitive::String,
        Type::Void
        | Type::User(_)
        | Type::Class(_)
        | Type::Interface(_)
        | Type::Enum(_)
        | Type::Array(_)
        | Type::Task(_)
        | Type::List(_)
        | Type::Unknown => {
            return None;
        }
    })
}

pub(crate) fn to_hir(primitive: Primitive) -> aster_hir::Type {
    use aster_hir::Type;
    match primitive {
        Primitive::Bool => Type::Bool,
        Primitive::Char => Type::Char,
        Primitive::SByte => Type::SByte,
        Primitive::Byte => Type::Byte,
        Primitive::Short => Type::Short,
        Primitive::UShort => Type::UShort,
        Primitive::Int => Type::Int,
        Primitive::UInt => Type::UInt,
        Primitive::Long => Type::Long,
        Primitive::ULong => Type::ULong,
        Primitive::Float => Type::Float,
        Primitive::Double => Type::Double,
        Primitive::Decimal => Type::Decimal,
        Primitive::String => Type::String,
    }
}
