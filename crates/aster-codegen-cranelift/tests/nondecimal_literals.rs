//! Native execution of hexadecimal and binary integer literals. Each case is a
//! boolean ASTER expression that must be true; the generated `Check` returns the
//! 1-based index of the first false case (0 when all hold).

use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};

use aster_codegen_cranelift::{ExecutionValue, execute};
use aster_compiler::{compile, compile_project};

fn check_all(cases: &[&str], declarations: &str) {
    let mut source = String::from(declarations);
    source.push_str("\npublic uint Id(uint value) { return value; }\npublic int Check()\n{\n");
    for (index, condition) in cases.iter().enumerate() {
        writeln!(
            source,
            "    if (!({condition})) {{ return {}; }}",
            index + 1
        )
        .expect("write");
    }
    source.push_str("    return 0;\n}\n");
    let compilation = compile(&source).unwrap_or_else(|diagnostics| {
        panic!("generated source must compile: {diagnostics:#?}\n{source}")
    });
    match execute(&compilation.mir, "Check") {
        Ok(ExecutionValue::Int(0)) => {}
        Ok(ExecutionValue::Int(index)) => {
            let index = usize::try_from(index).expect("positive");
            panic!("false: {}", cases[index - 1]);
        }
        other => panic!("unexpected result {other:?}"),
    }
}

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

fn run_project(source: &str, function: &str) -> ExecutionValue {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("aster-literals-{}-{id}.aster", std::process::id()));
    std::fs::write(&path, source).expect("write temporary project source");
    let compilation = compile_project(&path);
    std::fs::remove_file(&path).expect("remove temporary project source");
    let compilation = compilation.unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"));
    execute(&compilation.compilation.mir, function).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn every_boundary_equals_its_decimal_and_binary_spelling() {
    check_all(
        &[
            "0xFF == 255",
            "0XFF == 255",
            "0xff == 255",
            "0b11111111 == 255",
            "0B1010 == 10",
            "0x0 == 0",
            "0b0 == 0",
            "0x7FFFFFFF == 2147483647",
            "0b1111111111111111111111111111111 == 2147483647",
            "0x80000000 == 2147483648U",
            "0b10000000000000000000000000000000 == 2147483648U",
            "0xFFFFFFFF == 4294967295U",
            "0b11111111111111111111111111111111 == 4294967295U",
            "0x100000000 == 4294967296L",
            "0b100000000000000000000000000000000 == 4294967296L",
            "0x7FFFFFFFFFFFFFFF == 9223372036854775807L",
            "0x8000000000000000 == 9223372036854775808UL",
            "0xFFFFFFFFFFFFFFFF == 18446744073709551615UL",
            "0b1111111111111111111111111111111111111111111111111111111111111111 == 18446744073709551615UL",
            "0xFFU == 255U",
            "0x100000000U == 4294967296UL",
            "0xFFL == 255L",
            "0xFFUL == 255UL",
            "0xFFLU == 255UL",
            "0xFFuL == 255UL",
            "0b101ul == 5UL",
            "Id(0x80000000) == 0x80000000",
            "Id(0xFFFFFFFF) == 4294967295U",
        ],
        "",
    );
}

#[test]
fn explicit_casts_reinterpret_bit_patterns() {
    check_all(
        &[
            "(int)0x80000000 == -2147483647 - 1",
            "(int)0xFFFFFFFF == -1",
            "(int)0x7FFFFFFF == 2147483647",
            "(long)0x8000000000000000 == -9223372036854775807L - 1L",
            "(long)0xFFFFFFFFFFFFFFFF == -1L",
            "(sbyte)0xFF == -1",
            "(sbyte)0x80 == -128",
            "(short)0xFFFF == -1",
            "(short)0x8000 == -32768",
            "(byte)0x1FF == 255",
            "(ushort)0x1FFFF == 65535",
            "(uint)0x100000001 == 1U",
            "(byte)0b101111111 == 127",
        ],
        "",
    );
}

