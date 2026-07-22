//! Postfix `?` on the official `aster.core.Option<T>`: `Some(value)`
//! continues the enclosing expression with `value`; `None` early-returns the
//! enclosing function's own `Option<U>.None` (`U` need not equal `T`). Every
//! test goes through the full pipeline (`compile_project`, matching
//! `string_try_parse.rs`'s established convention for sources needing the
//! real `aster.core` declarations).

use std::sync::atomic::{AtomicU64, Ordering};

use aster_codegen_cranelift::{ExecutionValue, MemoryStats, execute, execute_with_stats};
use aster_compiler::{compile_project, mir};

fn run(source: &str, function: &str) -> Result<ExecutionValue, String> {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "aster-option-try-{}-{id}.aster",
        std::process::id()
    ));
    std::fs::write(&path, source).expect("write temporary project");
    let compilation = compile_project(&path).map_err(|diagnostics| format!("{diagnostics:#?}"));
    std::fs::remove_file(&path).ok();
    execute(&compilation?.compilation.mir, function).map_err(|error| error.to_string())
}

fn compile_mir(source: &str) -> mir::Module {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "aster-option-try-mir-{}-{id}.aster",
        std::process::id()
    ));
    std::fs::write(&path, source).expect("write temporary project");
    let compilation = compile_project(&path).expect("source should compile");
    std::fs::remove_file(&path).ok();
    compilation.compilation.mir
}

// --- 1-2: TryParseInt()? with Some / None ----------------------------------

#[test]
fn try_parse_int_question_mark_with_some() {
    let source = "using aster.core;\n\
        public Option<int> ParsePort(string text) {\n\
            int value = text.TryParseInt()?;\n\
            return Option<int>.Some(value);\n\
        }\n\
        public int Main() { switch (ParsePort(\"8080\")) { case Some(value): return value; case None: return -1; } }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(8080)));
}

#[test]
fn try_parse_int_question_mark_with_none() {
    let source = "using aster.core;\n\
        public Option<int> ParsePort(string text) {\n\
            int value = text.TryParseInt()?;\n\
            return Option<int>.Some(value);\n\
        }\n\
        public int Main() { switch (ParsePort(\"nope\")) { case Some(value): return value; case None: return -1; } }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(-1)));
}

// --- 3: function returning Option<U> different from operand's T ------------

#[test]
fn the_function_return_type_need_not_match_the_operand_payload_type() {
    let source = "using aster.core;\n\
        public Option<string> Convert(string text) {\n\
            int value = text.TryParseInt()?;\n\
            if (value > 0) { return Option<string>.Some(\"positive\"); }\n\
            return Option<string>.None;\n\
        }\n\
        public int Main() {\n\
            switch (Convert(\"5\")) { case Some(word): return word == \"positive\" ? 1 : 0; case None: return -1; }\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(1)));
}

#[test]
fn the_function_return_type_need_not_match_and_none_still_propagates() {
    let source = "using aster.core;\n\
        public Option<string> Convert(string text) {\n\
            int value = text.TryParseInt()?;\n\
            return Option<string>.Some(\"positive\");\n\
        }\n\
        public int Main() {\n\
            switch (Convert(\"nope\")) { case Some(word): return 1; case None: return -1; }\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(-1)));
}

// --- 4: receiver evaluated exactly once -------------------------------------

#[test]
fn the_operand_is_evaluated_exactly_once() {
    let source = "using aster.core;\n\
        public class Counter {\n\
            public int calls;\n\
            public Option<int> Get() { calls = calls + 1; return Option<int>.Some(7); }\n\
        }\n\
        public Option<int> Use(Counter counter) {\n\
            int value = counter.Get()?;\n\
            return Option<int>.Some(counter.calls * 1000 + value);\n\
        }\n\
        public int Main() {\n\
            Counter counter = new Counter();\n\
            switch (Use(counter)) { case Some(value): return value; case None: return -1; }\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(1007)));
}

// --- 5: transitive helper ----------------------------------------------------

#[test]
fn a_transitive_helper_chain_propagates_none() {
    let source = "using aster.core;\n\
        public Option<int> Inner(string text) {\n\
            int value = text.TryParseInt()?;\n\
            return Option<int>.Some(value);\n\
        }\n\
        public Option<int> Outer(string text) {\n\
            int value = Inner(text)?;\n\
            return Option<int>.Some(value * 2);\n\
        }\n\
        public int Main() { switch (Outer(\"nope\")) { case Some(value): return value; case None: return -1; } }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(-1)));
}

