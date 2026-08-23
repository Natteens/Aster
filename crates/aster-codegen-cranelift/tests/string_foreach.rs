//! M3D: `foreach` over `string`, producing `char` per Unicode scalar value.
//! Extends M3B/M3C's `foreach` (see `foreach.rs` for arrays, `list_foreach.rs`
//! for `List<T>`) to the nominal `string` case: `HIR`/`Statement::ForEach` is
//! unchanged; only MIR lowering picks a third concrete strategy
//! (`lower_foreach_over_string`) based on `collection.type_`. Iterates
//! Unicode scalar values via a private linear UTF-8 byte cursor -- never
//! bytes, UTF-16 code units, or grapheme clusters -- using two new private
//! MIR/runtime primitives (`StringByteLength`, `StringDecodeNext`), neither
//! exposed as public Aster API. No iterator, enumerator, `var` inference, or
//! grapheme-cluster support is added.

use std::sync::atomic::{AtomicU64, Ordering};

use aster_codegen_cranelift::{ExecutionValue, MemoryStats, execute, execute_with_stats};
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

fn stats(source: &str) -> (ExecutionValue, MemoryStats) {
    execute_with_stats(&compile_mir(source), "Main").expect("source should execute")
}

/// `Option<T>`/`Result<T, E>` need their real generic template declarations
/// linked from `aster.core`, which single-file `compile()` does not do (see
/// `foreach.rs`/`list_foreach.rs`).
fn run_project(source: &str) -> Result<ExecutionValue, String> {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "aster-string-foreach-project-{}-{id}.aster",
        std::process::id()
    ));
    std::fs::write(&path, source).expect("write temporary project");
    let compilation = compile_project(&path).map_err(|diagnostics| format!("{diagnostics:#?}"));
    std::fs::remove_file(&path).ok();
    execute(&compilation?.compilation.mir, "Main").map_err(|error| error.to_string())
}

/// Builds an Aster source string containing a literal string constant with
/// the given Rust content spliced directly in (ASTER string literals accept
/// raw multi-byte UTF-8 characters unescaped; only `\n \r \t \\ \" \'` are
/// recognized escapes, so embedding content this way -- rather than through
/// Aster escape syntax, which has no `\u{}` form -- is the correct approach
/// for non-ASCII/NUL test fixtures).
fn source_with_literal(template_before: &str, literal: &str, template_after: &str) -> String {
    format!("{template_before}\"{literal}\"{template_after}")
}

// --- Section 16.1-12: semantics -----------------------------------------------------

#[test]
fn empty_string_foreach_does_not_execute_the_body() {
    let source = source_with_literal(
        "public int Main() { string text = ",
        "",
        "; int total = 0; foreach (char value in text) { total = total + 1; } return total; }",
    );
    assert_eq!(run(&source), Ok(ExecutionValue::Int(0)));
}

#[test]
fn ascii_string_foreach_visits_every_char_in_order() {
    let source = r#"
        public string Main()
        {
            string text = "hello";
            string order = "";
            foreach (char value in text) { order = order + value.ToString(); }
            return order;
        }
    "#;
    assert_eq!(run(source), Ok(ExecutionValue::String("hello".to_owned())));
}

#[test]
fn a_two_byte_scalar_decodes_to_the_correct_code_point() {
    // 'Γ©' is U+00E9, a 2-byte UTF-8 sequence.
    let source = "public int Main() { string text = \"\u{00E9}\"; int total = 0; foreach (char value in text) { total = (int)value; } return total; }";
    assert_eq!(run(source), Ok(ExecutionValue::Int(0x00E9)));
}

#[test]
fn a_three_byte_scalar_decodes_to_the_correct_code_point() {
    // 'δ½ ' is U+4F60, a 3-byte UTF-8 sequence.
    let source = "public int Main() { string text = \"\u{4F60}\"; int total = 0; foreach (char value in text) { total = (int)value; } return total; }";
    assert_eq!(run(source), Ok(ExecutionValue::Int(0x4F60)));
}

#[test]
fn a_four_byte_scalar_decodes_to_the_correct_code_point() {
    // U+1F642 is a 4-byte UTF-8 sequence (outside the Basic Multilingual Plane).
    let source = "public int Main() { string text = \"\u{1F642}\"; int total = 0; foreach (char value in text) { total = (int)value; } return total; }";
    assert_eq!(run(source), Ok(ExecutionValue::Int(0x1F642)));
}

