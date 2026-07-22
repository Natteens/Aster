//! M2C (revised): `aster.io.CombinePath(string, string) -> Result<string,
//! IOError>`, a purely lexical path-join operation. A nominal `Path` struct
//! was attempted and removed: empirical audit showed structs support
//! neither static nor instance methods on the Cranelift backend, and
//! struct-literal construction requires every referenced field to be
//! `public` (no same-namespace privilege), so a `Path` field would have
//! been publicly writable -- unable to preserve "immutable, validated-only"
//! as a real invariant. ASTER 1.0 therefore represents paths as plain
//! `string`; `Path` is deferred until the language can build opaque,
//! validated values without a public field.

use std::sync::atomic::{AtomicU64, Ordering};

use aster_codegen_cranelift::{ExecutionValue, MemoryStats, execute, execute_with_stats};
use aster_compiler::{compile_project, mir};

fn compile(source: &str) -> Result<mir::Module, String> {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("aster-path-{}-{id}.aster", std::process::id()));
    std::fs::write(&path, source).expect("write temporary project");
    let compilation = compile_project(&path).map_err(|diagnostics| format!("{diagnostics:#?}"));
    std::fs::remove_file(&path).ok();
    compilation.map(|compilation| compilation.compilation.mir)
}

fn compile_mir(source: &str) -> mir::Module {
    compile(source).expect("source should compile")
}

fn run(source: &str, function: &str) -> Result<ExecutionValue, String> {
    execute(&compile_mir(source), function).map_err(|error| error.to_string())
}

/// Doubles backslashes so raw text embeds safely inside an ASTER string
/// literal (ASTER's lexer treats `\` as an escape prefix).
fn aster_escape(text: &str) -> String {
    text.replace('\\', "\\\\")
}

// --- Combination ---------------------------------------------------------------

#[test]
fn combine_inserts_a_forward_slash_when_the_base_has_no_trailing_separator() {
    let source = "using aster.core;\nusing aster.io;\n\
        public string Main() {\n\
            switch (CombinePath(\"Data\", \"config.txt\")) {\n\
                case Ok(text): return text;\n\
                case Error(e): return \"error\";\n\
            }\n\
        }";
    assert_eq!(
        run(source, "Main"),
        Ok(ExecutionValue::String("Data/config.txt".to_owned()))
    );
}

#[test]
fn combine_does_not_duplicate_an_existing_trailing_forward_slash() {
    let source = "using aster.core;\nusing aster.io;\n\
        public string Main() {\n\
            switch (CombinePath(\"Data/\", \"config.txt\")) {\n\
                case Ok(text): return text;\n\
                case Error(e): return \"error\";\n\
            }\n\
        }";
    assert_eq!(
        run(source, "Main"),
        Ok(ExecutionValue::String("Data/config.txt".to_owned()))
    );
}

#[test]
fn combine_preserves_a_trailing_backslash_base_without_converting_it() {
    let source = "using aster.core;\nusing aster.io;\n\
        public string Main() {\n\
            switch (CombinePath(\"Data\\\\\", \"config.txt\")) {\n\
                case Ok(text): return text;\n\
                case Error(e): return \"error\";\n\
            }\n\
        }";
    assert_eq!(
        run(source, "Main"),
        Ok(ExecutionValue::String("Data\\config.txt".to_owned()))
    );
}

#[test]
fn combine_rejects_an_empty_base() {
    let source = "using aster.core;\nusing aster.io;\n\
        public int Main() {\n\
            switch (CombinePath(\"\", \"config.txt\")) {\n\
                case Ok(text): return -1;\n\
                case Error(e): switch (e.Kind) { case InvalidPath: return e.OsCode; default: return -2; }\n\
            }\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(0)));
}

#[test]
fn combine_rejects_an_empty_child() {
    let source = "using aster.core;\nusing aster.io;\n\
        public int Main() {\n\
            switch (CombinePath(\"Data\", \"\")) {\n\
                case Ok(text): return -1;\n\
                case Error(e): switch (e.Kind) { case InvalidPath: return e.OsCode; default: return -2; }\n\
            }\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(0)));
}

