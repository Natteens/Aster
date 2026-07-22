//! End-to-end tests for M1C: canonical, deterministic, culture-invariant
//! `bool`/`char`/`int`/`uint`/`long`/`ulong`/`float`/`double` -> `string`
//! conversion via `.ToString()`. Mirrors `string_try_parse_float.rs`'s
//! established conventions (helpers, `compile_project` with
//! `using aster.core;`), and reuses the exact `stringify` mechanism that
//! already backs string interpolation -- not a parallel infrastructure.

use std::sync::atomic::{AtomicU64, Ordering};

use aster_codegen_cranelift::{ExecutionValue, MemoryStats, execute, execute_with_stats};
use aster_compiler::{compile_project, mir};

fn run(source: &str, function: &str) -> Result<ExecutionValue, String> {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("aster-to-string-{}-{id}.aster", std::process::id()));
    std::fs::write(&path, source).expect("write temporary project");
    let compilation = compile_project(&path).map_err(|diagnostics| format!("{diagnostics:#?}"));
    std::fs::remove_file(&path).ok();
    execute(&compilation?.compilation.mir, function).map_err(|error| error.to_string())
}

fn compile_errors(source: &str) -> String {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "aster-to-string-err-{}-{id}.aster",
        std::process::id()
    ));
    std::fs::write(&path, source).expect("write temporary project");
    let result = compile_project(&path);
    std::fs::remove_file(&path).ok();
    match result {
        Ok(_) => String::new(),
        Err(diagnostics) => format!("{diagnostics:#?}"),
    }
}

fn compile_mir(source: &str) -> mir::Module {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "aster-to-string-mir-{}-{id}.aster",
        std::process::id()
    ));
    std::fs::write(&path, source).expect("write temporary project");
    let compilation = compile_project(&path).expect("source should compile");
    std::fs::remove_file(&path).ok();
    compilation.compilation.mir
}

fn string_result(source: &str) -> String {
    match run(source, "Main") {
        Ok(ExecutionValue::String(text)) => text,
        other => panic!("expected a string result, got {other:?}"),
    }
}

// --- Section 21.1-6: semantics ----------------------------------------------

#[test]
fn eight_signatures_resolve_and_return_expected_values() {
    let source = "public string Main() { return (123).ToString(); }";
    assert_eq!(string_result(source), "123");
    let source = "public string Main() { return (-45L).ToString(); }";
    assert_eq!(string_result(source), "-45");
    let source = "public string Main() { return true.ToString(); }";
    assert_eq!(string_result(source), "true");
    let source = "public string Main() { char c = 'a'; return c.ToString(); }";
    assert_eq!(string_result(source), "a");
    let source = "public string Main() { uint value = 42; return value.ToString(); }";
    assert_eq!(string_result(source), "42");
    let source = "public string Main() { ulong value = 42; return value.ToString(); }";
    assert_eq!(string_result(source), "42");
    let source = "public string Main() { float value = 2.5f; return value.ToString(); }";
    assert_eq!(string_result(source), "2.5");
    let source = "public string Main() { double value = 2.5; return value.ToString(); }";
    assert_eq!(string_result(source), "2.5");
}

#[test]
fn extra_arguments_are_rejected() {
    let errors = compile_errors("public string Main() { return (123).ToString(\"D\"); }");
    assert!(
        errors.contains("ToString") && errors.contains("0 argument"),
        "expected an arity diagnostic, got {errors}"
    );
    let errors = compile_errors("public string Main() { return true.ToString(1); }");
    assert!(
        errors.contains("ToString") && errors.contains("0 argument"),
        "expected an arity diagnostic, got {errors}"
    );
}

#[test]
fn result_is_typed_string() {
    let source = "public string Main() { string text = (5).ToString(); return text; }";
    assert_eq!(string_result(source), "5");
}

