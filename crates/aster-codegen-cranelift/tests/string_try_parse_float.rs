//! End-to-end tests for M1B2: deterministic `string.TryParseFloat()`/
//! `TryParseDouble()` -> `Option<T>`. Mirrors `string_try_parse.rs`'s
//! established conventions (helpers, `compile_project` with
//! `using aster.core;`) and extends the same canonical `TryParse*` family
//! introduced in M1B1, not a parallel infrastructure.

use std::sync::atomic::{AtomicU64, Ordering};

use aster_codegen_cranelift::{ExecutionValue, MemoryStats, execute, execute_with_stats};
use aster_compiler::{compile_project, mir};

fn run(source: &str, function: &str) -> Result<ExecutionValue, String> {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "aster-try-parse-float-{}-{id}.aster",
        std::process::id()
    ));
    std::fs::write(&path, source).expect("write temporary project");
    let compilation = compile_project(&path).map_err(|diagnostics| format!("{diagnostics:#?}"));
    std::fs::remove_file(&path).ok();
    execute(&compilation?.compilation.mir, function).map_err(|error| error.to_string())
}

fn compile_errors(source: &str) -> String {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "aster-try-parse-float-err-{}-{id}.aster",
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
        "aster-try-parse-float-mir-{}-{id}.aster",
        std::process::id()
    ));
    std::fs::write(&path, source).expect("write temporary project");
    let compilation = compile_project(&path).expect("source should compile");
    std::fs::remove_file(&path).ok();
    compilation.compilation.mir
}

/// `public double Main() { Option<{option_type}> parsed = "{text}".{method}();
/// switch (parsed) { case Some(value): return {some_expr}; case None: return -1.0; } }`
fn parse_and_switch(option_type: &str, method: &str, text: &str, some_expr: &str) -> String {
    format!(
        "using aster.core;\n\
         public double Main() {{\n\
             Option<{option_type}> parsed = \"{text}\".{method}();\n\
             switch (parsed) {{ case Some(value): return {some_expr}; case None: return -1.0; }}\n\
         }}"
    )
}

fn find_try_parse_float_mut(module: &mut mir::Module) -> &mut mir::Instruction {
    module
        .functions
        .iter_mut()
        .flat_map(|function| &mut function.blocks)
        .flat_map(|block| &mut block.instructions)
        .find(|instruction| {
            matches!(
                instruction,
                mir::Instruction::CallIntrinsic {
                    intrinsic: mir::Intrinsic::StringTryParseFloat
                        | mir::Intrinsic::StringTryParseDouble,
                    ..
                }
            )
        })
        .expect("a TryParseFloat/TryParseDouble intrinsic call exists")
}

// --- Section 20.1-6: semantics ----------------------------------------------

#[test]
fn two_signatures_resolve_and_return_expected_values() {
    let source = parse_and_switch("float", "TryParseFloat", "2.5", "(double)value");
    assert_eq!(run(&source, "Main"), Ok(ExecutionValue::Double(2.5)));
    let source = parse_and_switch("double", "TryParseDouble", "3.5", "value");
    assert_eq!(run(&source, "Main"), Ok(ExecutionValue::Double(3.5)));
}

#[test]
fn extra_arguments_are_rejected() {
    for (method, args) in [("TryParseFloat", "1.0f"), ("TryParseDouble", "false")] {
        let errors = compile_errors(&format!(
            "using aster.core;\n\
             public int Main() {{ var parsed = \"1.0\".{method}({args}); return 0; }}"
        ));
        assert!(
            errors.contains(method) && errors.contains("0 argument"),
            "expected an arity diagnostic for {method}, got {errors}"
        );
    }
}

#[test]
fn a_non_string_receiver_does_not_activate_the_intrinsic() {
    let source = "public class Widget {\n\
        public int TryParseDouble() { return 9; }\n\
    }\n\
    public int Main() { Widget widget = new Widget(); return widget.TryParseDouble(); }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(9)));
}