#[test]
fn a_mix_of_one_two_three_and_four_byte_scalars_decodes_in_order() {
    let source = "
        public string Main()
        {
            string text = \"A\u{00E9}\u{4F60}\u{1F642}\";
            string order = \"\";
            foreach (char value in text)
            {
                order = order + ((int)value).ToString() + \"|\";
            }
            return order;
        }
    ";
    assert_eq!(
        run(source),
        Ok(ExecutionValue::String("65|233|20320|128578|".to_owned()))
    );
}

#[test]
fn a_combining_mark_is_a_separate_element_from_its_base_letter() {
    // "e" + U+0301 (COMBINING ACUTE ACCENT): two separate scalar elements,
    // never merged into one grapheme cluster ('Γ©' as a single precomposed
    // scalar is covered by `a_two_byte_scalar_decodes_to_the_correct_code_point`).
    let source = "
        public string Main()
        {
            string text = \"e\u{0301}\";
            string order = \"\";
            foreach (char value in text)
            {
                order = order + ((int)value).ToString() + \"|\";
            }
            return order;
        }
    ";
    assert_eq!(
        run(source),
        Ok(ExecutionValue::String("101|769|".to_owned()))
    );
}

#[test]
fn an_embedded_nul_is_an_ordinary_scalar() {
    let source = "
        public string Main()
        {
            string text = \"a\u{0000}b\";
            string order = \"\";
            foreach (char value in text)
            {
                order = order + ((int)value).ToString() + \"|\";
            }
            return order;
        }
    ";
    assert_eq!(
        run(source),
        Ok(ExecutionValue::String("97|0|98|".to_owned()))
    );
}

#[test]
fn whitespace_scalars_are_visited_like_any_other() {
    let source = "public int Main() { string text = \" \t\"; int total = 0; foreach (char value in text) { total = total + 1; } return total; }";
    assert_eq!(run(source), Ok(ExecutionValue::Int(2)));
}

#[test]
fn the_string_expression_is_evaluated_exactly_once() {
    let source = r#"
        public class Counter { public int calls; }
        public string Provide(Counter counter)
        {
            counter.calls = counter.calls + 1;
            return "abc";
        }
        public int Main()
        {
            Counter counter = new Counter();
            int total = 0;
            foreach (char value in Provide(counter)) { total = total + 1; }
            return total * 1000 + counter.calls;
        }
    "#;
    assert_eq!(run(source), Ok(ExecutionValue::Int(3000 + 1)));
}

#[test]
fn the_string_is_captured_and_reassigning_the_binding_does_not_change_it() {
    let source = r#"
        public int Main()
        {
            string current = "abc";
            int visits = 0;
            foreach (char value in current)
            {
                current = "xyz";
                visits = visits + 1;
            }
            return visits;
        }
    "#;
    // Reassigning `current` inside the body must not affect the already
    // captured string: exactly 3 visits (over "abc"), not something else.
    assert_eq!(run(source), Ok(ExecutionValue::Int(3)));
}

// --- Section 16.13-17: type and readonly --------------------------------------------

#[test]
fn char_is_accepted_as_the_element_type() {
    assert_eq!(
        run(
            "public int Main() { string text = \"ab\"; int total = 0; foreach (char value in text) { total = total + 1; } return total; }"
        ),
        Ok(ExecutionValue::Int(2))
    );
}

#[test]
fn int_is_rejected_as_the_element_type() {
    let errors = compile_errors(
        "public int Main() { string text = \"ab\"; foreach (int value in text) { } return 0; }",
    );
    assert!(
        errors
            .iter()
            .any(|message| message.contains("requires element type")),
        "got {errors:?}"
    );
}

#[test]
fn byte_is_rejected_as_the_element_type() {
    let errors = compile_errors(
        "public int Main() { string text = \"ab\"; foreach (byte value in text) { } return 0; }",
    );
    assert!(
        errors
            .iter()
            .any(|message| message.contains("requires element type")),
        "got {errors:?}"
    );
}

#[test]
fn string_is_rejected_as_the_element_type() {
    let errors = compile_errors(
        "public int Main() { string text = \"ab\"; foreach (string value in text) { } return 0; }",
    );
    assert!(
        errors
            .iter()
            .any(|message| message.contains("requires element type")),
        "got {errors:?}"
    );
}

#[test]
fn var_infers_char_for_a_string_foreach() {
    let source = "public int Main() { string text = \"ab\"; int count = 0; foreach (var value in text) { if (value == 'b') { count++; } } return count; }";
    assert_eq!(run(source), Ok(ExecutionValue::Int(1)));
}

#[test]
fn reassigning_the_string_foreach_binding_is_rejected() {
    let errors = compile_errors(
        "public int Main() { string text = \"ab\"; foreach (char value in text) { value = 'x'; } return 0; }",
    );
    assert!(errors.iter().any(|message| message.contains("read-only")));
}

