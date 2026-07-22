//! End-to-end tests for M1D: `aster.io.Write`/`WriteLine`/`ReadLine`, three
//! official free functions bound by `SymbolId` (never by name) to the
//! `ConsoleWrite`/`ConsoleWriteLine`/`ConsoleReadLine` intrinsics. Mirrors
//! `string_try_parse_float.rs`/`to_string.rs`'s established conventions
//! (helpers, `compile_project` with `using`), and uses an in-memory
//! `aster_runtime::ConsoleBackend` throughout so nothing here touches the
//! real terminal.

use std::sync::atomic::{AtomicU64, Ordering};

use aster_codegen_cranelift::{
    ExecutionValue, MemoryStats, execute, execute_with_console, execute_with_console_and_stats,
};
use aster_compiler::{compile_project, mir};
use aster_runtime::MemoryConsoleBackend;

fn compile(source: &str) -> Result<mir::Module, String> {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "aster-console-io-{}-{id}.aster",
        std::process::id()
    ));
    std::fs::write(&path, source).expect("write temporary project");
    let compilation = compile_project(&path).map_err(|diagnostics| format!("{diagnostics:#?}"));
    std::fs::remove_file(&path).ok();
    compilation.map(|compilation| compilation.compilation.mir)
}

fn compile_errors(source: &str) -> String {
    match compile(source) {
        Ok(_) => String::new(),
        Err(message) => message,
    }
}

fn compile_mir(source: &str) -> mir::Module {
    compile(source).expect("source should compile")
}

/// Runs `function` with an in-memory console pre-loaded with `input`,
/// returning the result plus every byte written.
fn run_with_io(
    source: &str,
    function: &str,
    input: &str,
) -> (Result<ExecutionValue, String>, Vec<u8>) {
    let module = compile_mir(source);
    let backend = MemoryConsoleBackend::new(input.as_bytes());
    let output_handle = backend.clone();
    let result = execute_with_console(&module, function, Box::new(backend))
        .map_err(|error| error.to_string());
    (result, output_handle.output())
}

fn run(source: &str, function: &str) -> Result<ExecutionValue, String> {
    run_with_io(source, function, "").0
}

// --- Section 22.1-6: semantics -----------------------------------------------

#[test]
fn the_three_official_signatures_resolve_and_execute() {
    let source = "using aster.io;\n\
        public string Main() {\n\
            Write(\"a\");\n\
            WriteLine(\"b\");\n\
            return \"ok\";\n\
        }";
    let (result, output) = run_with_io(source, "Main", "");
    assert_eq!(result, Ok(ExecutionValue::String("ok".to_owned())));
    assert_eq!(output, b"ab\n");
}

#[test]
fn extra_or_missing_arguments_are_rejected() {
    let errors = compile_errors(
        "using aster.io;\npublic string Main() { Write(\"a\", \"b\"); return \"x\"; }",
    );
    assert!(!errors.is_empty(), "expected an arity diagnostic for Write");
    let errors = compile_errors("using aster.io;\npublic string Main() { Write(); return \"x\"; }");
    assert!(!errors.is_empty(), "expected an arity diagnostic for Write");
    let errors =
        compile_errors("using aster.io;\npublic string Main() { WriteLine(5); return \"x\"; }");
    assert!(
        !errors.is_empty(),
        "expected a type diagnostic for a non-string WriteLine argument"
    );
}

#[test]
fn a_user_function_named_write_line_without_using_aster_io_is_a_normal_function() {
    let source = "public string WriteLine(string value) { return value + \"!\"; }\n\
        public string Main() { return WriteLine(\"hi\"); }";
    assert_eq!(
        run(source, "Main"),
        Ok(ExecutionValue::String("hi!".to_owned()))
    );
}

#[test]
fn a_user_declaration_colliding_with_the_official_export_is_rejected() {
    let errors = compile_errors(
        "using aster.io;\n\
         public string WriteLine(string value) { return value; }\n\
         public string Main() { return WriteLine(\"hi\"); }",
    );
    assert!(
        errors.contains("conflicts with the official export"),
        "expected a conflict diagnostic, got {errors}"
    );
}

#[test]
fn get_text_is_evaluated_exactly_once_before_write_line() {
    let source = "using aster.io;\n\
        public class Counter {\n\
            public int calls;\n\
            public string GetText() { calls = calls + 1; return \"text\"; }\n\
        }\n\
        public string Main() {\n\
            Counter counter = new Counter();\n\
            WriteLine(counter.GetText());\n\
            return counter.calls.ToString();\n\
        }";
    let (result, output) = run_with_io(source, "Main", "");
    assert_eq!(result, Ok(ExecutionValue::String("1".to_owned())));
    assert_eq!(output, b"text\n");
}