#[test]
fn receiver_is_evaluated_exactly_once() {
    let source = "using aster.core;\n\
        public class Counter {\n\
            public int calls;\n\
            public string GetText() { calls = calls + 1; return \"9.5\"; }\n\
        }\n\
        public double Main() {\n\
            Counter counter = new Counter();\n\
            Option<double> parsed = counter.GetText().TryParseDouble();\n\
            switch (parsed) { case Some(value): return counter.calls * 1000.0 + value; case None: return -1.0; }\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Double(1009.5)));
}

// --- Section 20.7-9: float grammar and limits -------------------------------

#[test]
fn float_valid_grammar_cases() {
    let cases: [(&str, f64); 10] = [
        ("0", 0.0),
        ("+0", 0.0),
        ("-0", -0.0),
        ("1", 1.0),
        ("1.0", 1.0),
        ("1.", 1.0),
        (".5", 0.5),
        ("-.5", -0.5),
        ("+0.25", 0.25),
        ("1e3", 1000.0),
    ];
    for (text, expected) in cases {
        let source = parse_and_switch("float", "TryParseFloat", text, "(double)value");
        assert_eq!(
            run(&source, "Main"),
            Ok(ExecutionValue::Double(expected)),
            "\"{text}\".TryParseFloat()"
        );
    }
}

#[test]
fn float_invalid_grammar_cases() {
    for text in [
        "",
        "+",
        "-",
        ".",
        "e10",
        "1e",
        "1e+",
        "1.2.3",
        "1e2e3",
        " 1.0",
        "1.0 ",
        "1_000.0",
        "1,5",
        "0x1.0",
        "1f",
        "1d",
        "\u{FF11}\u{FF12}.\u{FF15}",
    ] {
        let source = parse_and_switch("float", "TryParseFloat", text, "1.0");
        assert_eq!(
            run(&source, "Main"),
            Ok(ExecutionValue::Double(-1.0)),
            "\"{text}\".TryParseFloat() should be None"
        );
    }
}

/// Runs `text.TryParseFloat()`/`TryParseDouble()` and returns the payload as
/// a `double` (`-1.0` sentinel on `None`), sidestepping ASTER-side ternary
/// expressions entirely -- the magnitude comparisons below are done in Rust
/// instead, which also gives exact IEEE-754 precision.
fn parse_return_value(
    option_type: &str,
    method: &str,
    text: &str,
) -> Result<ExecutionValue, String> {
    let cast = if option_type == "float" {
        "(double)value"
    } else {
        "value"
    };
    let source = parse_and_switch(option_type, method, text, cast);
    run(&source, "Main")
}

#[test]
fn float_limits() {
    // Maximum finite f32: parses to `Some` with a large finite magnitude.
    let value = parse_return_value("float", "TryParseFloat", "3.4028235e38")
        .expect("execution should succeed");
    assert_eq!(value, ExecutionValue::Double(f64::from(f32::MAX)));

    // Immediately overflowing f32: `None`.
    assert_eq!(
        parse_return_value("float", "TryParseFloat", "3.4028236e39"),
        Ok(ExecutionValue::Double(-1.0)),
        "overflow past f32::MAX must be None"
    );

    // Smallest positive normal f32.
    let value = parse_return_value("float", "TryParseFloat", "1.17549435e-38")
        .expect("execution should succeed");
    let ExecutionValue::Double(value) = value else {
        panic!("expected a double");
    };
    assert!(value > 0.0 && value < 1e-30);

    // A subnormal f32: still finite, still `Some`.
    let value =
        parse_return_value("float", "TryParseFloat", "1.4e-45").expect("execution should succeed");
    let ExecutionValue::Double(value) = value else {
        panic!("expected a double");
    };
    assert!(value > 0.0, "a subnormal is a valid, finite Some");

    // Maximum-magnitude negative finite f32.
    let value = parse_return_value("float", "TryParseFloat", "-3.4028235e38")
        .expect("execution should succeed");
    assert_eq!(value, ExecutionValue::Double(f64::from(-f32::MAX)));
}

// --- Section 20.10-12: double grammar and limits ----------------------------