#[test]
fn combine_rejects_an_embedded_nul_in_the_base() {
    let source = "using aster.core;\nusing aster.io;\n\
        public int Main() {\n\
            char nul = (char)0;\n\
            string withNul = \"a\" + nul.ToString() + \"b\";\n\
            switch (CombinePath(withNul, \"config.txt\")) {\n\
                case Ok(text): return -1;\n\
                case Error(e): switch (e.Kind) { case InvalidPath: return e.OsCode; default: return -2; }\n\
            }\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(0)));
}

#[test]
fn combine_rejects_an_embedded_nul_in_the_child() {
    let source = "using aster.core;\nusing aster.io;\n\
        public int Main() {\n\
            char nul = (char)0;\n\
            string withNul = \"a\" + nul.ToString() + \"b\";\n\
            switch (CombinePath(\"Data\", withNul)) {\n\
                case Ok(text): return -1;\n\
                case Error(e): switch (e.Kind) { case InvalidPath: return e.OsCode; default: return -2; }\n\
            }\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(0)));
}

#[test]
fn combine_rejects_every_documented_form_of_absolute_child() {
    let rejected = [
        "/etc/config",
        "\\Windows\\config",
        "C:/config",
        "C:\\config",
        "//server/share",
        "\\\\server\\share",
    ];
    for child in rejected {
        let escaped = aster_escape(child);
        let source = format!(
            "using aster.core;\nusing aster.io;\n\
             public int Main() {{\n\
                 switch (CombinePath(\"Data\", \"{escaped}\")) {{\n\
                     case Ok(text): return -1;\n\
                     case Error(e): switch (e.Kind) {{ case InvalidPath: return 0; default: return -2; }}\n\
                 }}\n\
             }}"
        );
        assert_eq!(
            run(&source, "Main"),
            Ok(ExecutionValue::Int(0)),
            "child {child:?}"
        );
    }
}

#[test]
fn combine_does_not_reject_a_colon_that_is_not_a_drive_letter_prefix() {
    let source = "using aster.core;\nusing aster.io;\n\
        public string Main() {\n\
            switch (CombinePath(\"Data\", \"note:done.txt\")) {\n\
                case Ok(text): return text;\n\
                case Error(e): return \"error\";\n\
            }\n\
        }";
    assert_eq!(
        run(source, "Main"),
        Ok(ExecutionValue::String("Data/note:done.txt".to_owned()))
    );
}

#[test]
fn combine_preserves_unicode_whitespace_dots_and_internal_separators_exactly() {
    let cases = [
        ("Data", "  spaced.txt  ", "Data/  spaced.txt  "),
        ("Data", "./inner/../up.txt", "Data/./inner/../up.txt"),
        ("Data", ".hidden", "Data/.hidden"),
        ("Data", "café/日本語.txt", "Data/café/日本語.txt"),
        ("Data", "sub:section.txt", "Data/sub:section.txt"),
        ("nested/dir", "child.txt", "nested/dir/child.txt"),
    ];
    for (base, child, expected) in cases {
        let source = format!(
            "using aster.core;\nusing aster.io;\n\
             public string Main() {{\n\
                 switch (CombinePath(\"{base}\", \"{child}\")) {{\n\
                     case Ok(text): return text;\n\
                     case Error(e): return \"error\";\n\
                 }}\n\
             }}"
        );
        assert_eq!(
            run(&source, "Main"),
            Ok(ExecutionValue::String(expected.to_owned())),
            "base {base:?} child {child:?}"
        );
    }
}

// --- Result<string, IOError> / postfix `?` --------------------------------------

#[test]
fn build_path_propagates_success_through_postfix_try() {
    let source = "using aster.core;\nusing aster.io;\n\
        public Result<string, IOError> BuildPath() { return CombinePath(\"Data\", \"config.txt\"); }\n\
        public Result<string, IOError> Run() {\n\
            string path = BuildPath()?;\n\
            return Result<string, IOError>.Ok(path);\n\
        }\n\
        public string Main() {\n\
            switch (Run()) { case Ok(text): return text; case Error(e): return \"error\"; }\n\
        }";
    assert_eq!(
        run(source, "Main"),
        Ok(ExecutionValue::String("Data/config.txt".to_owned()))
    );
}