#[test]
fn unsupported_receivers_are_rejected() {
    let errors = compile_errors(
        "public class Widget { public int value; }\n\
         public string Main() { Widget widget = new Widget(); return widget.ToString(); }",
    );
    assert!(
        errors.contains("ToString") || errors.contains("no method"),
        "expected a diagnostic rejecting ToString on a user class, got {errors}"
    );
    let errors = compile_errors(
        "public string Main() { List<int> values = new List<int>(); return values.ToString(); }",
    );
    assert!(!errors.is_empty(), "expected List<T> to reject ToString");
}

#[test]
fn user_defined_to_string_methods_are_preserved() {
    let source = "public class Widget {\n\
        public string ToString() { return \"custom\"; }\n\
    }\n\
    public string Main() { Widget widget = new Widget(); return widget.ToString(); }";
    assert_eq!(string_result(source), "custom");
}

#[test]
fn receiver_is_evaluated_exactly_once() {
    let source = "public class Counter {\n\
        public int calls;\n\
        public int GetValue() { calls = calls + 1; return 9; }\n\
    }\n\
    public string Main() {\n\
        Counter counter = new Counter();\n\
        string text = counter.GetValue().ToString();\n\
        return counter.calls.ToString() + \":\" + text;\n\
    }";
    assert_eq!(string_result(source), "1:9");
}

// --- Section 21.7-22: values -------------------------------------------------

#[test]
fn bool_values_format_as_lowercase_ascii() {
    assert_eq!(
        string_result("public string Main() { return true.ToString(); }"),
        "true"
    );
    assert_eq!(
        string_result("public string Main() { return false.ToString(); }"),
        "false"
    );
}

#[test]
fn char_values_format_as_their_utf8_scalar_with_no_quoting_or_escaping() {
    for (literal, expected) in [("'a'", "a"), ("'é'", "é"), ("'β'", "β"), ("'你'", "你")] {
        let source = format!("public string Main() {{ char c = {literal}; return c.ToString(); }}");
        assert_eq!(string_result(&source), expected, "char literal {literal}");
    }
}

#[test]
fn integer_min_max_and_zero_format_with_no_padding_or_thousands_separators() {
    let cases: [(&str, &str); 9] = [
        ("int value = 0; return value.ToString();", "0"),
        ("int value = 42; return value.ToString();", "42"),
        ("int value = -42; return value.ToString();", "-42"),
        (
            "int value = -2147483648; return value.ToString();",
            "-2147483648",
        ),
        (
            "int value = 2147483647; return value.ToString();",
            "2147483647",
        ),
        ("uint value = 0; return value.ToString();", "0"),
        (
            "uint value = 4294967295; return value.ToString();",
            "4294967295",
        ),
        (
            "long value = -9223372036854775807L - 1L; return value.ToString();",
            "-9223372036854775808",
        ),
        (
            "ulong value = 18446744073709551615UL; return value.ToString();",
            "18446744073709551615",
        ),
    ];
    for (body, expected) in cases {
        let source = format!("public string Main() {{ {body} }}");
        assert_eq!(string_result(&source), expected, "{body}");
    }
}

#[test]
fn float_and_double_common_values_use_a_dot_decimal_separator_and_no_suffix() {
    assert_eq!(
        string_result("public string Main() { float value = 2.5f; return value.ToString(); }"),
        "2.5"
    );
    assert_eq!(
        string_result("public string Main() { double value = 2.5; return value.ToString(); }"),
        "2.5"
    );
}

#[test]
fn negative_zero_keeps_its_sign_in_text() {
    assert_eq!(
        string_result("public string Main() { float value = -0.0f; return value.ToString(); }"),
        "-0"
    );
    assert_eq!(
        string_result("public string Main() { double value = -0.0; return value.ToString(); }"),
        "-0"
    );
}

