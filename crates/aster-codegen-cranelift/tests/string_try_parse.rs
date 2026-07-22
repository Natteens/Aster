//! End-to-end tests for M1B1: deterministic `string.TryParse*()` -> `Option<T>`
//! for `bool`, `int`, `uint`, `long`, `ulong`. Every case goes through the
//! full pipeline (parser, semantic analysis, generics/monomorphization, HIR,
//! MIR, escape analysis, codegen, JIT execution) via `compile_project` with
//! `using aster.core;`, matching `result_propagation_jit.rs`'s established
//! convention for tests that need the real `Option`/`Result` declarations.

use std::sync::atomic::{AtomicU64, Ordering};

use aster_codegen_cranelift::{ExecutionValue, MemoryStats, execute, execute_with_stats};
use aster_compiler::{compile_project, mir};

fn compile_mir(source: &str) -> mir::Module {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "aster-try-parse-mir-{}-{id}.aster",
        std::process::id()
    ));
    std::fs::write(&path, source).expect("write temporary project");
    let compilation = compile_project(&path).expect("source should compile");
    std::fs::remove_file(&path).ok();
    compilation.compilation.mir
}

/// Finds the sole `CallIntrinsic` instruction for one of the five
/// `TryParse*` intrinsics in `module`, for adversarial mutation.
fn find_try_parse_mut(module: &mut mir::Module) -> &mut mir::Instruction {
    module
        .functions
        .iter_mut()
        .flat_map(|function| &mut function.blocks)
        .flat_map(|block| &mut block.instructions)
        .find(|instruction| {
            matches!(
                instruction,
                mir::Instruction::CallIntrinsic {
                    intrinsic: mir::Intrinsic::StringTryParseBool
                        | mir::Intrinsic::StringTryParseInt
                        | mir::Intrinsic::StringTryParseUInt
                        | mir::Intrinsic::StringTryParseLong
                        | mir::Intrinsic::StringTryParseULong,
                    ..
                }
            )
        })
        .expect("a TryParse* intrinsic call exists")
}

fn run(source: &str, function: &str) -> Result<ExecutionValue, String> {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("aster-try-parse-{}-{id}.aster", std::process::id()));
    std::fs::write(&path, source).expect("write temporary project");
    let compilation = compile_project(&path).map_err(|diagnostics| format!("{diagnostics:#?}"));
    std::fs::remove_file(&path).ok();
    execute(&compilation?.compilation.mir, function).map_err(|error| error.to_string())
}