#[test]
fn build_path_propagates_invalid_path_through_postfix_try() {
    let source = "using aster.core;\nusing aster.io;\n\
        public Result<int, IOError> Run() {\n\
            string path = CombinePath(\"\", \"x\")?;\n\
            return Result<int, IOError>.Ok(path.Length);\n\
        }\n\
        public int Main() {\n\
            switch (Run()) {\n\
                case Ok(v): return -1;\n\
                case Error(e): switch (e.Kind) { case InvalidPath: return e.OsCode; default: return -2; }\n\
            }\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(0)));
}

#[test]
fn combine_path_is_evaluated_exactly_once_per_argument() {
    let source = "using aster.core;\nusing aster.io;\n\
        public class Counter {\n\
            public int calls;\n\
            public string Base() { calls = calls + 1; return \"Data\"; }\n\
        }\n\
        public Result<int, IOError> Wrap(Counter counter) {\n\
            string path = CombinePath(counter.Base(), \"config.txt\")?;\n\
            return Result<int, IOError>.Ok(path.Length);\n\
        }\n\
        public int Main() {\n\
            Counter counter = new Counter();\n\
            Wrap(counter);\n\
            return counter.calls;\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(1)));
}

// --- Lifetime / escape analysis: reused string infrastructure -------------------

#[test]
fn a_locally_combined_path_is_correct_when_used_only_locally() {
    let source = "using aster.core;\nusing aster.io;\n\
        public Result<int, IOError> Run() {\n\
            string path = CombinePath(\"Data\", \"local.txt\")?;\n\
            return Result<int, IOError>.Ok(path.Length);\n\
        }\n\
        public int Main() {\n\
            switch (Run()) { case Ok(v): return v; case Error(e): return -1; }\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(14)));
}

#[test]
fn a_combined_path_returned_from_a_function_stays_correct_after_further_allocations() {
    let source = "using aster.core;\nusing aster.io;\n\
        public Result<string, IOError> Build() { return CombinePath(\"Hello, \", \"World!\"); }\n\
        public Result<string, IOError> AppendJunk(string result) {\n\
            string junk = (\"filler-\" + \"text-\") + (\"more-\" + \"filler\");\n\
            return Result<string, IOError>.Ok(result + \"|\" + junk.Length.ToString());\n\
        }\n\
        public Result<string, IOError> Run() {\n\
            string built = Build()?;\n\
            return AppendJunk(built);\n\
        }\n\
        public string Main() {\n\
            switch (Run()) { case Ok(text): return text; case Error(e): return \"error\"; }\n\
        }";
    assert_eq!(
        run(source, "Main"),
        Ok(ExecutionValue::String("Hello, /World!|23".to_owned()))
    );
}

#[test]
fn a_combined_path_stored_in_a_class_field_survives_a_round_trip() {
    let source = "using aster.core;\nusing aster.io;\n\
        public class Holder { public string stored; public Holder() { stored = \"\"; } }\n\
        public Result<int, IOError> Run() {\n\
            Holder holder = new Holder();\n\
            holder.stored = CombinePath(\"Data\", \"held.txt\")?;\n\
            return Result<int, IOError>.Ok(holder.stored.Length);\n\
        }\n\
        public int Main() {\n\
            switch (Run()) { case Ok(v): return v; case Error(e): return -1; }\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(13)));
}

#[test]
fn a_combined_path_in_an_option_string_preserves_the_text() {
    let source = "using aster.core;\nusing aster.io;\n\
        public Option<string> Maybe(bool present) {\n\
            if (present) {\n\
                switch (CombinePath(\"Data\", \"maybe.txt\")) {\n\
                    case Ok(text): return Option<string>.Some(text);\n\
                    case Error(e): return Option<string>.None;\n\
                }\n\
            }\n\
            return Option<string>.None;\n\
        }\n\
        public int Main() {\n\
            switch (Maybe(true)) {\n\
                case Some(text):\n\
                    if (text.Length != 14) { return 1; }\n\
                    switch (Maybe(false)) {\n\
                        case Some(t2): return 3;\n\
                        case None: return 0;\n\
                    }\n\
                case None: return 2;\n\
            }\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(0)));
}