#[test]
fn maximum_finite_minimum_normal_and_subnormal_values_format_without_error() {
    // Extreme magnitudes need scientific notation to write concisely, and
    // ASTER's own literal lexer does not accept an `e`/`E` exponent (only
    // `TryParseFloat`/`TryParseDouble` parse that form from a `string`), so
    // each value is parsed in from text first.
    let cases = [
        ("3.4028235e38", "TryParseFloat", "float max"),
        ("1.17549435e-38", "TryParseFloat", "float min normal"),
        ("1.4e-45", "TryParseFloat", "float subnormal"),
        ("1.7976931348623157e308", "TryParseDouble", "double max"),
        (
            "2.2250738585072014e-308",
            "TryParseDouble",
            "double min normal",
        ),
        ("5e-324", "TryParseDouble", "double subnormal"),
    ];
    for (literal, method, label) in cases {
        let source = format!(
            "using aster.core;\n\
             public string Main() {{\n\
                 switch (\"{literal}\".{method}()) {{ case Some(value): return value.ToString(); case None: return \"none\"; }}\n\
             }}"
        );
        let text = string_result(&source);
        assert!(!text.is_empty(), "{label} produced an empty string");
        assert!(
            text.parse::<f64>().is_ok(),
            "{label} produced non-numeric text {text:?}"
        );
    }
}

#[test]
fn nan_and_infinite_values_format_to_a_stable_text_and_still_reject_on_parse() {
    let nan = string_result(
        "using aster.core;\n\
         public string Main() { double zero = 0.0; double value = zero / zero; return value.ToString(); }",
    );
    assert_eq!(nan, "NaN");
    let positive_infinity = string_result(
        "using aster.core;\n\
         public string Main() { double zero = 0.0; double value = 1.0 / zero; return value.ToString(); }",
    );
    assert_eq!(positive_infinity, "inf");
    let negative_infinity = string_result(
        "using aster.core;\n\
         public string Main() { double zero = 0.0; double value = -1.0 / zero; return value.ToString(); }",
    );
    assert_eq!(negative_infinity, "-inf");

    // The parsing side deliberately keeps rejecting all three textual forms.
    for text in [&nan, &positive_infinity, &negative_infinity] {
        let source = format!(
            "using aster.core;\n\
             public double Main() {{\n\
                 Option<double> parsed = \"{text}\".TryParseDouble();\n\
                 switch (parsed) {{ case Some(value): return value; case None: return -1.0; }}\n\
             }}"
        );
        assert_eq!(run(&source, "Main"), Ok(ExecutionValue::Double(-1.0)));
    }
}

// --- Section 21.23-26: round-trip -------------------------------------------

#[test]
fn every_integer_type_round_trips_through_to_string_and_try_parse() {
    let cases = [
        (
            "int",
            "TryParseInt",
            ["0", "-2147483648", "2147483647", "42", "-42"].as_slice(),
        ),
        ("uint", "TryParseUInt", ["0", "4294967295", "42"].as_slice()),
        (
            "long",
            "TryParseLong",
            ["0", "-9223372036854775807 - 1L", "9223372036854775807"].as_slice(),
        ),
        (
            "ulong",
            "TryParseULong",
            ["0", "18446744073709551615"].as_slice(),
        ),
    ];
    for (type_name, method, literals) in cases {
        for literal in literals {
            let suffix = match type_name {
                "long" if literal.contains('-') => "",
                "long" => "L",
                "ulong" => "UL",
                _ => "",
            };
            let source = format!(
                "using aster.core;\n\
                 public string Main() {{\n\
                     {type_name} original = {literal}{suffix};\n\
                     string text = original.ToString();\n\
                     Option<{type_name}> parsed = text.{method}();\n\
                     switch (parsed) {{ case Some(value): return value == original ? \"match\" : \"mismatch\"; case None: return \"none\"; }}\n\
                 }}"
            );
            assert_eq!(
                run(&source, "Main"),
                Ok(ExecutionValue::String("match".to_owned())),
                "{type_name} round trip for {literal}"
            );
        }
    }
}

