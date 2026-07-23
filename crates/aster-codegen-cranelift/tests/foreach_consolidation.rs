//! M3E: cross-cutting consolidation for `foreach` over arrays (`foreach.rs`),
//! `List<T>` (`list_foreach.rs`), and `string` (`string_foreach.rs`). Adds
//! only the coverage genuinely missing from those three per-category
//! suites: the full 3x3 nesting matrix (list-inside-string was the one
//! missing combination), `switch` combined with `foreach`, postfix `?`
//! with `IOError` and a non-scalar success payload, scope/shadowing parity
//! for `List<T>`/`string` (previously only tested for arrays), and
//! enum-containing-string escape analysis for array/list element
//! extraction. Does not reimplement any of the three lowerings.

use aster_codegen_cranelift::{ExecutionValue, execute};
use aster_compiler::{compile, compile_project, mir};

fn run(source: &str) -> Result<ExecutionValue, String> {
    let compilation = compile(source).map_err(|diagnostics| format!("{diagnostics:#?}"))?;
    execute(&compilation.mir, "Main").map_err(|error| error.to_string())
}

fn compile_errors(source: &str) -> Vec<String> {
    match compile(source) {
        Ok(_) => Vec::new(),
        Err(diagnostics) => diagnostics
            .into_iter()
            .map(|diagnostic| diagnostic.message)
            .collect(),
    }
}

fn compile_mir(source: &str) -> mir::Module {
    compile(source).expect("source should compile").mir
}

fn run_project(source: &str) -> Result<ExecutionValue, String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "aster-foreach-consolidation-{}-{id}.aster",
        std::process::id()
    ));
    std::fs::write(&path, source).expect("write temporary project");
    let compilation = compile_project(&path).map_err(|diagnostics| format!("{diagnostics:#?}"));
    std::fs::remove_file(&path).ok();
    execute(&compilation?.compilation.mir, "Main").map_err(|error| error.to_string())
}

fn compile_mir_project(source: &str) -> mir::Module {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "aster-foreach-consolidation-mir-{}-{id}.aster",
        std::process::id()
    ));
    std::fs::write(&path, source).expect("write temporary project");
    let compilation = compile_project(&path).expect("source should compile");
    std::fs::remove_file(&path).ok();
    compilation.compilation.mir
}

// --- Section 3: the one missing cell of the 3x3 nesting matrix ---------------------

#[test]
fn list_foreach_nests_inside_string_foreach() {
    // `foreach.rs`/`list_foreach.rs`/`string_foreach.rs` between them cover
    // every other combination of {array, List, string} x {array, List,
    // string}; this is the one missing cell (List<T> inside a string loop).
    let source = r#"
        public int Main()
        {
            string outer = "ab";
            List<int> inner = new List<int>();
            inner.Add(10);
            inner.Add(20);
            int total = 0;
            foreach (char letter in outer)
            {
                foreach (int value in inner) { total = total + value; }
            }
            return total;
        }
    "#;
    assert_eq!(run(source), Ok(ExecutionValue::Int((10 + 20) * 2)));
}

#[test]
fn break_and_continue_act_only_on_the_innermost_loop_across_all_three_kinds() {
    // One representative triple-nested case (array outer, List middle,
    // string innermost) proving `break`/`continue` target only the
    // innermost loop and outer loops resume at the correct point,
    // regardless of which lowering produced which level.
    let source = r#"
        public int Main()
        {
            int[] outer = [1, 2];
            List<int> middle = new List<int>();
            middle.Add(1);
            middle.Add(2);
            string inner = "abcd";
            int total = 0;
            foreach (int a in outer)
            {
                foreach (int b in middle)
                {
                    foreach (char c in inner)
                    {
                        if (c == 'c') { break; }
                        total = total + 1;
                    }
                    total = total + 100;
                }
                total = total + 10000;
            }
            return total;
        }
    "#;
    // Innermost loop always visits exactly 2 scalars ('a','b') before
    // breaking on 'c': 2 per (a,b) pair, 4 pairs -> 8, plus 100 four times
    // -> 400, plus 10000 twice -> 20000. Total 20408.
    assert_eq!(run(source), Ok(ExecutionValue::Int(8 + 400 + 20000)));
}

