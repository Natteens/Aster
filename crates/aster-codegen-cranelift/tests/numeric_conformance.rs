//! Native execution evidence for the numeric rules in
//! `docs/specification/02-types.md`. Expected values are computed from the
//! specification text (two's-complement wrapping at the promoted width,
//! IEEE 754 semantics) rather than from the compiler.

use aster_codegen_cranelift::{ExecutionValue, execute, validate};
use aster_compiler::compile;

fn run_all(cases: &[(String, ExecutionValue)]) {
    let mut source = String::new();
    for (index, (body, _)) in cases.iter().enumerate() {
        source.push_str(&body.replace('@', &format!("C{index}")));
        source.push('\n');
    }
    let compilation = compile(&source)
        .unwrap_or_else(|diagnostics| panic!("valid source: {diagnostics:#?}\n{source}"));
    for (index, (body, expected)) in cases.iter().enumerate() {
        assert_eq!(
            execute(&compilation.mir, &format!("C{index}")),
            Ok(expected.clone()),
            "{body}"
        );
    }
}

fn run_error(source: &str, function: &str) -> String {
    let compilation = compile(source)
        .unwrap_or_else(|diagnostics| panic!("valid source: {diagnostics:#?}\n{source}"));
    execute(&compilation.mir, function)
        .expect_err("execution should fail with a controlled runtime error")
        .to_string()
}

struct Integer {
    name: &'static str,
    bits: u32,
    signed: bool,
    min: &'static str,
    max: &'static str,
}

const INTEGERS: [Integer; 8] = [
    Integer {
        name: "sbyte",
        bits: 8,
        signed: true,
        min: "-128",
        max: "127",
    },
    Integer {
        name: "byte",
        bits: 8,
        signed: false,
        min: "0",
        max: "255",
    },
    Integer {
        name: "short",
        bits: 16,
        signed: true,
        min: "-32768",
        max: "32767",
    },
    Integer {
        name: "ushort",
        bits: 16,
        signed: false,
        min: "0",
        max: "65535",
    },
    Integer {
        name: "int",
        bits: 32,
        signed: true,
        min: "-2147483647 - 1",
        max: "2147483647",
    },
    Integer {
        name: "uint",
        bits: 32,
        signed: false,
        min: "0U",
        max: "4294967295U",
    },
    Integer {
        name: "long",
        bits: 64,
        signed: true,
        min: "-9223372036854775807L - 1L",
        max: "9223372036854775807L",
    },
    Integer {
        name: "ulong",
        bits: 64,
        signed: false,
        min: "0UL",
        max: "18446744073709551615UL",
    },
];

impl Integer {
    fn range(&self) -> (i128, i128) {
        if self.signed {
            (
                -(1_i128 << (self.bits - 1)),
                (1_i128 << (self.bits - 1)) - 1,
            )
        } else {
            (0, (1_i128 << self.bits) - 1)
        }
    }

    /// The specification's operation type for two equal operands.
    fn operation_type(&self) -> &'static str {
        if self.bits < 32 { "int" } else { self.name }
    }
}

