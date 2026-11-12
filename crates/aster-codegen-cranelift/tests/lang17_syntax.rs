//! LANG-017 end to end: `do`-`while`, `%=`, block comments, exponent literals
//! and digit separators execute natively with C# semantics.

use aster_codegen_cranelift::{ExecutionValue, execute};
use aster_compiler::compile;

fn run(source: &str, function: &str) -> Result<ExecutionValue, String> {
    let compilation = compile(source).map_err(|diagnostics| format!("{diagnostics:#?}"))?;
    execute(&compilation.mir, function).map_err(|error| error.to_string())
}

fn int(source: &str) -> i32 {
    match run(source, "Run") {
        Ok(ExecutionValue::Int(value)) => value,
        other => panic!("expected an int result, got {other:?}\n{source}"),
    }
}

fn rejected(source: &str) -> bool {
    compile(source).is_err()
}

#[test]
fn do_while_runs_the_body_once_when_the_condition_is_false() {
    assert_eq!(
        int("public int Run() { int n = 0; do { n += 1; } while (false); return n; }"),
        1
    );
    assert_eq!(
        int("public int Run() { int n = 10; do { n += 1; } while (n < 5); return n; }"),
        11
    );
    assert_eq!(
        int("public int Run() { int n = 0; do { n += 1; } while (n < 5); return n; }"),
        5
    );
}

#[test]
fn do_while_break_and_continue() {
    // `continue` jumps to the condition check, not back to the body start.
    let source = r"
        public int Run() {
            int i = 0; int evens = 0;
            do {
                i += 1;
                if (i % 2 == 1) { continue; }
                evens += 1;
            } while (i < 10);
            return i * 100 + evens;
        }
    ";
    assert_eq!(int(source), 1005);
    // `continue` with an initially false condition must exit, not loop.
    assert_eq!(
        int("public int Run() { int n = 0; do { n += 1; continue; } while (false); return n; }"),
        1
    );
    assert_eq!(
        int(
            "public int Run() { int n = 0; do { n += 1; if (n == 3) { break; } } while (true); return n; }"
        ),
        3
    );
}

#[test]
fn do_while_nests_with_other_loops() {
    let source = r"
        public int Run() {
            int total = 0; int i = 0;
            do {
                int j = 0;
                do {
                    j += 1;
                    if (j == 2) { continue; }
                    if (j == 4) { break; }
                    total += 1;
                } while (j < 10);
                for (int k = 0; k < 2; k++) {
                    int m = 0;
                    do { m += 1; total += 10; } while (m < 2);
                }
                i += 1;
            } while (i < 3);
            return total;
        }
    ";
    // inner: j=1 (+1), j=2 skipped, j=3 (+1), j=4 break => 2; for: 2*2*10 = 40.
    assert_eq!(int(source), 3 * (2 + 40));
}

#[test]
fn do_while_scoping_and_condition_typing_are_checked() {
    assert!(rejected(
        "public int Run() { do { int x = 1; } while (x < 3); return 0; }"
    ));
    assert!(rejected(
        "public int Run() { int n = 0; do { n += 1; } while (n); return n; }"
    ));
    assert!(rejected("public void F() { break; }"));
}

#[test]
fn remainder_assignment_on_numeric_types() {
    assert_eq!(int("public int Run() { int a = 17; a %= 5; return a; }"), 2);
    assert_eq!(
        int("public int Run() { int a = -17; a %= 5; return a; }"),
        -2
    );
    assert_eq!(
        run(
            "public long Run() { long a = 10000000000; a %= 7; return a; }",
            "Run"
        ),
        Ok(ExecutionValue::Long(10_000_000_000 % 7))
    );
    // Floating-point `%` is not implemented by the JIT yet (a codegen gap that
    // predates LANG-017): `%=` type-checks like `%` and fails identically there.
    for (compound, binary) in [
        (
            "public float Run() { float a = 7.5f; a %= 2.0f; return a; }",
            "public float Run() { float a = 7.5f; return a % 2.0f; }",
        ),
        (
            "public double Run() { double a = 7.5; a %= 2.0; return a; }",
            "public double Run() { double a = 7.5; return a % 2.0; }",
        ),
    ] {
        assert!(compile(compound).is_ok());
        let expected = run(binary, "Run").expect_err("float remainder is a JIT gap");
        assert_eq!(run(compound, "Run"), Err(expected));
    }
    // Same typing as `+=`: an int target with a long operand would narrow.
    assert!(rejected(
        "public int Run() { int a = 5; long b = 3; a %= b; return a; }"
    ));
    assert!(rejected(
        "public int Run() { string a = \"x\"; a %= 2; return 0; }"
    ));
    assert!(rejected(
        "public int Run() { const int a = 5; a %= 2; return a; }"
    ));
    // Division by zero keeps the existing remainder behavior.
    assert!(
        run(
            "public int Run() { int a = 5; int z = 0; a %= z; return a; }",
            "Run"
        )
        .is_err()
    );
}

