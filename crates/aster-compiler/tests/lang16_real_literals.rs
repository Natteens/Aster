//! Tests for `T-LANG-16` (`Q-LANG-3`): unsuffixed real literals default to `double`,
//! `f`/`F` yields `float`, `d`/`D` yields `double`, and assigning a `double`
//! real literal where `float` is expected emits a fix-it when it fits finite `f32`.

use std::sync::atomic::{AtomicU64, Ordering};

use aster_compiler::{Compilation, compile, compile_project, hir, mir};
use aster_diagnostics::Diagnostic;

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

fn compile_valid(source: &str) -> Compilation {
    compile(source)
        .unwrap_or_else(|diagnostics| panic!("expected valid source: {diagnostics:#?}\n{source}"))
}

fn compile_errors(source: &str) -> Vec<Diagnostic> {
    compile(source).expect_err("source should be rejected")
}

fn compile_with_stdlib(source: &str) -> Compilation {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "aster-lang16-compiler-{}-{id}.aster",
        std::process::id()
    ));
    std::fs::write(&path, source).expect("write temporary source");
    let result = compile_project(&path);
    std::fs::remove_file(&path).expect("remove temporary source");
    result
        .unwrap_or_else(|diagnostics| panic!("expected valid source: {diagnostics:#?}\n{source}"))
        .compilation
}

fn first_variable_types(compilation: &Compilation, function_name: &str) -> Vec<hir::Type> {
    let hir::Item::Function(function) = compilation
        .hir
        .items
        .iter()
        .find(|item| matches!(item, hir::Item::Function(f) if f.name == function_name))
        .expect("HIR function")
    else {
        unreachable!()
    };
    function
        .body
        .as_ref()
        .expect("function body")
        .statements
        .iter()
        .filter_map(|statement| match statement {
            hir::Statement::Variable(variable) => Some(variable.type_.clone()),
            _ => None,
        })
        .collect()
}

fn first_call_argument_types(compilation: &Compilation, function_name: &str) -> Vec<hir::Type> {
    let hir::Item::Function(function) = compilation
        .hir
        .items
        .iter()
        .find(|item| matches!(item, hir::Item::Function(f) if f.name == function_name))
        .expect("HIR function")
    else {
        unreachable!()
    };
    for statement in &function.body.as_ref().expect("function body").statements {
        if let hir::Statement::Return(Some(expr)) = statement
            && let hir::ExpressionKind::Call { arguments, .. } = &expr.kind
        {
            return arguments.iter().map(|arg| arg.type_.clone()).collect();
        }
    }
    panic!("expected return call in `{function_name}`");
}

#[test]
fn unsuffixed_real_literals_infer_double_across_forms() {
    let compilation = compile_valid(
        r"
        public void Check()
        {
            var a = 1.5;
            var b = 1e9;
            var c = 1.5e2;
            var d = 1_000.5;
            var e = 1e40;
        }
        ",
    );
    assert_eq!(
        first_variable_types(&compilation, "Check"),
        [
            hir::Type::Double,
            hir::Type::Double,
            hir::Type::Double,
            hir::Type::Double,
            hir::Type::Double,
        ]
    );
    let mir_fn = compilation
        .mir
        .functions
        .iter()
        .find(|f| f.name == "Check")
        .expect("MIR function");
    let local_types: Vec<_> = mir_fn
        .locals
        .iter()
        .filter(|l| !l.temporary)
        .map(|l| l.type_.clone())
        .collect();
    assert_eq!(
        local_types,
        [
            mir::Type::Double,
            mir::Type::Double,
            mir::Type::Double,
            mir::Type::Double,
            mir::Type::Double,
        ]
    );
}

#[test]
fn explicit_float_and_double_suffixes_preserve_exact_types() {
    let compilation = compile_valid(
        r"
        public void Check()
        {
            var a = 1.5f;
            var b = 1.5F;
            var c = 1e3f;
            var d = 2f;
            var e = 1.5d;
            var f = 1.5D;
            var g = 1e3d;
            var h = 2d;
        }
        ",
    );
    assert_eq!(
        first_variable_types(&compilation, "Check"),
        [
            hir::Type::Float,
            hir::Type::Float,
            hir::Type::Float,
            hir::Type::Float,
            hir::Type::Double,
            hir::Type::Double,
            hir::Type::Double,
            hir::Type::Double,
        ]
    );
}