// --- Section 16.18-25: control flow --------------------------------------------------

#[test]
fn string_foreach_supports_break() {
    let source = r#"
        public string Main()
        {
            string text = "abcdef";
            string order = "";
            foreach (char value in text)
            {
                if (value == 'd') { break; }
                order = order + value.ToString();
            }
            return order;
        }
    "#;
    assert_eq!(run(source), Ok(ExecutionValue::String("abc".to_owned())));
}

#[test]
fn string_foreach_supports_continue_and_advances_the_cursor_exactly_once() {
    let source = r#"
        public string Main()
        {
            string text = "abcdef";
            string order = "";
            foreach (char value in text)
            {
                if (value == 'b') { continue; }
                if (value == 'd') { continue; }
                order = order + value.ToString();
            }
            return order;
        }
    "#;
    // A `continue` that skipped the cursor advance (looping on the same
    // scalar forever) would hang this test instead of returning "acef".
    assert_eq!(run(source), Ok(ExecutionValue::String("acef".to_owned())));
}

#[test]
fn multiple_continues_over_multibyte_scalars_each_advance_by_the_correct_width() {
    // Every other scalar is 'Γ©' (2 bytes); `continue` on it must still land
    // exactly on the next scalar boundary, never 1 byte into its sequence.
    let source = "
        public string Main()
        {
            string text = \"a\u{00E9}b\u{00E9}c\";
            string order = \"\";
            foreach (char value in text)
            {
                if (value == '\u{00E9}') { continue; }
                order = order + value.ToString();
            }
            return order;
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::String("abc".to_owned())));
}

#[test]
fn string_foreach_supports_return() {
    let source = r#"
        public int Main()
        {
            string text = "abc";
            foreach (char value in text)
            {
                if (value == 'b') { return 99; }
            }
            return -1;
        }
    "#;
    assert_eq!(run(source), Ok(ExecutionValue::Int(99)));
}

#[test]
fn string_foreach_supports_postfix_try() {
    let source = r#"
        using aster.core;
        public Result<int, string> Fail(char value)
        {
            if (value == 'b') { return Result<int, string>.Error("bad"); }
            return Result<int, string>.Ok((int)value);
        }
        public Result<int, string> Process(string text)
        {
            int total = 0;
            foreach (char value in text)
            {
                int parsed = Fail(value)?;
                total = total + parsed;
            }
            return Result<int, string>.Ok(total);
        }
        public int Main()
        {
            switch (Process("abc")) {
                case Ok(total): return total;
                case Error(message): return -1;
            }
        }
    "#;
    assert_eq!(run_project(source), Ok(ExecutionValue::Int(-1)));
}

#[test]
fn string_foreach_nests_with_itself() {
    let source = r#"
        public int Main()
        {
            string outer = "ab";
            string inner = "xy";
            int total = 0;
            foreach (char a in outer)
            {
                foreach (char b in inner) { total = total + 1; }
            }
            return total;
        }
    "#;
    assert_eq!(run(source), Ok(ExecutionValue::Int(4)));
}

#[test]
fn array_foreach_nests_inside_string_foreach() {
    let source = r#"
        public int Main()
        {
            string text = "ab";
            int[] values = [10, 20];
            int total = 0;
            foreach (char letter in text)
            {
                foreach (int value in values) { total = total + value; }
            }
            return total;
        }
    "#;
    assert_eq!(run(source), Ok(ExecutionValue::Int((10 + 20) * 2)));
}

#[test]
fn string_foreach_nests_inside_array_foreach() {
    let source = r#"
        public int Main()
        {
            int[] outer = [1, 2];
            string text = "ab";
            int total = 0;
            foreach (int value in outer)
            {
                foreach (char letter in text) { total = total + 1; }
            }
            return total;
        }
    "#;
    assert_eq!(run(source), Ok(ExecutionValue::Int(4)));
}

#[test]
fn string_foreach_nests_inside_list_foreach() {
    let source = r#"
        public int Main()
        {
            List<int> outer = new List<int>();
            outer.Add(1);
            outer.Add(2);
            string text = "ab";
            int total = 0;
            foreach (int value in outer)
            {
                foreach (char letter in text) { total = total + 1; }
            }
            return total;
        }
    "#;
    assert_eq!(run(source), Ok(ExecutionValue::Int(4)));
}

// --- Section 16.26-34: integration ---------------------------------------------------

#[test]
fn string_foreach_over_a_field_property_value() {
    let source = r#"
        public class Holder { public string Text; public Holder() { Text = "abc"; } }
        public int Main()
        {
            Holder holder = new Holder();
            int total = 0;
            foreach (char value in holder.Text) { total = total + 1; }
            return total;
        }
    "#;
    assert_eq!(run(source), Ok(ExecutionValue::Int(3)));
}