// --- Section 22.7-15: output --------------------------------------------------

#[test]
fn write_emits_ascii_unicode_and_empty_strings_verbatim() {
    for text in ["abc", "héllo ✓ 你好 🙂", ""] {
        let source = format!(
            "using aster.io;\n\
             public string Main() {{ Write(\"{text}\"); return \"ok\"; }}"
        );
        let (result, output) = run_with_io(&source, "Main", "");
        assert_eq!(result, Ok(ExecutionValue::String("ok".to_owned())));
        assert_eq!(output, text.as_bytes(), "Write(\"{text}\")");
    }
}

#[test]
fn write_line_appends_exactly_one_lf() {
    let source = "using aster.io;\npublic string Main() { WriteLine(\"abc\"); return \"ok\"; }";
    let (_, output) = run_with_io(source, "Main", "");
    assert_eq!(output, b"abc\n");

    let source = "using aster.io;\npublic string Main() { WriteLine(\"\"); return \"ok\"; }";
    let (_, output) = run_with_io(source, "Main", "");
    assert_eq!(output, b"\n");
}

#[test]
fn calls_are_emitted_in_program_order() {
    let source = "using aster.io;\n\
        public string GetFirst() { return \"1\"; }\n\
        public string GetSecond() { return \"2\"; }\n\
        public string Main() {\n\
            Write(GetFirst());\n\
            WriteLine(GetSecond());\n\
            Write(\"3\");\n\
            return \"ok\";\n\
        }";
    let (_, output) = run_with_io(source, "Main", "");
    assert_eq!(output, b"12\n3");
}

#[test]
fn write_is_visible_before_a_subsequent_read_line_prompt() {
    // `Write("Name: "); ReadLine();` -- output must be captured before the
    // read happens, proving the implicit flush already took effect.
    let source = "using aster.core;\nusing aster.io;\n\
        public string Main() {\n\
            Write(\"Name: \");\n\
            Option<string> name = ReadLine();\n\
            switch (name) { case Some(value): return value; case None: return \"none\"; }\n\
        }";
    let (result, output) = run_with_io(source, "Main", "Ada\n");
    assert_eq!(result, Ok(ExecutionValue::String("Ada".to_owned())));
    assert_eq!(output, b"Name: ");
}

// --- Section 22.16-27: input ---------------------------------------------------

fn read_first_line(input: &str) -> (Result<ExecutionValue, String>, Vec<u8>) {
    let source = "using aster.core;\nusing aster.io;\n\
        public string Main() {\n\
            Option<string> line = ReadLine();\n\
            switch (line) { case Some(value): return value; case None: return \"__EOF__\"; }\n\
        }";
    run_with_io(source, "Main", input)
}

#[test]
fn read_line_ascii_unicode_empty_lf_crlf_and_no_trailing_newline() {
    for (input, expected) in [
        ("hello\n", "hello"),
        ("héllo 你好 🙂\n", "héllo 你好 🙂"),
        ("\n", ""),
        ("hello\r\n", "hello"),
        ("hello", "hello"),
    ] {
        let (result, _) = read_first_line(input);
        assert_eq!(
            result,
            Ok(ExecutionValue::String(expected.to_owned())),
            "input {input:?}"
        );
    }
}

#[test]
fn read_line_reports_eof_immediately_and_after_lines() {
    let (result, _) = read_first_line("");
    assert_eq!(result, Ok(ExecutionValue::String("__EOF__".to_owned())));

    let source = "using aster.core;\nusing aster.io;\n\
        public string Main() {\n\
            string first = \"\";\n\
            switch (ReadLine()) { case Some(value): first = value; case None: first = \"none\"; }\n\
            string second = \"\";\n\
            switch (ReadLine()) { case Some(value): second = value; case None: second = \"none\"; }\n\
            string third = \"\";\n\
            switch (ReadLine()) { case Some(value): third = value; case None: third = \"none\"; }\n\
            return first + \"|\" + second + \"|\" + third;\n\
        }";
    let (result, _) = run_with_io(source, "Main", "one\ntwo\n");
    assert_eq!(
        result,
        Ok(ExecutionValue::String("one|two|none".to_owned()))
    );
}

