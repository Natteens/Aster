//! Narrow runtime boundary for math-domain and integer-arithmetic failures.

use crate::{
    ExecutionContext,
    string::{AsterStrHeader, decode_str},
};

/// Record a structured equality assertion failure without exposing a Rust
/// panic or an ungoverned ASTER allocation path.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_assert_equal(
    context: *mut ExecutionContext,
    expected: *const AsterStrHeader,
    actual: *const AsterStrHeader,
) {
    if context.is_null() {
        return;
    }
    // SAFETY: generated code supplies live immutable ASTER strings for the
    // duration of this call. Invalid internal pointers degrade to a controlled
    // message instead of being dereferenced by the host.
    #[allow(unsafe_code)]
    let (expected, actual) = unsafe { (decode_str(expected), decode_str(actual)) };
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    match (expected, actual) {
        (Some(expected), Some(actual)) => context.fail(format!(
            "assertion failed: values are not equal\nexpected: {expected}\nactual:   {actual}"
        )),
        _ => context.fail("assertion failed: values are not equal"),
    }
}

/// Execute one fixed unary floating-point operation exposed by
/// `aster.math.Math`. Operation codes are compiler-private and invalid
/// adulterated values fail closed to NaN.
#[must_use]
pub extern "C" fn aster_rt_math_unary_float(value: f32, operation: i32) -> f32 {
    match operation {
        0 => value.sqrt(),
        1 => value.floor(),
        2 => value.ceil(),
        3 => value.round_ties_even(),
        4 => value.sin(),
        5 => value.cos(),
        6 => value.tan(),
        _ => f32::NAN,
    }
}

/// `double` counterpart of [`aster_rt_math_unary_float`].
#[must_use]
pub extern "C" fn aster_rt_math_unary_double(value: f64, operation: i32) -> f64 {
    match operation {
        0 => value.sqrt(),
        1 => value.floor(),
        2 => value.ceil(),
        3 => value.round_ties_even(),
        4 => value.sin(),
        5 => value.cos(),
        6 => value.tan(),
        _ => f64::NAN,
    }
}

#[must_use]
pub extern "C" fn aster_rt_math_pow_float(value: f32, exponent: f32) -> f32 {
    value.powf(exponent)
}