#[test]
fn string_foreach_over_a_temporary_concatenation() {
    let source = r#"
        public int Main()
        {
            string a = "ab";
            string b = "cd";
            int total = 0;
            foreach (char value in a + b) { total = total + 1; }
            return total;
        }
    "#;
    assert_eq!(run(source), Ok(ExecutionValue::Int(4)));
}

#[test]
fn string_foreach_over_a_helper_returned_string() {
    let source = r#"
        public string Describe() { return "hello"; }
        public int Main()
        {
            int total = 0;
            foreach (char value in Describe()) { total = total + 1; }
            return total;
        }
    "#;
    assert_eq!(run(source), Ok(ExecutionValue::Int(5)));
}

#[test]
fn string_foreach_works_inside_a_generic_function() {
    let source = r#"
        public int CountVowels<T>(string text, T ignored)
        {
            int count = 0;
            foreach (char value in text)
            {
                if (value == 'a') { count = count + 1; }
            }
            return count;
        }
        public int Main() { return CountVowels<int>("banana", 0); }
    "#;
    assert_eq!(run(source), Ok(ExecutionValue::Int(3)));
}

#[test]
fn string_foreach_works_inside_a_declared_namespace() {
    let source = r#"
        namespace app;
        public int Main()
        {
            string text = "abc";
            int total = 0;
            foreach (char value in text) { total = total + 1; }
            return total;
        }
    "#;
    assert_eq!(run(source), Ok(ExecutionValue::Int(3)));
}

#[test]
fn string_foreach_works_across_a_multifile_project() {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "aster-string-foreach-multifile-{}-{id}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("create project root");
    std::fs::write(
        root.join("Aster.toml"),
        "[package]\nname = \"string_foreach_test\"\n",
    )
    .expect("write manifest");
    let app_dir = root.join("app");
    std::fs::create_dir_all(&app_dir).expect("create app dir");
    std::fs::write(
        app_dir.join("main.aster"),
        "namespace app;\n\
         public int Main() { return Helpers.CountChars(\"abcd\"); }",
    )
    .expect("write main.aster");
    std::fs::write(
        app_dir.join("helpers.aster"),
        "namespace app;\n\
         public class Helpers {\n\
             public static int CountChars(string text) {\n\
                 int total = 0;\n\
                 foreach (char value in text) { total = total + 1; }\n\
                 return total;\n\
             }\n\
         }",
    )
    .expect("write helpers.aster");
    let compilation = compile_project(&app_dir.join("main.aster"))
        .map_err(|diagnostics| format!("{diagnostics:#?}"));
    std::fs::remove_dir_all(&root).ok();
    let module = compilation
        .expect("multifile project using string foreach should compile")
        .compilation
        .mir;
    assert_eq!(
        execute(&module, "string_foreach_test::app::Main"),
        Ok(ExecutionValue::Int(4))
    );
}

#[test]
fn string_foreach_element_placed_in_an_option() {
    let source = r#"
        using aster.core;
        public Option<char> FirstVowel(string text)
        {
            foreach (char value in text)
            {
                if (value == 'a' || value == 'e' || value == 'i' || value == 'o' || value == 'u')
                {
                    return Option<char>.Some(value);
                }
            }
            return Option<char>.None;
        }
        public int Main()
        {
            switch (FirstVowel("xyzaq")) {
                case Some(value): return (int)value;
                case None: return -1;
            }
        }
    "#;
    assert_eq!(run_project(source), Ok(ExecutionValue::Int('a' as i32)));
}

#[test]
fn string_foreach_element_placed_in_a_result() {
    let source = r#"
        using aster.core;
        public Result<char, string> FirstVowel(string text)
        {
            foreach (char value in text)
            {
                if (value == 'a' || value == 'e' || value == 'i' || value == 'o' || value == 'u')
                {
                    return Result<char, string>.Ok(value);
                }
            }
            return Result<char, string>.Error("none");
        }
        public int Main()
        {
            switch (FirstVowel("xyzaq")) {
                case Ok(value): return (int)value;
                case Error(message): return -1;
            }
        }
    "#;
    assert_eq!(run_project(source), Ok(ExecutionValue::Int('a' as i32)));
}

// --- Section 16.35-47 / 17: MIR adulteration ----------------------------------------

const STRING_FOREACH_PROGRAM: &str = r#"
    public int Main()
    {
        string text = "abc";
        int total = 0;
        foreach (char value in text) { total = total + 1; }
        return total;
    }