#[test]
fn read_line_consumes_multiple_lines_in_order() {
    let source = "using aster.core;\nusing aster.io;\n\
        public string Main() {\n\
            string a = \"\";\n\
            string b = \"\";\n\
            switch (ReadLine()) { case Some(value): a = value; case None: a = \"none\"; }\n\
            switch (ReadLine()) { case Some(value): b = value; case None: b = \"none\"; }\n\
            return a + \",\" + b;\n\
        }";
    let (result, _) = run_with_io(source, "Main", "first\nsecond\n");
    assert_eq!(
        result,
        Ok(ExecutionValue::String("first,second".to_owned()))
    );
}

// --- Section 22.28-35: integration ---------------------------------------------

#[test]
fn interpolation_and_to_string_feed_write_line() {
    let source = "using aster.io;\n\
        public string Main() {\n\
            int health = 42;\n\
            WriteLine($\"Health: {health}\");\n\
            WriteLine(health.ToString());\n\
            return \"ok\";\n\
        }";
    let (_, output) = run_with_io(source, "Main", "");
    assert_eq!(output, b"Health: 42\n42\n");
}

#[test]
fn try_parse_int_after_read_line_round_trips() {
    let source = "using aster.core;\nusing aster.io;\n\
        public int Main() {\n\
            Option<string> line = ReadLine();\n\
            switch (line) {\n\
                case Some(text):\n\
                    switch (text.TryParseInt()) { case Some(value): return value; case None: return -1; }\n\
                case None: return -2;\n\
            }\n\
        }";
    let (result, _) = run_with_io(source, "Main", "42\n");
    assert_eq!(result, Ok(ExecutionValue::Int(42)));
}

#[test]
fn postfix_try_propagates_read_line_into_an_option_returning_function() {
    let source = "using aster.core;\nusing aster.io;\n\
        public Option<string> Prompt() {\n\
            string line = ReadLine()?;\n\
            return Option<string>.Some(line + \"!\");\n\
        }\n\
        public string Main() {\n\
            switch (Prompt()) { case Some(value): return value; case None: return \"none\"; }\n\
        }";
    let (result, _) = run_with_io(source, "Main", "hi\n");
    assert_eq!(result, Ok(ExecutionValue::String("hi!".to_owned())));
    let (result, _) = run_with_io(source, "Main", "");
    assert_eq!(result, Ok(ExecutionValue::String("none".to_owned())));
}

#[test]
fn read_line_works_inside_a_helper() {
    let source = "using aster.core;\nusing aster.io;\n\
        public Option<string> ReadOne() { return ReadLine(); }\n\
        public string Main() {\n\
            switch (ReadOne()) { case Some(value): return value; case None: return \"none\"; }\n\
        }";
    let (result, _) = run_with_io(source, "Main", "value\n");
    assert_eq!(result, Ok(ExecutionValue::String("value".to_owned())));
}

#[test]
fn write_and_read_line_work_across_namespaces_and_files() {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let project_root =
        std::env::temp_dir().join(format!("aster-console-io-ns-{}-{id}", std::process::id()));
    let directory = project_root.join("app");
    std::fs::create_dir_all(&directory).expect("create namespace directory");
    std::fs::write(
        project_root.join("Aster.toml"),
        "[application]\nentry = \"app.Main\"\n",
    )
    .expect("write manifest");
    let path = directory.join("main.aster");
    let source = "namespace app;\n\
        using aster.core;\n\
        using aster.io;\n\
        public string Main() {\n\
            WriteLine(\"hello\");\n\
            switch (ReadLine()) { case Some(value): return value; case None: return \"none\"; }\n\
        }";
    std::fs::write(&path, source).expect("write namespaced project file");
    let compilation = compile_project(&path).map_err(|diagnostics| format!("{diagnostics:#?}"));
    std::fs::remove_dir_all(&project_root).ok();
    let compilation = compilation.expect("namespaced source should compile");
    let backend = MemoryConsoleBackend::new("world\n".as_bytes());
    let output_handle = backend.clone();
    let result = execute_with_console(&compilation.compilation.mir, "Main", Box::new(backend));
    assert_eq!(result, Ok(ExecutionValue::String("world".to_owned())));
    assert_eq!(output_handle.output(), b"hello\n");
}

#[test]
fn a_read_line_result_stored_in_a_field_stays_valid() {
    let source = "using aster.core;\nusing aster.io;\n\
        public class Holder {\n\
            public string text;\n\
            public Holder() { text = \"\"; }\n\
        }\n\
        public string Main() {\n\
            Holder holder = new Holder();\n\
            switch (ReadLine()) { case Some(value): holder.text = value; case None: holder.text = \"none\"; }\n\
            return holder.text;\n\
        }";
    let (result, _) = run_with_io(source, "Main", "stored\n");
    assert_eq!(result, Ok(ExecutionValue::String("stored".to_owned())));
}