#[test]
fn double_valid_grammar_cases() {
    let cases: [(&str, f64); 10] = [
        ("0", 0.0),
        ("+0", 0.0),
        ("-0", -0.0),
        ("1", 1.0),
        ("1.0", 1.0),
        ("1.", 1.0),
        (".5", 0.5),
        ("-.5", -0.5),
        ("+0.25", 0.25),
        ("-1.25e+10", -12_500_000_000.0),
    ];
    for (text, expected) in cases {
        let source = parse_and_switch("double", "TryParseDouble", text, "value");
        assert_eq!(
            run(&source, "Main"),
            Ok(ExecutionValue::Double(expected)),
            "\"{text}\".TryParseDouble()"
        );
    }
}

#[test]
fn double_invalid_grammar_cases() {
    for text in [
        "", "+", "-", ".", "e10", "1e", "1e-", "1.2.3", "1e2e3", " 1.0", "1.0 ", "1_000.0", "1,5",
        "0x1.0", "1f", "1d",
    ] {
        let source = parse_and_switch("double", "TryParseDouble", text, "1.0");
        assert_eq!(
            run(&source, "Main"),
            Ok(ExecutionValue::Double(-1.0)),
            "\"{text}\".TryParseDouble() should be None"
        );
    }
}

#[test]
fn double_limits() {
    // Maximum finite f64.
    let value = parse_return_value("double", "TryParseDouble", "1.7976931348623157e308")
        .expect("execution should succeed");
    assert_eq!(value, ExecutionValue::Double(f64::MAX));

    // Immediately overflowing f64: `None`.
    assert_eq!(
        parse_return_value("double", "TryParseDouble", "1.7976931348623159e308"),
        Ok(ExecutionValue::Double(-1.0)),
        "overflow past f64::MAX must be None"
    );

    // Smallest positive normal f64.
    let value = parse_return_value("double", "TryParseDouble", "2.2250738585072014e-308")
        .expect("execution should succeed");
    let ExecutionValue::Double(value) = value else {
        panic!("expected a double");
    };
    assert!(value > 0.0 && value < 1e-300);

    // A subnormal f64: still finite, still `Some`.
    let value =
        parse_return_value("double", "TryParseDouble", "5e-324").expect("execution should succeed");
    let ExecutionValue::Double(value) = value else {
        panic!("expected a double");
    };
    assert!(value > 0.0, "a subnormal is a valid, finite Some");

    // Maximum-magnitude negative finite f64.
    let value = parse_return_value("double", "TryParseDouble", "-1.7976931348623157e308")
        .expect("execution should succeed");
    assert_eq!(value, ExecutionValue::Double(-f64::MAX));
}

// --- Section 20.15: NaN/infinity textual forms rejected ---------------------

#[test]
fn nan_and_infinity_textual_forms_are_rejected() {
    for text in [
        "NaN",
        "nan",
        "inf",
        "-INF",
        "Infinity",
        "+Infinity",
        "-Infinity",
    ] {
        let source = parse_and_switch("double", "TryParseDouble", text, "1.0");
        assert_eq!(
            run(&source, "Main"),
            Ok(ExecutionValue::Double(-1.0)),
            "\"{text}\".TryParseDouble() should be None"
        );
        let source = parse_and_switch("float", "TryParseFloat", text, "1.0");
        assert_eq!(
            run(&source, "Main"),
            Ok(ExecutionValue::Double(-1.0)),
            "\"{text}\".TryParseFloat() should be None"
        );
    }
}

// --- Section 20.16-20: integration -------------------------------------------

#[test]
fn switch_over_option_double_works() {
    let source = "using aster.core;\n\
        public double Main() { Option<double> value = Option<double>.Some(4.5); \
        switch (value) { case Some(number): return number; case None: return 0.0; } }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Double(4.5)));
}

