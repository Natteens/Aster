use aster_compiler::{compile, hir};

fn assert_valid(source: &str) {
    if let Err(diagnostics) = compile(source) {
        panic!("expected valid source, got: {diagnostics:#?}");
    }
}

fn assert_error(source: &str, expected: &str) {
    let diagnostics = compile(source).expect_err("source should be rejected");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains(expected)),
        "expected a diagnostic containing {expected:?}, got {diagnostics:#?}"
    );
}

const FREE_ASYNC: &str = "public int Compute() { return 42; } \
     public async Task<int> Calculate() { int value = await Task.Run(Compute); return value + 1; }";

#[test]
fn accepts_free_async_task_int_with_one_await() {
    assert_valid(FREE_ASYNC);
}

#[test]
fn accepts_static_async_method_matching_the_example() {
    assert_valid(
        "public static class Jobs { \
             public static int Compute() { return 42; } \
             public static async Task<int> Calculate() { int value = await Task.Run(Compute); return value + 1; } }",
    );
}

#[test]
fn an_unrelated_wait_method_is_allowed_after_the_await() {
    assert_valid(
        "public class Probe { public int Wait() { return 2; } } \
         public int Compute() { return 40; } \
         public async Task<int> Calculate() { int value = await Task.Run(Compute); Probe probe = new Probe(); return value + probe.Wait(); }",
    );
}

#[test]
fn hir_marks_the_function_async_and_types_await_as_the_scalar_result() {
    let compilation = compile(FREE_ASYNC).expect("valid async compiles");
    let function = compilation
        .hir
        .items
        .iter()
        .find_map(|item| match item {
            hir::Item::Function(function) if function.name == "Calculate" => Some(function),
            _ => None,
        })
        .expect("Calculate is lowered to HIR");
    assert!(function.is_async, "Calculate must be marked async in HIR");

    let body = function.body.as_ref().expect("Calculate has a body");
    let hir::Statement::Variable(variable) = &body.statements[0] else {
        panic!("first statement should declare a local");
    };
    let initializer = variable
        .initializer
        .as_ref()
        .expect("the local is initialized by the await");
    let hir::ExpressionKind::Await {
        result_type,
        operand,
    } = &initializer.kind
    else {
        panic!(
            "initializer should be an Await node, got {:?}",
            initializer.kind
        );
    };
    assert_eq!(**result_type, hir::Type::Int, "await Task<int> yields int");
    assert_eq!(
        initializer.type_,
        hir::Type::Int,
        "await expression is typed int"
    );
    assert!(
        matches!(operand.type_, hir::Type::Task(ref inner) if **inner == hir::Type::Int),
        "the awaited operand is a Task<int>"
    );
}

#[test]
fn return_is_checked_against_t_not_task_of_t() {
    // A `return` producing the declared `Task<int>` instead of `int` is rejected,
    // proving return statements are checked against `T`.
    assert_error(
        "public int Compute() { return 1; } \
         public async Task<int> Calculate() { return Task.Run(Compute); }",
        "expected",
    );
}

#[test]
fn await_outside_async_is_rejected() {
    assert_error(
        "public int Compute() { return 1; } \
         public int Run() { return await Task.Run(Compute); }",
        "`await` is only valid inside an `async` function",
    );
}

#[test]
fn async_returning_a_non_task_type_is_rejected() {
    assert_error(
        "public int Compute() { return 1; } \
         public async int Calculate() { int value = await Task.Run(Compute); return value; }",
        "must return `Task<T>`",
    );
}

#[test]
fn async_task_void_is_rejected() {
    assert_error(
        "public int Compute() { return 1; } \
         public async Task<void> Calculate() { int value = await Task.Run(Compute); }",
        "`async Task<void>` is not supported",
    );
}

#[test]
fn async_without_await_is_rejected() {
    assert_error(
        "public async Task<int> Calculate() { return 1; }",
        "exactly one `await`",
    );
}

