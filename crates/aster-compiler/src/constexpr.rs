//! Compile-time evaluation of constant expressions.
//!
//! One evaluator serves both semantic validation (diagnosing non-constant
//! initializers, overflow, and division by zero) and HIR lowering (folding
//! constants into literals). It mirrors runtime semantics: the promotion
//! table from `crate::primitives`, two's-complement integer casts, IEEE-754
//! floating point, short-circuit logic, and lazily evaluated `?:` branches.
//! Constant integer arithmetic is checked and diagnosed, while runtime
//! integer arithmetic currently wraps; this distinction is intentional.
//!
//! `decimal` constants are limited to a single literal for now; arithmetic on
//! them becomes possible together with the executable decimal runtime.

use aster_diagnostics::Span;
use aster_syntax::{BinaryOperator, Expression, ExpressionKind, Literal, TypeRef, UnaryOperator};

use crate::primitives::{
    self, IntegerFit, Primitive, UnsignedFit, classify_integer, classify_unsigned,
};

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ConstValue {
    /// Any integer value together with its Aster type.
    Integer(i128, Primitive),
    Float(f32),
    Double(f64),
    Bool(bool),
    Char(char),
    Str(String),
    /// An exact base-10 literal, kept as text until a runtime representation exists.
    Decimal(String),
}

/// The integer behind a constant, when it is an integer.
pub(crate) fn integer_value(value: &ConstValue) -> Option<i128> {
    match value {
        ConstValue::Integer(value, _) => Some(*value),
        _ => None,
    }
}

