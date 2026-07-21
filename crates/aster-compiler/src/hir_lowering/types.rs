use super::{ConstValue, Lowerer, ast, hir};
use crate::primitives::{self, IntegerFit, UnsignedFit, classify_integer, classify_unsigned};

impl Lowerer<'_> {
    pub(super) fn variable_declared_type(&self, variable: &ast::VariableDeclaration) -> hir::Type {
        match &variable.kind {
            ast::VariableKind::Explicit(type_ref) | ast::VariableKind::Constant(type_ref) => {
                self.resolve_type(type_ref)
            }
            ast::VariableKind::Inferred => hir::Type::Unknown,
        }
    }

    pub(super) fn resolve_type(&self, type_ref: &ast::TypeRef) -> hir::Type {
        if let Some(inner) = type_ref
            .name
            .strip_prefix("Task<")
            .and_then(|rest| rest.strip_suffix('>'))
        {
            // `Task` is reserved (see `semantic::validate_no_reserved_type_names`);
            // this is the central point that resolves it to the intrinsic
            // `hir::Type::Task`, exactly like `T[]` resolves to `Type::Array`.
            return hir::Type::Task(Box::new(
                self.resolve_type(&ast::TypeRef::new(inner, type_ref.span)),
            ));
        }
        if let Some(element) = type_ref.name.strip_suffix("[]") {
            return hir::Type::Array(Box::new(
                self.resolve_type(&ast::TypeRef::new(element, type_ref.span)),
            ));
        }
        if type_ref.name == "void" {
            return hir::Type::Void;
        }
        if let Some(primitive) = primitives::from_name(&type_ref.name) {
            return primitives::to_hir(primitive);
        }
        self.types
            .get(type_ref.name.as_str())
            .copied()
            .map_or(hir::Type::Unknown, |symbol| {
                if self.class_types.contains(&symbol) {
                    hir::Type::Class(symbol)
                } else if self.interface_types.contains(&symbol) {
                    hir::Type::Interface(symbol)
                } else if self.enum_types.contains(&symbol) {
                    hir::Type::Enum(symbol)
                } else {
                    hir::Type::User(symbol)
                }
            })
    }
}

pub(super) fn literal_value(literal: &ast::Literal) -> (hir::Literal, hir::Type) {
    match literal {
        ast::Literal::Integer(value) => {
            let type_ = match classify_integer(value) {
                Some(IntegerFit::Long) => hir::Type::Long,
                _ => hir::Type::Int,
            };
            (hir::Literal::Integer(value.clone()), type_)
        }
        ast::Literal::Long(value) => (hir::Literal::Integer(value.clone()), hir::Type::Long),
        ast::Literal::UInt(value) => {
            let type_ = match classify_unsigned(value) {
                Some(UnsignedFit::ULong) => hir::Type::ULong,
                _ => hir::Type::UInt,
            };
            (hir::Literal::Integer(value.clone()), type_)
        }
        ast::Literal::ULong(value) => (hir::Literal::Integer(value.clone()), hir::Type::ULong),
        ast::Literal::Float(value) => (hir::Literal::Float(value.clone()), hir::Type::Float),
        ast::Literal::Double(value) => (hir::Literal::Float(value.clone()), hir::Type::Double),
        ast::Literal::Decimal(value) => (hir::Literal::Decimal(value.clone()), hir::Type::Decimal),
        ast::Literal::String(value) => (hir::Literal::String(value.clone()), hir::Type::String),
        ast::Literal::Character(value) => (hir::Literal::Character(*value), hir::Type::Char),
        ast::Literal::Boolean(value) => (hir::Literal::Boolean(*value), hir::Type::Bool),
    }
}

/// Materialize an evaluated constant as a literal expression.
pub(super) fn constant_expression(value: &ConstValue) -> hir::Expression {
    let (literal, type_) = match value {
        ConstValue::Integer(value, kind) => (
            hir::Literal::Integer(value.to_string()),
            primitives::to_hir(*kind),
        ),
        ConstValue::Float(value) => (hir::Literal::Float(value.to_string()), hir::Type::Float),
        ConstValue::Double(value) => (hir::Literal::Float(value.to_string()), hir::Type::Double),
        ConstValue::Decimal(value) => (hir::Literal::Decimal(value.clone()), hir::Type::Decimal),
        ConstValue::Bool(value) => (hir::Literal::Boolean(*value), hir::Type::Bool),
        ConstValue::Char(value) => (hir::Literal::Character(*value), hir::Type::Char),
        ConstValue::Str(value) => (hir::Literal::String(value.clone()), hir::Type::String),
    };
    hir::Expression {
        type_,
        kind: hir::ExpressionKind::Literal(literal),
    }
}

/// Wrap an expression in a `Convert` node when its type differs from the
/// validated target type. Only value types are ever converted.
pub(super) fn convert(expression: hir::Expression, target: &hir::Type) -> hir::Expression {
    if let (hir::Type::Class(class), hir::Type::Interface(interface)) =
        (expression.type_.clone(), target)
    {
        return hir::Expression {
            type_: target.clone(),
            kind: hir::ExpressionKind::UpcastInterface {
                object: Box::new(expression),
                class,
                interface: *interface,
            },
        };
    }
    if &expression.type_ == target
        || matches!(
            expression.type_,
            hir::Type::Unknown | hir::Type::User(_) | hir::Type::Class(_) | hir::Type::Interface(_)
        )
        || matches!(
            target,
            hir::Type::Unknown
                | hir::Type::Void
                | hir::Type::User(_)
                | hir::Type::Class(_)
                | hir::Type::Interface(_)
        )
    {
        return expression;
    }
    hir::Expression {
        type_: target.clone(),
        kind: hir::ExpressionKind::Convert {
            operand: Box::new(expression),
        },
    }
}

/// The validated common type of two `?:` branches (the promotion table for
/// numeric branches, or their identical type otherwise).
pub(super) fn conditional_type(left: &hir::Type, right: &hir::Type) -> hir::Type {
    if left == right {
        return left.clone();
    }
    match (left, right) {
        (hir::Type::Class(_), hir::Type::Interface(_)) => return right.clone(),
        (hir::Type::Interface(_), hir::Type::Class(_)) => return left.clone(),
        _ => {}
    }
    promoted(left, right).unwrap_or_else(|| left.clone())
}

/// The validated common numeric type of two operands, from the central table.
pub(super) fn promoted(left: &hir::Type, right: &hir::Type) -> Option<hir::Type> {
    let (left, right) = (primitives::of_hir(left)?, primitives::of_hir(right)?);
    primitives::promote(left, right).map(primitives::to_hir)
}

pub(super) fn binary_type(
    operator: ast::BinaryOperator,
    left: &hir::Type,
    right: &hir::Type,
) -> hir::Type {
    use ast::BinaryOperator::{
        Equal, Greater, GreaterEqual, Less, LessEqual, LogicalAnd, LogicalOr, NotEqual,
    };
    if matches!(
        operator,
        Equal | NotEqual | Less | LessEqual | Greater | GreaterEqual | LogicalAnd | LogicalOr
    ) {
        return hir::Type::Bool;
    }
    if left == right {
        return left.clone();
    }
    promoted(left, right).unwrap_or_else(|| left.clone())
}
