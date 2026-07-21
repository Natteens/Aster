//! Executing an `async` function before the async runtime exists must produce
//! a controlled Aster runtime error, never a Rust panic or a host trap. The
//! async body is not lowered to executable MIR yet; its placeholder reports the
//! error through the same controlled channel as every other runtime failure.

use aster_codegen_cranelift::{ExecutionValue, execute};
use aster_compiler::compile;

#[test]
fn executing_an_async_function_reports_a_controlled_error() {
    let source = "public int Compute() { return 1; } \
         public async Task<int> Calculate() { int value = await Task.Run(Compute); return value; } \
         public int Main() { Calculate(); return 0; }";
    let compilation = compile(source).expect("async source compiles");

    // Reaching this assertion at all proves there was no host trap or Rust
    // panic: a controlled `BackendError` was returned instead.
    let error = execute(&compilation.mir, "Main")
        .expect_err("executing an async function must fail in a controlled way");
    assert!(
        error.to_string().contains("async runtime"),
        "unexpected error: {error}"
    );
}

#[test]
fn task_run_and_wait_still_execute_without_async() {
    let source = "public int Compute() { return 41; } \
         public int Main() { return Task.Run(Compute).Wait() + 1; }";
    let compilation = compile(source).expect("task source compiles");
    let value = execute(&compilation.mir, "Main").expect("Task.Run/Wait still execute");
    assert_eq!(value, ExecutionValue::Int(42));
}