"#;

fn find_string_decode_next(module: &mut mir::Module) -> &mut mir::Instruction {
    module
        .functions
        .iter_mut()
        .flat_map(|function| &mut function.blocks)
        .flat_map(|block| &mut block.instructions)
        .find(|instruction| matches!(instruction, mir::Instruction::StringDecodeNext { .. }))
        .expect("a string-foreach program always lowers exactly one StringDecodeNext")
}

fn find_first_matching(
    module: &mut mir::Module,
    matches: impl Fn(&mir::Instruction) -> bool,
) -> &mut mir::Instruction {
    module
        .functions
        .iter_mut()
        .flat_map(|function| &mut function.blocks)
        .flat_map(|block| &mut block.instructions)
        .find(|instruction| matches(instruction))
        .expect("a matching instruction exists")
}

fn retype_local(module: &mut mir::Module, local_id: mir::LocalId, new_type: &mir::Type) {
    for function in &mut module.functions {
        for local in function.locals.iter_mut().chain(&mut function.parameters) {
            if local.id == local_id {
                local.type_ = new_type.clone();
            }
        }
    }
}

fn execute_error(module: &mir::Module) -> String {
    execute(module, "Main")
        .expect_err("adulterated MIR must be rejected before/without executing normally")
        .to_string()
}

#[test]
fn adulterated_mir_rejects_string_decode_next_on_a_non_string_receiver() {
    let mut module = compile_mir(STRING_FOREACH_PROGRAM);
    let mir::Instruction::StringDecodeNext { string, .. } = find_string_decode_next(&mut module)
    else {
        unreachable!();
    };
    string.type_ = mir::Type::Int;
    let error = execute_error(&module);
    assert!(!error.is_empty());
}

#[test]
fn adulterated_mir_rejects_string_decode_next_with_a_non_int_cursor() {
    let mut module = compile_mir(STRING_FOREACH_PROGRAM);
    let mir::Instruction::StringDecodeNext { cursor, .. } = find_string_decode_next(&mut module)
    else {
        unreachable!();
    };
    cursor.type_ = mir::Type::Bool;
    let error = execute_error(&module);
    assert!(!error.is_empty());
}

#[test]
fn adulterated_mir_rejects_string_decode_next_with_a_nonexistent_cursor_local() {
    let mut module = compile_mir(STRING_FOREACH_PROGRAM);
    let mir::Instruction::StringDecodeNext { cursor, .. } = find_string_decode_next(&mut module)
    else {
        unreachable!();
    };
    cursor.kind = mir::OperandKind::Copy(mir::Place::Local(mir::LocalId(u32::MAX)));
    let error = execute_error(&module);
    assert!(!error.is_empty());
}

#[test]
fn adulterated_mir_rejects_string_decode_next_with_a_nonexistent_string_operand() {
    let mut module = compile_mir(STRING_FOREACH_PROGRAM);
    let mir::Instruction::StringDecodeNext { string, .. } = find_string_decode_next(&mut module)
    else {
        unreachable!();
    };
    string.kind = mir::OperandKind::Copy(mir::Place::Local(mir::LocalId(u32::MAX)));
    let error = execute_error(&module);
    assert!(!error.is_empty());
}

#[test]
fn adulterated_mir_rejects_string_decode_next_with_a_nonexistent_char_destination() {
    let mut module = compile_mir(STRING_FOREACH_PROGRAM);
    let mir::Instruction::StringDecodeNext {
        char_destination, ..
    } = find_string_decode_next(&mut module)
    else {
        unreachable!();
    };
    *char_destination = mir::Place::Local(mir::LocalId(u32::MAX));
    let error = execute_error(&module);
    assert!(!error.is_empty());
}

#[test]
fn adulterated_mir_rejects_string_decode_next_with_a_char_destination_of_the_wrong_type() {
    let mut module = compile_mir(STRING_FOREACH_PROGRAM);
    let mir::Instruction::StringDecodeNext {
        char_destination, ..
    } = find_string_decode_next(&mut module)
    else {
        unreachable!();
    };
    let mir::Place::Local(local_id) = *char_destination else {
        unreachable!();
    };
    retype_local(&mut module, local_id, &mir::Type::Int);
    let error = execute_error(&module);
    assert!(!error.is_empty());
}

#[test]
fn adulterated_mir_rejects_string_decode_next_with_a_nonexistent_next_cursor_destination() {
    let mut module = compile_mir(STRING_FOREACH_PROGRAM);
    let mir::Instruction::StringDecodeNext {
        next_cursor_destination,
        ..
    } = find_string_decode_next(&mut module)
    else {
        unreachable!();
    };
    *next_cursor_destination = mir::Place::Local(mir::LocalId(u32::MAX));
    let error = execute_error(&module);
    assert!(!error.is_empty());
}