#[test]
fn a_result_of_combine_path_preserves_success_and_error_through_switch() {
    let source = "using aster.core;\nusing aster.io;\n\
        public int Main() {\n\
            switch (CombinePath(\"Data\", \"ok.txt\")) {\n\
                case Ok(text):\n\
                    if (text.Length != 11) { return 1; }\n\
                    switch (CombinePath(\"\", \"x\")) {\n\
                        case Ok(t2): return 3;\n\
                        case Error(e): return 0;\n\
                    }\n\
                case Error(e): return 2;\n\
            }\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(0)));
}

#[test]
fn a_generic_function_returns_a_combined_path_argument_with_its_text_intact() {
    let source = "using aster.core;\nusing aster.io;\n\
        public T First<T>(T a, T b) { return a; }\n\
        public Result<int, IOError> Run() {\n\
            string picked = CombinePath(\"Data\", \"picked.txt\")?;\n\
            string other = CombinePath(\"Data\", \"other.txt\")?;\n\
            string chosen = First(picked, other);\n\
            return Result<int, IOError>.Ok(chosen.Length);\n\
        }\n\
        public int Main() {\n\
            switch (Run()) { case Ok(v): return v; case Error(e): return -1; }\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(15)));
}

#[test]
fn a_combined_path_built_and_used_across_two_files_preserves_its_text() {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let root =
        std::env::temp_dir().join(format!("aster-path-multifile-{}-{id}", std::process::id()));
    std::fs::create_dir_all(&root).expect("create project root");
    std::fs::write(
        root.join("Aster.toml"),
        "[application]\nentry = \"app.Main\"\n",
    )
    .expect("write manifest");
    let app_dir = root.join("app");
    std::fs::create_dir_all(&app_dir).expect("create app dir");
    std::fs::write(
        app_dir.join("main.aster"),
        "namespace app;\n\
         using aster.core;\n\
         using aster.io;\n\
         public Result<int, IOError> Run() {\n\
             string built = Helpers.Build(\"cross-file.txt\")?;\n\
             return Result<int, IOError>.Ok(built.Length);\n\
         }\n\
         public int Main() {\n\
             switch (Run()) { case Ok(v): return v; case Error(e): return -1; }\n\
         }",
    )
    .expect("write main.aster");
    std::fs::write(
        app_dir.join("helpers.aster"),
        "namespace app;\n\
         using aster.core;\n\
         using aster.io;\n\
         public class Helpers {\n\
             public static Result<string, IOError> Build(string name) { return CombinePath(\"Data\", name); }\n\
         }",
    )
    .expect("write helpers.aster");
    let compilation = compile_project(&app_dir.join("main.aster"))
        .map_err(|diagnostics| format!("{diagnostics:#?}"));
    std::fs::remove_dir_all(&root).ok();
    let compilation = compilation.expect("multifile project using CombinePath should compile");
    assert_eq!(
        execute(&compilation.compilation.mir, "Main"),
        Ok(ExecutionValue::Int(19))
    );
}

// --- Memory: allocation counts -------------------------------------------------

fn stats_for(source: &str) -> MemoryStats {
    let module = compile_mir(source);
    let (_, stats) = execute_with_stats(&module, "Main").expect("source should execute");
    stats
}

#[test]
fn thousands_of_combine_path_calls_allocate_only_the_resulting_string_no_objects() {
    let source = "using aster.core;\nusing aster.io;\n\
        public Result<int, IOError> Run() {\n\
            int total = 0;\n\
            for (int i = 0; i < 5000; i++) {\n\
                string path = CombinePath(\"Data\", \"config.txt\")?;\n\
                total = total + path.Length;\n\
            }\n\
            return Result<int, IOError>.Ok(total);\n\
        }\n\
        public int Main() {\n\
            switch (Run()) { case Ok(v): return v; case Error(e): return -1; }\n\
        }";
    let stats = stats_for(source);
    assert_eq!(stats.object_allocations, 0);
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(15 * 5000)));
}

// --- Nominal-type regression: no aster.io.Path export ---------------------------