#[test]
fn a_transitive_helper_chain_propagates_some() {
    let source = "using aster.core;\n\
        public Option<int> Inner(string text) {\n\
            int value = text.TryParseInt()?;\n\
            return Option<int>.Some(value);\n\
        }\n\
        public Option<int> Outer(string text) {\n\
            int value = Inner(text)?;\n\
            return Option<int>.Some(value * 2);\n\
        }\n\
        public int Main() { switch (Outer(\"21\")) { case Some(value): return value; case None: return -1; } }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(42)));
}

// --- 6: nested Option<Option<int>> ------------------------------------------

#[test]
fn question_mark_works_with_a_nested_option_payload() {
    let source = "using aster.core;\n\
        public Option<Option<int>> Wrap(string text) {\n\
            int value = text.TryParseInt()?;\n\
            return Option<Option<int>>.Some(Option<int>.Some(value));\n\
        }\n\
        public int Main() {\n\
            switch (Wrap(\"9\")) {\n\
                case Some(inner): switch (inner) { case Some(value): return value; case None: return -2; }\n\
                case None: return -1;\n\
            }\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(9)));
}

#[test]
fn question_mark_unwraps_an_outer_option_whose_payload_is_itself_an_option() {
    let source = "using aster.core;\n\
        public Option<Option<int>> Make(string text) {\n\
            int value = text.TryParseInt()?;\n\
            return Option<Option<int>>.Some(Option<int>.Some(value));\n\
        }\n\
        public Option<Option<int>> Reuse(string text) {\n\
            Option<int> inner = Make(text)?;\n\
            return Option<Option<int>>.Some(inner);\n\
        }\n\
        public int Main() {\n\
            switch (Reuse(\"11\")) {\n\
                case Some(inner): switch (inner) { case Some(value): return value; case None: return -2; }\n\
                case None: return -1;\n\
            }\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(11)));
}

// --- 7: Option of a reference (class identity preserved) --------------------

#[test]
fn question_mark_preserves_class_reference_identity() {
    let source = "using aster.core;\n\
        public class Counter { public int value; }\n\
        public Option<int> Use(Counter counter) {\n\
            Counter extracted = Option<Counter>.Some(counter)?;\n\
            extracted.value = extracted.value + 100;\n\
            return Option<int>.Some(extracted.value);\n\
        }\n\
        public int Main() {\n\
            Counter counter = new Counter();\n\
            counter.value = 1;\n\
            int _ignored = 0;\n\
            switch (Use(counter)) { case Some(value): _ignored = value; case None: }\n\
            return counter.value;\n\
        }";
    // `extracted` must be the exact same object as `counter`, so mutating
    // through it is visible through `counter` back in `Main`.
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(101)));
}

// --- 8: Option of a struct (copied by value) --------------------------------

#[test]
fn question_mark_works_with_a_struct_payload() {
    let source = "using aster.core;\n\
        public struct Point { public int x; public int y; }\n\
        public Option<Point> MakePoint(string text) {\n\
            int value = text.TryParseInt()?;\n\
            return Option<Point>.Some(Point { x: value, y: value * 2 });\n\
        }\n\
        public int Main() {\n\
            switch (MakePoint(\"3\")) { case Some(point): return point.x + point.y; case None: return -1; }\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(9)));
}

// --- Section 5: every target primitive plus List<int> -----------------------

#[test]
fn question_mark_works_for_every_try_parse_target_primitive() {
    let cases: [(&str, &str, &str, ExecutionValue); 5] = [
        ("bool", "TryParseBool", "true", ExecutionValue::Int(1)),
        ("int", "TryParseInt", "42", ExecutionValue::Int(42)),
        ("uint", "TryParseUInt", "42", ExecutionValue::Int(42)),
        ("long", "TryParseLong", "42", ExecutionValue::Int(42)),
        ("ulong", "TryParseULong", "42", ExecutionValue::Int(42)),
    ];
    for (target, method, text, expected) in cases {
        let some_expr = if target == "bool" {
            "value ? 1 : 0".to_owned()
        } else {
            "(int)value".to_owned()
        };
        let source = format!(
            "using aster.core;\n\
             public Option<int> Use(string text) {{\n\
                 {target} value = text.{method}()?;\n\
                 return Option<int>.Some({some_expr});\n\
             }}\n\
             public int Main() {{ switch (Use(\"{text}\")) {{ case Some(value): return value; case None: return -1; }} }}"
        );
        assert_eq!(run(&source, "Main"), Ok(expected), "target {target} failed");
    }
}