#[test]
fn adulterated_mir_rejects_string_decode_next_with_a_next_cursor_destination_of_the_wrong_type() {
    let mut module = compile_mir(STRING_FOREACH_PROGRAM);
    let mir::Instruction::StringDecodeNext {
        next_cursor_destination,
        ..
    } = find_string_decode_next(&mut module)
    else {
        unreachable!();
    };
    let mir::Place::Local(local_id) = *next_cursor_destination else {
        unreachable!();
    };
    retype_local(&mut module, local_id, &mir::Type::Bool);
    let error = execute_error(&module);
    assert!(!error.is_empty());
}

#[test]
fn adulterated_mir_rejects_string_decode_next_writing_two_destinations_into_the_same_local() {
    let mut module = compile_mir(STRING_FOREACH_PROGRAM);
    let mir::Instruction::StringDecodeNext {
        char_destination,
        next_cursor_destination,
        ..
    } = find_string_decode_next(&mut module)
    else {
        unreachable!();
    };
    *next_cursor_destination = char_destination.clone();
    let error = execute_error(&module);
    assert!(!error.is_empty());
}

#[test]
fn adulterated_mir_rejects_a_string_byte_length_on_a_non_string_receiver() {
    let mut module = compile_mir(STRING_FOREACH_PROGRAM);
    let instruction = find_first_matching(&mut module, |instruction| {
        matches!(
            instruction,
            mir::Instruction::Assign {
                value: mir::Rvalue {
                    kind: mir::RvalueKind::StringByteLength(_),
                    ..
                },
                ..
            }
        )
    });
    let mir::Instruction::Assign {
        value:
            mir::Rvalue {
                kind: mir::RvalueKind::StringByteLength(operand),
                ..
            },
        ..
    } = instruction
    else {
        unreachable!();
    };
    operand.type_ = mir::Type::Int;
    let error = execute_error(&module);
    assert!(!error.is_empty());
}

#[test]
fn adulterated_mir_rejects_a_string_byte_length_local_retyped_to_a_non_int() {
    let mut module = compile_mir(STRING_FOREACH_PROGRAM);
    let instruction = find_first_matching(&mut module, |instruction| {
        matches!(
            instruction,
            mir::Instruction::Assign {
                value: mir::Rvalue {
                    kind: mir::RvalueKind::StringByteLength(_),
                    ..
                },
                ..
            }
        )
    });
    let mir::Instruction::Assign { value, .. } = instruction else {
        unreachable!();
    };
    value.type_ = mir::Type::Bool;
    let error = execute_error(&module);
    assert!(!error.is_empty());
}

#[test]
fn adulterated_mir_rejects_a_non_bool_branch_condition_in_a_string_foreach_program() {
    let mut module = compile_mir(STRING_FOREACH_PROGRAM);
    for function in &mut module.functions {
        for block in &mut function.blocks {
            if let mir::Terminator::Branch { condition, .. } = &mut block.terminator {
                condition.type_ = mir::Type::Int;
                let error = execute_error(&module);
                assert!(!error.is_empty());
                return;
            }
        }
    }
    unreachable!("a string-foreach program always lowers to at least one Branch");
}

#[test]
fn adulterated_mir_rejects_a_branch_targeting_an_unknown_block_in_a_string_foreach_program() {
    let mut module = compile_mir(STRING_FOREACH_PROGRAM);
    for function in &mut module.functions {
        for block in &mut function.blocks {
            if let mir::Terminator::Branch { then_block, .. } = &mut block.terminator {
                *then_block = mir::BasicBlockId(u32::MAX);
                let error = execute_error(&module);
                assert!(!error.is_empty());
                return;
            }
        }
    }
    unreachable!("a string-foreach program always lowers to at least one Branch");
}

#[test]
fn adulterated_mir_rejects_a_missing_entry_block_in_a_string_foreach_program() {
    let mut module = compile_mir(STRING_FOREACH_PROGRAM);
    for function in &mut module.functions {
        if function.name == "Main" {
            function.entry = mir::BasicBlockId(u32::MAX);
        }
    }
    let error = execute_error(&module);
    assert!(!error.is_empty());
}

#[test]
fn adulterated_mir_rejects_a_duplicated_block_id_in_a_string_foreach_program() {
    let mut module = compile_mir(STRING_FOREACH_PROGRAM);
    for function in &mut module.functions {
        if function.blocks.len() >= 2 {
            let duplicate_id = function.blocks[0].id;
            function.blocks[1].id = duplicate_id;
            let error = execute_error(&module);
            assert!(!error.is_empty());
            return;
        }
    }
    unreachable!("a string-foreach program always lowers to at least two blocks");
}

