//! Cranelift JIT execution tests for `T-LANG-16` (`Q-LANG-3`): unsuffixed real
//! literals default to `double`, `f`/`F` yields `float`, `d`/`D` yields `double`.

use std::sync::atomic::{AtomicU64, Ordering};

use aster_codegen_cranelift::{ExecutionValue, execute};
use aster_compiler::{compile, compile_project};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

fn run(source: &str, function: &str) -> ExecutionValue {
    let compilation = compile(source)
        .unwrap_or_else(|diagnostics| panic!("expected valid source: {diagnostics:#?}\n{source}"));
    execute(&compilation.mir, function)
        .unwrap_or_else(|error| panic!("JIT execution failed: {error}\n{source}"))
}

fn run_with_stdlib(source: &str, function: &str) -> ExecutionValue {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "aster-lang16-jit-{}-{id}.aster",
        std::process::id()
    ));
    std::fs::write(&path, source).expect("write temporary source");
    let compilation = compile_project(&path)
        .unwrap_or_else(|diagnostics| panic!("expected valid source: {diagnostics:#?}\n{source}"))
        .compilation;
    std::fs::remove_file(&path).expect("remove temporary source");
    execute(&compilation.mir, function)
        .unwrap_or_else(|error| panic!("JIT execution failed: {error}\n{source}"))
}

#[test]
fn executes_unsuffixed_real_literals_as_double() {
    assert_eq!(
        run("public double Run() { var x = 1.5; return x; }", "Run"),
        ExecutionValue::Double(1.5)
    );
    assert_eq!(
        run("public double Run() { var x = 1e9; return x; }", "Run"),
        ExecutionValue::Double(1e9)
    );
    assert_eq!(
        run("public double Run() { var x = 1.5e2; return x; }", "Run"),
        ExecutionValue::Double(150.0)
    );
    assert_eq!(
        run("public double Run() { var x = 1e40; return x; }", "Run"),
        ExecutionValue::Double(1e40)
    );
}

#[test]
fn executes_suffixed_float_and_double_literals_with_expected_precision() {
    assert_eq!(
        run("public float Run() { var x = 1.5f; return x; }", "Run"),
        ExecutionValue::Float(1.5)
    );
    assert_eq!(
        run("public float Run() { var x = 1.5F; return x; }", "Run"),
        ExecutionValue::Float(1.5)
    );
    assert_eq!(
        run("public float Run() { var x = 2f; return x; }", "Run"),
        ExecutionValue::Float(2.0)
    );
    assert_eq!(
        run("public double Run() { var x = 1.5d; return x; }", "Run"),
        ExecutionValue::Double(1.5)
    );
    assert_eq!(
        run("public double Run() { var x = 1.5D; return x; }", "Run"),
        ExecutionValue::Double(1.5)
    );
    assert_eq!(
        run("public double Run() { var x = 2d; return x; }", "Run"),
        ExecutionValue::Double(2.0)
    );
}

#[test]
fn executes_overload_selection_promotion_and_const_double() {
    let overload_source = r"
        public int Pick(float value) { return 1; }
        public int Pick(double value) { return 2; }
        public int RunUnsuffixed() { return Pick(1.5); }
        public int RunFloat() { return Pick(1.5f); }
    ";
    assert_eq!(
        run(overload_source, "RunUnsuffixed"),
        ExecutionValue::Int(2)
    );
    assert_eq!(run(overload_source, "RunFloat"), ExecutionValue::Int(1));

    assert_eq!(
        run("public double Run() { return 1.5f + 2.5; }", "Run"),
        ExecutionValue::Double(4.0)
    );
    assert_eq!(
        run(
            "const double Combined = 1.5 + 1e2; public double Run() { return Combined; }",
            "Run"
        ),
        ExecutionValue::Double(101.5)
    );
    assert_eq!(
        run_with_stdlib(
            "using aster.math; public double Run() { return Math.Abs(-1.5); }",
            "Run"
        ),
        ExecutionValue::Double(1.5)
    );
}

#[test]
fn rejects_unsuffixed_real_literal_assigned_to_float_before_codegen() {
    let diagnostics = compile("public float Run() { float x = 1.5; return x; }")
        .expect_err("unsuffixed real literal must not assign to float");
    assert_eq!(diagnostics[0].code, "A2477");
    assert_eq!(diagnostics[0].help.as_deref(), Some("use `1.5f`"));
}