#[test]
fn overload_resolution_prefers_double_for_unsuffixed_and_float_for_f_suffix() {
    let compilation = compile_valid(
        r"
        public int Pick(float value) { return 1; }
        public int Pick(double value) { return 2; }
        public int CallUnsuffixed() { return Pick(1.5); }
        public int CallFloat() { return Pick(1.5f); }
        ",
    );
    assert_eq!(
        first_call_argument_types(&compilation, "CallUnsuffixed"),
        [hir::Type::Double]
    );
    assert_eq!(
        first_call_argument_types(&compilation, "CallFloat"),
        [hir::Type::Float]
    );
}

#[test]
fn math_stdlib_overloads_select_double_for_unsuffixed_real_literals() {
    let compilation = compile_with_stdlib(
        r"
        using aster.math;
        public double AbsCase() { return Math.Abs(-1.5); }
        public double SinCase() { return Math.Sin(0.0); }
        public float AbsFloatCase() { return Math.Abs(-1.5f); }
        ",
    );
    assert_eq!(
        first_call_argument_types(&compilation, "AbsCase"),
        [hir::Type::Double]
    );
    assert_eq!(
        first_call_argument_types(&compilation, "SinCase"),
        [hir::Type::Double]
    );
    assert_eq!(
        first_call_argument_types(&compilation, "AbsFloatCase"),
        [hir::Type::Float]
    );
}

#[test]
fn binary_promotion_and_const_double_work_with_unsuffixed_literals() {
    let compilation = compile_valid(
        r"
        const double Combined = 1.5 + 1e2;
        public void Check()
        {
            var promoted = 1.5f + 2.5;
            var folded = Combined;
        }
        ",
    );
    assert_eq!(
        first_variable_types(&compilation, "Check"),
        [hir::Type::Double, hir::Type::Double]
    );
}

#[test]
fn assigning_finite_real_literal_to_float_reports_fixit_diagnostic() {
    for (source, expected_help) in [
        ("public void F() { float x = 1.5; }", "use `1.5f`"),
        ("public void F() { float x = 1e9; }", "use `1e9f`"),
        ("public void F() { float x = -1.5; }", "use `-1.5f`"),
        ("public void F() { float x = 1_000.5; }", "use `1000.5f`"),
        (
            "public void G(float v) {} public void F() { G(1.5); }",
            "use `1.5f`",
        ),
        ("public float F() { return 1.5; }", "use `1.5f`"),
        ("public void F() { float x = 0.0f; x = 1.5; }", "use `1.5f`"),
        (
            "public struct S { public float x; } public void F() { var s = S { x: 1.5 }; }",
            "use `1.5f`",
        ),
        ("public class C { public float x = 1.5; }", "use `1.5f`"),
        ("public void F(float x = 1.5) {}", "use `1.5f`"),
        ("public void F() { const float C = 1.5; }", "use `1.5f`"),
        (
            "public void F() { List<float> xs = new List<float>(); xs.Add(1.5); }",
            "use `1.5f`",
        ),
    ] {
        let diagnostics = compile_errors(source);
        assert_eq!(diagnostics.len(), 1, "expected 1 error for `{source}`");
        assert_eq!(diagnostics[0].code, "A2477", "{source}");
        assert_eq!(
            diagnostics[0].message, "expected `float`, found `double`",
            "{source}"
        );
        assert_eq!(
            diagnostics[0].help.as_deref(),
            Some(expected_help),
            "{source}"
        );
    }
}

#[test]
fn out_of_f32_range_double_literal_does_not_suggest_invalid_float_suffix() {
    for source in [
        "public void F() { float x = 1e40; }",
        "public void F() { float x = -1e40; }",
    ] {
        let diagnostics = compile_errors(source);
        assert_eq!(diagnostics.len(), 1, "{source}");
        assert_eq!(diagnostics[0].code, "A2391", "{source}");
        assert_eq!(
            diagnostics[0].message, "expected `float`, found `double`",
            "{source}"
        );
        let help = diagnostics[0].help.as_deref().unwrap_or("");
        assert!(
            !help.contains("1e40f"),
            "must not suggest invalid float literal in `{help}`"
        );
        assert_eq!(help, "use an expression of type `float`");
    }
}

#[test]
fn overflow_for_explicit_float_and_double_literals_is_rejected() {
    let float_err = compile_errors("public void F() { float x = 1e40f; }");
    assert_eq!(float_err[0].code, "A2360");
    assert!(
        float_err[0]
            .message
            .contains("outside `float` finite range")
    );

    let double_err = compile_errors("public void F() { double x = 1e400; }");
    assert_eq!(double_err[0].code, "A2360");
    assert!(
        double_err[0]
            .message
            .contains("outside `double` finite range")
    );
}