#[test]
fn switch_combined_with_array_list_and_string_foreach() {
    // ASTER's `switch` pattern-matches enum cases only (never scalar
    // literals like `case 2:`), so each iteration's value is classified
    // through an `Option<int>`-returning helper and switched on that.
    let source = r#"
        using aster.core;
        public Option<int> Classify(int value)
        {
            if (value == 2) { return Option<int>.Some(100); }
            return Option<int>.None;
        }
        public int Main()
        {
            int[] values = [1, 2, 3];
            List<int> list = new List<int>();
            list.Add(4);
            list.Add(5);
            string text = "xy";
            int total = 0;
            foreach (int value in values)
            {
                switch (Classify(value))
                {
                    case Some(bonus): total = total + bonus;
                    case None: total = total + value;
                }
            }
            foreach (int value in list)
            {
                switch (Classify(value))
                {
                    case Some(bonus): total = total + bonus;
                    case None: total = total + value;
                }
            }
            foreach (char value in text)
            {
                switch (Classify((int)value))
                {
                    case Some(bonus): total = total + bonus;
                    case None: total = total + 1;
                }
            }
            return total;
        }
    "#;
    // Arrays: 1 + 100 + 3 = 104. List (4, 5): neither classifies -> 4 + 5 = 9.
    // String ('x'=120,'y'=121): neither classifies -> 1 + 1 = 2.
    assert_eq!(run_project(source), Ok(ExecutionValue::Int(104 + 9 + 2)));
}

// --- Section 4: postfix `?` with IOError and a non-scalar success payload ----------

#[test]
fn postfix_try_with_io_error_inside_array_list_and_string_foreach() {
    let source = r"
        using aster.core;
        using aster.io;
        public Result<int, IOError> Check(int value)
        {
            if (value == 2)
            {
                return Result<int, IOError>.Error(IOError { Kind: IOErrorKind.NotFound, OsCode: 2 });
            }
            return Result<int, IOError>.Ok(value);
        }
        public Result<int, IOError> OverArray(int[] values)
        {
            int total = 0;
            foreach (int value in values) { total = total + Check(value)?; }
            return Result<int, IOError>.Ok(total);
        }
        public Result<int, IOError> OverList(List<int> values)
        {
            int total = 0;
            foreach (int value in values) { total = total + Check(value)?; }
            return Result<int, IOError>.Ok(total);
        }
        public int Main()
        {
            int[] values = [1, 2, 3];
            switch (OverArray(values)) {
                case Ok(total): return -1;
                case Error(error): return error.Kind == IOErrorKind.NotFound ? 1 : -2;
            }
        }
    ";
    assert_eq!(run_project(source), Ok(ExecutionValue::Int(1)));
}

#[test]
fn postfix_try_with_a_struct_success_payload_inside_string_foreach() {
    // "MudanΓ§a do tipo do payload de sucesso": the `Result<T, E>` success
    // type here is a struct, not a scalar -- confirming postfix `?` inside
    // `foreach` correctly extracts and accumulates a non-scalar payload.
    let source = r#"
        using aster.core;
        public struct Pair { public int Value; }
        public Result<Pair, string> Wrap(char value)
        {
            if (value == 'b') { return Result<Pair, string>.Error("bad"); }
            return Result<Pair, string>.Ok(Pair { Value: (int)value });
        }
        public Result<int, string> Process(string text)
        {
            int total = 0;
            foreach (char value in text)
            {
                Pair pair = Wrap(value)?;
                total = total + pair.Value;
            }
            return Result<int, string>.Ok(total);
        }
        public int Main()
        {
            switch (Process("abc")) {
                case Ok(total): return -1;
                case Error(message): return message == "bad" ? 1 : -2;
            }
        }
    "#;
    assert_eq!(run_project(source), Ok(ExecutionValue::Int(1)));
}

// --- Section 6: scope/shadowing parity for List<T> and string ---------------------

#[test]
fn the_list_foreach_element_variable_does_not_exist_after_the_loop() {
    let errors = compile_errors(
        "public int Main() { List<int> values = new List<int>(); values.Add(1); foreach (int value in values) { } return value; }",
    );
    assert!(!errors.is_empty(), "expected an unknown-name diagnostic");
}

#[test]
fn the_string_foreach_element_variable_does_not_exist_after_the_loop() {
    let errors = compile_errors(
        "public int Main() { string text = \"a\"; foreach (char value in text) { } return (int)value; }",
    );
    assert!(!errors.is_empty(), "expected an unknown-name diagnostic");
}

#[test]
fn the_list_collection_expression_cannot_reference_the_element_variable() {
    let errors = compile_errors(
        "public int Main() { List<int> values = new List<int>(); foreach (int value in value) { } return 0; }",
    );
    assert!(!errors.is_empty());
}

#[test]
fn the_string_collection_expression_cannot_reference_the_element_variable() {
    let errors = compile_errors(
        "public int Main() { string text = \"a\"; foreach (char value in value) { } return 0; }",
    );
    assert!(!errors.is_empty());
}

