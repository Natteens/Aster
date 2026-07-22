//! M1E: closure/integration/adversarial suite for Milestone M1 (Text,
//! Parsing, Formatting, Terminal I/O). Does not test any single M1A-D
//! feature in isolation again (see `string_methods.rs`, `string_try_parse.rs`,
//! `string_try_parse_float.rs`, `option_try_propagation.rs`, `to_string.rs`,
//! `console_io.rs`); covers only what only makes sense to test with several
//! features combined: the mandatory end-to-end program, cross-feature
//! evaluation order, nested-enum escape analysis, and console adversarial
//! cases the M1D suite didn't exercise (lone `\r`, which M1E's audit found
//! was mishandled -- see the `strip_line_terminator` fix in
//! `aster-runtime/src/io.rs`).

use std::sync::atomic::{AtomicU64, Ordering};

use aster_codegen_cranelift::{ExecutionValue, execute, execute_with_console};
use aster_compiler::{compile_project, mir};
use aster_runtime::MemoryConsoleBackend;

fn compile(source: &str) -> Result<mir::Module, String> {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("aster-m1e-{}-{id}.aster", std::process::id()));
    std::fs::write(&path, source).expect("write temporary project");
    let compilation = compile_project(&path).map_err(|diagnostics| format!("{diagnostics:#?}"));
    std::fs::remove_file(&path).ok();
    compilation.map(|compilation| compilation.compilation.mir)
}

fn run(source: &str, function: &str) -> Result<ExecutionValue, String> {
    execute(&compile(source).expect("source should compile"), function)
        .map_err(|error| error.to_string())
}

fn run_with_io(
    source: &str,
    function: &str,
    input: &str,
) -> (Result<ExecutionValue, String>, Vec<u8>) {
    let module = compile(source).expect("source should compile");
    let backend = MemoryConsoleBackend::new(input.as_bytes());
    let output_handle = backend.clone();
    let result =
        execute_with_console(&module, function, Box::new(backend)).map_err(|e| e.to_string());
    (result, output_handle.output())
}

// --- Section 5: mandatory integrated program --------------------------------

const INTEGRATED_PROGRAM: &str = "using aster.core;\nusing aster.io;\n\
    public int Main() {\n\
        Write(\"Input: \");\n\
        Option<string> maybeLine = ReadLine();\n\
        switch (maybeLine) { case Some(line): return Process(line); case None: return 1; }\n\
    }\n\
    public int Process(string line) {\n\
        if (!line.Contains(\":\")) { WriteLine(\"invalid\"); return 1; }\n\
        int separator = line.IndexOf(\":\");\n\
        string name = line.Substring(0, separator);\n\
        string valueText = line.Substring(separator + 1);\n\
        Option<double> parsed = valueText.TryParseDouble();\n\
        switch (parsed) { case Some(value): return PrintResult(name, value); case None: return 2; }\n\
    }\n\
    public int PrintResult(string name, double value) {\n\
        WriteLine($\"{name}: {value.ToString()}\");\n\
        return 0;\n\
    }";

#[test]
fn integrated_program_runs_end_to_end_via_in_memory_backend() {
    let (result, output) = run_with_io(INTEGRATED_PROGRAM, "Main", "score:2.5\n");
    assert_eq!(result, Ok(ExecutionValue::Int(0)));
    assert_eq!(output, b"Input: score: 2.5\n");

    let (result, output) = run_with_io(INTEGRATED_PROGRAM, "Main", "noseparator\n");
    assert_eq!(result, Ok(ExecutionValue::Int(1)));
    assert_eq!(output, b"Input: invalid\n");

    let (result, output) = run_with_io(INTEGRATED_PROGRAM, "Main", "score:notanumber\n");
    assert_eq!(result, Ok(ExecutionValue::Int(2)));
    assert_eq!(output, b"Input: ");

    let (result, output) = run_with_io(INTEGRATED_PROGRAM, "Main", "");
    assert_eq!(result, Ok(ExecutionValue::Int(1)));
    assert_eq!(output, b"Input: ");
}

// The `aster run` subprocess variant of this same program lives in
// `aster-cli/tests/cli.rs` (`integrated_m1_program_runs_via_aster_run_subprocess`)
// since only that crate's test binary gets `CARGO_BIN_EXE_aster` from Cargo.

// --- Section 8 (supplement to M1A): Unicode gaps not covered by string_methods.rs ---