fn wrap(value: i128, name: &str) -> i128 {
    let integer = INTEGERS
        .iter()
        .find(|integer| integer.name == name)
        .expect("integer type");
    let modulus = 1_i128 << integer.bits;
    let mut wrapped = value.rem_euclid(modulus);
    if integer.signed && wrapped >= modulus / 2 {
        wrapped -= modulus;
    }
    wrapped
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn value(name: &str, number: i128) -> ExecutionValue {
    match name {
        "sbyte" => ExecutionValue::SByte(number as i8),
        "byte" => ExecutionValue::Byte(number as u8),
        "short" => ExecutionValue::Short(number as i16),
        "ushort" => ExecutionValue::UShort(number as u16),
        "int" => ExecutionValue::Int(number as i32),
        "uint" => ExecutionValue::UInt(number as u32),
        "long" => ExecutionValue::Long(number as i64),
        "ulong" => ExecutionValue::ULong(number as u64),
        other => panic!("unexpected type {other}"),
    }
}

fn literal(integer: &Integer, number: i128) -> String {
    // The type minimum cannot be spelled as a negated suffixed literal.
    let (minimum, maximum) = integer.range();
    if number == minimum {
        return integer.min.to_owned();
    }
    if number == maximum {
        return integer.max.to_owned();
    }
    let text = number.to_string();
    match integer.name {
        "uint" => format!("{text}U"),
        "long" => format!("{text}L"),
        "ulong" => format!("{text}UL"),
        _ => text,
    }
}

#[test]
fn equal_operand_arithmetic_wraps_at_the_operation_width_for_every_integer_type() {
    for integer in &INTEGERS {
        let mut cases = Vec::new();
        let (minimum, maximum) = integer.range();
        let operation = integer.operation_type();
        let operands: [(i128, i128); 4] =
            [(maximum, 1), (minimum, 1), (maximum, 2), (maximum, maximum)];
        for (left, right) in operands {
            for (operator, result) in [
                ("+", left.wrapping_add(right)),
                ("-", left.wrapping_sub(right)),
                ("*", left.wrapping_mul(right)),
            ] {
                let body = format!(
                    "public {operation} @() {{ {0} a = {1}; {0} b = {2}; return a {operator} b; }}",
                    integer.name,
                    literal(integer, left),
                    literal(integer, right)
                );
                cases.push((body, value(operation, wrap(result, operation))));
            }
        }
        run_all(&cases);
    }
}

#[test]
fn integer_range_boundaries_round_trip_and_compare_with_the_right_signedness() {
    let mut cases = Vec::new();
    for integer in &INTEGERS {
        let (minimum, maximum) = integer.range();
        for number in [minimum, maximum] {
            cases.push((
                format!(
                    "public {0} @() {{ {0} a = {1}; return a; }}",
                    integer.name,
                    if number == minimum {
                        integer.min
                    } else {
                        integer.max
                    }
                ),
                value(integer.name, number),
            ));
        }
        cases.push((
            format!(
                "public bool @() {{ {0} lo = {1}; {0} hi = {2}; return lo < hi; }}",
                integer.name, integer.min, integer.max
            ),
            ExecutionValue::Bool(true),
        ));
    }
    run_all(&cases);
}

#[test]
fn mixed_operand_promotion_follows_the_specification() {
    let cases: Vec<(&str, ExecutionValue)> = vec![
        // small + small -> int, with sign and zero extension.
        (
            "public int @() { byte a = 200; sbyte b = -100; return a + b; }",
            ExecutionValue::Int(100),
        ),
        (
            "public int @() { short a = 30000; ushort b = 60000; return a + b; }",
            ExecutionValue::Int(90_000),
        ),
        (
            "public int @() { sbyte a = -1; ushort b = 65535; return a + b; }",
            ExecutionValue::Int(65_534),
        ),
        // int + uint -> long
        (
            "public long @() { int a = -1; uint b = 4000000000U; return a + b; }",
            ExecutionValue::Long(3_999_999_999),
        ),
        (
            "public long @() { int a = 2147483647; uint b = 4294967295U; return a * b; }",
            ExecutionValue::Long(2_147_483_647 * 4_294_967_295),
        ),
        // uint + ulong -> ulong (wraps at 64 bits)
        (
            "public ulong @() { uint a = 4294967295U; ulong b = 1UL; return a + b; }",
            ExecutionValue::ULong(4_294_967_296),
        ),
        (
            "public ulong @() { uint a = 1U; ulong b = 18446744073709551615UL; return a + b; }",
            ExecutionValue::ULong(0),
        ),
        // integers with floating point
        (
            "public double @() { int a = 16777217; float b = 0.5f; return a + b; }",
            ExecutionValue::Double(16_777_217.5),
        ),
        (
            "public float @() { short a = 3; float b = 0.5f; return a * b; }",
            ExecutionValue::Float(1.5),
        ),
        (
            "public double @() { uint a = 4294967295U; float b = 0.5f; return a + b; }",
            ExecutionValue::Double(4_294_967_295.5),
        ),
        (
            "public double @() { float a = 0.5f; double b = 0.25d; return a + b; }",
            ExecutionValue::Double(0.75),
        ),
        (
            "public float @() { float a = 0.1f; float b = 0.2f; return a + b; }",
            ExecutionValue::Float(0.1_f32 + 0.2_f32),
        ),
        // comparisons use the promoted type
        (
            "public bool @() { int a = -1; uint b = 1U; return a < b; }",
            ExecutionValue::Bool(true),
        ),
        (
            "public bool @() { int a = -1; uint b = 4294967295U; return a == b; }",
            ExecutionValue::Bool(false),
        ),
        (
            "public bool @() { sbyte a = -1; byte b = 1; return a < b; }",
            ExecutionValue::Bool(true),
        ),
        (
            "public bool @() { byte a = 255; ushort b = 255; return a == b; }",
            ExecutionValue::Bool(true),
        ),
        (
            "public bool @() { uint a = 4000000000U; int b = 5; return a > b; }",
            ExecutionValue::Bool(true),
        ),
        (
            "public bool @() { ulong a = 18446744073709551615UL; ulong b = 1UL; return a > b; }",
            ExecutionValue::Bool(true),
        ),
        (
            "public bool @() { float a = 0.1f; double b = 0.1d; return a == b; }",
            ExecutionValue::Bool(false),
        ),
    ];
    let cases: Vec<_> = cases
        .into_iter()
        .map(|(body, value)| (body.to_owned(), value))
        .collect();
    run_all(&cases);
}

#[test]
fn floating_point_follows_ieee_754_without_runtime_failures() {
    // Division by zero is not a runtime failure for floating point: it yields an
    // infinity or NaN, and NaN is unordered.
    let source = r"
        public bool Infinity() { double z = 0.0d; double inf = 1.0d / z; return inf > 0.0d && inf == inf * 2.0d; }
        public bool NegativeInfinity() { double z = 0.0d; double a = -1.0d; return a / z < 0.0d; }
        public bool NaNIsNotEqualToItself() { double z = 0.0d; double n = z / z; return n == n; }
        public bool NaNIsNotLess() { double z = 0.0d; double n = z / z; return n < 1.0d; }
        public bool NaNIsNotEqualOperator() { double z = 0.0d; double n = z / z; return n != n; }
        public bool FloatInfinity() { float z = 0.0f; float a = 1.0f; return a / z > 1.0f; }
    ";
    let compilation = compile(source).expect("valid source");
    for (function, expected) in [
        ("Infinity", true),
        ("NegativeInfinity", true),
        ("NaNIsNotEqualToItself", false),
        ("NaNIsNotLess", false),
        ("NaNIsNotEqualOperator", true),
        ("FloatInfinity", true),
    ] {
        assert_eq!(
            execute(&compilation.mir, function),
            Ok(ExecutionValue::Bool(expected)),
            "{function}"
        );
    }
}

#[test]
fn division_and_remainder_failures_are_controlled_for_every_integer_type() {
    for integer in &INTEGERS {
        let operation = integer.operation_type();
        for (operator, message) in [
            ("/", "Aster runtime error: integer division by zero"),
            ("%", "Aster runtime error: integer remainder by zero"),
        ] {
            let source = format!(
                "public {operation} Fail() {{ {0} a = {1}; {0} z = {2}; return a {operator} z; }}",
                integer.name,
                literal(integer, 7),
                literal(integer, 0)
            );
            assert_eq!(
                run_error(&source, "Fail"),
                message,
                "{} {operator}",
                integer.name
            );
        }
    }
    // Signed minimum divided by -1 overflows only at 32 and 64 bits; narrower
    // operands are promoted to `int` first and produce a normal result.
    for (type_name, minimum, one, operation) in [
        ("int", "-2147483647 - 1", "-1", "int"),
        ("long", "-9223372036854775807L - 1L", "-1L", "long"),
    ] {
        for (operator, message) in [
            ("/", "Aster runtime error: signed integer division overflow"),
            (
                "%",
                "Aster runtime error: signed integer remainder overflow",
            ),
        ] {
            let source = format!(
                "public {operation} Fail() {{ {type_name} a = {minimum}; {type_name} b = {one}; return a {operator} b; }}"
            );
            assert_eq!(
                run_error(&source, "Fail"),
                message,
                "{type_name} {operator}"
            );
        }
    }
    let cases: Vec<(String, ExecutionValue)> = vec![
        (
            "public int @() { sbyte a = -128; sbyte b = -1; return a / b; }".to_owned(),
            ExecutionValue::Int(128),
        ),
        (
            "public int @() { short a = -32768; short b = -1; return a / b; }".to_owned(),
            ExecutionValue::Int(32_768),
        ),
        (
            "public int @() { sbyte a = -128; sbyte b = -1; return a % b; }".to_owned(),
            ExecutionValue::Int(0),
        ),
        // Signed division truncates toward zero; remainder takes the dividend's sign.
        (
            "public int @() { int a = -7; int b = 2; return a / b; }".to_owned(),
            ExecutionValue::Int(-3),
        ),
        (
            "public int @() { int a = -7; int b = 3; return a % b; }".to_owned(),
            ExecutionValue::Int(-1),
        ),
        // Unsigned division and remainder use unsigned semantics.
        (
            "public uint @() { uint a = 4294967295U; uint b = 10U; return a / b; }".to_owned(),
            ExecutionValue::UInt(429_496_729),
        ),
        (
            "public ulong @() { ulong a = 18446744073709551615UL; ulong b = 10UL; return a % b; }"
                .to_owned(),
            ExecutionValue::ULong(5),
        ),
    ];
    run_all(&cases);
}

#[test]
fn explicit_integer_casts_wrap_and_extend_by_the_source_sign() {
    let cases: Vec<(&str, ExecutionValue)> = vec![
        (
            "public byte @() { int a = 300; return (byte)a; }",
            ExecutionValue::Byte(44),
        ),
        (
            "public sbyte @() { int a = 200; return (sbyte)a; }",
            ExecutionValue::SByte(-56),
        ),
        (
            "public short @() { int a = 70000; return (short)a; }",
            ExecutionValue::Short(4464),
        ),
        (
            "public ushort @() { int a = -1; return (ushort)a; }",
            ExecutionValue::UShort(65_535),
        ),
        (
            "public uint @() { int a = -1; return (uint)a; }",
            ExecutionValue::UInt(u32::MAX),
        ),
        (
            "public int @() { uint a = 4000000000U; return (int)a; }",
            ExecutionValue::Int(-294_967_296),
        ),
        (
            "public long @() { sbyte a = -1; return (long)a; }",
            ExecutionValue::Long(-1),
        ),
        (
            "public long @() { byte a = 255; return (long)a; }",
            ExecutionValue::Long(255),
        ),
        (
            "public ulong @() { int a = -1; return (ulong)a; }",
            ExecutionValue::ULong(u64::MAX),
        ),
        (
            "public ulong @() { uint a = 4294967295U; return (ulong)a; }",
            ExecutionValue::ULong(4_294_967_295),
        ),
        (
            "public uint @() { sbyte a = -1; return (uint)a; }",
            ExecutionValue::UInt(u32::MAX),
        ),
        (
            "public int @() { ushort a = 65535; return (int)a; }",
            ExecutionValue::Int(65_535),
        ),
        (
            "public long @() { ulong a = 18446744073709551615UL; return (long)a; }",
            ExecutionValue::Long(-1),
        ),
        (
            "public int @() { byte b = 200; return (int)(byte)(b + b); }",
            ExecutionValue::Int(144),
        ),
    ];
    let cases: Vec<_> = cases
        .into_iter()
        .map(|(body, value)| (body.to_owned(), value))
        .collect();
    run_all(&cases);
}

#[test]
fn float_to_integer_casts_truncate_saturate_and_map_nan_to_zero() {
    let cases: Vec<(&str, ExecutionValue)> = vec![
        (
            "public int @() { float a = 2.9f; return (int)a; }",
            ExecutionValue::Int(2),
        ),
        (
            "public int @() { float a = -2.9f; return (int)a; }",
            ExecutionValue::Int(-2),
        ),
        (
            "public int @() { double z = 0.0d; double n = z / z; return (int)n; }",
            ExecutionValue::Int(0),
        ),
        (
            "public long @() { double z = 0.0d; double n = z / z; return (long)n; }",
            ExecutionValue::Long(0),
        ),
        (
            "public int @() { double a = 100000000000000000000.0d; return (int)a; }",
            ExecutionValue::Int(i32::MAX),
        ),
        (
            "public int @() { double a = -100000000000000000000.0d; return (int)a; }",
            ExecutionValue::Int(i32::MIN),
        ),
        (
            "public long @() { double a = 100000000000000000000.0d; return (long)a; }",
            ExecutionValue::Long(i64::MAX),
        ),
        (
            "public uint @() { float a = -1.0f; return (uint)a; }",
            ExecutionValue::UInt(0),
        ),
        (
            "public ulong @() { double a = -1.0d; return (ulong)a; }",
            ExecutionValue::ULong(0),
        ),
        (
            "public ulong @() { double a = 100000000000000000000.0d; return (ulong)a; }",
            ExecutionValue::ULong(u64::MAX),
        ),
        // Integer to floating point rounds to nearest when not representable.
        (
            "public float @() { int a = 16777217; return (float)a; }",
            ExecutionValue::Float(16_777_216.0),
        ),
        (
            "public double @() { int a = 2147483647; return (double)a; }",
            ExecutionValue::Double(2_147_483_647.0),
        ),
    ];
    let cases: Vec<_> = cases
        .into_iter()
        .map(|(body, value)| (body.to_owned(), value))
        .collect();
    run_all(&cases);
}

#[test]
fn value_preserving_implicit_conversions_keep_the_value() {
    let cases: Vec<(&str, ExecutionValue)> = vec![
        (
            "public int @() { sbyte a = -5; int b = a; return b; }",
            ExecutionValue::Int(-5),
        ),
        (
            "public int @() { byte a = 255; int b = a; return b; }",
            ExecutionValue::Int(255),
        ),
        (
            "public short @() { byte a = 255; short b = a; return b; }",
            ExecutionValue::Short(255),
        ),
        (
            "public uint @() { ushort a = 65535; uint b = a; return b; }",
            ExecutionValue::UInt(65_535),
        ),
        (
            "public long @() { uint a = 4294967295U; long b = a; return b; }",
            ExecutionValue::Long(4_294_967_295),
        ),
        (
            "public float @() { short a = -300; float b = a; return b; }",
            ExecutionValue::Float(-300.0),
        ),
        (
            "public double @() { uint a = 4294967295U; double b = a; return b; }",
            ExecutionValue::Double(4_294_967_295.0),
        ),
        (
            "public double @() { float a = 0.5f; double b = a; return b; }",
            ExecutionValue::Double(0.5),
        ),
    ];
    let cases: Vec<_> = cases
        .into_iter()
        .map(|(body, value)| (body.to_owned(), value))
        .collect();
    run_all(&cases);
}

#[test]
fn negation_and_increment_keep_the_operand_width() {
    let cases: Vec<(&str, ExecutionValue)> = vec![
        (
            "public sbyte @() { sbyte a = -128; return -a; }",
            ExecutionValue::SByte(-128),
        ),
        (
            "public short @() { short a = 7; return -a; }",
            ExecutionValue::Short(-7),
        ),
        (
            "public int @() { int a = -2147483647 - 1; return -a; }",
            ExecutionValue::Int(i32::MIN),
        ),
        (
            "public byte @() { byte a = 255; a++; return a; }",
            ExecutionValue::Byte(0),
        ),
        (
            "public sbyte @() { sbyte a = 127; ++a; return a; }",
            ExecutionValue::SByte(-128),
        ),
        (
            "public ushort @() { ushort a = 0; a--; return a; }",
            ExecutionValue::UShort(65_535),
        ),
        (
            "public byte @() { byte a = 255; byte b = a++; return b; }",
            ExecutionValue::Byte(255),
        ),
    ];
    let cases: Vec<_> = cases
        .into_iter()
        .map(|(body, value)| (body.to_owned(), value))
        .collect();
    run_all(&cases);
}

#[test]
fn nonnumeric_binary_operations_are_unaffected() {
    let cases: Vec<(&str, ExecutionValue)> = vec![
        (
            "public string @() { string a = \"ab\"; string b = \"cd\"; return a + b; }",
            ExecutionValue::String("abcd".to_owned()),
        ),
        (
            "public bool @() { string a = \"x\"; string b = \"x\"; return a == b; }",
            ExecutionValue::Bool(true),
        ),
        (
            "public bool @() { bool a = true; bool b = false; return a && !b; }",
            ExecutionValue::Bool(true),
        ),
        (
            "public bool @() { bool a = false; bool b = true; return a || b; }",
            ExecutionValue::Bool(true),
        ),
        (
            "public bool @() { char a = 'a'; char b = 'b'; return a == b; }",
            ExecutionValue::Bool(false),
        ),
    ];
    let cases: Vec<_> = cases
        .into_iter()
        .map(|(body, value)| (body.to_owned(), value))
        .collect();
    run_all(&cases);
}

#[test]
fn floating_point_remainder_is_rejected_before_execution() {
    for (parameters, left, right) in [
        ("float a, float b", "1.5f", "0.5f"),
        ("int a, double b", "3", "0.5d"),
    ] {
        let source = format!(
            "public double R({parameters}) {{ return a % b; }} public double Main() {{ return R({left}, {right}); }}"
        );
        let compilation = compile(&source).expect("semantic analysis accepts the operation");
        let error = validate(&compilation.mir).expect_err("the backend rejects floating remainder");
        assert!(
            error.to_string().contains("floating-point remainder"),
            "{error}"
        );
    }
}