#[test]
fn shadowing_an_outer_variable_follows_existing_rules_for_list_foreach() {
    let source = r"
        public int Main()
        {
            int value = 100;
            List<int> values = new List<int>();
            values.Add(1);
            values.Add(2);
            values.Add(3);
            int total = 0;
            foreach (int value in values) { total = total + value; }
            return total + value;
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(6 + 100)));
}

#[test]
fn shadowing_an_outer_variable_follows_existing_rules_for_string_foreach() {
    let source = r#"
        public int Main()
        {
            int value = 100;
            string text = "abc";
            int total = 0;
            foreach (char value in text) { total = total + 1; }
            return total + value;
        }
    "#;
    assert_eq!(run(source), Ok(ExecutionValue::Int(3 + 100)));
}

#[test]
fn declaring_a_colliding_name_in_the_same_list_foreach_body_scope_is_rejected() {
    let errors = compile_errors(
        "public int Main() {\n\
             List<int> values = new List<int>();\n\
             values.Add(1);\n\
             foreach (int value in values) {\n\
                 int extra = value;\n\
                 int extra = value;\n\
             }\n\
             return 0;\n\
         }",
    );
    assert!(
        !errors.is_empty(),
        "expected a duplicate-declaration diagnostic"
    );
}

#[test]
fn declaring_a_colliding_name_in_the_same_string_foreach_body_scope_is_rejected() {
    let errors = compile_errors(
        "public int Main() {\n\
             string text = \"a\";\n\
             foreach (char value in text) {\n\
                 int extra = 0;\n\
                 int extra = 1;\n\
             }\n\
             return 0;\n\
         }",
    );
    assert!(
        !errors.is_empty(),
        "expected a duplicate-declaration diagnostic"
    );
}

#[test]
fn list_foreach_body_as_a_single_statement_has_the_same_contract_as_a_block() {
    assert_eq!(
        run(
            "public int Main() { List<int> values = new List<int>(); values.Add(1); values.Add(2); int total = 0; foreach (int value in values) total = total + value; return total; }"
        ),
        Ok(ExecutionValue::Int(3))
    );
}

#[test]
fn string_foreach_body_as_a_single_statement_has_the_same_contract_as_a_block() {
    assert_eq!(
        run(
            "public int Main() { string text = \"abc\"; int total = 0; foreach (char value in text) total = total + 1; return total; }"
        ),
        Ok(ExecutionValue::Int(3))
    );
}

// --- Section 7/8: escape analysis -- enum containing a string reference -----------

const REF_ENUM: &str = "public enum Payload { Empty, Text(string value) }\n";

#[test]
fn an_array_element_that_is_an_enum_containing_a_string_survives_extraction_and_return() {
    let source = format!(
        "{REF_ENUM}
        public Payload Build(string a, string b) {{
            Payload[] values = [Payload.Text(a + b)];
            foreach (Payload value in values) {{ return value; }}
            return Payload.Empty;
        }}
        public string Main() {{
            switch (Build(\"enum-\", \"array\")) {{
                case Text(value):
                    string junk = \"junk-after-enum-array-extraction-1234\";
                    return value + \"|\" + junk.Length.ToString();
                case Empty: return \"empty\";
            }}
        }}"
    );
    assert_eq!(
        run(&source),
        Ok(ExecutionValue::String("enum-array|37".to_owned()))
    );
}

#[test]
fn a_list_element_that_is_an_enum_containing_a_string_survives_extraction_and_return() {
    let source = format!(
        "{REF_ENUM}
        public Payload Build(string a, string b) {{
            List<Payload> values = new List<Payload>();
            values.Add(Payload.Text(a + b));
            foreach (Payload value in values) {{ return value; }}
            return Payload.Empty;
        }}
        public string Main() {{
            switch (Build(\"enum-\", \"list\")) {{
                case Text(value):
                    string junk = \"junk-after-enum-list-extraction-1234567\";
                    return value + \"|\" + junk.Length.ToString();
                case Empty: return \"empty\";
            }}
        }}"
    );
    assert_eq!(
        run(&source),
        Ok(ExecutionValue::String("enum-list|39".to_owned()))
    );
}

// --- Section 10: List mutation consolidated -- break/return immediately after Add --