#[test]
fn two_awaits_are_rejected() {
    assert_error(
        "public int Compute() { return 1; } \
         public async Task<int> Calculate() { int a = await Task.Run(Compute); int b = await Task.Run(Compute); return a + b; }",
        "only one `await`",
    );
}

#[test]
fn await_over_a_non_task_is_rejected() {
    assert_error(
        "public async Task<int> Calculate() { int value = await 5; return value; }",
        "`await` requires a `Task<T>` operand",
    );
}

#[test]
fn await_over_a_task_variable_instead_of_direct_task_run_is_rejected() {
    assert_error(
        "public int Compute() { return 1; } \
         public async Task<int> Calculate() { Task<int> t = Task.Run(Compute); int value = await t; return value; }",
        "must directly await `Task.Run(...)`",
    );
}

#[test]
fn async_instance_method_is_rejected() {
    assert_error(
        "public class Worker { \
             public static int Compute() { return 1; } \
             public async Task<int> Run() { int value = await Task.Run(Compute); return value; } }",
        "`async` instance methods are not supported",
    );
}

#[test]
fn async_with_parameters_is_rejected() {
    assert_error(
        "public int Compute() { return 1; } \
         public async Task<int> Calculate(int extra) { int value = await Task.Run(Compute); return value; }",
        "cannot declare parameters",
    );
}

#[test]
fn async_task_of_non_transferable_type_is_rejected() {
    assert_error(
        "public int Compute() { return 1; } \
         public async Task<string> Calculate() { int value = await Task.Run(Compute); return \"x\"; }",
        "requires a scalar result",
    );
}

#[test]
fn non_linear_control_flow_in_async_is_rejected() {
    for body in [
        "if (true) { int v = await Task.Run(Compute); } return 0;",
        "while (true) { int v = await Task.Run(Compute); }",
        "for (int i = 0; i < 1; i++) { int v = await Task.Run(Compute); } return 0;",
    ] {
        let source = format!(
            "public int Compute() {{ return 1; }} \
             public async Task<int> Calculate() {{ {body} }}"
        );
        assert_error(&source, "linear control flow");
    }
}

#[test]
fn reference_local_before_await_is_rejected() {
    assert_error(
        "public int Compute() { return 1; } \
         public async Task<int> Calculate() { string label = \"n\"; int value = await Task.Run(Compute); return value; }",
        "reference-typed local cannot be declared before `await`",
    );
}

#[test]
fn existing_task_run_and_wait_still_compile() {
    assert_valid(
        "public int Compute() { return 1; } \
         public int Main() { return Task.Run(Compute).Wait(); }",
    );
}

#[test]
fn async_interface_method_without_body_is_rejected() {
    assert_error(
        "public interface IJob { async Task<int> Run(); }",
        "an `async` function must have a body",
    );
}

#[test]
fn inferred_reference_local_before_await_is_rejected_when_string() {
    assert_error(
        "public string GetString() { return \"n\"; } \
         public int Compute() { return 1; } \
         public async Task<int> Calculate() { var text = GetString(); int result = await Task.Run(Compute); return result; }",
        "reference-typed local cannot be declared before `await`",
    );
}

#[test]
fn inferred_reference_local_before_await_is_rejected_when_class() {
    assert_error(
        "public class Thing { } \
         public int Compute() { return 1; } \
         public async Task<int> Calculate() { var thing = new Thing(); int result = await Task.Run(Compute); return result; }",
        "reference-typed local cannot be declared before `await`",
    );
}

#[test]
fn inferred_reference_local_before_await_is_rejected_when_array() {
    assert_error(
        "public int Compute() { return 1; } \
         public async Task<int> Calculate() { var buffer = new int[1]; int result = await Task.Run(Compute); return result; }",
        "reference-typed local cannot be declared before `await`",
    );
}

#[test]
fn inferred_scalar_local_before_await_is_allowed() {
    assert_valid(
        "public int Compute() { return 1; } \
         public async Task<int> Calculate() { var number = 10; int result = await Task.Run(Compute); return number + result; }",
    );
}