#[test]
fn remainder_assignment_on_fields_and_array_elements_evaluates_the_target_once() {
    let source = r"
        public class Counter {
            public int calls;
            public int value;
            public Counter() { calls = 0; value = 23; }
            public int Index() { calls++; return 1; }
        }
        public int Run() {
            int[] values = [0, 100, 0];
            Counter counter = new();
            Counter[] holder = [counter];
            counter.value %= 5;
            values[counter.Index()] %= 7;
            holder[0].value %= 2;
            return values[1] * 1000 + counter.value * 100 + counter.calls;
        }
    ";
    // 23 % 5 = 3, then 3 % 2 = 1; 100 % 7 = 2; Index called once.
    assert_eq!(int(source), 2 * 1000 + 100 + 1);
}

#[test]
fn block_comments_in_every_position_execute() {
    let source = r#"
        /* leading */ public /* a */ int /* b */ Run /* c */ ( /* d */ ) /* e */ {
            /* multi
               line */
            int x /* f */ = /* g */ 4 /* h */ ; /* i */
            string s = "/* not a comment */";
            x /* j */ += /* k */ s.Length; // line comment with /* inside
            return /* l */ x /* m */ ; /* trailing */
        }
        /* end */
    "#;
    assert_eq!(int(source), 4 + 19);
    assert_eq!(int("public int Run() { return 1 /* a */ / /* b */ 1; }"), 1);
    assert!(rejected("public int Run() { return 1; } /* unterminated"));
    assert!(rejected(
        "public int Run() { /* /* nested */ return 1; */ }"
    ));
}

#[test]
fn separators_and_exponent_literals_execute() {
    let source = r"
        public int Run() {
            int a = 1_000_000;
            int b = 0xFF_FF;
            int c = 0b1010_1010;
            int d = 1__0;
            int e = 0x_FF;
            return a + b + c + d + e;
        }
    ";
    assert_eq!(int(source), 1_000_000 + 65_535 + 170 + 10 + 255);
    // Today an unsuffixed real literal is a `float`; an exponent keeps that.
    assert_eq!(
        run("public float Run() { return 1e3; }", "Run"),
        Ok(ExecutionValue::Float(1000.0))
    );
    assert_eq!(
        run("public double Run() { return 2.5E-3d; }", "Run"),
        Ok(ExecutionValue::Double(0.0025))
    );
    assert_eq!(
        run("public double Run() { return 1_0e1_0d; }", "Run"),
        Ok(ExecutionValue::Double(10e10))
    );
    assert_eq!(
        run("public float Run() { return 2.5E-3; }", "Run"),
        Ok(ExecutionValue::Float(0.0025))
    );
    assert_eq!(
        run("public double Run() { return 1_000.5_0d; }", "Run"),
        Ok(ExecutionValue::Double(1000.5))
    );
    // A real literal never converts to an integer type.
    assert!(rejected("public int Run() { int x = 1e3; return x; }"));
}

#[test]
fn malformed_separators_and_exponents_are_compile_errors() {
    for literal in [
        "1_", "1_.5", "1._5", "1e", "1e+", "1e_3", "0xFF_", "0x_", "1_f",
    ] {
        assert!(
            rejected(&format!(
                "public int Run() {{ var x = {literal}; return 0; }}"
            )),
            "`{literal}` must be rejected"
        );
    }
    // `_1` is an identifier, not a literal, so it is an undefined name.
    assert!(rejected("public int Run() { return _1; }"));
}