#[test]
fn two_independent_executions_never_share_output_or_input() {
    let source = "using aster.core;\nusing aster.io;\n\
        public string Main() {\n\
            Write(\"out\");\n\
            switch (ReadLine()) { case Some(value): return value; case None: return \"none\"; }\n\
        }";
    let module = compile_mir(source);
    let first_backend = MemoryConsoleBackend::new("first\n".as_bytes());
    let first_handle = first_backend.clone();
    let second_backend = MemoryConsoleBackend::new("second\n".as_bytes());
    let second_handle = second_backend.clone();
    let first_result = execute_with_console(&module, "Main", Box::new(first_backend));
    let second_result = execute_with_console(&module, "Main", Box::new(second_backend));
    assert_eq!(first_result, Ok(ExecutionValue::String("first".to_owned())));
    assert_eq!(
        second_result,
        Ok(ExecutionValue::String("second".to_owned()))
    );
    assert_eq!(first_handle.output(), b"out");
    assert_eq!(second_handle.output(), b"out");
}

// --- Section 22.36-39: concurrency ---------------------------------------------

#[test]
fn console_io_is_rejected_inside_a_task_run_body() {
    let source = "using aster.io;\n\
        public int Body() { WriteLine(\"from a worker\"); return 0; }\n\
        public int Main() { Task<int> task = Task.Run(Body); return task.Wait(); }";
    let errors =
        run(source, "Main").expect_err("expected Task.Run with console I/O to be rejected");
    assert!(!errors.is_empty());
}

#[test]
fn console_io_is_rejected_inside_a_parallel_for_body() {
    let source = "using aster.io;\n\
        public void Body(int index) { Write(index.ToString()); }\n\
        public int Main() { Parallel.For(0, 4, Body); return 0; }";
    let errors =
        run(source, "Main").expect_err("expected Parallel.For with console I/O to be rejected");
    assert!(!errors.is_empty());
}

#[test]
fn console_io_is_rejected_inside_a_parallel_for_each_body() {
    let source = "using aster.io;\n\
        public void Body(int value) { Write(value.ToString()); }\n\
        public int Main() {\n\
            int[] values = new int[3];\n\
            Parallel.ForEach(values, Body);\n\
            return 0;\n\
        }";
    let errors =
        run(source, "Main").expect_err("expected Parallel.ForEach with console I/O to be rejected");
    assert!(!errors.is_empty());
}

#[test]
fn console_io_is_rejected_inside_a_parallel_reduce_operator() {
    let source = "using aster.io;\n\
        public int Accumulate(int total, int value) { WriteLine(value.ToString()); return total + value; }\n\
        public int Combine(int left, int right) { return left + right; }\n\
        public int Main() {\n\
            int[] values = new int[3];\n\
            return Parallel.Reduce(values, 0, Accumulate, Combine);\n\
        }";
    let errors =
        run(source, "Main").expect_err("expected Parallel.Reduce with console I/O to be rejected");
    assert!(
        !errors.is_empty(),
        "expected Parallel.Reduce with console I/O to be rejected"
    );
}

// --- Section 22.40-44: adulterated MIR -----------------------------------------

fn find_intrinsic_mut(
    module: &mut mir::Module,
    matches: impl Fn(&mir::Intrinsic) -> bool,
) -> &mut mir::Instruction {
    module
        .functions
        .iter_mut()
        .flat_map(|function| &mut function.blocks)
        .flat_map(|block| &mut block.instructions)
        .find(|instruction| {
            matches!(
                instruction,
                mir::Instruction::CallIntrinsic { intrinsic, .. } if matches(intrinsic)
            )
        })
        .expect("a matching intrinsic call exists")
}

#[test]
fn adulterated_mir_rejects_a_non_string_write_argument() {
    let mut module =
        compile_mir("using aster.io;\npublic string Main() { Write(\"a\"); return \"ok\"; }");
    let mir::Instruction::CallIntrinsic { arguments, .. } =
        find_intrinsic_mut(&mut module, |i| matches!(i, mir::Intrinsic::ConsoleWrite))
    else {
        unreachable!();
    };
    arguments[0].type_ = mir::Type::Int;
    let error = execute(&module, "Main").expect_err("a non-string Write argument must be rejected");
    assert!(!error.to_string().is_empty());
}