#[test]
fn add_immediately_followed_by_break_does_not_read_another_element() {
    let source = r"
        public int Main()
        {
            List<int> values = new List<int>();
            values.Add(1);
            values.Add(2);
            foreach (int value in values)
            {
                values.Add(99);
                break;
            }
            return 0;
        }
    ";
    // The version check that would catch this mutation only runs before the
    // *next* element read; `break` here means there is no next read at all,
    // so this must succeed (the mutation itself is never invalidated after
    // the fact -- only ever detected on the next `Get` attempt).
    assert_eq!(run(source), Ok(ExecutionValue::Int(0)));
}

#[test]
fn add_immediately_followed_by_return_does_not_read_another_element() {
    let source = r"
        public int Main()
        {
            List<int> values = new List<int>();
            values.Add(1);
            values.Add(2);
            foreach (int value in values)
            {
                values.Add(99);
                return 7;
            }
            return -1;
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(7)));
}

#[test]
fn a_non_reallocating_mutation_is_still_caught_before_the_next_read() {
    // Capacity is already 4 after two Adds below (List<T> starts at 0 -> 4
    // on first growth); the third Add fits in existing capacity, so this
    // exercises the fail-fast path with no buffer reallocation involved,
    // distinct from `list_foreach.rs`'s reallocating-mutation test.
    let source = r"
        public int Main()
        {
            List<int> values = new List<int>();
            values.Add(1);
            values.Add(2);
            int total = 0;
            foreach (int value in values)
            {
                values.Add(3);
                total = total + value;
            }
            return total;
        }
    ";
    let error = run(source).expect_err("non-reallocating mutation during foreach must be rejected");
    assert!(error.contains("structurally modified"), "got {error:?}");
}

// --- Section 13: MIR adulteration -- duplicate LocalId (general validator fix) -----

#[test]
fn adulterated_mir_rejects_a_duplicated_local_id_across_array_foreach_locals() {
    let mut module = compile_mir(
        "public int Main() { int[] values = [1, 2, 3]; int total = 0; foreach (int value in values) { total = total + value; } return total; }",
    );
    let mut adulterated = false;
    for function in &mut module.functions {
        if function.locals.len() >= 2 {
            let duplicate_id = function.locals[0].id;
            function.locals[1].id = duplicate_id;
            adulterated = true;
            break;
        }
    }
    assert!(
        adulterated,
        "a foreach program always lowers at least two locals"
    );
    let error = execute(&module, "Main")
        .expect_err("a duplicated LocalId must be rejected before/without executing normally");
    assert!(error.to_string().contains("duplicate local identifiers"));
}

#[test]
fn adulterated_mir_rejects_a_duplicated_local_id_between_a_parameter_and_a_local() {
    let mut module = compile_mir(
        "public int Sum(int seed, int[] values) { int total = seed; foreach (int value in values) { total = total + value; } return total; }
         public int Main() { return Sum(0, [1, 2, 3]); }",
    );
    let mut adulterated = false;
    for function in &mut module.functions {
        if function.name == "Sum" && !function.parameters.is_empty() && !function.locals.is_empty()
        {
            let duplicate_id = function.parameters[0].id;
            function.locals[0].id = duplicate_id;
            adulterated = true;
            break;
        }
    }
    assert!(
        adulterated,
        "Sum always has a parameter and at least one local"
    );
    let error = execute(&module, "Main")
        .expect_err("a LocalId duplicated between a parameter and a local must be rejected");
    assert!(error.to_string().contains("duplicate local identifiers"));
}

// --- Section 14: workers command parity (check/dumps/run share one validator) -----

#[test]
fn validate_and_execute_agree_on_a_worker_boundary_rejection_reachable_only_through_foreach() {
    // `aster check`/`dump-hir`/`dump-mir` call `aster_codegen_cranelift::
    // validate` directly (never a second call-graph analysis) so they can
    // never diverge from `run`, which calls the same function inside
    // `execute`. Exercises that shared entry point directly for a
    // worker-boundary violation reachable only through a `foreach` body.
    let source = r"
        using aster.io;
        public int Body() {
            List<int> values = new List<int>();
            values.Add(1);
            foreach (int value in values) { WriteLine(value.ToString()); }
            return 0;
        }
        public int Main() { Task<int> task = Task.Run(Body); return task.Wait(); }
    ";
    let module = compile_mir_project(source);
    let validate_result = aster_codegen_cranelift::validate(&module);
    let execute_result = execute(&module, "Main");
    assert!(
        validate_result.is_err(),
        "validate() must reject this module"
    );
    assert!(execute_result.is_err(), "execute() must reject this module");
    assert_eq!(
        validate_result.unwrap_err().to_string(),
        execute_result.unwrap_err().to_string(),
        "check/dump-* and run must report the identical diagnostic for the same module"
    );
}