#[test]
fn postfix_try_propagates_some_and_none_for_double() {
    let source = "using aster.core;\n\
        public Option<double> ParsePositive(string text) {\n\
            double value = text.TryParseDouble()?;\n\
            if (value <= 0.0) { return Option<double>.None; }\n\
            return Option<double>.Some(value);\n\
        }\n\
        public double Main() {\n\
            switch (ParsePositive(\"2.5\")) { case Some(value): return value; case None: return -1.0; }\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Double(2.5)));

    let source = "using aster.core;\n\
        public Option<double> ParsePositive(string text) {\n\
            double value = text.TryParseDouble()?;\n\
            if (value <= 0.0) { return Option<double>.None; }\n\
            return Option<double>.Some(value);\n\
        }\n\
        public double Main() {\n\
            switch (ParsePositive(\"nope\")) { case Some(value): return value; case None: return -1.0; }\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Double(-1.0)));
}

#[test]
fn postfix_try_return_type_may_differ_from_the_payload_type() {
    let source = "using aster.core;\n\
        public Option<string> Describe(string text) {\n\
            double value = text.TryParseDouble()?;\n\
            if (value > 0.0) { return Option<string>.Some(\"positive\"); }\n\
            return Option<string>.None;\n\
        }\n\
        public int Main() {\n\
            switch (Describe(\"5.0\")) { case Some(word): return word == \"positive\" ? 1 : 0; case None: return -1; }\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(1)));
}

#[test]
fn try_parse_float_works_inside_a_generic_function() {
    let source = "using aster.core;\n\
        public T First<T>(T a, T b) { return a; }\n\
        public double Main() {\n\
            Option<float> parsed = First(\"1.5\", \"2.5\").TryParseFloat();\n\
            switch (parsed) { case Some(value): return (double)value; case None: return -1.0; }\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Double(1.5)));
}

#[test]
fn try_parse_double_works_across_namespaces() {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let project_root = std::env::temp_dir().join(format!(
        "aster-try-parse-float-ns-{}-{id}",
        std::process::id()
    ));
    let directory = project_root.join("app");
    std::fs::create_dir_all(&directory).expect("create namespace directory");
    std::fs::write(
        project_root.join("Aster.toml"),
        "[package]\nname = \"string_try_parse_float_test\"\n",
    )
    .expect("write manifest");
    let path = directory.join("main.aster");
    let source = "namespace app;\n\
        using aster.core;\n\
        public double Main() {\n\
            Option<double> parsed = \"7.25\".TryParseDouble();\n\
            switch (parsed) { case Some(value): return value; case None: return -1.0; }\n\
        }";
    std::fs::write(&path, source).expect("write namespaced project file");
    let compilation = compile_project(&path).map_err(|diagnostics| format!("{diagnostics:#?}"));
    std::fs::remove_dir_all(&project_root).ok();
    let compilation = compilation.expect("namespaced source should compile");
    assert_eq!(
        execute(
            &compilation.compilation.mir,
            "string_try_parse_float_test::app::Main",
        ),
        Ok(ExecutionValue::Double(7.25))
    );
}

// --- Section 20.21-27: adulterated MIR / runtime -----------------------------

#[test]
fn adulterated_mir_rejects_a_non_string_receiver() {
    let mut module = compile_mir(&parse_and_switch(
        "double",
        "TryParseDouble",
        "1.0",
        "value",
    ));
    let mir::Instruction::CallIntrinsic { arguments, .. } = find_try_parse_float_mut(&mut module)
    else {
        unreachable!();
    };
    arguments[0].type_ = mir::Type::Int;
    let error = execute(&module, "Main").expect_err("non-string receiver must be rejected");
    assert!(!error.to_string().is_empty());
}

#[test]
fn adulterated_mir_rejects_a_scalar_return_type() {
    let mut module = compile_mir(&parse_and_switch(
        "double",
        "TryParseDouble",
        "1.0",
        "value",
    ));
    let mir::Instruction::CallIntrinsic {
        destination,
        return_type,
        ..
    } = find_try_parse_float_mut(&mut module)
    else {
        unreachable!();
    };
    *return_type = mir::Type::Double;
    if let Some(mir::Place::Local(local)) = destination {
        let local = *local;
        for function in &mut module.functions {
            for declared in function
                .locals
                .iter_mut()
                .chain(function.parameters.iter_mut())
            {
                if declared.id == local {
                    declared.type_ = mir::Type::Double;
                }
            }
        }
    }
    let error = execute(&module, "Main").expect_err("a scalar return type must be rejected");
    assert!(!error.to_string().is_empty());
}

