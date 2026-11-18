//! Floating-point `%` and `%=` execute with IEEE `fmod` semantics (C#): the
//! result takes the sign of the dividend, is NaN for a zero divisor or an
//! infinite dividend, and is the dividend for an infinite divisor.

// Results are exact IEEE values; comparisons and narrowing are intentional.
#![allow(clippy::float_cmp, clippy::cast_possible_truncation)]

use aster_codegen_cranelift::{ExecutionValue, execute};
use aster_compiler::compile;

fn run(source: &str) -> ExecutionValue {
    let compilation = compile(source).unwrap_or_else(|d| panic!("{d:#?}\n{source}"));
    execute(&compilation.mir, "Run").unwrap_or_else(|e| panic!("{e}\n{source}"))
}

/// Source text for a value that cannot be constant folded away.
fn literal_double(value: f64) -> String {
    if value.is_nan() {
        "(zero / zero)".to_owned()
    } else if value.is_infinite() {
        let sign = if value < 0.0 { "-" } else { "" };
        format!("({sign}one / zero)")
    } else if value == 0.0 && value.is_sign_negative() {
        "(-zero)".to_owned()
    } else {
        format!("({value:?}d)")
    }
}

fn literal_float(value: f32) -> String {
    format!("((float){})", literal_double(f64::from(value)))
}

const DOUBLE_PRELUDE: &str = "double zero = 0.0; double one = 1.0;";

fn double_remainder(left: f64, right: f64) -> f64 {
    let source = format!(
        "public double Run() {{ {DOUBLE_PRELUDE} double a = {}; double b = {}; return a % b; }}",
        literal_double(left),
        literal_double(right)
    );
    match run(&source) {
        ExecutionValue::Double(value) => value,
        other => panic!("expected double, got {other:?}"),
    }
}

fn float_remainder(left: f32, right: f32) -> f32 {
    let source = format!(
        "public float Run() {{ {DOUBLE_PRELUDE} float a = {}; float b = {}; return a % b; }}",
        literal_float(left),
        literal_float(right)
    );
    match run(&source) {
        ExecutionValue::Float(value) => value,
        other => panic!("expected float, got {other:?}"),
    }
}

fn same_double(actual: f64, expected: f64) -> bool {
    if expected.is_nan() {
        actual.is_nan()
    } else {
        actual.to_bits() == expected.to_bits()
    }
}

const VALUES: [f64; 14] = [
    0.0,
    -0.0,
    1.0,
    -1.0,
    5.5,
    -5.5,
    7.5,
    2.0,
    -2.0,
    0.3,
    1.0e300,
    f64::INFINITY,
    f64::NEG_INFINITY,
    f64::NAN,
];

#[test]
fn double_remainder_matches_rust_fmod_for_all_special_pairs() {
    for left in VALUES {
        for right in VALUES {
            let actual = double_remainder(left, right);
            let expected = left % right;
            assert!(
                same_double(actual, expected),
                "{left:?} % {right:?}: got {actual:?}, expected {expected:?}"
            );
        }
    }
}

#[test]
fn float_remainder_matches_rust_fmod_for_all_special_pairs() {
    for left in VALUES {
        for right in VALUES {
            let (left, right) = (left as f32, right as f32);
            let actual = float_remainder(left, right);
            let expected = left % right;
            let equal = if expected.is_nan() {
                actual.is_nan()
            } else {
                actual.to_bits() == expected.to_bits()
            };
            assert!(
                equal,
                "{left:?} % {right:?}: got {actual:?}, expected {expected:?}"
            );
        }
    }
}

#[test]
fn documented_cases() {
    assert_eq!(double_remainder(7.5, 2.0), 1.5);
    assert_eq!(double_remainder(-7.5, 2.0), -1.5);
    assert_eq!(double_remainder(7.5, -2.0), 1.5);
    assert_eq!(double_remainder(-7.5, -2.0), -1.5);
    assert!(double_remainder(1.0, 0.0).is_nan());
    assert!(double_remainder(f64::INFINITY, 2.0).is_nan());
    assert!(double_remainder(f64::NEG_INFINITY, 2.0).is_nan());
    assert_eq!(double_remainder(3.0, f64::INFINITY), 3.0);
    assert_eq!(double_remainder(-3.0, f64::NEG_INFINITY), -3.0);
    let negative_zero = double_remainder(-0.0, 5.0);
    assert!(negative_zero == 0.0 && negative_zero.is_sign_negative());
    let negative_zero = double_remainder(-4.0, 2.0);
    assert!(negative_zero == 0.0 && negative_zero.is_sign_negative());
    assert_eq!(float_remainder(7.5, 2.0), 1.5);
    assert_eq!(float_remainder(-7.5, 2.0), -1.5);
    assert!(float_remainder(1.0, 0.0).is_nan());
}

#[test]
fn compound_remainder_on_locals() {
    assert_eq!(
        run("public double Run() { double a = -7.5; a %= 2.0; return a; }"),
        ExecutionValue::Double(-7.5 % 2.0)
    );
    assert_eq!(
        run("public float Run() { float a = 7.5f; a %= -2.0f; return a; }"),
        ExecutionValue::Float(7.5f32 % -2.0f32)
    );
}

#[test]
fn compound_remainder_on_fields_and_array_elements() {
    let source = "
        public class Holder { public double d; public float f; }
        public double Run() {
            Holder h = new Holder();
            h.d = 10.25;
            h.f = 10.25f;
            h.d %= 3.0;
            h.f %= 3.0f;
            double[] values = new double[2];
            values[0] = -9.5;
            values[1] = 9.5;
            values[0] %= 4.0;
            values[1] %= -4.0;
            float[] floats = new float[1];
            floats[0] = 5.75f;
            floats[0] %= 2.5f;
            return h.d * 1000.0 + (double)h.f * 100.0 + values[0] * 10.0 + values[1] + (double)floats[0];
        }";
    let expected = (10.25f64 % 3.0) * 1000.0
        + f64::from(10.25f32 % 3.0f32) * 100.0
        + (-9.5f64 % 4.0) * 10.0
        + (9.5f64 % -4.0)
        + f64::from(5.75f32 % 2.5f32);
    assert_eq!(run(source), ExecutionValue::Double(expected));
}

#[test]
fn integer_remainder_is_unchanged() {
    assert_eq!(
        run("public int Run() { int a = -17; return a % 5; }"),
        ExecutionValue::Int(-2)
    );
}