#[test]
fn question_mark_works_with_a_list_payload() {
    let source = "using aster.core;\n\
        public Option<List<int>> MakeList(string text) {\n\
            int value = text.TryParseInt()?;\n\
            List<int> values = new List<int>();\n\
            values.Add(value);\n\
            return Option<List<int>>.Some(values);\n\
        }\n\
        public int Main() {\n\
            switch (MakeList(\"6\")) { case Some(values): return values.Get(0); case None: return -1; }\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(6)));
}

// --- 9-11: rejections --------------------------------------------------------

#[test]
fn a_function_returning_a_plain_value_is_rejected() {
    let source = "using aster.core;\n\
        public int Main() { int port = \"8080\".TryParseInt()?; return port; }";
    let errors = run(source, "Main").expect_err("expected a compile error");
    assert!(
        errors.contains("requires the enclosing function to return") && errors.contains("Option"),
        "got {errors}"
    );
}

#[test]
fn a_function_returning_void_is_rejected() {
    let source = "using aster.core;\n\
        public void Main2(string text) { int port = text.TryParseInt()?; }\n\
        public int Main() { Main2(\"1\"); return 0; }";
    let errors = run(source, "Main").expect_err("expected a compile error");
    assert!(
        errors.contains("requires the enclosing function to return") && errors.contains("Option"),
        "got {errors}"
    );
}

#[test]
fn a_function_returning_result_is_rejected() {
    let source = "using aster.core;\n\
        public Result<int, string> Parse(string text) {\n\
            int value = text.TryParseInt()?;\n\
            return Result<int, string>.Ok(value);\n\
        }\n\
        public int Main() { switch (Parse(\"1\")) { case Ok(value): return value; case Error(message): return -1; } }";
    let errors = run(source, "Main")
        .expect_err("Option? inside a Result-returning function must be rejected");
    assert!(
        errors.contains("requires the enclosing function to return") && errors.contains("Option"),
        "got {errors}"
    );
}

#[test]
fn result_question_mark_inside_an_option_returning_function_is_rejected() {
    let source = "using aster.core;\n\
        public Result<int, string> Parse(string text) {\n\
            if (text == \"ok\") { return Result<int, string>.Ok(1); }\n\
            return Result<int, string>.Error(\"bad\");\n\
        }\n\
        public Option<int> Use(string text) {\n\
            int value = Parse(text)?;\n\
            return Option<int>.Some(value);\n\
        }\n\
        public int Main() { switch (Use(\"ok\")) { case Some(value): return value; case None: return -1; } }";
    let errors = run(source, "Main")
        .expect_err("Result<T,E>? inside a function returning Option<U> must be rejected");
    assert!(
        errors.contains("Result") && errors.contains("requires the enclosing function to return"),
        "got {errors}"
    );
}

#[test]
fn a_fake_option_enum_is_rejected() {
    // A user-declared, non-generic enum literally named `Option` with
    // `None`/`Some` cases, with no `using aster.core;` in scope: `?` must
    // not accept it, since only the official `aster.core.Option<T>` is
    // recognized (via `Context::official_option`, discovered from the
    // linked stdlib), never a structurally similar or same-named type.
    let source = "public enum Option { None, Some(int value) }\n\
        public Option UseFake(int input) {\n\
            int value = Option.Some(input)?;\n\
            return Option.Some(value);\n\
        }\n\
        public int Main() { return 0; }";
    let errors = run(source, "Main").expect_err("a fake `Option` enum must be rejected");
    assert!(
        errors.contains("official") || errors.contains("works only with"),
        "got {errors}"
    );
}

// --- 12-14: regressions ------------------------------------------------------

#[test]
fn result_question_mark_still_works_unchanged() {
    let source = "using aster.core;\n\
        public Result<int, string> Parse(string text) {\n\
            if (text == \"42\") { return Result<int, string>.Ok(42); }\n\
            return Result<int, string>.Error(\"bad\");\n\
        }\n\
        public Result<int, string> Calc() { int v = Parse(\"42\")?; return Result<int, string>.Ok(v); }\n\
        public int Main() { switch (Calc()) { case Ok(v): return v; case Error(e): return -1; } }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn switch_over_option_still_works_without_question_mark() {
    let source = "using aster.core;\n\
        public int Main() { Option<int> value = Option<int>.Some(42); \
        switch (value) { case Some(number): return number; case None: return 0; } }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn parsing_without_question_mark_still_works() {
    let source = "using aster.core;\n\
        public int Main() { Option<int> parsed = \"99\".TryParseInt(); \
        switch (parsed) { case Some(value): return value; case None: return -1; } }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(99)));
}

