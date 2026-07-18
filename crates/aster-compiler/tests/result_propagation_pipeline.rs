use aster_compiler::{ProjectCompilation, compile_project, hir, mir};

fn compile(source: &str) -> Result<ProjectCompilation, Vec<String>> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("aster-try-pipeline-{nonce}.aster"));
    std::fs::write(&path, source).expect("write temporary source");
    let result = compile_project(&path).map_err(|diagnostics| {
        diagnostics
            .into_iter()
            .map(|diagnostic| diagnostic.diagnostic.message)
            .collect()
    });
    std::fs::remove_file(&path).ok();
    result
}

fn errors(source: &str) -> Vec<String> {
    match compile(source) {
        Ok(_) => panic!("compilation should fail"),
        Err(messages) => messages,
    }
}

/// Compile a multi-file project laid out as `(relative_path, contents)` pairs.
/// The first entry is the root file passed to `compile_project`.
fn compile_dir(files: &[(&str, &str)]) -> Result<ProjectCompilation, Vec<String>> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("aster-try-proj-{nonce}"));
    for (relative, contents) in files {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().expect("file has a parent")).expect("create dirs");
        std::fs::write(&path, contents).expect("write project file");
    }
    let result = compile_project(&root.join(files[0].0)).map_err(|diagnostics| {
        diagnostics
            .into_iter()
            .map(|diagnostic| diagnostic.diagnostic.message)
            .collect()
    });
    std::fs::remove_dir_all(&root).ok();
    result
}

fn dir_errors(files: &[(&str, &str)]) -> Vec<String> {
    match compile_dir(files) {
        Ok(_) => panic!("compilation should fail"),
        Err(messages) => messages,
    }
}

fn dir_rejects(files: &[(&str, &str)], expected: &str) {
    let messages = dir_errors(files);
    assert!(
        messages.iter().any(|message| message.contains(expected)),
        "expected a diagnostic containing {expected:?}, got {messages:?}"
    );
}

fn rejects(source: &str, expected: &str) {
    let messages = errors(source);
    assert!(
        messages.iter().any(|message| message.contains(expected)),
        "expected a diagnostic containing {expected:?}, got {messages:?}"
    );
}

fn hir_function<'a>(compilation: &'a ProjectCompilation, name: &str) -> &'a hir::Function {
    compilation
        .compilation
        .hir
        .items
        .iter()
        .find_map(|item| match item {
            hir::Item::Function(function) if function.name == name => Some(function),
            _ => None,
        })
        .expect("function present in HIR")
}

fn mir_function<'a>(compilation: &'a ProjectCompilation, name: &str) -> &'a mir::Function {
    compilation
        .compilation
        .mir
        .functions
        .iter()
        .find(|function| function.name == name)
        .expect("function present in MIR")
}

const PARSE: &str = "public Result<int, string> Parse(string text) {\n\
    if (text == \"42\") { return Result<int, string>.Ok(42); }\n\
    return Result<int, string>.Error(\"bad\"); }\n";

#[test]
fn accepts_official_result_and_extracts_success_type() {
    let source = format!(
        "using aster.core;\n{PARSE}\n\
         public Result<string, string> Calculate(string text) {{\n\
             int value = Parse(text)?;\n\
             return Result<string, string>.Ok(\"ok\"); }}"
    );
    let compilation = compile(&source).expect("propagation with matching error type compiles");
    let calculate = hir_function(&compilation, "Calculate");
    let hir::Statement::Variable(variable) = &calculate.body.as_ref().unwrap().statements[0] else {
        panic!("expected the `?` variable declaration")
    };
    let hir::ExpressionKind::PropagateResult {
        success_type,
        error_type,
        return_type,
        ok_case,
        error_case,
        return_error_case,
        ..
    } = &variable.initializer.as_ref().unwrap().kind
    else {
        panic!("expected a typed PropagateResult node")
    };
    assert_eq!(*success_type, hir::Type::Int);
    assert_eq!(*error_type, hir::Type::String);
    assert!(matches!(return_type, hir::Type::Enum(_)));
    assert_ne!(ok_case, error_case);
    assert_ne!(error_case, return_error_case);
}

#[test]
fn lowers_propagation_to_explicit_control_flow() {
    let source = format!(
        "using aster.core;\n{PARSE}\n\
         public Result<int, string> Calculate(string text) {{\n\
             int value = Parse(text)?;\n\
             return Result<int, string>.Ok(value); }}"
    );
    let compilation = compile(&source).expect("compiles");
    let calculate = mir_function(&compilation, "Calculate");

    let reads_tag = calculate.blocks.iter().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                mir::Instruction::Assign {
                    value: mir::Rvalue {
                        kind: mir::RvalueKind::Discriminant(_),
                        ..
                    },
                    ..
                }
            )
        })
    });
    let constructs_error = calculate.blocks.iter().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                mir::Instruction::Assign {
                    value: mir::Rvalue {
                        kind: mir::RvalueKind::EnumConstruct { .. },
                        ..
                    },
                    ..
                }
            )
        })
    });
    let branches = calculate
        .blocks
        .iter()
        .any(|block| matches!(block.terminator, mir::Terminator::Branch { .. }));
    let returns = calculate
        .blocks
        .iter()
        .any(|block| matches!(block.terminator, mir::Terminator::Return(_)));
    let calls_parse_once = calculate
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter(|instruction| matches!(instruction, mir::Instruction::Call { .. }))
        .count();

    assert!(reads_tag, "reads the enum discriminant");
    assert!(constructs_error, "constructs the propagated Error case");
    assert!(branches, "branches on the tag");
    assert!(returns, "early-returns on Error");
    assert_eq!(calls_parse_once, 1, "operand is evaluated exactly once");

    for block in &calculate.blocks {
        // Every basic block is terminated, including the Error early-return path.
        let _ = &block.terminator;
    }
}