#[test]
fn adulterated_mir_rejects_the_wrong_option_specialization() {
    let source = "using aster.core;\n\
        public double Main() {\n\
            Option<float> parsed = \"1.0\".TryParseFloat();\n\
            Option<double> other = \"2.0\".TryParseDouble();\n\
            switch (other) { case Some(value): if (value > 0.0) {} case None: }\n\
            switch (parsed) { case Some(value): return (double)value; case None: return -1.0; }\n\
        }";
    let mut module = compile_mir(source);
    let double_option_symbol = module
        .enums
        .iter()
        .find(|definition| {
            definition
                .cases
                .iter()
                .any(|case| case.fields.len() == 1 && case.fields[0].type_ == mir::Type::Double)
        })
        .expect("an Option<double> specialization exists")
        .symbol;
    let mir::Instruction::CallIntrinsic {
        intrinsic,
        return_type,
        ..
    } = find_try_parse_float_mut(&mut module)
    else {
        unreachable!();
    };
    assert_eq!(*intrinsic, mir::Intrinsic::StringTryParseFloat);
    *return_type = mir::Type::Enum(double_option_symbol);
    let error = execute(&module, "Main")
        .expect_err("TryParseFloat returning Option<double> must be rejected");
    assert!(!error.to_string().is_empty());
}

#[test]
fn adulterated_mir_rejects_an_undeclared_destination_local() {
    let mut module = compile_mir(&parse_and_switch(
        "float",
        "TryParseFloat",
        "1.0",
        "(double)value",
    ));
    let mir::Instruction::CallIntrinsic { destination, .. } = find_try_parse_float_mut(&mut module)
    else {
        unreachable!();
    };
    *destination = Some(mir::Place::Local(mir::LocalId(u32::MAX)));
    let error = execute(&module, "Main").expect_err("an undeclared destination must be rejected");
    assert!(!error.to_string().is_empty());
}

#[test]
fn runtime_rejects_invalid_utf8_in_a_controlled_buffer() {
    // Exercised at the runtime layer in `aster-runtime/src/string.rs`
    // (`try_parse_float/double_rejects_invalid_utf8_...`); this end-to-end
    // test only confirms a normal parse failure via the full pipeline does
    // not surface as a runtime error.
    let source = parse_and_switch("double", "TryParseDouble", "not-a-number", "1.0");
    assert_eq!(run(&source, "Main"), Ok(ExecutionValue::Double(-1.0)));
}

#[test]
fn a_parse_failure_does_not_contaminate_a_later_independent_call() {
    let source = "using aster.core;\n\
        public double Main() {\n\
            double first = -100.0;\n\
            switch (\"nope\".TryParseDouble()) { case Some(value): first = value; case None: first = -1.0; }\n\
            double second = -100.0;\n\
            switch (\"6.5\".TryParseDouble()) { case Some(value): second = value; case None: second = -1.0; }\n\
            return first * 1000.0 + second;\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Double(-993.5)));
}

// --- Section 20.28-30: memory ------------------------------------------------

fn stats_for(source: &str) -> MemoryStats {
    let module = compile_mir(source);
    let (_, stats) = execute_with_stats(&module, "Main").expect("source should execute");
    stats
}

#[test]
fn repeated_valid_float_parses_allocate_nothing() {
    let source = "using aster.core;\n\
        public double Main() {\n\
            double total = 0.0;\n\
            for (int i = 0; i < 5000; i++) {\n\
                switch (\"1.5\".TryParseDouble()) { case Some(value): total = total + value; case None: }\n\
            }\n\
            return total;\n\
        }";
    let stats = stats_for(source);
    assert_eq!(stats.string_allocations, 0);
    assert_eq!(stats.object_allocations, 0);
    assert_eq!(stats.used_bytes, 0);
    assert_eq!(stats.total_allocations, 0);
}