impl ConstValue {
    /// Convert toward a declared type name: implicit widening, plus narrowing
    /// of integer constants whose value fits the target range. Returns the
    /// value unchanged when no conversion applies; the type checker owns
    /// mismatch reporting.
    #[must_use]
    pub(crate) fn coerce_to(self, type_name: &str) -> Self {
        let Some(target) = primitives::from_name(type_name) else {
            return self;
        };
        match self {
            Self::Integer(value, _) if target.is_integer() => Self::Integer(value, target),
            #[allow(clippy::cast_precision_loss)]
            Self::Integer(value, _) if target == Primitive::Float => Self::Float(value as f32),
            #[allow(clippy::cast_precision_loss)]
            Self::Integer(value, _) if target == Primitive::Double => Self::Double(value as f64),
            Self::Integer(value, _) if target == Primitive::Decimal => {
                Self::Decimal(value.to_string())
            }
            Self::Float(value) if target == Primitive::Double => Self::Double(f64::from(value)),
            other => other,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ConstError {
    /// The expression cannot be evaluated at compile time.
    NotConstant(Span),
    /// Integer arithmetic exceeded the range of its type.
    Overflow(Span, &'static str),
    /// Integer division or remainder by zero.
    DivisionByZero(Span),
}

/// Evaluate a validated expression at compile time. `resolve` supplies the
/// values of previously evaluated constants by name.
pub(crate) fn evaluate(
    expression: &Expression,
    resolve: &dyn Fn(&str) -> Option<ConstValue>,
) -> Result<ConstValue, ConstError> {
    let span = expression.span;
    match &expression.kind {
        ExpressionKind::Literal(literal) => literal_value(literal, span),
        ExpressionKind::Name(name) => resolve(name).ok_or(ConstError::NotConstant(span)),
        ExpressionKind::Unary { operator, operand } => {
            let value = evaluate(operand, resolve)?;
            unary(*operator, value, span)
        }
        ExpressionKind::Binary {
            left,
            operator,
            right,
        } => binary(*operator, left, right, resolve, span),
        ExpressionKind::Conditional {
            condition,
            when_true,
            when_false,
        } => match evaluate(condition, resolve)? {
            ConstValue::Bool(true) => evaluate(when_true, resolve),
            ConstValue::Bool(false) => evaluate(when_false, resolve),
            _ => Err(ConstError::NotConstant(span)),
        },
        ExpressionKind::Cast { target, operand } => {
            let value = evaluate(operand, resolve)?;
            cast(target, value, span)
        }
        ExpressionKind::StructLiteral { .. }
        | ExpressionKind::This
        | ExpressionKind::NewObject { .. }
        | ExpressionKind::ArrayLiteral(_)
        | ExpressionKind::NewArray { .. }
        | ExpressionKind::Index { .. }
        | ExpressionKind::Member { .. }
        | ExpressionKind::Call { .. }
        | ExpressionKind::IncrementDecrement { .. }
        | ExpressionKind::Try { .. }
        | ExpressionKind::Await { .. }
        | ExpressionKind::Assignment { .. }
        | ExpressionKind::InterpolatedString { .. } => Err(ConstError::NotConstant(span)),
    }
}

fn literal_value(literal: &Literal, span: Span) -> Result<ConstValue, ConstError> {
    let integer = |text: &str, primitive| {
        Ok(ConstValue::Integer(
            text.parse().expect("classified literal parses"),
            primitive,
        ))
    };
    match literal {
        Literal::Integer(text) => match classify_integer(text) {
            Some(IntegerFit::Int) => integer(text, Primitive::Int),
            Some(IntegerFit::Long) => integer(text, Primitive::Long),
            None => Err(ConstError::Overflow(span, "long")),
        },
        Literal::Long(text) => {
            if primitives::fits_long(text) {
                integer(text, Primitive::Long)
            } else {
                Err(ConstError::Overflow(span, "long"))
            }
        }
        Literal::UInt(text) => match classify_unsigned(text) {
            Some(UnsignedFit::UInt) => integer(text, Primitive::UInt),
            Some(UnsignedFit::ULong) => integer(text, Primitive::ULong),
            None => Err(ConstError::Overflow(span, "ulong")),
        },
        Literal::ULong(text) => {
            if primitives::fits_ulong(text) {
                integer(text, Primitive::ULong)
            } else {
                Err(ConstError::Overflow(span, "ulong"))
            }
        }
        Literal::Float(text) => Ok(ConstValue::Float(
            text.parse().expect("lexed float literal parses"),
        )),
        Literal::Double(text) => Ok(ConstValue::Double(
            text.parse().expect("lexed double literal parses"),
        )),
        Literal::Decimal(text) => Ok(ConstValue::Decimal(text.clone())),
        Literal::String(value) => Ok(ConstValue::Str(value.clone())),
        Literal::Character(value) => Ok(ConstValue::Char(*value)),
        Literal::Boolean(value) => Ok(ConstValue::Bool(*value)),
    }
}

fn unary(operator: UnaryOperator, value: ConstValue, span: Span) -> Result<ConstValue, ConstError> {
    match (operator, value) {
        (UnaryOperator::Not, ConstValue::Bool(value)) => Ok(ConstValue::Bool(!value)),
        (UnaryOperator::Negate, ConstValue::Integer(value, kind)) => {
            if kind.is_unsigned() {
                return Err(ConstError::NotConstant(span));
            }
            in_range(-value, kind, span)
        }
        (UnaryOperator::Negate, ConstValue::Float(value)) => Ok(ConstValue::Float(-value)),
        (UnaryOperator::Negate, ConstValue::Double(value)) => Ok(ConstValue::Double(-value)),
        _ => Err(ConstError::NotConstant(span)),
    }
}

fn binary(
    operator: BinaryOperator,
    left: &Expression,
    right: &Expression,
    resolve: &dyn Fn(&str) -> Option<ConstValue>,
    span: Span,
) -> Result<ConstValue, ConstError> {
    use BinaryOperator::{Add, Equal, LogicalAnd, LogicalOr, NotEqual};
    if matches!(operator, LogicalAnd | LogicalOr) {
        let ConstValue::Bool(left) = evaluate(left, resolve)? else {
            return Err(ConstError::NotConstant(span));
        };
        if operator == LogicalAnd && !left {
            return Ok(ConstValue::Bool(false));
        }
        if operator == LogicalOr && left {
            return Ok(ConstValue::Bool(true));
        }
        return match evaluate(right, resolve)? {
            ConstValue::Bool(right) => Ok(ConstValue::Bool(right)),
            _ => Err(ConstError::NotConstant(span)),
        };
    }
    let left = evaluate(left, resolve)?;
    let right = evaluate(right, resolve)?;
    match (&left, &right) {
        (ConstValue::Str(left), ConstValue::Str(right)) => match operator {
            Equal => Ok(ConstValue::Bool(left == right)),
            NotEqual => Ok(ConstValue::Bool(left != right)),
            Add => Ok(ConstValue::Str(format!("{left}{right}"))),
            _ => Err(ConstError::NotConstant(span)),
        },
        (ConstValue::Bool(left), ConstValue::Bool(right)) => match operator {
            Equal => Ok(ConstValue::Bool(left == right)),
            NotEqual => Ok(ConstValue::Bool(left != right)),
            _ => Err(ConstError::NotConstant(span)),
        },
        (ConstValue::Char(left), ConstValue::Char(right)) => match operator {
            Equal => Ok(ConstValue::Bool(left == right)),
            NotEqual => Ok(ConstValue::Bool(left != right)),
            _ => Err(ConstError::NotConstant(span)),
        },
        (ConstValue::Integer(a, left_kind), ConstValue::Integer(b, right_kind)) => {
            let kind = primitives::promote(*left_kind, *right_kind)
                .filter(|kind| kind.is_integer())
                .ok_or(ConstError::NotConstant(span))?;
            integer_binary(operator, *a, *b, kind, span)
        }
        _ => {
            let (a, b) = float_operands(&left, &right).ok_or(ConstError::NotConstant(span))?;
            if matches!(
                (&left, &right),
                (ConstValue::Double(_), _) | (_, ConstValue::Double(_))
            ) {
                double_binary(operator, a, b, span)
            } else {
                #[allow(clippy::cast_possible_truncation)]
                float_binary(operator, a as f32, b as f32, span)
            }
        }
    }
}

/// Both operands as `f64` when the pair mixes integers and floating point.
#[allow(clippy::cast_precision_loss)]
fn float_operands(left: &ConstValue, right: &ConstValue) -> Option<(f64, f64)> {
    let widen = |value: &ConstValue| match value {
        ConstValue::Integer(value, _) => Some(*value as f64),
        ConstValue::Float(value) => Some(f64::from(*value)),
        ConstValue::Double(value) => Some(*value),
        _ => None,
    };
    Some((widen(left)?, widen(right)?))
}

fn integer_binary(
    operator: BinaryOperator,
    left: i128,
    right: i128,
    kind: Primitive,
    span: Span,
) -> Result<ConstValue, ConstError> {
    use BinaryOperator::{Add, Divide, Multiply, Remainder, Subtract};
    match operator {
        Add => checked_integer(left.checked_add(right), kind, span),
        Subtract => checked_integer(left.checked_sub(right), kind, span),
        Multiply => checked_integer(left.checked_mul(right), kind, span),
        Divide | Remainder if right == 0 => Err(ConstError::DivisionByZero(span)),
        Divide => in_range(left / right, kind, span),
        Remainder => in_range(left % right, kind, span),
        _ => comparison(operator, &left, &right, span),
    }
}

fn checked_integer(
    value: Option<i128>,
    kind: Primitive,
    span: Span,
) -> Result<ConstValue, ConstError> {
    value.map_or(Err(ConstError::Overflow(span, kind.name())), |value| {
        in_range(value, kind, span)
    })
}

fn in_range(value: i128, kind: Primitive, span: Span) -> Result<ConstValue, ConstError> {
    let (minimum, maximum) = kind.integer_range().expect("integer kind");
    if (minimum..=maximum).contains(&value) {
        Ok(ConstValue::Integer(value, kind))
    } else {
        Err(ConstError::Overflow(span, kind.name()))
    }
}

fn float_binary(
    operator: BinaryOperator,
    left: f32,
    right: f32,
    span: Span,
) -> Result<ConstValue, ConstError> {
    use BinaryOperator::{Add, Divide, Multiply, Remainder, Subtract};
    match operator {
        Add => Ok(ConstValue::Float(left + right)),
        Subtract => Ok(ConstValue::Float(left - right)),
        Multiply => Ok(ConstValue::Float(left * right)),
        Divide => Ok(ConstValue::Float(left / right)),
        Remainder => Err(ConstError::NotConstant(span)),
        _ => comparison(operator, &left, &right, span),
    }
}

fn double_binary(
    operator: BinaryOperator,
    left: f64,
    right: f64,
    span: Span,
) -> Result<ConstValue, ConstError> {
    use BinaryOperator::{Add, Divide, Multiply, Remainder, Subtract};
    match operator {
        Add => Ok(ConstValue::Double(left + right)),
        Subtract => Ok(ConstValue::Double(left - right)),
        Multiply => Ok(ConstValue::Double(left * right)),
        Divide => Ok(ConstValue::Double(left / right)),
        Remainder => Err(ConstError::NotConstant(span)),
        _ => comparison(operator, &left, &right, span),
    }
}

fn comparison<T: PartialOrd>(
    operator: BinaryOperator,
    left: &T,
    right: &T,
    span: Span,
) -> Result<ConstValue, ConstError> {
    use BinaryOperator::{Equal, Greater, GreaterEqual, Less, LessEqual, NotEqual};
    let result = match operator {
        Equal => left == right,
        NotEqual => left != right,
        Less => left < right,
        LessEqual => left <= right,
        Greater => left > right,
        GreaterEqual => left >= right,
        _ => return Err(ConstError::NotConstant(span)),
    };
    Ok(ConstValue::Bool(result))
}

/// Explicit casts mirror the JIT: truncating two's-complement integer
/// narrowing, saturating float-to-integer conversion, and NaN to zero.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]
fn cast(target: &TypeRef, value: ConstValue, span: Span) -> Result<ConstValue, ConstError> {
    use ConstValue::{Char, Double, Float, Integer};
    let Some(kind) = primitives::from_name(&target.name) else {
        return Err(ConstError::NotConstant(span));
    };
    Ok(match (value, kind) {
        (Integer(value, _), _) if kind.is_integer() => {
            ConstValue::Integer(truncate(value, kind), kind)
        }
        (Integer(value, _), Primitive::Float) => Float(value as f32),
        (Integer(value, _), Primitive::Double) => Double(value as f64),
        (Integer(value, _), Primitive::Char) => {
            Char(char::from_u32(truncate(value, Primitive::UInt) as u32).unwrap_or('\u{FFFD}'))
        }
        (Char(value), _) if kind.is_integer() => {
            ConstValue::Integer(truncate(i128::from(u32::from(value)), kind), kind)
        }
        (Float(value), _) if kind.is_integer() => {
            ConstValue::Integer(saturate(f64::from(value), kind), kind)
        }
        (Double(value), _) if kind.is_integer() => ConstValue::Integer(saturate(value, kind), kind),
        (Float(value), Primitive::Double) => Double(f64::from(value)),
        (Double(value), Primitive::Float) => Float(value as f32),
        (Float(value), Primitive::Float) => Float(value),
        (Double(value), Primitive::Double) => Double(value),
        _ => return Err(ConstError::NotConstant(span)),
    })
}

/// Two's-complement truncation of an integer value into a narrower kind.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]
fn truncate(value: i128, kind: Primitive) -> i128 {
    match kind {
        Primitive::SByte => i128::from(value as i8),
        Primitive::Byte => i128::from(value as u8),
        Primitive::Short => i128::from(value as i16),
        Primitive::UShort => i128::from(value as u16),
        Primitive::Int => i128::from(value as i32),
        Primitive::UInt => i128::from(value as u32),
        Primitive::Long => i128::from(value as i64),
        Primitive::ULong => i128::from(value as u64),
        _ => value,
    }
}

/// Saturating float-to-integer conversion; NaN becomes zero.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn saturate(value: f64, kind: Primitive) -> i128 {
    let (minimum, maximum) = kind.integer_range().expect("integer kind");
    if value.is_nan() {
        return 0;
    }
    (value as i128).clamp(minimum, maximum)
}

#[cfg(test)]
mod tests {
    use crate::primitives::Primitive;
    use aster_syntax::{lex, parse};

    use super::{ConstError, ConstValue, evaluate};

    fn evaluate_source(source: &str) -> Result<ConstValue, ConstError> {
        let module =
            parse(lex(&format!("const int Value = {source};")).expect("lexes")).expect("parses");
        let aster_syntax::Item::Variable(variable) = &module.items[0] else {
            panic!("expected a constant");
        };
        evaluate(variable.initializer.as_ref().expect("initializer"), &|_| {
            None
        })
    }

    fn integer(value: i128, kind: Primitive) -> ConstValue {
        ConstValue::Integer(value, kind)
    }

    #[test]
    fn folds_arithmetic_and_precedence() {
        assert_eq!(evaluate_source("1 + 2 * 3"), Ok(integer(7, Primitive::Int)));
        assert_eq!(
            evaluate_source("(10 - 4) / 3"),
            Ok(integer(2, Primitive::Int))
        );
        assert_eq!(evaluate_source("7 % 4"), Ok(integer(3, Primitive::Int)));
    }

    #[test]
    fn folds_conditionals_and_logic() {
        assert_eq!(
            evaluate_source("true && 2 > 1 ? 10 : 20"),
            Ok(integer(10, Primitive::Int))
        );
        assert_eq!(
            evaluate_source("false && 1 / 0 == 0"),
            Ok(ConstValue::Bool(false))
        );
    }

    #[test]
    fn reports_overflow_and_division_by_zero() {
        assert!(matches!(
            evaluate_source("2147483647 + 1"),
            Err(ConstError::Overflow(_, "int"))
        ));
        assert!(matches!(
            evaluate_source("1 / 0"),
            Err(ConstError::DivisionByZero(_))
        ));
    }

    #[test]
    fn promotes_mixed_and_unsigned_operands() {
        assert_eq!(
            evaluate_source("2147483647L + 1"),
            Ok(integer(2_147_483_648, Primitive::Long))
        );
        assert_eq!(
            evaluate_source("10u + 5u"),
            Ok(integer(15, Primitive::UInt))
        );
        assert!(matches!(
            evaluate_source("10ul * 3"),
            Err(ConstError::NotConstant(_))
        ));
        assert_eq!(evaluate_source("10u + 5"), Ok(integer(15, Primitive::Long)));
        assert!(matches!(
            evaluate_source("4294967295u + 1u"),
            Err(ConstError::Overflow(_, "uint"))
        ));
    }

    #[test]
    fn casts_and_decimal_literals() {
        assert_eq!(
            evaluate_source("(byte)300"),
            Ok(integer(44, Primitive::Byte))
        );
        assert_eq!(
            evaluate_source("1.5m"),
            Ok(ConstValue::Decimal("1.5".to_owned()))
        );
        assert!(matches!(
            evaluate_source("1m + 2m"),
            Err(ConstError::NotConstant(_))
        ));
    }

    #[test]
    fn rejects_non_constant_expressions() {
        assert!(matches!(
            evaluate_source("Compute()"),
            Err(ConstError::NotConstant(_))
        ));
    }
}