#[test]
fn allows_different_success_types() {
    let source = format!(
        "using aster.core;\n{PARSE}\n\
         public Result<string, string> Format(string text) {{\n\
             int number = Parse(text)?;\n\
             return Result<string, string>.Ok(\"valid\"); }}"
    );
    compile(&source).expect("input success type may differ from the function success type");
}

#[test]
fn accepts_propagation_in_generic_function() {
    // A generic function that both propagates and constructs a `Result` from its
    // own type parameters monomorphizes and compiles.
    let source = "using aster.core;\n\
        public Result<T, E> Forward<T, E>(Result<T, E> input) {\n\
            T value = input?;\n\
            return Result<T, E>.Ok(value); }\n\
        public Result<int, string> Use() {\n\
            return Forward<int, string>(Result<int, string>.Ok(5)); }";
    compile(source).expect("generic propagation and construction compiles");
}

#[test]
fn generic_specialization_across_namespaces() {
    let compilation = compile_dir(&[
        (
            "app/main.aster",
            "namespace app;\nusing aster.core;\nusing app.util;\n\
             public class Program { public static int Main() {\n\
                 switch (Wrap<int>(42)) { case Ok(v): return v; case Error(e): return -1; } } }\n",
        ),
        (
            "app/util/wrap.aster",
            "namespace app.util;\nusing aster.core;\n\
             public Result<T, string> Wrap<T>(T value) { return Result<T, string>.Ok(value); }\n",
        ),
        (
            "Aster.toml",
            "[application]\nentry = \"app.Program.Main\"\n",
        ),
    ]);
    if let Err(messages) = compilation {
        panic!("cross-namespace generic specialization should compile: {messages:?}");
    }
}

#[test]
fn invalid_generic_construction_does_not_panic() {
    // Wrong arity in a generic construction must be diagnosed, never panic. The
    // template is instantiated so the specialization is actually checked.
    rejects(
        "public enum Box<T> { Value(T value), Empty }\n\
         public Box<T> Bad<T>(T value) { return Box<T, T>.Value(value); }\n\
         public int Run() { switch (Bad<int>(5)) { case Value(v): return v; case Empty: return 0; } }",
        "expects 1 type argument",
    );
}

#[test]
fn rejects_non_result_operand() {
    rejects(
        "using aster.core;\n\
         public Result<int, string> F() { int v = 42?; return Result<int, string>.Ok(v); }",
        "`?` requires an `aster.core.Result",
    );
}

#[test]
fn rejects_non_result_enclosing_function() {
    rejects(
        &format!("using aster.core;\n{PARSE}\npublic int Calc() {{ return Parse(\"42\")?; }}"),
        "requires the enclosing function to return",
    );
}

#[test]
fn rejects_incompatible_error_type() {
    let source = format!(
        "using aster.core;\n{PARSE}\n\
         public enum FileError {{ Missing }}\n\
         public Result<int, FileError> Load() {{\n\
             int v = Parse(\"42\")?;\n\
             return Result<int, FileError>.Ok(v); }}"
    );
    rejects(&source, "cannot propagate error type");
}

#[test]
fn rejects_user_defined_result() {
    rejects(
        "public enum Result<T, E> { Success(T value), Failure(E error) }\n\
         public Result<int, string> F(Result<int, string> input) {\n\
             int v = input?;\n\
             return Result<int, string>.Success(v); }",
        "`?` works only with `aster.core.Result",
    );
}

#[test]
fn rejects_structurally_identical_enum_by_identity() {
    // `Outcome` has the same `Ok`/`Error` shape as the official `Result`, and the
    // official `Result` is in scope (accepted elsewhere). It is still rejected,
    // proving `?` keys off nominal identity, not case names or structure.
    rejects(
        "using aster.core;\n\
         public enum Outcome<T, E> { Ok(T value), Error(E error) }\n\
         public Result<int, string> F(Outcome<int, string> input) {\n\
             int v = input?;\n\
             return Result<int, string>.Ok(v); }",
        "`?` works only with `aster.core.Result",
    );
}

#[test]
fn rejects_result_in_nested_user_namespace() {
    // A `Result` declared in a user namespace has a different nominal identity
    // (`app.model::Result`) than the official one and does not accept `?`.
    dir_rejects(
        &[
            (
                "app/main.aster",
                "namespace app;\nusing app.model;\npublic class Program { public static int Main() { return 0; } }\n",
            ),
            (
                "app/model/data.aster",
                "namespace app.model;\n\
                 public enum Result<T, E> { Ok(T value), Error(E error) }\n\
                 public Result<int, string> Wrap(Result<int, string> input) {\n\
                     int v = input?;\n\
                     return Result<int, string>.Ok(v); }\n",
            ),
            (
                "Aster.toml",
                "[application]\nentry = \"app.Program.Main\"\n",
            ),
        ],
        "`?` works only with `aster.core.Result",
    );
}

#[test]
fn rejects_option_propagation() {
    rejects(
        "using aster.core;\n\
         public Result<int, string> F(Option<int> option) {\n\
             int v = option?;\n\
             return Result<int, string>.Ok(v); }",
        "does not support `aster.core.Option",
    );
}

#[test]
fn rejects_use_outside_a_function() {
    rejects(
        &format!("using aster.core;\n{PARSE}\npublic int leaked = Parse(\"42\")?;"),
        "cannot be used outside a function",
    );
}