/// Round trips `literal` through `TryParseDouble() -> ToString() ->
/// TryParseDouble()`. The original value is itself parsed in from text
/// (rather than written as an ASTER source literal) because ASTER's literal
/// lexer has no exponent syntax, while several required samples (subnormals,
/// the widest finite magnitudes) need one to be written concisely.
fn round_trip_double(literal: &str) -> f64 {
    let source = format!(
        "using aster.core;\n\
         public double Main() {{\n\
             switch (\"{literal}\".TryParseDouble()) {{\n\
                 case Some(original):\n\
                     string text = original.ToString();\n\
                     switch (text.TryParseDouble()) {{ case Some(value): return value; case None: return -999.0; }}\n\
                 case None: return -888.0;\n\
             }}\n\
         }}"
    );
    let ExecutionValue::Double(value) = run(&source, "Main").expect("execution should succeed")
    else {
        panic!("expected a double");
    };
    value
}

/// Like [`round_trip_double`], but for `float`/`TryParseFloat`. The result is
/// cast to `double` only at the very end, purely to extract it from `Main`;
/// widening `-0.0f32`/finite `f32` values to `f64` preserves their exact bit
/// pattern (sign, magnitude, and rounding), so the bitwise comparison in the
/// caller still exercises `float`'s own precision, not `double`'s.
fn round_trip_float(literal: &str) -> f64 {
    let source = format!(
        "using aster.core;\n\
         public double Main() {{\n\
             switch (\"{literal}\".TryParseFloat()) {{\n\
                 case Some(original):\n\
                     string text = original.ToString();\n\
                     switch (text.TryParseFloat()) {{ case Some(value): return (double)value; case None: return -999.0; }}\n\
                 case None: return -888.0;\n\
             }}\n\
         }}"
    );
    let ExecutionValue::Double(value) = run(&source, "Main").expect("execution should succeed")
    else {
        panic!("expected a double");
    };
    value
}

#[test]
fn double_round_trips_bitwise_across_a_wide_sample() {
    for literal in [
        "0.0",
        "-0.0",
        "1.5",
        "1000.0",
        "1.7976931348623157e308",
        "-1.7976931348623157e308",
        "2.2250738585072014e-308",
        "5e-324",
        "1e20",
        "123456789012345.0",
    ] {
        let original: f64 = literal.parse().expect("valid f64 literal");
        let result = round_trip_double(literal);
        assert_eq!(
            result.to_bits(),
            original.to_bits(),
            "double round trip mismatch for {literal}"
        );
    }
}

#[test]
fn float_round_trips_bitwise_across_a_wide_sample() {
    for literal in [
        "0.0",
        "-0.0",
        "1.5",
        "1000.0",
        "3.4028235e38",
        "-3.4028235e38",
        "1.17549435e-38",
        "1.4e-45",
        "0.1",
        "123456790.0",
    ] {
        let original: f32 = literal.parse().expect("valid f32 literal");
        let result = round_trip_float(literal);
        assert_eq!(
            result.to_bits(),
            f64::from(original).to_bits(),
            "float round trip mismatch for {literal}"
        );
    }
}

#[test]
fn negative_zero_round_trips_bitwise_for_float_and_double() {
    assert_eq!(round_trip_double("-0.0").to_bits(), (-0.0_f64).to_bits());
    assert_eq!(
        round_trip_float("-0.0").to_bits(),
        f64::from(-0.0_f32).to_bits()
    );
}

// --- Section 21.27-33: integration -------------------------------------------

#[test]
fn to_string_matches_interpolation_for_every_value_type() {
    let cases = [
        "bool value = true; return value.ToString() == $\"{value}\" ? \"match\" : \"mismatch\";",
        "char value = 'z'; return value.ToString() == $\"{value}\" ? \"match\" : \"mismatch\";",
        "int value = -42; return value.ToString() == $\"{value}\" ? \"match\" : \"mismatch\";",
        "uint value = 42; return value.ToString() == $\"{value}\" ? \"match\" : \"mismatch\";",
        "long value = -9223372036854775807L - 1L; return value.ToString() == $\"{value}\" ? \"match\" : \"mismatch\";",
        "ulong value = 18446744073709551615UL; return value.ToString() == $\"{value}\" ? \"match\" : \"mismatch\";",
        "float value = 0.1f; return value.ToString() == $\"{value}\" ? \"match\" : \"mismatch\";",
        "double value = -42.5; return value.ToString() == $\"{value}\" ? \"match\" : \"mismatch\";",
    ];
    for body in cases {
        let source = format!("public string Main() {{ {body} }}");
        assert_eq!(string_result(&source), "match", "{body}");
    }
}