#[test]
fn three_byte_char_combining_mark_and_embedded_nul_are_scalar_correct() {
    // "a" (1B) + 你 U+4F60 (3B) + "e" + U+0301 combining acute (2B, a
    // separate scalar from "e" -- no grapheme clustering) + NUL (a real byte,
    // not the `\0` escape ASTER's lexer doesn't support) + "z": 6 scalars.
    let source = format!(
        "public int Main() {{\n\
             string text = \"a你e\u{0301}{nul}z\";\n\
             if (text.Length != 6) {{ return 1; }}\n\
             if (text.IndexOf(\"你\") != 1) {{ return 2; }}\n\
             if (text.Substring(2, 2) != \"e\u{0301}\") {{ return 3; }}\n\
             if (!text.Contains(\"{nul}z\")) {{ return 4; }}\n\
             return 0;\n\
         }}",
        nul = '\0'
    );
    assert_eq!(run(&source, "Main"), Ok(ExecutionValue::Int(0)));
}

// --- Section 10: escape analysis for reference payloads inside nested enums ---

#[test]
fn option_of_option_string_payload_escapes_correctly_when_returned() {
    let source = "using aster.core;\nusing aster.io;\n\
        public string Main() {\n\
            Option<Option<string>> outer = Option<Option<string>>.Some(ReadLine());\n\
            switch (outer) {\n\
                case Some(inner):\n\
                    switch (inner) { case Some(value): return value; case None: return \"inner-none\"; }\n\
                case None: return \"outer-none\";\n\
            }\n\
        }";
    let (result, _) = run_with_io(source, "Main", "nested\n");
    assert_eq!(result, Ok(ExecutionValue::String("nested".to_owned())));
}

#[test]
fn result_of_option_string_payload_survives_field_storage() {
    let source = "using aster.core;\nusing aster.io;\n\
        public class Holder {\n\
            public Result<Option<string>, string> slot;\n\
            public Holder() { slot = Result<Option<string>, string>.Error(\"init\"); }\n\
        }\n\
        public string Main() {\n\
            Holder holder = new Holder();\n\
            holder.slot = Result<Option<string>, string>.Ok(ReadLine());\n\
            switch (holder.slot) {\n\
                case Ok(inner):\n\
                    switch (inner) { case Some(value): return value; case None: return \"none\"; }\n\
                case Error(message): return message;\n\
            }\n\
        }";
    let (result, _) = run_with_io(source, "Main", "stored\n");
    assert_eq!(result, Ok(ExecutionValue::String("stored".to_owned())));
}

#[test]
fn user_enum_holding_a_string_payload_from_read_line_escapes_through_a_helper() {
    let source = "using aster.core;\nusing aster.io;\n\
        enum Message { Empty, Text(string value) }\n\
        public Message Wrap(Option<string> input) {\n\
            switch (input) { case Some(value): return Message.Text(value); case None: return Message.Empty; }\n\
        }\n\
        public string Extract(Message message) {\n\
            switch (message) { case Text(value): return value; case Empty: return \"empty\"; }\n\
        }\n\
        public string Main() {\n\
            Message wrapped = Wrap(ReadLine());\n\
            return Extract(wrapped);\n\
        }";
    let (result, _) = run_with_io(source, "Main", "via-helper\n");
    assert_eq!(result, Ok(ExecutionValue::String("via-helper".to_owned())));
}

#[test]
fn an_aliased_option_string_used_only_locally_still_reads_correctly() {
    // A non-escaping use (only `.Length` is read out) exercises the
    // `Temporary` path; the enum copy/alias/discriminant-read machinery must
    // still produce the right value even when the string never leaves the
    // function.
    let source = "using aster.core;\nusing aster.io;\n\
        public int Main() {\n\
            Option<string> line = ReadLine();\n\
            Option<string> alias = line;\n\
            switch (alias) { case Some(value): return value.Length; case None: return -1; }\n\
        }";
    let (result, _) = run_with_io(source, "Main", "abcde\n");
    assert_eq!(result, Ok(ExecutionValue::Int(5)));
}

#[test]
fn postfix_try_on_nested_option_string_propagates_the_inner_value() {
    let source = "using aster.core;\nusing aster.io;\n\
        public Option<string> Prompt() {\n\
            Option<string> outer = Option<string>.Some(ReadLine()?);\n\
            return outer;\n\
        }\n\
        public string Main() {\n\
            switch (Prompt()) { case Some(value): return value; case None: return \"none\"; }\n\
        }";
    let (result, _) = run_with_io(source, "Main", "propagated\n");
    assert_eq!(result, Ok(ExecutionValue::String("propagated".to_owned())));
}