#[test]
fn repeated_invalid_float_parses_allocate_nothing() {
    let source = "using aster.core;\n\
        public double Main() {\n\
            double total = 0.0;\n\
            for (int i = 0; i < 5000; i++) {\n\
                switch (\"nope\".TryParseDouble()) { case Some(value): total = total + value; case None: total = total - 1.0; }\n\
            }\n\
            return total;\n\
        }";
    let stats = stats_for(source);
    assert_eq!(stats.string_allocations, 0);
    assert_eq!(stats.object_allocations, 0);
    assert_eq!(stats.used_bytes, 0);
    assert_eq!(stats.total_allocations, 0);
}

#[test]
fn a_mix_of_valid_and_invalid_float_parses_shows_no_growth() {
    let source = "using aster.core;\n\
        public double Main() {\n\
            double total = 0.0;\n\
            for (int i = 0; i < 5000; i++) {\n\
                string text = (i % 2 == 0) ? \"1.25\" : \"nope\";\n\
                switch (text.TryParseDouble()) { case Some(value): total = total + value; case None: total = total - 1.0; }\n\
            }\n\
            return total;\n\
        }";
    let stats = stats_for(source);
    assert_eq!(stats.string_allocations, 0);
    assert_eq!(stats.object_allocations, 0);
    assert_eq!(stats.used_bytes, 0);
    assert_eq!(stats.total_allocations, 0);
}

// --- Section 20.31-35: regressions --------------------------------------------

#[test]
fn integral_and_bool_parsers_still_work() {
    let source = "using aster.core;\n\
        public int Main() {\n\
            int total = 0;\n\
            switch (\"true\".TryParseBool()) { case Some(v): total = total + (v ? 1 : 0); case None: }\n\
            switch (\"12345\".TryParseInt()) { case Some(v): total = total + v; case None: }\n\
            switch (\"42\".TryParseUInt()) { case Some(v): total = total + (int)v; case None: }\n\
            switch (\"99\".TryParseLong()) { case Some(v): total = total + (int)v; case None: }\n\
            switch (\"7\".TryParseULong()) { case Some(v): total = total + (int)v; case None: }\n\
            return total;\n\
        }";
    assert_eq!(
        run(source, "Main"),
        Ok(ExecutionValue::Int(1 + 12345 + 42 + 99 + 7))
    );
}

#[test]
fn result_question_mark_still_works() {
    let source = "using aster.core;\n\
        public Result<int, string> Parse(string text) {\n\
            if (text == \"42\") { return Result<int, string>.Ok(42); }\n\
            return Result<int, string>.Error(\"bad\"); }\n\
        public Result<int, string> Calc() { int v = Parse(\"42\")?; return Result<int, string>.Ok(v); }\n\
        public int Main() { switch (Calc()) { case Ok(v): return v; case Error(e): return -1; } }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn m1a_string_search_methods_still_work_alongside_float_parsing() {
    let source = "using aster.core;\n\
        public double Main() {\n\
            string text = \"value is 3.25\";\n\
            bool contains = text.Contains(\"3.25\");\n\
            int index = text.IndexOf(\"3.25\");\n\
            string sub = text.Substring(9);\n\
            Option<double> parsed = sub.TryParseDouble();\n\
            double value = -1.0;\n\
            switch (parsed) { case Some(v): value = v; case None: }\n\
            if (contains && index == 9) { return value; }\n\
            return -2.0;\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Double(3.25)));
}

#[test]
fn a_constructed_option_double_still_cannot_cross_a_worker_boundary_via_task_run() {
    let source = "using aster.core;\n\
        public Option<double> Make() { return \"1.0\".TryParseDouble(); }\n\
        public int Main() { Task<Option<double>> task = Task.Run(Make); return 0; }";
    let errors = match run(source, "Main") {
        Err(message) => message,
        Ok(value) => panic!("expected a compile error, got {value:?}"),
    };
    assert!(
        errors.contains("cross a worker boundary"),
        "expected the non-transferable-result diagnostic, got {errors}"
    );
}