#[test]
fn to_string_result_concatenates_normally() {
    let source = "public string Main() { return \"n=\" + (5).ToString() + \"!\"; }";
    assert_eq!(string_result(source), "n=5!");
}

#[test]
fn to_string_result_feeds_back_into_try_parse() {
    let source = "using aster.core;\n\
        public Option<int> RoundTrip(int value) { return value.ToString().TryParseInt(); }\n\
        public int Main() { switch (RoundTrip(77)) { case Some(v): return v; case None: return -1; } }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(77)));
}

#[test]
fn to_string_composes_with_option_postfix_try() {
    let source = "using aster.core;\n\
        public Option<string> Describe(int value) {\n\
            int v = Option<int>.Some(value)?;\n\
            return Option<string>.Some(v.ToString());\n\
        }\n\
        public string Main() { switch (Describe(9)) { case Some(text): return text; case None: return \"none\"; } }";
    assert_eq!(string_result(source), "9");
}

#[test]
fn to_string_works_inside_a_helper_and_a_generic_function() {
    let source = "public string Stringify(int value) { return value.ToString(); }\n\
        public string Main() { return Stringify(13); }";
    assert_eq!(string_result(source), "13");

    let source = "public T First<T>(T a, T b) { return a; }\n\
        public string Main() { int value = First(11, 22); return value.ToString(); }";
    assert_eq!(string_result(source), "11");
}

#[test]
fn to_string_works_across_namespaces() {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let project_root =
        std::env::temp_dir().join(format!("aster-to-string-ns-{}-{id}", std::process::id()));
    let directory = project_root.join("app");
    std::fs::create_dir_all(&directory).expect("create namespace directory");
    std::fs::write(
        project_root.join("Aster.toml"),
        "[application]\nentry = \"app.Main\"\n",
    )
    .expect("write manifest");
    let path = directory.join("main.aster");
    let source = "namespace app;\n\
        public string Main() { int value = 314; return value.ToString(); }";
    std::fs::write(&path, source).expect("write namespaced project file");
    let compilation = compile_project(&path).map_err(|diagnostics| format!("{diagnostics:#?}"));
    std::fs::remove_dir_all(&project_root).ok();
    let compilation = compilation.expect("namespaced source should compile");
    assert_eq!(
        execute(&compilation.compilation.mir, "Main"),
        Ok(ExecutionValue::String("314".to_owned()))
    );
}

#[test]
fn to_string_works_on_array_and_list_elements_and_a_result_payload() {
    let source = "public string Main() { int[] values = new int[1]; values[0] = 5; return values[0].ToString(); }";
    assert_eq!(string_result(source), "5");

    let source = "public string Main() {\n\
        List<int> values = new List<int>();\n\
        values.Add(7);\n\
        return values.Get(0).ToString();\n\
    }";
    assert_eq!(string_result(source), "7");

    let source = "using aster.core;\n\
        public Result<double, string> Compute() { return Result<double, string>.Ok(1.5); }\n\
        public string Main() {\n\
            switch (Compute()) { case Ok(value): return value.ToString(); case Error(e): return e; }\n\
        }";
    assert_eq!(string_result(source), "1.5");
}

// --- Section 21.34-39: adulterated MIR / runtime -----------------------------

fn find_format_primitive_mut(module: &mut mir::Module) -> &mut mir::Instruction {
    module
        .functions
        .iter_mut()
        .flat_map(|function| &mut function.blocks)
        .flat_map(|block| &mut block.instructions)
        .find(|instruction| {
            matches!(
                instruction,
                mir::Instruction::CallIntrinsic {
                    intrinsic: mir::Intrinsic::StringFromLong
                        | mir::Intrinsic::StringFromULong
                        | mir::Intrinsic::StringFromDouble
                        | mir::Intrinsic::StringFromFloat
                        | mir::Intrinsic::StringFromBool
                        | mir::Intrinsic::StringFromChar,
                    ..
                }
            )
        })
        .expect("a StringFrom* intrinsic call exists")
}