// --- Section 9: cross-feature evaluation order ------------------------------

#[test]
fn each_string_operation_evaluates_its_receiver_exactly_once() {
    let cases = [
        (
            "Contains",
            "if (counter.Next().Contains(\"x\")) { return counter.calls; } return counter.calls;",
        ),
        (
            "Substring",
            "string s = counter.Next().Substring(1); return counter.calls;",
        ),
        (
            "TryParseInt",
            "switch (counter.Next().TryParseInt()) { case Some(v): return counter.calls; case None: return counter.calls; }",
        ),
        (
            "TryParseDouble",
            "switch (counter.Next().TryParseDouble()) { case Some(v): return counter.calls; case None: return counter.calls; }",
        ),
    ];
    for (label, tail) in cases {
        let source = format!(
            "using aster.core;\n\
             public class Counter {{ public int calls; public string Next() {{ calls = calls + 1; return \"1x2\"; }} }}\n\
             public int Main() {{\n\
                 Counter counter = new Counter();\n\
                 {tail}\n\
             }}"
        );
        assert_eq!(run(&source, "Main"), Ok(ExecutionValue::Int(1)), "{label}");
    }
}

#[test]
fn to_string_and_write_line_evaluate_their_argument_exactly_once() {
    let source = "using aster.io;\n\
        public class Counter { public int calls; public int Next() { calls = calls + 1; return 41; } }\n\
        public string Main() {\n\
            Counter counter = new Counter();\n\
            string text = counter.Next().ToString();\n\
            return counter.calls.ToString() + \":\" + text;\n\
        }";
    assert_eq!(
        run(source, "Main"),
        Ok(ExecutionValue::String("1:41".to_owned()))
    );

    let source = "using aster.io;\n\
        public class Counter { public int calls; public string Next() { calls = calls + 1; return \"text\"; } }\n\
        public string Main() {\n\
            Counter counter = new Counter();\n\
            WriteLine(counter.Next());\n\
            return counter.calls.ToString();\n\
        }";
    let (result, output) = run_with_io(source, "Main", "");
    assert_eq!(result, Ok(ExecutionValue::String("1".to_owned())));
    assert_eq!(output, b"text\n");
}

#[test]
fn read_line_performs_exactly_one_read_per_call() {
    let source = "using aster.core;\nusing aster.io;\n\
        public string Main() {\n\
            Option<string> first = ReadLine();\n\
            Option<string> second = ReadLine();\n\
            string a = \"\"; string b = \"\";\n\
            switch (first) { case Some(v): a = v; case None: a = \"none\"; }\n\
            switch (second) { case Some(v): b = v; case None: b = \"none\"; }\n\
            return a + \",\" + b;\n\
        }";
    let (result, _) = run_with_io(source, "Main", "one\ntwo\n");
    assert_eq!(result, Ok(ExecutionValue::String("one,two".to_owned())));
}

#[test]
fn sequential_write_and_write_line_preserve_program_order() {
    let source = "using aster.io;\n\
        public string GetFirst() { return \"1\"; }\n\
        public string GetSecond() { return \"2\"; }\n\
        public string Main() { Write(GetFirst()); WriteLine(GetSecond()); return \"ok\"; }";
    let (_, output) = run_with_io(source, "Main", "");
    assert_eq!(output, b"12\n");
}

// --- Section 11: console adversarial cases not yet covered by console_io.rs ---

#[test]
fn a_lone_trailing_cr_with_no_lf_is_preserved_as_content() {
    let source = "using aster.core;\nusing aster.io;\n\
        public string Main() {\n\
            switch (ReadLine()) { case Some(value): return value; case None: return \"none\"; }\n\
        }";
    let (result, _) = run_with_io(source, "Main", "hello\r");
    assert_eq!(result, Ok(ExecutionValue::String("hello\r".to_owned())));
}

#[test]
fn crlf_only_and_lf_only_both_produce_an_empty_line() {
    let source = "using aster.core;\nusing aster.io;\n\
        public int Main() {\n\
            switch (ReadLine()) { case Some(value): return value.Length; case None: return -1; }\n\
        }";
    let (result, _) = run_with_io(source, "Main", "\r\n");
    assert_eq!(result, Ok(ExecutionValue::Int(0)));
    let (result, _) = run_with_io(source, "Main", "\n");
    assert_eq!(result, Ok(ExecutionValue::Int(0)));
}

