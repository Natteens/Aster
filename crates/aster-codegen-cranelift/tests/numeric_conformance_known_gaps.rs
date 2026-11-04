//! Reproducers for confirmed defects that are intentionally not fixed in the
//! test-only conformance change. Each test is `#[ignore]`d with the reason and
//! passes once the corresponding correction lands; remove the attribute with
//! that correction.

use aster_codegen_cranelift::{ExecutionValue, execute};
use aster_compiler::compile;

/// `docs/specification/02-types.md` ("Explicit casts"): float-to-integer casts
/// truncate toward zero, saturate at the target range, and convert NaN to
/// zero. Casting `float` or `double` directly to `sbyte`, `byte`, `short`, or
/// `ushort` instead aborts the process: Cranelift's x64 backend panics with
/// "internal error: entered unreachable code" while emitting the saturating
/// conversion (`fcvt_to_sint_sat`/`fcvt_to_uint_sat` with an 8- or 16-bit
/// result type in `crates/aster-codegen-cranelift/src/values.rs`). The
/// 32- and 64-bit targets work.
#[test]
#[ignore = "known defect: float/double to sbyte, byte, short, ushort casts panic inside Cranelift"]
fn float_to_narrow_integer_casts_saturate() {
    for (source, expected) in [
        (
            "public byte Cast() { float a = 300.7f; return (byte)a; }",
            ExecutionValue::Byte(255),
        ),
        (
            "public byte Cast() { float a = -5.5f; return (byte)a; }",
            ExecutionValue::Byte(0),
        ),
        (
            "public sbyte Cast() { double a = -200.0d; return (sbyte)a; }",
            ExecutionValue::SByte(-128),
        ),
        (
            "public short Cast() { float a = 70000.0f; return (short)a; }",
            ExecutionValue::Short(32_767),
        ),
        (
            "public ushort Cast() { double a = 70000.0d; return (ushort)a; }",
            ExecutionValue::UShort(65_535),
        ),
        (
            "public ushort Cast() { double z = 0.0d; double n = z / z; return (ushort)n; }",
            ExecutionValue::UShort(0),
        ),
    ] {
        let compilation = compile(source).expect("valid source");
        assert_eq!(execute(&compilation.mir, "Cast"), Ok(expected), "{source}");
    }
}