#[test]
fn adulterated_mir_rejects_a_non_void_write_line_return() {
    let mut module =
        compile_mir("using aster.io;\npublic string Main() { WriteLine(\"a\"); return \"ok\"; }");
    let mir::Instruction::CallIntrinsic { return_type, .. } =
        find_intrinsic_mut(&mut module, |i| {
            matches!(i, mir::Intrinsic::ConsoleWriteLine)
        })
    else {
        unreachable!();
    };
    *return_type = mir::Type::Int;
    let error =
        execute(&module, "Main").expect_err("a non-void WriteLine return type must be rejected");
    assert!(!error.to_string().is_empty());
}

#[test]
fn adulterated_mir_rejects_an_undeclared_read_line_destination() {
    let mut module = compile_mir(
        "using aster.core;\nusing aster.io;\n\
         public string Main() { switch (ReadLine()) { case Some(v): return v; case None: return \"none\"; } }",
    );
    let mir::Instruction::CallIntrinsic { destination, .. } =
        find_intrinsic_mut(&mut module, |i| {
            matches!(
                i,
                mir::Intrinsic::ConsoleReadLine | mir::Intrinsic::ConsoleReadLineTemporary
            )
        })
    else {
        unreachable!();
    };
    *destination = Some(mir::Place::Local(mir::LocalId(u32::MAX)));
    let error =
        execute(&module, "Main").expect_err("an undeclared ReadLine destination must be rejected");
    assert!(!error.to_string().is_empty());
}

#[test]
fn adulterated_mir_rejects_the_wrong_option_specialization_for_read_line() {
    let source = "using aster.core;\nusing aster.io;\n\
        public string Main() {\n\
            Option<int> other = \"1\".TryParseInt();\n\
            switch (other) { case Some(v): if (v > 0) {} case None: }\n\
            switch (ReadLine()) { case Some(value): return value; case None: return \"none\"; }\n\
        }";
    let mut module = compile_mir(source);
    let int_option_symbol = module
        .enums
        .iter()
        .find(|definition| {
            definition
                .cases
                .iter()
                .any(|case| case.fields.len() == 1 && case.fields[0].type_ == mir::Type::Int)
        })
        .expect("an Option<int> specialization exists")
        .symbol;
    let mir::Instruction::CallIntrinsic { return_type, .. } =
        find_intrinsic_mut(&mut module, |i| {
            matches!(
                i,
                mir::Intrinsic::ConsoleReadLine | mir::Intrinsic::ConsoleReadLineTemporary
            )
        })
    else {
        unreachable!();
    };
    *return_type = mir::Type::Enum(int_option_symbol);
    let error =
        execute(&module, "Main").expect_err("ReadLine returning Option<int> must be rejected");
    assert!(!error.to_string().is_empty());
}

// --- Section 22.45-47: memory ---------------------------------------------------

fn stats_with_io(source: &str, input: &str) -> MemoryStats {
    let module = compile_mir(source);
    let backend = MemoryConsoleBackend::new(input.as_bytes());
    let (_, stats) = execute_with_console_and_stats(&module, "Main", Box::new(backend))
        .expect("source should execute");
    stats
}

#[test]
fn repeated_writes_do_not_grow_the_arena() {
    let source = "using aster.io;\n\
        public int Main() {\n\
            for (int i = 0; i < 5000; i++) { Write(\"x\"); }\n\
            return 0;\n\
        }";
    let stats = stats_with_io(source, "");
    assert_eq!(stats.string_allocations, 0);
    assert_eq!(stats.object_allocations, 0);
    assert_eq!(stats.used_bytes, 0);
}

#[test]
fn repeated_temporary_read_lines_reclaim_memory() {
    let source = "using aster.core;\nusing aster.io;\n\
        public int Main() {\n\
            int total = 0;\n\
            for (int i = 0; i < 3000; i++) {\n\
                switch (ReadLine()) { case Some(text): total = total + text.Length; case None: }\n\
            }\n\
            return total;\n\
        }";
    let input = "a\n".repeat(3000);
    let stats = stats_with_io(source, &input);
    assert_eq!(stats.used_bytes, 0);
}

#[test]
fn a_persistent_read_line_result_survives_the_call() {
    let source = "using aster.core;\nusing aster.io;\n\
        public string Main() {\n\
            switch (ReadLine()) { case Some(value): return value; case None: return \"none\"; }\n\
        }";
    let (result, _) = run_with_io(source, "Main", "kept\n");
    assert_eq!(result, Ok(ExecutionValue::String("kept".to_owned())));
}