#[test]
fn no_official_path_type_is_exported_by_aster_io() {
    // A user-declared `Path` (anywhere) must not collide with anything from
    // `aster.io`: there is no official `aster.io::Path` symbol anymore.
    let source = "using aster.core;\nusing aster.io;\n\
        public struct Path { public string Value; }\n\
        public int Main() {\n\
            Path p = Path { Value: \"user-owned\" };\n\
            switch (CombinePath(\"Data\", \"config.txt\")) {\n\
                case Ok(text): return p.Value.Length + text.Length;\n\
                case Error(e): return -1;\n\
            }\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(10 + 15)));
}

// --- Validation: MIR adulteration (reuses IOError's generic struct check) ------

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

const CONSTRUCT_VIA_COMBINE: &str = "using aster.core;\nusing aster.io;\n\
    public Result<int, IOError> Run() {\n\
        string path = CombinePath(\"Data\", \"ok.txt\")?;\n\
        return Result<int, IOError>.Ok(path.Length);\n\
    }\n\
    public int Main() {\n\
        switch (Run()) { case Ok(v): return v; case Error(e): return -1; }\n\
    }";

#[test]
fn adulterated_mir_rejects_an_io_error_construction_reached_through_combine_path_with_a_missing_field()
 {
    let mut module = compile_mir(CONSTRUCT_VIA_COMBINE);
    // `CombinePath`'s own `IOError { Kind: InvalidPath, OsCode: 0 }`
    // construction is compiled into this module (`aster.io` is embedded
    // stdlib source); adulterate it to prove the same generic
    // `validate_struct_literal_shapes` check still guards it.
    let instruction = find_first_matching(&mut module, |instruction| {
        matches!(
            instruction,
            mir::Instruction::Assign {
                value: mir::Rvalue {
                    type_: mir::Type::User(_),
                    kind: mir::RvalueKind::Aggregate(_),
                    ..
                },
                ..
            }
        )
    });
    let mir::Instruction::Assign {
        value:
            mir::Rvalue {
                kind: mir::RvalueKind::Aggregate(fields),
                ..
            },
        ..
    } = instruction
    else {
        unreachable!();
    };
    fields.pop();
    let error = execute(&module, "Main").expect_err("an IOError missing a field must be rejected");
    assert!(!error.to_string().is_empty());
}

// --- Security: no panic/abort/trap on any input -------------------------------

#[test]
fn no_combine_path_input_panics_the_compiler_or_the_runtime() {
    let probes = [
        ("\"\"", "\"\""),
        ("\"Data\"", "\"\""),
        ("\"Data\"", "\"/\""),
        ("\"Data\"", "\"\\\\\\\\\""),
        ("\"Data\"", "\"C:\""),
        ("\"Data\"", "\"a\""),
        ("\"Data\"", "\"....\""),
    ];
    for (base, child) in probes {
        let source = format!(
            "using aster.core;\nusing aster.io;\n\
             public int Main() {{\n\
                 switch (CombinePath({base}, {child})) {{\n\
                     case Ok(text): return text.Length;\n\
                     case Error(e): return e.OsCode;\n\
                 }}\n\
             }}"
        );
        // A plain `Ok`/`Err` result (never a panic unwinding this test
        // process) is itself the proof of no panic/abort/trap.
        let _ = run(&source, "Main");
    }
}

// --- Regressions ---------------------------------------------------------------

#[test]
fn io_error_result_option_strings_and_assignments_still_work_alongside_combine_path() {
    assert_eq!(
        run(
            "using aster.io;\n\
             public int Main() {\n\
                 IOError error = IOError { Kind: IOErrorKind.NotFound, OsCode: 4 };\n\
                 switch (error.Kind) { case NotFound: return error.OsCode; default: return -1; }\n\
             }",
            "Main"
        ),
        Ok(ExecutionValue::Int(4))
    );
    assert_eq!(
        run(
            "using aster.core;\n\
             public Result<int, string> Parse() { return Result<int, string>.Ok(9); }\n\
             public int Main() { switch (Parse()) { case Ok(v): return v; case Error(e): return -1; } }",
            "Main"
        ),
        Ok(ExecutionValue::Int(9))
    );
    assert_eq!(
        run(
            "public int Main() {\n\
                 int total = 0;\n\
                 total = total + 1;\n\
                 return total;\n\
             }",
            "Main"
        ),
        Ok(ExecutionValue::Int(1))
    );
}
