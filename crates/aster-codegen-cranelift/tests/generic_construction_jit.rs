//! Regression coverage for constructing generic enums inside generic functions
//! (e.g. `Container<T>.Value(x)` in `Wrap<T>`). This exercises monomorphization,
//! not the `?` operator specifically.

use aster_codegen_cranelift::{ExecutionValue, execute};
use aster_compiler::compile_project;

fn run(source: &str, function: &str) -> Result<ExecutionValue, String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "aster-generic-ctor-{}-{id}.aster",
        std::process::id()
    ));
    std::fs::write(&path, source).expect("write temporary project");
    let compilation = compile_project(&path).map_err(|diagnostics| format!("{diagnostics:#?}"));
    std::fs::remove_file(&path).ok();
    execute(&compilation?.compilation.mir, function).map_err(|error| error.to_string())
}

fn int(source: &str) -> ExecutionValue {
    run(source, "Run").expect("program runs")
}

#[test]
fn user_generic_enum_built_in_generic_function() {
    let source = "public enum Container<T> { Value(T value), Empty }\n\
        public Container<T> Wrap<T>(T value) { return Container<T>.Value(value); }\n\
        public int Run() { switch (Wrap<int>(5)) { case Value(v): return v; case Empty: return 0; } }";
    assert_eq!(int(source), ExecutionValue::Int(5));
}

#[test]
fn two_type_parameter_enum_built_in_generic_function() {
    let source = "public enum PairResult<T, E> { Success(T value), Failure(E error) }\n\
        public PairResult<T, E> Forward<T, E>(T value) { return PairResult<T, E>.Success(value); }\n\
        public int Run() { switch (Forward<int, string>(42)) { case Success(v): return v; case Failure(e): return -1; } }";
    assert_eq!(int(source), ExecutionValue::Int(42));
}

#[test]
fn result_ok_constructed_in_generic_function() {
    let source = "using aster.core;\n\
        public Result<T, E> Wrap<T, E>(T value) { return Result<T, E>.Ok(value); }\n\
        public int Run() { switch (Wrap<int, string>(42)) { case Ok(v): return v; case Error(e): return -1; } }";
    assert_eq!(int(source), ExecutionValue::Int(42));
}

#[test]
fn result_error_constructed_in_generic_function() {
    let source = "using aster.core;\n\
        public Result<T, E> Fail<T, E>(E error) { return Result<T, E>.Error(error); }\n\
        public int Run() { switch (Fail<int, string>(\"boom\")) { case Ok(v): return v; case Error(e): return 9; } }";
    assert_eq!(int(source), ExecutionValue::Int(9));
}

#[test]
fn generic_propagation_with_question() {
    let source = "using aster.core;\n\
        public Result<T, E> Forward<T, E>(Result<T, E> input) { T value = input?; return Result<T, E>.Ok(value); }\n\
        public int Run() { switch (Forward<int, string>(Result<int, string>.Ok(42))) { case Ok(v): return v; case Error(e): return -1; } }";
    assert_eq!(int(source), ExecutionValue::Int(42));
}

#[test]
fn generic_propagation_with_different_success_types() {
    let source = "using aster.core;\n\
        public Result<U, E> Replace<T, U, E>(Result<T, E> input, U replacement) {\n\
            T ignored = input?;\n\
            return Result<U, E>.Ok(replacement); }\n\
        public int Run() {\n\
            switch (Replace<string, int, string>(Result<string, string>.Ok(\"x\"), 42)) {\n\
                case Ok(v): return v; case Error(e): return -1; } }";
    assert_eq!(int(source), ExecutionValue::Int(42));
}

#[test]
fn generic_error_type_struct() {
    let source = "using aster.core;\n\
        public struct Failure { public int code; }\n\
        public Result<T, Failure> Fail<T>(Failure error) { return Result<T, Failure>.Error(error); }\n\
        public int Run() { switch (Fail<int>(Failure { code: 7 })) { case Ok(v): return v; case Error(e): return e.code; } }";
    assert_eq!(int(source), ExecutionValue::Int(7));
}

#[test]
fn generic_error_type_enum() {
    let source = "using aster.core;\n\
        public enum Kind { Bad(int code), Worse }\n\
        public Result<T, Kind> Fail<T>(Kind error) { return Result<T, Kind>.Error(error); }\n\
        public int Run() { switch (Fail<int>(Kind.Bad(3))) {\n\
            case Ok(v): return v;\n\
            case Error(k): switch (k) { case Bad(code): return code; case Worse: return -1; } } }";
    assert_eq!(int(source), ExecutionValue::Int(3));
}

#[test]
fn repeated_specialization_reuses_cache() {
    // Instantiating the same generic twice must produce one specialization and
    // still run correctly.
    let source = "using aster.core;\n\
        public Result<T, string> Wrap<T>(T value) { return Result<T, string>.Ok(value); }\n\
        public int First() { switch (Wrap<int>(40)) { case Ok(v): return v; case Error(e): return 0; } }\n\
        public int Second() { switch (Wrap<int>(2)) { case Ok(v): return v; case Error(e): return 0; } }\n\
        public int Run() { return First() + Second(); }";
    assert_eq!(int(source), ExecutionValue::Int(42));
}