#[must_use]
pub extern "C" fn aster_rt_math_pow_double(value: f64, exponent: f64) -> f64 {
    value.powf(exponent)
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_math_domain_error(context: *mut ExecutionContext, code: i32) {
    if context.is_null() {
        return;
    }
    // SAFETY: JIT-generated calls receive the live host-owned context as their
    // hidden first argument and cannot retain it after the invocation.
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    let message = match code {
        0 => "Math.Abs cannot represent the magnitude of the minimum int value",
        1 => "Math.Abs cannot represent the magnitude of the minimum long value",
        2 => "Math.Clamp requires min to be less than or equal to max",
        3 => "assertion failed: expected condition to be true",
        4 => "assertion failed: expected condition to be false",
        5 => "assertion failed: values are not equal",
        _ => "unknown runtime error",
    };
    context.fail(message);
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_integer_arithmetic_error(context: *mut ExecutionContext, code: i32) {
    if context.is_null() {
        return;
    }
    // SAFETY: JIT-generated calls receive the live host-owned context as their
    // hidden first argument and cannot retain it after the invocation.
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    let message = match code {
        0 => "integer division by zero",
        1 => "integer remainder by zero",
        2 => "signed integer division overflow",
        3 => "signed integer remainder overflow",
        _ => "unknown integer arithmetic error",
    };
    context.fail(message);
}

#[cfg(test)]
mod tests {
    use super::{
        aster_rt_integer_arithmetic_error, aster_rt_math_domain_error, aster_rt_math_pow_double,
        aster_rt_math_pow_float, aster_rt_math_unary_double, aster_rt_math_unary_float,
    };
    use crate::ExecutionContext;

    #[test]
    fn records_a_controlled_domain_error() {
        let mut context = ExecutionContext::new();
        aster_rt_math_domain_error(&raw mut context, 2);
        assert!(context.take_error().unwrap().contains("min"));
    }

    #[test]
    fn records_controlled_integer_arithmetic_errors() {
        for (code, expected) in [
            (0, "integer division by zero"),
            (1, "integer remainder by zero"),
            (2, "signed integer division overflow"),
            (3, "signed integer remainder overflow"),
        ] {
            let mut context = ExecutionContext::new();
            aster_rt_integer_arithmetic_error(&raw mut context, code);
            assert_eq!(context.take_error().as_deref(), Some(expected));
        }
    }

    #[test]
    #[allow(clippy::cast_possible_truncation, clippy::float_cmp)]
    fn floating_helpers_preserve_ieee_classification_and_ties_to_even() {
        assert_eq!(
            aster_rt_math_unary_double(0.0, 0).to_bits(),
            0.0_f64.to_bits()
        );
        assert_eq!(
            aster_rt_math_unary_double(-0.0, 0).to_bits(),
            (-0.0_f64).to_bits()
        );
        assert_eq!(aster_rt_math_unary_double(f64::INFINITY, 0), f64::INFINITY);
        assert!(aster_rt_math_unary_double(-1.0, 0).is_nan());
        assert!(aster_rt_math_unary_double(f64::NAN, 0).is_nan());

        for (value, expected) in [
            (0.5, 0.0),
            (1.5, 2.0),
            (2.5, 2.0),
            (-0.5, -0.0),
            (-1.5, -2.0),
            (-2.5, -2.0),
        ] {
            let (value, expected): (f64, f64) = (value, expected);
            assert_eq!(
                aster_rt_math_unary_double(value, 3).to_bits(),
                expected.to_bits()
            );
            assert_eq!(
                aster_rt_math_unary_float(value as f32, 3).to_bits(),
                (expected as f32).to_bits()
            );
        }
        for operation in [1, 2, 3] {
            assert!(aster_rt_math_unary_double(f64::NAN, operation).is_nan());
            assert_eq!(
                aster_rt_math_unary_double(f64::INFINITY, operation),
                f64::INFINITY
            );
            assert_eq!(
                aster_rt_math_unary_double(f64::NEG_INFINITY, operation),
                f64::NEG_INFINITY
            );
            assert_eq!(
                aster_rt_math_unary_double(-0.0, operation).to_bits(),
                (-0.0_f64).to_bits()
            );
        }

        assert_eq!(aster_rt_math_pow_double(-2.0, 3.0), -8.0);
        assert!(aster_rt_math_pow_double(-2.0, 0.5).is_nan());
        assert!(aster_rt_math_pow_double(f64::NAN, 2.0).is_nan());
        assert_eq!(aster_rt_math_pow_double(2.0, -3.0), 0.125);
        assert_eq!(aster_rt_math_pow_double(f64::INFINITY, -1.0), 0.0);
        assert_eq!(aster_rt_math_pow_float(-2.0, 3.0), -8.0);

        assert_eq!(
            aster_rt_math_unary_double(0.0, 4).to_bits(),
            0.0_f64.to_bits()
        );
        assert_eq!(
            aster_rt_math_unary_double(-0.0, 6).to_bits(),
            (-0.0_f64).to_bits()
        );
        assert_eq!(aster_rt_math_unary_double(0.0, 5), 1.0);
        assert!(aster_rt_math_unary_double(f64::INFINITY, 4).is_nan());
        assert!(aster_rt_math_unary_double(f64::INFINITY, 5).is_nan());
        assert!(aster_rt_math_unary_double(f64::INFINITY, 6).is_nan());
        assert!((aster_rt_math_unary_double(std::f64::consts::FRAC_PI_2, 4) - 1.0).abs() < 1e-15);
    }
}