fn compile_errors(source: &str) -> String {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "aster-try-parse-err-{}-{id}.aster",
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

/// Builds `Option<{option_type}> parsed = "{text}".{method}(); switch
/// (parsed) { case Some(value): return {some_expr}; case None: return -1; }`
/// wrapped in `public {return_type} Main()`, with `using aster.core;`.
fn parse_and_switch(
    return_type: &str,
    option_type: &str,
    method: &str,
    text: &str,
    some_expr: &str,
    none_value: &str,
) -> String {
    format!(
        "using aster.core;\n\
         public {return_type} Main() {{\n\
             Option<{option_type}> parsed = \"{text}\".{method}();\n\
             switch (parsed) {{ case Some(value): return {some_expr}; case None: return {none_value}; }}\n\
         }}"
    )
}

// --- Section 19.1-6: semantics ------------------------------------------

#[test]
fn five_signatures_resolve_and_return_expected_values() {
    let cases: [(&str, &str, &str, &str, ExecutionValue); 5] = [
        (
            "bool",
            "TryParseBool",
            "true",
            "value ? 1 : 0",
            ExecutionValue::Int(1),
        ),
        ("int", "TryParseInt", "42", "value", ExecutionValue::Int(42)),
        (
            "uint",
            "TryParseUInt",
            "42",
            "(int)value",
            ExecutionValue::Int(42),
        ),
        (
            "long",
            "TryParseLong",
            "42",
            "(int)value",
            ExecutionValue::Int(42),
        ),
        (
            "ulong",
            "TryParseULong",
            "42",
            "(int)value",
            ExecutionValue::Int(42),
        ),
    ];
    for (option_type, method, text, some_expr, expected) in cases {
        let source = parse_and_switch("int", option_type, method, text, some_expr, "-1");
        assert_eq!(run(&source, "Main"), Ok(expected), "method {method} failed");
    }
}

#[test]
fn extra_arguments_are_rejected() {
    let errors = compile_errors(
        "using aster.core;\n\
         public int Main() { Option<int> parsed = \"1\".TryParseInt(10); return 0; }",
    );
    assert!(
        errors.contains("TryParseInt") && errors.contains("0 argument"),
        "expected an arity diagnostic, got {errors}"
    );
    let errors = compile_errors(
        "using aster.core;\n\
         public int Main() { Option<bool> parsed = \"true\".TryParseBool(false); return 0; }",
    );
    assert!(
        errors.contains("TryParseBool") && errors.contains("0 argument"),
        "expected an arity diagnostic, got {errors}"
    );
}

#[test]
fn a_non_string_receiver_does_not_activate_the_intrinsic() {
    // A user class with its own zero-argument `TryParseInt` keeps using
    // ordinary instance-method resolution; the receiver-type gate is
    // `receiver == Type::String`, not the method name.
    let source = "public class Widget {\n\
        public int TryParseInt() { return 7; }\n\
    }\n\
    public int Main() { Widget widget = new Widget(); return widget.TryParseInt(); }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(7)));
}

#[test]
fn receiver_is_evaluated_exactly_once() {
    let source = "using aster.core;\n\
        public class Counter {\n\
            public int calls;\n\
            public string GetText() { calls = calls + 1; return \"99\"; }\n\
        }\n\
        public int Main() {\n\
            Counter counter = new Counter();\n\
            Option<int> parsed = counter.GetText().TryParseInt();\n\
            switch (parsed) { case Some(value): return counter.calls * 1000 + value; case None: return -1; }\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(1099)));
}

// --- Section 19.7: bool ---------------------------------------------------

#[test]
fn bool_valid_and_invalid_cases() {
    let cases: [(&str, Option<bool>); 10] = [
        ("true", Some(true)),
        ("false", Some(false)),
        ("TRUE", Some(true)),
        ("False", Some(false)),
        ("fAlSe", Some(false)),
        ("1", None),
        ("0", None),
        ("yes", None),
        ("no", None),
        ("verdadeiro", None),
    ];
    for (text, expected) in cases {
        let source = parse_and_switch("int", "bool", "TryParseBool", text, "value ? 1 : 0", "-1");
        let expected_value = match expected {
            Some(true) => 1,
            Some(false) => 0,
            None => -1,
        };
        assert_eq!(
            run(&source, "Main"),
            Ok(ExecutionValue::Int(expected_value)),
            "\"{text}\".TryParseBool()"
        );
    }
}

#[test]
fn bool_rejects_empty_and_whitespace() {
    for text in ["", " true", "false "] {
        let source = format!(
            "using aster.core;\n\
             public int Main() {{\n\
                 Option<bool> parsed = \"{text}\".TryParseBool();\n\
                 switch (parsed) {{ case Some(value): return value ? 1 : 0; case None: return -1; }}\n\
             }}"
        );
        assert_eq!(
            run(&source, "Main"),
            Ok(ExecutionValue::Int(-1)),
            "\"{text}\".TryParseBool() should be None"
        );
    }
}

// --- Section 19.8-11: signed and unsigned integers ------------------------

#[test]
fn int_valid_invalid_and_boundary_cases() {
    let cases: [(&str, Option<i32>); 12] = [
        ("0", Some(0)),
        ("+0", Some(0)),
        ("-0", Some(0)),
        ("1", Some(1)),
        ("-1", Some(-1)),
        ("000123", Some(123)),
        ("-2147483648", Some(i32::MIN)),
        ("2147483647", Some(i32::MAX)),
        ("-2147483649", None),
        ("2147483648", None),
        ("+", None),
        ("-", None),
    ];
    for (text, expected) in cases {
        let source = parse_and_switch("int", "int", "TryParseInt", text, "value", "-999999");
        let expected_value = expected.unwrap_or(-999_999);
        assert_eq!(
            run(&source, "Main"),
            Ok(ExecutionValue::Int(expected_value)),
            "\"{text}\".TryParseInt()"
        );
    }
}

#[test]
fn int_rejects_garbage_whitespace_and_unicode_digits() {
    for text in ["12x", " 123", "123 ", "1_000", "\u{FF11}\u{FF12}\u{FF13}"] {
        let source = format!(
            "using aster.core;\n\
             public int Main() {{\n\
                 Option<int> parsed = \"{text}\".TryParseInt();\n\
                 switch (parsed) {{ case Some(value): return value; case None: return -1; }}\n\
             }}"
        );
        assert_eq!(
            run(&source, "Main"),
            Ok(ExecutionValue::Int(-1)),
            "\"{text}\".TryParseInt() should be None"
        );
    }
}

#[test]
fn long_valid_invalid_and_boundary_cases() {
    let cases: [(&str, bool, i64); 8] = [
        ("0", true, 0),
        ("+0", true, 0),
        ("-9223372036854775808", true, i64::MIN),
        ("9223372036854775807", true, i64::MAX),
        ("-9223372036854775809", false, 0),
        ("9223372036854775808", false, 0),
        ("+", false, 0),
        ("-", false, 0),
    ];
    for (text, is_some, expected_value) in cases {
        let source = format!(
            "using aster.core;\n\
             public long Main() {{\n\
                 Option<long> parsed = \"{text}\".TryParseLong();\n\
                 switch (parsed) {{ case Some(value): return value; case None: return -999999L; }}\n\
             }}"
        );
        let expected = if is_some {
            ExecutionValue::Long(expected_value)
        } else {
            ExecutionValue::Long(-999_999)
        };
        assert_eq!(
            run(&source, "Main"),
            Ok(expected),
            "\"{text}\".TryParseLong()"
        );
    }
}

#[test]
fn uint_valid_invalid_and_boundary_cases() {
    let cases: [(&str, Option<u32>); 8] = [
        ("0", Some(0)),
        ("+0", Some(0)),
        ("42", Some(42)),
        ("+42", Some(42)),
        ("4294967295", Some(u32::MAX)),
        ("4294967296", None),
        ("-0", None),
        ("-1", None),
    ];
    for (text, expected) in cases {
        let source = format!(
            "using aster.core;\n\
             public long Main() {{\n\
                 Option<uint> parsed = \"{text}\".TryParseUInt();\n\
                 switch (parsed) {{ case Some(value): return (long)value; case None: return -1L; }}\n\
             }}"
        );
        let expected_value = expected.map_or(-1, i64::from);
        assert_eq!(
            run(&source, "Main"),
            Ok(ExecutionValue::Long(expected_value)),
            "\"{text}\".TryParseUInt()"
        );
    }
}

#[test]
fn uint_rejects_whitespace_and_invalid_characters() {
    for text in [" 1", "1 ", "1x"] {
        let source = format!(
            "using aster.core;\n\
             public int Main() {{\n\
                 Option<uint> parsed = \"{text}\".TryParseUInt();\n\
                 switch (parsed) {{ case Some(value): return 1; case None: return -1; }}\n\
             }}"
        );
        assert_eq!(
            run(&source, "Main"),
            Ok(ExecutionValue::Int(-1)),
            "\"{text}\".TryParseUInt() should be None"
        );
    }
}

#[test]
fn ulong_valid_invalid_and_boundary_cases() {
    let cases: [(&str, bool, u64); 6] = [
        ("0", true, 0),
        ("+0", true, 0),
        ("18446744073709551615", true, u64::MAX),
        ("18446744073709551616", false, 0),
        ("-0", false, 0),
        ("-1", false, 0),
    ];
    for (text, is_some, expected_value) in cases {
        let source = format!(
            "using aster.core;\n\
             public long Main() {{\n\
                 Option<ulong> parsed = \"{text}\".TryParseULong();\n\
                 switch (parsed) {{ case Some(value): return (long)value; case None: return -1L; }}\n\
             }}"
        );
        #[allow(clippy::cast_possible_wrap)]
        let expected = if is_some {
            ExecutionValue::Long(expected_value as i64)
        } else {
            ExecutionValue::Long(-1)
        };
        assert_eq!(
            run(&source, "Main"),
            Ok(expected),
            "\"{text}\".TryParseULong()"
        );
    }
}

// --- Section 19.12-16: integration ----------------------------------------

#[test]
fn plain_option_still_works_without_try_parse() {
    let source = "using aster.core;\n\
        public int Main() { Option<int> value = Option<int>.Some(42); \
        switch (value) { case Some(number): return number; case None: return 0; } }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn postfix_try_on_option_requires_the_enclosing_function_to_return_option() {
    // `Option<T>?` is now supported, but only inside a function that
    // returns the official `aster.core.Option<U>`; a plain scalar return
    // must still be rejected, with the updated diagnostic.
    let source = "using aster.core;\n\
        public int Main() { int port = \"8080\".TryParseInt()?; return port; }";
    let errors = match run(source, "Main") {
        Err(message) => message,
        Ok(value) => panic!("expected a compile error, got {value:?}"),
    };
    assert!(
        errors.contains("requires the enclosing function to return") && errors.contains("Option"),
        "expected the function-return-type diagnostic, got {errors}"
    );
}

#[test]
fn a_helper_returning_option_composes_via_switch() {
    let source = "using aster.core;\n\
        public Option<int> ParsePort(string text) {\n\
            Option<int> parsed = text.TryParseInt();\n\
            switch (parsed) {\n\
                case Some(port): if (port <= 0) { return Option<int>.None; } return Option<int>.Some(port);\n\
                case None: return Option<int>.None;\n\
            }\n\
        }\n\
        public int Main() { switch (ParsePort(\"8080\")) { case Some(value): return value; case None: return -1; } }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(8080)));
}

#[test]
fn try_parse_works_inside_a_generic_function() {
    let source = "using aster.core;\n\
        public T First<T>(T a, T b) { return a; }\n\
        public int Main() {\n\
            Option<int> parsed = First(\"123\", \"456\").TryParseInt();\n\
            switch (parsed) { case Some(value): return value; case None: return -1; }\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(123)));
}

#[test]
fn try_parse_works_across_namespaces() {
    // `namespace app;` must live under a directory literally named `app`,
    // matching this compiler's directory-namespace convention.
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let project_root =
        std::env::temp_dir().join(format!("aster-try-parse-ns-{}-{id}", std::process::id()));
    let directory = project_root.join("app");
    std::fs::create_dir_all(&directory).expect("create namespace directory");
    // A manifest establishes `project_root` as the directory `app`'s
    // relative path is measured from; without one, the loader falls back to
    // the root file's own directory, which would make `app` look like the
    // global namespace instead.
    std::fs::write(
        project_root.join("Aster.toml"),
        "[application]\nentry = \"app.Main\"\n",
    )
    .expect("write manifest");
    let path = directory.join("main.aster");
    let source = "namespace app;\n\
        using aster.core;\n\
        public int Main() {\n\
            Option<int> parsed = \"777\".TryParseInt();\n\
            switch (parsed) { case Some(value): return value; case None: return -1; }\n\
        }";
    std::fs::write(&path, source).expect("write namespaced project file");
    let compilation = compile_project(&path).map_err(|diagnostics| format!("{diagnostics:#?}"));
    std::fs::remove_dir_all(
        directory
            .parent()
            .expect("namespace directory has a parent"),
    )
    .ok();
    let compilation = compilation.expect("namespaced source should compile");
    assert_eq!(
        execute(&compilation.compilation.mir, "Main"),
        Ok(ExecutionValue::Int(777))
    );
}

#[test]
fn long_running_calls_produce_consistent_results() {
    let source = "using aster.core;\n\
        public int Main() {\n\
            int total = 0;\n\
            for (int i = 0; i < 200; i++) {\n\
                Option<int> parsed = \"10\".TryParseInt();\n\
                switch (parsed) { case Some(value): total = total + value; case None: total = total - 1; }\n\
            }\n\
            return total;\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(2000)));
}

// --- Section 19.27-31: regressions -----------------------------------------

#[test]
fn m1a_string_search_methods_still_work_alongside_try_parse() {
    let source = "using aster.core;\n\
        public int Main() {\n\
            string text = \"hello world\";\n\
            bool contains = text.Contains(\"world\");\n\
            bool starts = text.StartsWith(\"hello\");\n\
            bool ends = text.EndsWith(\"world\");\n\
            int index = text.IndexOf(\"world\");\n\
            string sub = text.Substring(6);\n\
            Option<int> parsed = \"99\".TryParseInt();\n\
            int value = -1;\n\
            switch (parsed) { case Some(v): value = v; case None: }\n\
            int ok = (contains ? 1 : 0) + (starts ? 1 : 0) + (ends ? 1 : 0);\n\
            if (index == 6 && sub == \"world\" && ok == 3) { return value; }\n\
            return -1;\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(99)));
}

#[test]
fn concatenation_and_interpolation_still_work_alongside_try_parse() {
    let source = "using aster.core;\n\
        public int Main() {\n\
            string a = \"12\" + \"3\";\n\
            string b = $\"value={a}\";\n\
            Option<int> parsed = a.TryParseInt();\n\
            switch (parsed) { case Some(value): return b.Length > 0 ? value : -2; case None: return -1; }\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(123)));
}

#[test]
fn option_and_result_still_work_alongside_try_parse() {
    let source = "using aster.core;\n\
        public Result<int, string> Parse(string text) {\n\
            Option<int> parsed = text.TryParseInt();\n\
            switch (parsed) {\n\
                case Some(value): return Result<int, string>.Ok(value);\n\
                case None: return Result<int, string>.Error(\"bad\");\n\
            }\n\
        }\n\
        public int Main() {\n\
            switch (Parse(\"55\")) { case Ok(value): return value; case Error(message): return -1; }\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(55)));
}

#[test]
fn a_constructed_option_still_cannot_cross_a_worker_boundary_via_task_run() {
    let source = "using aster.core;\n\
        public Option<int> Make() { return \"1\".TryParseInt(); }\n\
        public int Main() { Task<Option<int>> task = Task.Run(Make); return 0; }";
    let errors = match run(source, "Main") {
        Err(message) => message,
        Ok(value) => panic!("expected a compile error, got {value:?}"),
    };
    assert!(
        errors.contains("cross a worker boundary"),
        "expected the non-transferable-result diagnostic, got {errors}"
    );
}

// --- Section 19.17-21: adulterated MIR must never reach the JIT -----------
//
// Each test hand-mutates a real, compiled `TryParse*` `CallIntrinsic`
// instruction and confirms `execute` (which validates the whole module
// before ever generating code) rejects it with a controlled error, never a
// panic or a silently-accepted wrong result.

const TRY_PARSE_SOURCE: &str = "using aster.core;\n\
    public int Main() {\n\
        Option<int> parsed = \"1\".TryParseInt();\n\
        switch (parsed) { case Some(value): return value; case None: return -1; }\n\
    }";

#[test]
fn adulterated_mir_rejects_a_non_string_receiver() {
    let mut module = compile_mir(TRY_PARSE_SOURCE);
    let mir::Instruction::CallIntrinsic { arguments, .. } = find_try_parse_mut(&mut module) else {
        unreachable!();
    };
    arguments[0].type_ = mir::Type::Int;
    let error = execute(&module, "Main").expect_err("non-string receiver must be rejected");
    assert!(!error.to_string().is_empty());
}

#[test]
fn adulterated_mir_rejects_a_scalar_return_type() {
    let mut module = compile_mir(TRY_PARSE_SOURCE);
    let mir::Instruction::CallIntrinsic {
        destination,
        return_type,
        ..
    } = find_try_parse_mut(&mut module)
    else {
        unreachable!();
    };
    *return_type = mir::Type::Int;
    // Keep the destination local's declared type in sync so this mutation
    // isolates the intrinsic-shape check, not the separate destination-type
    // consistency check.
    if let Some(mir::Place::Local(local)) = destination {
        let local = *local;
        for function in &mut module.functions {
            for declared in function
                .locals
                .iter_mut()
                .chain(function.parameters.iter_mut())
            {
                if declared.id == local {
                    declared.type_ = mir::Type::Int;
                }
            }
        }
    }
    let error = execute(&module, "Main").expect_err("a scalar return type must be rejected");
    assert!(!error.to_string().is_empty());
}

#[test]
fn adulterated_mir_rejects_an_undeclared_destination_local() {
    let mut module = compile_mir(TRY_PARSE_SOURCE);
    let mir::Instruction::CallIntrinsic { destination, .. } = find_try_parse_mut(&mut module)
    else {
        unreachable!();
    };
    *destination = Some(mir::Place::Local(mir::LocalId(u32::MAX)));
    let error = execute(&module, "Main").expect_err("an undeclared destination must be rejected");
    assert!(!error.to_string().is_empty());
}

#[test]
fn adulterated_mir_rejects_the_wrong_option_specialization() {
    // Compile a source that also specializes `Option<bool>`, so both
    // specializations exist as concrete enums in the same module; then point
    // `TryParseInt`'s return type at `Option<bool>`'s symbol instead of
    // `Option<int>`'s.
    let source = "using aster.core;\n\
        public int Main() {\n\
            Option<int> parsed = \"1\".TryParseInt();\n\
            Option<bool> other = \"true\".TryParseBool();\n\
            switch (other) { case Some(flag): if (flag) {} case None: }\n\
            switch (parsed) { case Some(value): return value; case None: return -1; }\n\
        }";
    let mut module = compile_mir(source);
    let bool_option_symbol = module
        .enums
        .iter()
        .find(|definition| {
            definition
                .cases
                .iter()
                .any(|case| case.fields.len() == 1 && case.fields[0].type_ == mir::Type::Bool)
        })
        .expect("an Option<bool> specialization exists")
        .symbol;
    let mir::Instruction::CallIntrinsic {
        intrinsic,
        return_type,
        ..
    } = find_try_parse_mut(&mut module)
    else {
        unreachable!();
    };
    assert_eq!(*intrinsic, mir::Intrinsic::StringTryParseInt);
    *return_type = mir::Type::Enum(bool_option_symbol);
    let error =
        execute(&module, "Main").expect_err("TryParseInt returning Option<bool> must be rejected");
    assert!(!error.to_string().is_empty());
}

#[test]
fn adulterated_mir_rejects_a_destination_type_mismatched_with_the_intrinsic() {
    // The destination local's own declared type disagrees with the
    // intrinsic's `return_type` (a different kind of "wrong specialization"
    // than the previous test: here `return_type` itself is still a
    // well-formed `Option<int>`, but the place storing the result is not
    // declared as that type).
    let mut module = compile_mir(TRY_PARSE_SOURCE);
    let mir::Instruction::CallIntrinsic { destination, .. } = find_try_parse_mut(&mut module)
    else {
        unreachable!();
    };
    let Some(mir::Place::Local(local)) = *destination else {
        unreachable!("destination is a local");
    };
    for function in &mut module.functions {
        for declared in function
            .locals
            .iter_mut()
            .chain(function.parameters.iter_mut())
        {
            if declared.id == local {
                declared.type_ = mir::Type::Int;
            }
        }
    }
    let error =
        execute(&module, "Main").expect_err("a mismatched destination local must be rejected");
    assert!(!error.to_string().is_empty());
}

// --- Section 19.24-26: memory ----------------------------------------------

fn stats_for(source: &str) -> MemoryStats {
    let module = compile_mir(source);
    let (_, stats) = execute_with_stats(&module, "Main").expect("source should execute");
    stats
}

#[test]
fn repeated_valid_parses_allocate_no_strings_or_objects() {
    let source = "using aster.core;\n\
        public int Main() {\n\
            int total = 0;\n\
            for (int i = 0; i < 5000; i++) {\n\
                Option<int> parsed = \"12345\".TryParseInt();\n\
                switch (parsed) { case Some(value): total = total + value; case None: }\n\
            }\n\
            return total;\n\
        }";
    let stats = stats_for(source);
    assert_eq!(stats.string_allocations, 0);
    assert_eq!(stats.object_allocations, 0);
    assert_eq!(stats.used_bytes, 0);
}

#[test]
fn repeated_invalid_parses_allocate_no_strings_or_objects() {
    let source = "using aster.core;\n\
        public int Main() {\n\
            int total = 0;\n\
            for (int i = 0; i < 5000; i++) {\n\
                Option<int> parsed = \"not-a-number\".TryParseInt();\n\
                switch (parsed) { case Some(value): total = total + value; case None: total = total - 1; }\n\
            }\n\
            return total;\n\
        }";
    let stats = stats_for(source);
    assert_eq!(stats.string_allocations, 0);
    assert_eq!(stats.object_allocations, 0);
    assert_eq!(stats.used_bytes, 0);
}

#[test]
fn a_mix_of_valid_and_invalid_parses_shows_no_growth() {
    let source = "using aster.core;\n\
        public int Main() {\n\
            int total = 0;\n\
            for (int i = 0; i < 5000; i++) {\n\
                string text = (i % 2 == 0) ? \"7\" : \"nope\";\n\
                Option<int> parsed = text.TryParseInt();\n\
                switch (parsed) { case Some(value): total = total + value; case None: total = total - 1; }\n\
            }\n\
            return total;\n\
        }";
    let stats = stats_for(source);
    assert_eq!(stats.string_allocations, 0);
    assert_eq!(stats.object_allocations, 0);
    assert_eq!(stats.used_bytes, 0);
    assert_eq!(stats.total_allocations, 0);
}

// --- Option<T>? -------------------------------------------------------

#[test]
fn smoke_option_try_propagates_some() {
    let source = "using aster.core;\n\
        public Option<int> ParsePort(string text) {\n\
            int value = text.TryParseInt()?;\n\
            return Option<int>.Some(value);\n\
        }\n\
        public int Main() { switch (ParsePort(\"8080\")) { case Some(value): return value; case None: return -1; } }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(8080)));
}

#[test]
fn smoke_option_try_propagates_none() {
    let source = "using aster.core;\n\
        public Option<int> ParsePort(string text) {\n\
            int value = text.TryParseInt()?;\n\
            return Option<int>.Some(value);\n\
        }\n\
        public int Main() { switch (ParsePort(\"nope\")) { case Some(value): return value; case None: return -1; } }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(-1)));
}