// --- 15: no additional allocation --------------------------------------------

#[test]
fn repeated_question_mark_propagation_allocates_nothing() {
    let source = "using aster.core;\n\
        public Option<int> ParsePort(string text) {\n\
            int value = text.TryParseInt()?;\n\
            return Option<int>.Some(value);\n\
        }\n\
        public int Main() {\n\
            int total = 0;\n\
            for (int i = 0; i < 5000; i++) {\n\
                string text = (i % 2 == 0) ? \"7\" : \"nope\";\n\
                switch (ParsePort(text)) { case Some(value): total = total + value; case None: total = total - 1; }\n\
            }\n\
            return total;\n\
        }";
    let module = compile_mir(source);
    let (_, stats): (ExecutionValue, MemoryStats) =
        execute_with_stats(&module, "Main").expect("source should execute");
    assert_eq!(stats.string_allocations, 0);
    assert_eq!(stats.object_allocations, 0);
    assert_eq!(stats.used_bytes, 0);
    assert_eq!(stats.total_allocations, 0);
}

// --- 16: an error does not contaminate the next call -------------------------

#[test]
fn a_none_propagation_does_not_contaminate_a_later_independent_call() {
    let source = "using aster.core;\n\
        public Option<int> ParsePort(string text) {\n\
            int value = text.TryParseInt()?;\n\
            return Option<int>.Some(value);\n\
        }\n\
        public int Main() {\n\
            int first = -100;\n\
            switch (ParsePort(\"nope\")) { case Some(value): first = value; case None: first = -1; }\n\
            int second = -100;\n\
            switch (ParsePort(\"55\")) { case Some(value): second = value; case None: second = -1; }\n\
            return first * 1000 + second;\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(-945)));
}

// --- 17: adulterated MIR must never reach the JIT ----------------------------

#[test]
fn adulterated_mir_rejects_a_none_construction_with_a_mismatched_case() {
    let source = "using aster.core;\n\
        public Option<int> ParsePort(string text) {\n\
            int value = text.TryParseInt()?;\n\
            return Option<int>.Some(value);\n\
        }\n\
        public int Main() { switch (ParsePort(\"8080\")) { case Some(value): return value; case None: return -1; } }";
    let mut module = compile_mir(source);
    let parse_port = module
        .functions
        .iter_mut()
        .find(|function| function.name == "ParsePort")
        .expect("ParsePort is declared");
    let mut mutated = false;
    for block in &mut parse_port.blocks {
        for instruction in &mut block.instructions {
            if let mir::Instruction::Assign {
                value:
                    mir::Rvalue {
                        kind: mir::RvalueKind::EnumConstruct { case, tag, fields },
                        ..
                    },
                ..
            } = instruction
                && fields.is_empty()
            {
                // This is the `None` early-return construction; point its
                // case symbol at a nonexistent one and corrupt its tag.
                *case = mir::SymbolId(u32::MAX);
                *tag = 999;
                mutated = true;
            }
        }
    }
    assert!(mutated, "the None EnumConstruct instruction was not found");
    let error = execute(&module, "Main").expect_err("a mismatched None case must be rejected");
    assert!(!error.to_string().is_empty());
}

#[test]
fn adulterated_mir_rejects_a_discriminant_over_a_non_enum_operand() {
    let source = "using aster.core;\n\
        public Option<int> ParsePort(string text) {\n\
            int value = text.TryParseInt()?;\n\
            return Option<int>.Some(value);\n\
        }\n\
        public int Main() { switch (ParsePort(\"8080\")) { case Some(value): return value; case None: return -1; } }";
    let mut module = compile_mir(source);
    let parse_port = module
        .functions
        .iter_mut()
        .find(|function| function.name == "ParsePort")
        .expect("ParsePort is declared");
    let mut mutated = false;
    for block in &mut parse_port.blocks {
        for instruction in &mut block.instructions {
            if let mir::Instruction::Assign {
                value:
                    mir::Rvalue {
                        kind: mir::RvalueKind::Discriminant(operand),
                        ..
                    },
                ..
            } = instruction
            {
                operand.type_ = mir::Type::Int;
                mutated = true;
            }
        }
    }
    assert!(mutated, "the Discriminant instruction was not found");
    let error =
        execute(&module, "Main").expect_err("a discriminant over a non-enum type must be rejected");
    assert!(!error.to_string().is_empty());
}