#[test]
fn adulterated_mir_rejects_an_incorrect_receiver_type() {
    let mut module = compile_mir("public string Main() { return (5).ToString(); }");
    let mir::Instruction::CallIntrinsic {
        intrinsic,
        arguments,
        ..
    } = find_format_primitive_mut(&mut module)
    else {
        unreachable!();
    };
    assert_eq!(*intrinsic, mir::Intrinsic::StringFromLong);
    arguments[0].type_ = mir::Type::Double;
    let error = execute(&module, "Main").expect_err("a mismatched receiver type must be rejected");
    assert!(!error.to_string().is_empty());
}

#[test]
fn adulterated_mir_rejects_a_non_string_return_type() {
    let mut module = compile_mir("public string Main() { return (5).ToString(); }");
    let mir::Instruction::CallIntrinsic { return_type, .. } =
        find_format_primitive_mut(&mut module)
    else {
        unreachable!();
    };
    *return_type = mir::Type::Int;
    let error = execute(&module, "Main").expect_err("a non-string return type must be rejected");
    assert!(!error.to_string().is_empty());
}

#[test]
fn adulterated_mir_rejects_an_undeclared_destination_local() {
    let mut module = compile_mir("public string Main() { return (5).ToString(); }");
    let mir::Instruction::CallIntrinsic { destination, .. } =
        find_format_primitive_mut(&mut module)
    else {
        unreachable!();
    };
    *destination = Some(mir::Place::Local(mir::LocalId(u32::MAX)));
    let error = execute(&module, "Main").expect_err("an undeclared destination must be rejected");
    assert!(!error.to_string().is_empty());
}

#[test]
fn a_format_failure_path_does_not_exist_and_normal_calls_stay_clean() {
    // `ToString()` on these eight primitives has no failure path (unlike
    // `TryParse*`, which can return `None`): every call either produces a
    // fully initialized string or the whole program is rejected before it
    // runs. This test simply confirms back-to-back independent calls each
    // produce their own correct, uncontaminated result.
    let source = "public string Main() {\n\
        string first = (1).ToString();\n\
        string second = (2).ToString();\n\
        string third = (3).ToString();\n\
        return first + second + third;\n\
    }";
    assert_eq!(string_result(source), "123");
}

// --- Section 21.40-42: memory ------------------------------------------------

fn stats_for(source: &str) -> MemoryStats {
    let module = compile_mir(source);
    let (_, stats) = execute_with_stats(&module, "Main").expect("source should execute");
    stats
}

#[test]
fn repeated_to_string_calls_reclaim_temporary_memory() {
    let source = "public int Main() {\n\
        int total = 0;\n\
        for (int i = 0; i < 5000; i++) {\n\
            string text = i.ToString();\n\
            total = total + text.Length;\n\
        }\n\
        return total;\n\
    }";
    let stats = stats_for(source);
    assert_eq!(stats.used_bytes, 0);
}

#[test]
fn a_persistent_to_string_result_stays_valid() {
    let source = "public class Holder {\n\
        public string text;\n\
        public Holder() { text = \"\"; }\n\
    }\n\
    public string Main() {\n\
        Holder holder = new Holder();\n\
        holder.text = (12345).ToString();\n\
        return holder.text;\n\
    }";
    assert_eq!(string_result(source), "12345");
}

#[test]
fn format_and_parse_cycles_show_no_unexplained_memory_growth() {
    let source = "using aster.core;\n\
        public int Main() {\n\
            int total = 0;\n\
            for (int i = 0; i < 3000; i++) {\n\
                string text = i.ToString();\n\
                switch (text.TryParseInt()) { case Some(v): total = total + v; case None: }\n\
            }\n\
            return total;\n\
        }";
    let stats = stats_for(source);
    assert_eq!(stats.used_bytes, 0);
}
