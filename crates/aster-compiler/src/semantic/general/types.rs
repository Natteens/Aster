use super::{
    Context, Diagnostic, Expression, ExpressionKind, HashSet, Literal, Primitive, TypeKind,
    TypeRef, UnaryOperator, primitives,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Type {
    Void,
    Bool,
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
    Char,
    String,
    User(String),
    Class(String),
    Interface(String),
    Enum(String),
    Array(Box<Type>),
    /// `aster.core.Task<T>`, recognized structurally like `Array`, never
    /// looked up as a user-declared generic type.
    Task(Box<Type>),
    /// `List<T>`, recognized structurally like `Task<T>`: a reserved,
    /// compiler-intrinsic reference type, never a user-declared generic.
    List(Box<Type>),
    Unknown,
}

impl Type {
    pub(super) fn display(&self) -> String {
        match self {
            Self::Void => "void".to_owned(),
            Self::User(name) | Self::Class(name) | Self::Interface(name) | Self::Enum(name) => {
                name.clone()
            }
            Self::Array(element) => format!("{}[]", element.display()),
            Self::Task(result) => format!("Task<{}>", result.display()),
            Self::List(element) => format!("List<{}>", element.display()),
            Self::Unknown => "<unknown>".to_owned(),
            _ => self
                .primitive()
                .expect("non-void, non-user type")
                .name()
                .to_owned(),
        }
    }

    /// The primitive behind this type, when it has one. All numeric rules
    /// (conversions, promotion, ranges) live in `crate::primitives`.
    pub(super) fn primitive(&self) -> Option<Primitive> {
        Some(match self {
            Self::Bool => Primitive::Bool,
            Self::Char => Primitive::Char,
            Self::SByte => Primitive::SByte,
            Self::Byte => Primitive::Byte,
            Self::Short => Primitive::Short,
            Self::UShort => Primitive::UShort,
            Self::Int => Primitive::Int,
            Self::UInt => Primitive::UInt,
            Self::Long => Primitive::Long,
            Self::ULong => Primitive::ULong,
            Self::Float => Primitive::Float,
            Self::Double => Primitive::Double,
            Self::Decimal => Primitive::Decimal,
            Self::String => Primitive::String,
            Self::Void
            | Self::User(_)
            | Self::Class(_)
            | Self::Interface(_)
            | Self::Enum(_)
            | Self::Array(_)
            | Self::Task(_)
            | Self::List(_)
            | Self::Unknown => {
                return None;
            }
        })
    }

    pub(super) fn from_primitive(primitive: Primitive) -> Self {
        match primitive {
            Primitive::Bool => Self::Bool,
            Primitive::Char => Self::Char,
            Primitive::SByte => Self::SByte,
            Primitive::Byte => Self::Byte,
            Primitive::Short => Self::Short,
            Primitive::UShort => Self::UShort,
            Primitive::Int => Self::Int,
            Primitive::UInt => Self::UInt,
            Primitive::Long => Self::Long,
            Primitive::ULong => Self::ULong,
            Primitive::Float => Self::Float,
            Primitive::Double => Self::Double,
            Primitive::Decimal => Self::Decimal,
            Primitive::String => Self::String,
        }
    }

    pub(super) fn is_numeric(&self) -> bool {
        self.primitive().is_some_and(Primitive::is_numeric)
    }

    pub(super) fn is_float(&self) -> bool {
        self.primitive().is_some_and(Primitive::is_float)
    }
}

pub(super) fn resolve_type(
    type_ref: &TypeRef,
    context: &Context,
    diagnostics: &mut Vec<Diagnostic>,
) -> Type {
    let type_ = resolve_type_readonly(type_ref, context);
    if type_ == Type::Unknown {
        if context
            .types
            .get(type_ref.name.as_str())
            .is_some_and(|info| info.is_static)
        {
            diagnostics.push(
                Diagnostic::error(
                    format!(
                        "static class `{}` cannot be used as a value type",
                        type_ref.name
                    ),
                    type_ref.span,
                )
                .with_help("call its static methods through the class name"),
            );
        } else {
            diagnostics.push(
                Diagnostic::error(format!("unknown type `{}`", type_ref.name), type_ref.span)
                    .with_help("declare the type or use a known basic type"),
            );
        }
    }
    type_
}

pub(super) fn resolve_type_readonly(type_ref: &TypeRef, context: &Context) -> Type {
    if let Some(inner) = type_ref
        .name
        .strip_prefix("Task<")
        .and_then(|rest| rest.strip_suffix('>'))
    {
        // `Task<T>` is a reserved intrinsic type (no user declaration named
        // `Task` can exist; see `semantic::validate_no_reserved_type_names`),
        // resolved structurally before any user-declared-type lookup runs,
        // exactly like `T[]`.
        let result = resolve_type_readonly(&TypeRef::new(inner, type_ref.span), context);
        return if result == Type::Unknown {
            Type::Unknown
        } else {
            Type::Task(Box::new(result))
        };
    }
    if let Some(inner) = type_ref
        .name
        .strip_prefix("List<")
        .and_then(|rest| rest.strip_suffix('>'))
    {
        // `List<T>` is a reserved intrinsic type (no user declaration named
        // `List` can exist; see `semantic::validate_no_reserved_type_names`),
        // resolved structurally before any user-declared-type lookup runs,
        // exactly like `Task<T>`. `void` and `decimal` (not executable yet)
        // collapse to `Unknown` here rather than a second, divergent
        // "executable type" list.
        let result = resolve_type_readonly(&TypeRef::new(inner, type_ref.span), context);
        return if matches!(result, Type::Void | Type::Decimal | Type::Unknown) {
            Type::Unknown
        } else {
            Type::List(Box::new(result))
        };
    }
    if let Some(element) = type_ref.name.strip_suffix("[]") {
        let element = resolve_type_readonly(&TypeRef::new(element, type_ref.span), context);
        return if matches!(element, Type::Void | Type::Unknown) {
            Type::Unknown
        } else {
            Type::Array(Box::new(element))
        };
    }
    if type_ref.name == "void" {
        return Type::Void;
    }
    if let Some(primitive) = primitives::from_name(&type_ref.name) {
        return Type::from_primitive(primitive);
    }
    let info = context.types.get(type_ref.name.as_str());
    if info.is_some_and(|info| info.is_static) {
        return Type::Unknown;
    }
    match info.and_then(|info| info.kind) {
        Some(TypeKind::Class) => Type::Class(type_ref.name.clone()),
        Some(TypeKind::Interface) => Type::Interface(type_ref.name.clone()),
        Some(TypeKind::Enum) => Type::Enum(type_ref.name.clone()),
        Some(_) => Type::User(type_ref.name.clone()),
        None => Type::Unknown,
    }
}

pub(super) fn compatible(expected: &Type, actual: &Type) -> bool {
    if expected == actual {
        return true;
    }
    match (expected.primitive(), actual.primitive()) {
        (Some(expected), Some(actual)) => primitives::implicit_converts(actual, expected),
        _ => false,
    }
}

pub(super) fn constant_integer(expression: &Expression) -> Option<i128> {
    match &expression.kind {
        ExpressionKind::Literal(Literal::Integer(value)) => value.parse().ok(),
        ExpressionKind::Unary {
            operator: UnaryOperator::Negate,
            operand,
        } => constant_integer(operand)?.checked_neg(),
        _ => None,
    }
}

pub(super) fn zero_initializable(
    type_: &Type,
    context: &Context,
    visiting: &mut HashSet<String>,
) -> bool {
    match type_ {
        Type::String
        | Type::Decimal
        | Type::Array(_)
        | Type::Class(_)
        | Type::Interface(_)
        | Type::Enum(_)
        | Type::List(_)
        | Type::Void
        | Type::Unknown => false,
        Type::User(name) => {
            if !visiting.insert(name.clone()) {
                return false;
            }
            let result = context.types.get(name).is_some_and(|info| {
                info.fields
                    .values()
                    .all(|field| zero_initializable(field, context, visiting))
            });
            visiting.remove(name);
            result
        }
        _ => true,
    }
}

/// The common type of two numeric operands, per the documented promotion table.
pub(super) fn promoted_numeric(left: &Type, right: &Type) -> Option<Type> {
    let (left, right) = (left.primitive()?, right.primitive()?);
    primitives::promote(left, right).map(Type::from_primitive)
}