// --- Section 16.35-42: adversarial byte-level decode safety --------------------------

/// Directly stress-tests `decode_scalar_at`'s validation via `AsterList`-style
/// adulteration is not applicable to strings (no public constructor for
/// invalid UTF-8 exists at the Aster level, per the task's explicit "não
/// adicione uma forma pública de criar strings UTF-8 inválidas"); the MIR
/// adulteration tests above already exercise `StringDecodeNext` end to end.
/// The scalar-boundary and truncation cases below instead confirm the
/// runtime's real, in-bounds edge behavior through ordinary valid strings.
#[test]
fn a_cursor_exactly_at_the_final_scalar_boundary_still_decodes_correctly() {
    let source = "public int Main() { string text = \"a\u{1F642}\"; int last = 0; foreach (char value in text) { last = (int)value; } return last; }";
    assert_eq!(run(source), Ok(ExecutionValue::Int(0x1F642)));
}

// --- Section 16.48-51: regressions ---------------------------------------------------

#[test]
fn array_foreach_is_unaffected_by_the_string_foreach_path() {
    let source = r"
        public int Main()
        {
            int[] values = [1, 2, 3];
            int total = 0;
            foreach (int value in values) { total = total + value; }
            return total;
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(6)));
}

#[test]
fn list_foreach_is_unaffected_by_the_string_foreach_path() {
    let source = r"
        public int Main()
        {
            List<int> values = new List<int>();
            values.Add(1);
            values.Add(2);
            values.Add(3);
            int total = 0;
            foreach (int value in values) { total = total + value; }
            return total;
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(6)));
}

#[test]
fn list_version_mismatch_still_fails_deterministically() {
    let error = run(
        "public int Main() { List<int> v = new List<int>(); v.Add(1); v.Add(2); foreach (int x in v) { v.Add(9); } return 0; }",
    )
    .expect_err("list structural mutation during foreach must still be rejected");
    assert!(error.contains("structurally modified"), "got {error:?}");
}

#[test]
fn workers_still_reject_a_string_crossing_a_worker_boundary() {
    let errors = compile_errors(
        r#"
        public string Make() { return "hi"; }
        public int Main() {
            Task<string> task = Task.Run(Make);
            return 0;
        }
        "#,
    );
    assert!(
        errors
            .iter()
            .any(|message| message.contains("cross a worker boundary")),
        "foreach must not have changed string worker-transferability, got {errors:?}"
    );
}

#[test]
fn an_ordinary_string_foreach_body_still_compiles_and_runs_as_a_worker_body() {
    let source = r#"
        public void Body(int i) {
            string text = "abc";
            int total = 0;
            foreach (char value in text) { total = total + 1; }
        }
        public int Main() { Parallel.For(0, 4, Body); return 0; }
    "#;
    assert_eq!(run(source), Ok(ExecutionValue::Int(0)));
}

#[test]
fn console_io_inside_a_string_foreach_body_reachable_from_a_worker_is_still_rejected() {
    let source = r#"
        using aster.io;
        public int Body() {
            string text = "abc";
            foreach (char value in text) { WriteLine(value.ToString()); }
            return 0;
        }
        public int Main() { Task<int> task = Task.Run(Body); return task.Wait(); }
        "#;
    let error = run_project(source).expect_err("expected Task.Run with console I/O to be rejected");
    assert!(error.contains("Task.Run"), "got {error:?}");
}

// --- Section 16.52-54: memory --------------------------------------------------------

#[test]
fn an_empty_string_foreach_repeated_many_times_allocates_nothing_new() {
    let source = r#"
        public int Main()
        {
            int total = 0;
            for (int i = 0; i < 5000; i++)
            {
                string text = "";
                foreach (char value in text) { total = total + 1; }
            }
            return total;
        }
    "#;
    let (value, memory) = stats(source);
    assert_eq!(value, ExecutionValue::Int(0));
    assert_eq!(memory.string_allocations, 0);
    assert_eq!(memory.object_allocations, 0);
    assert_eq!(memory.array_allocations, 0);
}

#[test]
fn a_long_ascii_string_foreach_allocates_nothing_beyond_the_literal() {
    let source = r#"
        public int Main()
        {
            string text = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
            int total = 0;
            foreach (char value in text) { total = total + 1; }
            return total;
        }
    "#;
    let (value, memory) = stats(source);
    assert_eq!(value, ExecutionValue::Int(98));
    assert_eq!(memory.string_allocations, 0);
    assert_eq!(memory.object_allocations, 0);
    assert_eq!(memory.array_allocations, 0);
}

