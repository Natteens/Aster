//! Regression coverage for float-to-narrow-integer casts, originally recorded
//! here as a known gap. `docs/specification/02-types.md` ("Explicit casts")
//! requires every float-to-integer cast to truncate toward zero, saturate at
//! the target range, and convert NaN to zero. Casting `float` or `double`
//! directly to `sbyte`, `byte`, `short`, or `ushort` used to abort the process
//! because Cranelift's x64 backend cannot emit the saturating conversion with
//! an 8- or 16-bit result type.
//!
//! Rust's own `as` casts have exactly the specified semantics (truncate,
//! saturate, NaN to zero), so they serve as the oracle.

use aster_codegen_cranelift::{ExecutionValue, execute};
use aster_compiler::compile;

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn expected(destination: &str, value: f64, single: bool) -> ExecutionValue {
    macro_rules! cast {
        ($variant:ident, $type:ty) => {
            ExecutionValue::$variant(if single {
                (value as f32) as $type
            } else {
                value as $type
            })
        };
    }
    match destination {
        "sbyte" => cast!(SByte, i8),
        "byte" => cast!(Byte, u8),
        "short" => cast!(Short, i16),
        "ushort" => cast!(UShort, u16),
        "int" => cast!(Int, i32),
        "uint" => cast!(UInt, u32),
        "long" => cast!(Long, i64),
        "ulong" => cast!(ULong, u64),
        other => panic!("unexpected destination {other}"),
    }
}

#[allow(clippy::cast_possible_truncation)]
/// Source text for a finite `value` as a floating literal of the given type.
fn literal(value: f64, single: bool) -> String {
    let text = if single {
        format!("{}", value as f32)
    } else {
        format!("{value}")
    };
    let text = if text.contains('.') {
        text
    } else {
        format!("{text}.0")
    };
    format!("{text}{}", if single { 'f' } else { 'd' })
}

const FINITE: [f64; 40] = [
    0.0,
    0.5,
    -0.5,
    1.0,
    1.9,
    -1.9,
    70.5,
    -70.5,
    127.0,
    127.9,
    128.0,
    -128.0,
    -128.9,
    -129.0,
    255.0,
    255.9,
    256.0,
    -1.0,
    -255.0,
    32_767.0,
    32_767.5,
    32_768.0,
    -32_768.0,
    -32_768.9,
    -32_769.0,
    65_535.0,
    65_535.9,
    65_536.0,
    70_000.0,
    -70_000.0,
    2_147_483_647.0,
    2_147_483_648.0,
    -2_147_483_648.0,
    -2_147_483_649.0,
    4_294_967_295.0,
    4_294_967_296.0,
    10_000_000_000.0,
    -10_000_000_000.0,
    1.0e30,
    -1.0e30,
];

const DESTINATIONS: [&str; 8] = [
    "sbyte", "byte", "short", "ushort", "int", "uint", "long", "ulong",
];

#[test]
fn float_to_narrow_integer_casts_saturate() {
    // `execute` compiles the whole module for every call, so each case is its
    // own single-function module.
    for single in [true, false] {
        let source_type = if single { "float" } else { "double" };
        let suffix = if single { 'f' } else { 'd' };
        for destination in DESTINATIONS {
            let mut cases: Vec<(String, f64)> = FINITE
                .iter()
                .map(|&value| (literal(value, single), value))
                .collect();
            // Negative zero, infinities and NaN cannot be spelled as literals.
            cases.extend([
                (format!("-0.0{suffix}"), -0.0),
                (format!("1.0{suffix} / zero"), f64::INFINITY),
                (format!("-1.0{suffix} / zero"), f64::NEG_INFINITY),
                ("zero / zero".to_owned(), f64::NAN),
            ]);
            for (expression, value) in cases {
                let source = format!(
                    "public {destination} Cast() {{ {source_type} zero = 0.0{suffix}; {source_type} a = {expression}; return ({destination})a; }}"
                );
                let compilation = compile(&source).unwrap_or_else(|diagnostics| {
                    panic!(
                        "valid source: {diagnostics:#?}
{source}"
                    )
                });
                assert_eq!(
                    execute(&compilation.mir, "Cast"),
                    Ok(expected(destination, value, single)),
                    "{source_type} {expression} -> {destination}"
                );
            }
        }
    }
}