#[test]
fn repeated_read_line_after_eof_never_allocates_or_errors() {
    let source = "using aster.core;\nusing aster.io;\n\
        public int Main() {\n\
            int none_count = 0;\n\
            for (int i = 0; i < 50; i++) {\n\
                switch (ReadLine()) { case Some(v): none_count = none_count - 1000; case None: none_count = none_count + 1; }\n\
            }\n\
            return none_count;\n\
        }";
    let module = compile(source).expect("compiles");
    let backend = MemoryConsoleBackend::new(Vec::new());
    let (value, stats) =
        aster_codegen_cranelift::execute_with_console_and_stats(&module, "Main", Box::new(backend))
            .expect("executes without error");
    assert_eq!(value, ExecutionValue::Int(50));
    assert_eq!(stats.used_bytes, 0);
    assert_eq!(stats.total_allocations, 0);
}

// --- Section 12: worker-console validation call-graph nuances ---------------

#[test]
fn console_io_is_rejected_transitively_through_a_helper() {
    let source = "using aster.io;\n\
        public void Helper() { WriteLine(\"nested\"); }\n\
        public int Body() { Helper(); return 0; }\n\
        public int Main() { Task<int> task = Task.Run(Body); return task.Wait(); }";
    let error = run(source, "Main").expect_err("transitive console I/O must be rejected");
    assert!(error.contains("Task.Run"));
}

#[test]
fn console_io_reachable_only_via_recursion_is_still_rejected() {
    let source = "using aster.io;\n\
        public int Countdown(int n) {\n\
            if (n <= 0) { WriteLine(\"done\"); return 0; }\n\
            return Countdown(n - 1);\n\
        }\n\
        public int Body() { return Countdown(3); }\n\
        public int Main() { Task<int> task = Task.Run(Body); return task.Wait(); }";
    let error = run(source, "Main").expect_err("recursive console I/O must be rejected");
    assert!(error.contains("Task.Run"));
}

#[test]
fn console_io_in_an_unreachable_function_does_not_taint_an_unrelated_task_run() {
    let source = "using aster.io;\n\
        public void NeverCalled() { WriteLine(\"should not matter\"); }\n\
        public int Body() { return 42; }\n\
        public int Main() { Task<int> task = Task.Run(Body); return task.Wait(); }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn a_plain_function_calling_console_io_outside_any_worker_is_unaffected() {
    let source = "using aster.io;\n\
        public int Body() { WriteLine(\"fine\"); return 0; }\n\
        public int Main() { return Body(); }";
    let (result, output) = run_with_io(source, "Main", "");
    assert_eq!(result, Ok(ExecutionValue::Int(0)));
    assert_eq!(output, b"fine\n");
}

// --- Section 13: MIR adulteration not yet exercised by any single M1 file ---

fn find_first_intrinsic_mut(
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
fn adulterated_mir_rejects_string_from_float_receiving_a_double_operand() {
    let mut module =
        compile("public string Main() { float value = 2.5f; return value.ToString(); }")
            .expect("compiles");
    let mir::Instruction::CallIntrinsic { arguments, .. } =
        find_first_intrinsic_mut(&mut module, |i| {
            matches!(i, mir::Intrinsic::StringFromFloat)
        })
    else {
        unreachable!();
    };
    arguments[0].type_ = mir::Type::Double;
    let error = execute(&module, "Main")
        .expect_err("StringFromFloat with a `double` operand must be rejected");
    assert!(!error.to_string().is_empty());
}

#[test]
fn adulterated_mir_rejects_string_from_double_receiving_a_float_operand() {
    let mut module =
        compile("public string Main() { double value = 2.5; return value.ToString(); }")
            .expect("compiles");
    let mir::Instruction::CallIntrinsic { arguments, .. } =
        find_first_intrinsic_mut(&mut module, |i| {
            matches!(i, mir::Intrinsic::StringFromDouble)
        })
    else {
        unreachable!();
    };
    arguments[0].type_ = mir::Type::Float;
    let error = execute(&module, "Main")
        .expect_err("StringFromDouble with a `float` operand must be rejected");
    assert!(!error.to_string().is_empty());
}