#[test]
fn literals_compose_with_bitwise_and_shift_operators() {
    check_all(
        &[
            "(0x0F | 0b1010) == 15",
            "(0xFF & 0x0F) == 15",
            "(0xF0 ^ 0xFF) == 15",
            "(0b1100 & 0b1010) == 8",
            "(0x1 << 31) == -2147483647 - 1",
            "(0x1 << 4) == 0x10",
            "(0x80000000 >> 31) == 1U",
            "(0x80000000 >>> 31) == 1U",
            "(0xFFFFFFFF >> 28) == 15U",
            "(~0xFFFFFFFF) == 0U",
            "(~0x0) == -1",
            "(~0xFF) == -256",
            "(0xFFFFFFFFFFFFFFFF >> 60) == 15UL",
            "(0xFFFFFFFFFFFFFFFF ^ 0xFFUL) == 18446744073709551360UL",
            "(0x8000000000000000 >> 63) == 1UL",
            // `uint` with `int` promotes to `long`; a `U` suffix keeps `uint`.
            "(0xFFFFFFFF | 1) == 4294967295L",
            "(0xFFFFFFFF | 1U) == 4294967295U",
            "(0xDEADBEEF & 0xFFFF0000U) == 0xDEAD0000",
            "((0xDEADBEEF >> 16) & 0xFFFFU) == 0xDEADU",
            "0xFF.ToString() == \"255\"",
            "0xFFFFFFFFU.ToString() == \"4294967295\"",
            "0b101.ToString() == \"5\"",
            "$\"{0xFF}-{0b101}-{0x80000000}\" == \"255-5-2147483648\"",
        ],
        "",
    );
}

#[test]
fn constants_variables_and_compound_assignment_use_the_literals() {
    check_all(
        &[
            "MASK == 4278255360U",
            "(MASK >> 24) == 255U",
            "ALL == 18446744073709551615UL",
            "HIGH == -2147483647 - 1",
            "SHIFTED == 16",
            "COMBINED == 0x1F",
            "Compound() == 0",
        ],
        "public const uint MASK = 0xFF00FF00;
         public const ulong ALL = 0xFFFFFFFFFFFFFFFF;
         public const int HIGH = (int)0x80000000;
         public const int SHIFTED = 0x1 << 4;
         public const int COMBINED = 0b10000 | 0xF;
         public int Compound()
         {
             uint flags = 0;
             flags |= 0xF0U;
             flags |= 0x0FU;
             if (flags != 0xFFU) { return 1; }
             flags &= ~0x0FU;
             if (flags != 0xF0U) { return 2; }
             flags ^= 0xFFU;
             if (flags != 0x0FU) { return 3; }
             ulong wide = 0x1;
             wide <<= 40;
             if (wide != 0x10000000000UL) { return 4; }
             int signed = 0b1010;
             signed ^= 0b0110;
             if (signed != 0b1100) { return 5; }
             return 0;
         }",
    );
}

#[test]
fn hexadecimal_splitmix64_constants_reproduce_the_library_generator() {
    let source = "
        using aster.random;
        public ulong Mix(ulong value)
        {
            value = (value ^ (value >> 30)) * 0xBF58476D1CE4E5B9;
            value = (value ^ (value >> 27)) * 0x94D049BB133111EB;
            return value ^ (value >> 31);
        }
        public int Compare(ulong seed)
        {
            Random random = new Random(seed);
            ulong state = seed;
            for (int i = 0; i < 16; i++)
            {
                state = state + 0x9E3779B97F4A7C15;
                if (random.NextULong() != Mix(state)) { return i + 1; }
            }
            return 0;
        }
        public int Zero() { return Compare(0x0UL); }
        public int One() { return Compare(0b1UL); }
        public int Pattern() { return Compare(0x7BUL); }
        public int Max() { return Compare(0xFFFFFFFFFFFFFFFF); }
    ";
    for function in ["Zero", "One", "Pattern", "Max"] {
        assert_eq!(
            run_project(source, function),
            ExecutionValue::Int(0),
            "{function}"
        );
    }
}
