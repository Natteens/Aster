//! `Random.NextFloat(min, max)` / `Random.NextDouble(min, max)`: half-open
//! intervals, boundary behaviour, controlled errors, and unchanged sequences.

use std::sync::atomic::{AtomicU64, Ordering};

use aster_codegen_cranelift::{ExecutionValue, execute};
use aster_compiler::{ProjectCompilation, compile_project};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

/// ASTER has no exponent literals, so extreme magnitudes are spelled in full
/// decimal; Rust's shortest round-trip formatting parses back to the same value.
fn expand(source: &str) -> String {
    source
        .replace("FMAX", &format!("{}.0f", f32::MAX))
        .replace("DMAX", &format!("{}.0d", f64::MAX))
        .replace("DTINY", &format!("{}d", 5e-324_f64))
        .replace("DSMALL", &format!("{}d", 1e-300_f64))
}

fn project(source: &str) -> ProjectCompilation {
    let source = expand(source);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "aster-floatrange-{}-{id}.aster",
        std::process::id()
    ));
    std::fs::write(&path, &source).expect("write temporary project source");
    let compilation = compile_project(&path);
    std::fs::remove_file(&path).expect("remove temporary project source");
    compilation.unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"))
}

fn run(compilation: &ProjectCompilation, function: &str) -> ExecutionValue {
    execute(&compilation.compilation.mir, function).unwrap_or_else(|error| panic!("{error}"))
}

fn assert_all_zero(compilation: &ProjectCompilation, functions: &[&str]) {
    for function in functions {
        assert_eq!(
            run(compilation, function),
            ExecutionValue::Int(0),
            "{function}"
        );
    }
}

#[test]
fn ordinary_intervals_stay_half_open() {
    let compilation = project(
        "using aster.random;
         public int FloatIn(float low, float high, ulong seed)
         {
             Random random = new Random(seed);
             for (int i = 0; i < 20000; i++)
             {
                 float value = random.NextFloat(low, high);
                 if (value < low || value >= high) { return 1; }
             }
             return 0;
         }
         public int DoubleIn(double low, double high, ulong seed)
         {
             Random random = new Random(seed);
             for (int i = 0; i < 20000; i++)
             {
                 double value = random.NextDouble(low, high);
                 if (value < low || value >= high) { return 1; }
             }
             return 0;
         }
         public int Positive() { return FloatIn(5.0f, 8.0f, 1UL) + DoubleIn(0.5d, 2.0d, 1UL); }
         public int Negative() { return FloatIn(-8.0f, -5.0f, 2UL) + DoubleIn(-2.0d, -0.5d, 2UL); }
         public int AroundZero() { return FloatIn(-3.0f, 4.0f, 3UL) + DoubleIn(-1.5d, 0.25d, 3UL); }
         public int Narrow()
         {
             return FloatIn(100.0f, 100.001f, 4UL) + DoubleIn(100.0d, 100.000000001d, 4UL)
                 + FloatIn(-0.000001f, 0.000001f, 5UL) + DoubleIn(-DSMALL, DSMALL, 5UL);
         }
         public int Reaches()
         {
             Random random = new Random(9UL);
             int below = 0;
             int above = 0;
             for (int i = 0; i < 2000; i++)
             {
                 double value = random.NextDouble(-1.0d, 1.0d);
                 if (value < 0.0d) { below++; } else { above++; }
             }
             if (below < 800 || above < 800) { return 1; }
             return 0;
         }",
    );
    assert_all_zero(
        &compilation,
        &["Positive", "Negative", "AroundZero", "Narrow", "Reaches"],
    );
}