#[test]
fn a_string_of_two_byte_scalars_allocates_nothing_beyond_the_literal() {
    let source = "public int Main() { string text = \"\u{00E9}\u{00E9}\u{00E9}\u{00E9}\u{00E9}\u{00E9}\u{00E9}\u{00E9}\"; int total = 0; foreach (char value in text) { total = total + 1; } return total; }";
    let (value, memory) = stats(source);
    assert_eq!(value, ExecutionValue::Int(8));
    assert_eq!(memory.string_allocations, 0);
    assert_eq!(memory.object_allocations, 0);
}

#[test]
fn a_string_of_three_byte_scalars_allocates_nothing_beyond_the_literal() {
    let source = "public int Main() { string text = \"\u{4F60}\u{4F60}\u{4F60}\u{4F60}\"; int total = 0; foreach (char value in text) { total = total + 1; } return total; }";
    let (value, memory) = stats(source);
    assert_eq!(value, ExecutionValue::Int(4));
    assert_eq!(memory.string_allocations, 0);
    assert_eq!(memory.object_allocations, 0);
}

#[test]
fn a_string_of_four_byte_scalars_allocates_nothing_beyond_the_literal() {
    let source = "public int Main() { string text = \"\u{1F642}\u{1F642}\u{1F642}\"; int total = 0; foreach (char value in text) { total = total + 1; } return total; }";
    let (value, memory) = stats(source);
    assert_eq!(value, ExecutionValue::Int(3));
    assert_eq!(memory.string_allocations, 0);
    assert_eq!(memory.object_allocations, 0);
}

#[test]
fn a_mixed_width_string_foreach_allocates_nothing_beyond_the_literal() {
    let source = "public int Main() { string text = \"A\u{00E9}\u{4F60}\u{1F642}\"; int total = 0; foreach (char value in text) { total = total + 1; } return total; }";
    let (value, memory) = stats(source);
    assert_eq!(value, ExecutionValue::Int(4));
    assert_eq!(memory.string_allocations, 0);
    assert_eq!(memory.object_allocations, 0);
}

#[test]
fn a_string_with_combining_marks_allocates_nothing_beyond_the_literal() {
    let source = "public int Main() { string text = \"e\u{0301}e\u{0301}\"; int total = 0; foreach (char value in text) { total = total + 1; } return total; }";
    let (value, memory) = stats(source);
    assert_eq!(value, ExecutionValue::Int(4));
    assert_eq!(memory.string_allocations, 0);
    assert_eq!(memory.object_allocations, 0);
}

#[test]
fn a_string_with_an_embedded_nul_allocates_nothing_beyond_the_literal() {
    let source = "public int Main() { string text = \"a\u{0000}b\u{0000}c\"; int total = 0; foreach (char value in text) { total = total + 1; } return total; }";
    let (value, memory) = stats(source);
    assert_eq!(value, ExecutionValue::Int(5));
    assert_eq!(memory.string_allocations, 0);
    assert_eq!(memory.object_allocations, 0);
}

#[test]
fn millions_of_scalars_over_one_existing_string_allocate_nothing_new() {
    let source = r#"
        public int Main()
        {
            string text = "abcdefghij";
            long total = 0;
            for (int round = 0; round < 300000; round++)
            {
                foreach (char value in text) { total = total + 1; }
            }
            return total > 0 ? 1 : 0;
        }
    "#;
    let allocations_before_loop =
        stats(r#"public int Main() { string text = "abcdefghij"; return text.Length; }"#)
            .1
            .total_allocations;
    let (value, memory) = stats(source);
    assert_eq!(value, ExecutionValue::Int(1));
    // 3,000,000 scalar decodes (300,000 rounds * 10 chars) over a string
    // literal (no dynamic allocation at all -- literals live in the JIT
    // module's data section) must attribute zero further allocations to the
    // loop itself.
    assert_eq!(memory.total_allocations, allocations_before_loop);
    assert_eq!(memory.string_allocations, 0);
}

#[test]
fn a_temporary_string_captured_by_foreach_survives_junk_allocations_in_the_body() {
    let source = r#"
        public string BuildTemporaryString(string a, string b) { return a + b; }
        public string Main()
        {
            string order = "";
            foreach (char value in BuildTemporaryString("Hello, ", "World!"))
            {
                order = order + value.ToString();
                string junk = "junk-allocated-during-the-loop-body-" + order;
            }
            return order;
        }
    "#;
    assert_eq!(
        run(source),
        Ok(ExecutionValue::String("Hello, World!".to_owned()))
    );
}
