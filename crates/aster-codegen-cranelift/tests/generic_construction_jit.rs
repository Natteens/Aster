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

#[test]
fn nested_generic_member_get_chain_executes() {
    let depth = 12;
    let mut type_name = "int".to_owned();
    let mut value = "42".to_owned();
    for _ in 0..depth {
        type_name = format!("Box<{type_name}>");
        value = format!("new {type_name}({value})");
    }
    let calls = std::iter::repeat_n("Get()", depth)
        .collect::<Vec<_>>()
        .join(".");
    let source = format!(
        "public class Box<T> {{ private T value; public Box(T value) {{ this.value = value; }} public T Get() {{ return value; }} }} public int Run() {{ {type_name} value = {value}; return value.{calls}; }}"
    );
    assert_eq!(int(&source), ExecutionValue::Int(42));
}

const SCORED: &str = "public interface IScored { int Score(); }\n\
    public class Small : IScored { private int value; public Small(int value) { this.value = value; } public int Score() { return value; } }\n\
    public class Large : IScored { private int value; public Large(int value) { this.value = value; } public int Score() { return value; } }\n";

#[test]
fn constrained_generic_function_executes() {
    let source = format!(
        "{SCORED}public T PickHigher<T>(T left, T right) where T : IScored {{ if (left.Score() >= right.Score()) {{ return left; }} return right; }}\n\
         public int Run() {{ return PickHigher(new Small(20), new Small(42)).Score(); }}"
    );
    assert_eq!(int(&source), ExecutionValue::Int(42));
}

#[test]
fn two_satisfying_types_specialize_and_execute_independently() {
    let source = format!(
        "{SCORED}public int Read<T>(T value) where T : IScored {{ return value.Score(); }}\n\
         public int Run() {{ return Read(new Small(2)) + Read(new Large(40)); }}"
    );
    assert_eq!(int(&source), ExecutionValue::Int(42));
}

#[test]
fn constrained_generic_class_member_executes() {
    let source = format!(
        "{SCORED}public class Box<T> where T : IScored {{ private T item; public Box(T item) {{ this.item = item; }} public int Read() {{ return item.Score(); }} }}\n\
         public int Run() {{ Box<Large> box = new Box<Large>(new Large(42)); return box.Read(); }}"
    );
    assert_eq!(int(&source), ExecutionValue::Int(42));
}

fn mir_of(source: &str, label: &str) -> aster_mir::Module {
    let path = std::env::temp_dir().join(format!("aster-{label}-{}.aster", std::process::id()));
    std::fs::write(&path, source).expect("write temporary project");
    let compilation = compile_project(&path).expect("program compiles");
    std::fs::remove_file(&path).ok();
    compilation.compilation.mir
}

/// A constraint is a compile-time contract only. Adding it must not change one
/// instruction of the generated program: same direct calls, no interface value
/// materialized, no dispatch mechanism introduced.
#[test]
fn a_constraint_introduces_no_boxing_or_interface_dispatch() {
    use aster_mir as mir;

    let constrained = mir_of(
        &format!(
            "{SCORED}public int Read<T>(T value) where T : IScored {{ return value.Score(); }}\n\
             public int Run() {{ return Read(new Small(42)); }}"
        ),
        "constraint-dispatch-with",
    );
    let unconstrained = mir_of(
        &format!(
            "{SCORED}public int Read<T>(T value) {{ return value.Score(); }}\n\
             public int Run() {{ return Read(new Small(42)); }}"
        ),
        "constraint-dispatch-without",
    );
    assert_eq!(
        format!("{constrained:#?}"),
        format!("{unconstrained:#?}"),
        "a constraint must not change generated MIR"
    );

    for function in &constrained.functions {
        for instruction in function.blocks.iter().flat_map(|block| &block.instructions) {
            assert!(
                !matches!(instruction, mir::Instruction::CallInterface { .. }),
                "a constraint must not introduce interface dispatch"
            );
            assert!(
                !matches!(
                    instruction,
                    mir::Instruction::Assign {
                        value: mir::Rvalue {
                            kind: mir::RvalueKind::MakeInterface { .. },
                            ..
                        },
                        ..
                    }
                ),
                "a constraint must not introduce an interface value"
            );
        }
    }
}