#[test]
fn adjacent_representable_bounds_return_the_only_member() {
    // [x, next(x)) contains exactly x, so every draw must be x.
    let compilation = project(
        "using aster.random;
         public int Adjacent()
         {
             Random random = new Random(11UL);
             for (int i = 0; i < 5000; i++)
             {
                 if (random.NextFloat(1.0f, 1.0000001f) != 1.0f) { return 1; }
                 if (random.NextDouble(1.0d, 1.0000000000000002d) != 1.0d) { return 2; }
                 if (random.NextFloat(-1.0000001f, -1.0f) != -1.0000001f) { return 3; }
                 if (random.NextDouble(-1.0000000000000002d, -1.0d) != -1.0000000000000002d) { return 4; }
             }
             return 0;
         }
         public int ThreeValues()
         {
             Random random = new Random(12UL);
             double high = 1.0000000000000007d;
             for (int i = 0; i < 20000; i++)
             {
                 double value = random.NextDouble(1.0d, high);
                 if (value < 1.0d || value >= high) { return 1; }
             }
             return 0;
         }",
    );
    assert_all_zero(&compilation, &["Adjacent", "ThreeValues"]);
}

#[test]
fn extreme_magnitudes_never_overflow_or_reach_the_bound() {
    let compilation = project(
        "using aster.random;
         using aster.math;
         public int Wide()
         {
             float fmax = FMAX;
             double dmax = DMAX;
             Random random = new Random(21UL);
             for (int i = 0; i < 20000; i++)
             {
                 float a = random.NextFloat(-fmax, fmax);
                 if (!Math.IsFinite(a) || a < -fmax || a >= fmax) { return 1; }
                 float b = random.NextFloat(0.0f, fmax);
                 if (!Math.IsFinite(b) || b < 0.0f || b >= fmax) { return 2; }
                 float c = random.NextFloat(-fmax, 0.0f);
                 if (!Math.IsFinite(c) || c < -fmax || c >= 0.0f) { return 3; }
                 double d = random.NextDouble(-dmax, dmax);
                 if (!Math.IsFinite(d) || d < -dmax || d >= dmax) { return 4; }
                 double e = random.NextDouble(0.0d, dmax);
                 if (!Math.IsFinite(e) || e < 0.0d || e >= dmax) { return 5; }
                 double f = random.NextDouble(-dmax, 0.0d);
                 if (!Math.IsFinite(f) || f < -dmax || f >= 0.0d) { return 6; }
                 double g = random.NextDouble(-dmax, -dmax / 2.0d);
                 if (!Math.IsFinite(g) || g < -dmax || g >= -dmax / 2.0d) { return 7; }
             }
             return 0;
         }
         public int OverflowingSpanUsesBothSigns()
         {
             double dmax = DMAX;
             Random random = new Random(22UL);
             int below = 0;
             int above = 0;
             for (int i = 0; i < 2000; i++)
             {
                 double value = random.NextDouble(-dmax, dmax);
                 if (value < 0.0d) { below++; } else { above++; }
             }
             if (below < 800 || above < 800) { return 1; }
             return 0;
         }
         public int Tiny()
         {
             double smallest = DTINY;
             Random random = new Random(23UL);
             for (int i = 0; i < 5000; i++)
             {
                 double value = random.NextDouble(0.0d, smallest);
                 if (value != 0.0d) { return 1; }
                 double near = random.NextDouble(-smallest, smallest);
                 if (near < -smallest || near >= smallest) { return 2; }
             }
             return 0;
         }",
    );
    assert_all_zero(
        &compilation,
        &["Wide", "OverflowingSpanUsesBothSigns", "Tiny"],
    );
}

#[test]
fn invalid_bounds_are_controlled_runtime_errors() {
    let compilation = project(
        "using aster.random;
         public float NanLow() { Random r = new Random(1UL); return r.NextFloat(0.0f / 0.0f, 1.0f); }
         public float NanHigh() { Random r = new Random(1UL); return r.NextFloat(0.0f, 0.0f / 0.0f); }
         public float InfHigh() { Random r = new Random(1UL); return r.NextFloat(0.0f, 1.0f / 0.0f); }
         public float InfLow() { Random r = new Random(1UL); return r.NextFloat(-1.0f / 0.0f, 0.0f); }
         public float FloatEqual() { Random r = new Random(1UL); return r.NextFloat(2.0f, 2.0f); }
         public float FloatReversed() { Random r = new Random(1UL); return r.NextFloat(3.0f, 2.0f); }
         public double DNanLow() { Random r = new Random(1UL); return r.NextDouble(0.0d / 0.0d, 1.0d); }
         public double DNanHigh() { Random r = new Random(1UL); return r.NextDouble(0.0d, 0.0d / 0.0d); }
         public double DInfHigh() { Random r = new Random(1UL); return r.NextDouble(0.0d, 1.0d / 0.0d); }
         public double DInfLow() { Random r = new Random(1UL); return r.NextDouble(-1.0d / 0.0d, 0.0d); }
         public double DEqual() { Random r = new Random(1UL); return r.NextDouble(2.0d, 2.0d); }
         public double DReversed() { Random r = new Random(1UL); return r.NextDouble(3.0d, 2.0d); }
         public double DZeroes() { Random r = new Random(1UL); return r.NextDouble(0.0d, -0.0d); }",
    );
    for function in [
        "NanLow",
        "NanHigh",
        "InfHigh",
        "InfLow",
        "FloatEqual",
        "FloatReversed",
        "DNanLow",
        "DNanHigh",
        "DInfHigh",
        "DInfLow",
        "DEqual",
        "DReversed",
        "DZeroes",
    ] {
        let error = execute(&compilation.compilation.mir, function)
            .expect_err("an invalid range is a controlled runtime error");
        assert!(
            error.to_string().contains("Aster runtime error"),
            "{function}: {error}"
        );
    }
}

#[test]
fn seeded_sequences_are_reproducible_and_existing_methods_are_unchanged() {
    let compilation = project(
        "using aster.random;
         public int Reproducible()
         {
             Random a = new Random(77UL);
             Random b = new Random(77UL);
             for (int i = 0; i < 1000; i++)
             {
                 if (a.NextFloat(-3.0f, 9.0f) != b.NextFloat(-3.0f, 9.0f)) { return 1; }
                 if (a.NextDouble(-1.0d, 5.0d) != b.NextDouble(-1.0d, 5.0d)) { return 2; }
             }
             return 0;
         }
         public int UnitRangeMatchesTheZeroArgumentMethods()
         {
             Random ranged = new Random(5UL);
             Random plain = new Random(5UL);
             for (int i = 0; i < 1000; i++)
             {
                 if (ranged.NextFloat(0.0f, 1.0f) != plain.NextFloat()) { return 1; }
                 if (ranged.NextDouble(0.0d, 1.0d) != plain.NextDouble()) { return 2; }
             }
             return 0;
         }
         public int OneSampleIsConsumedPerCall()
         {
             Random ranged = new Random(6UL);
             Random plain = new Random(6UL);
             ranged.NextFloat(10.0f, 20.0f);
             plain.NextUInt();
             if (ranged.NextULong() != plain.NextULong()) { return 1; }
             ranged.NextDouble(10.0d, 20.0d);
             plain.NextULong();
             if (ranged.NextULong() != plain.NextULong()) { return 2; }
             return 0;
         }
         public int ZeroArgumentFormulaIsUnchanged()
         {
             Random actual = new Random(123UL);
             Random source = new Random(123UL);
             for (int i = 0; i < 1000; i++)
             {
                 float expected = (float)(source.NextUInt() / 256u) / 16777216.0f;
                 if (actual.NextFloat() != expected) { return 1; }
                 double wide = (double)(source.NextULong() / 2048UL) / 9007199254740992.0d;
                 if (actual.NextDouble() != wide) { return 2; }
             }
             Random fixedSeed = new Random(123UL);
             if (fixedSeed.NextULong() != 13032462758197477675UL) { return 3; }
             return 0;
         }
         public int ScaledValuesFollowTheDocumentedFormula()
         {
             Random ranged = new Random(31UL);
             Random plain = new Random(31UL);
             for (int i = 0; i < 1000; i++)
             {
                 double unit = (double)(plain.NextULong() / 2048UL) / 9007199254740992.0d;
                 if (ranged.NextDouble(2.0d, 6.0d) != 2.0d + unit * 4.0d) { return 1; }
             }
             return 0;
         }",
    );
    assert_all_zero(
        &compilation,
        &[
            "Reproducible",
            "UnitRangeMatchesTheZeroArgumentMethods",
            "OneSampleIsConsumedPerCall",
            "ZeroArgumentFormulaIsUnchanged",
            "ScaledValuesFollowTheDocumentedFormula",
        ],
    );
}
