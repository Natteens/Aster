//! End-to-end tests for `aster.core.Task.Run`/`Task<T>.Wait`: parser through
//! semantic analysis, HIR, MIR, codegen, and the internal task runtime.
//!
//! There is exactly one execution path: `execute` (used directly here, and
//! the same function every other public entry point and `aster run` funnel
//! through). A module that never uses tasks never creates a task runtime;
//! one is created only when needed and is gone before `execute` returns.

use aster_codegen_cranelift::{ExecutionValue, execute, execute_symbol, execute_with_stats};
use aster_compiler::compile;

fn run(source: &str, entry: &str) -> Result<ExecutionValue, String> {
    let compilation = compile(source).map_err(|diagnostics| format!("{diagnostics:#?}"))?;
    execute(&compilation.mir, entry).map_err(|error| error.to_string())
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

#[test]
fn free_function_returns_int() {
    let source = r"
        public int Compute() { return 42; }
        public int Main() {
            Task<int> task = Task.Run(Compute);
            return task.Wait();
        }
    ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn static_method_returns_int() {
    let source = r"
        public class Ops { public static int Seven() { return 7; } }
        public int Main() {
            Task<int> task = Task.Run(Ops.Seven);
            return task.Wait();
        }
    ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(7)));
}

#[test]
fn two_or_more_tasks_produce_correct_results() {
    let source = r"
        public int Compute() { return 42; }
        public int Main() {
            Task<int> first = Task.Run(Compute);
            Task<int> second = Task.Run(Compute);
            Task<int> third = Task.Run(Compute);
            return first.Wait() + second.Wait() + third.Wait();
        }
    ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(126)));
}

#[test]
fn tasks_running_different_functions_produce_independent_results() {
    // Also evidence that dispatch is by resolved `SymbolId`, not by name:
    // if the backend re-derived the target from text, nothing here would
    // distinguish `A` from `B`.
    let source = r"
        public int A() { return 1; }
        public int B() { return 2; }
        public int Main() {
            Task<int> a = Task.Run(A);
            Task<int> b = Task.Run(B);
            return a.Wait() + b.Wait();
        }
    ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(3)));
}

#[test]
fn the_pool_is_reused_across_many_sequential_task_runs() {
    let source = r"
        public int Compute() { return 1; }
        public int Main() {
            int total = 0;
            total = total + Task.Run(Compute).Wait();
            total = total + Task.Run(Compute).Wait();
            total = total + Task.Run(Compute).Wait();
            total = total + Task.Run(Compute).Wait();
            total = total + Task.Run(Compute).Wait();
            return total;
        }
    ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(5)));
}

#[test]
fn a_controlled_runtime_error_propagates_through_wait() {
    let source = r"
        public int Failing() {
            int[] values = new int[1];
            return values[5];
        }
        public int Main() {
            Task<int> task = Task.Run(Failing);
            return task.Wait();
        }
    ";
    let error = run(source, "Main").expect_err("the task's controlled error must propagate");
    assert!(error.contains("array index 5"));
}

#[test]
fn worker_allocation_budget_failure_propagates_without_host_oom() {
    let source = r"
        public int Failing() {
            int[] values = new int[2147483647];
            return values.Length;
        }
        public int Main() {
            return Task.Run(Failing).Wait();
        }
    ";
    let error = run(source, "Main").expect_err("the worker allocation must fail closed");
    assert!(error.contains("Aster runtime error:"), "{error}");
    assert!(error.contains("execution memory limit"), "{error}");
    assert!(!error.contains("memory allocation of"), "{error}");
}

#[test]
fn a_function_with_parameters_is_rejected() {
    let errors = compile_errors(
        r"
        public int WithArgument(int value) { return value; }
        public int Main() {
            Task<int> task = Task.Run(WithArgument);
            return task.Wait();
        }
        ",
    );
    assert!(
        errors
            .iter()
            .any(|message| message.contains("Task.Run") || message.contains("WithArgument")),
        "expected a Task.Run diagnostic, got {errors:?}"
    );
}

#[test]
fn an_instance_method_is_rejected() {
    let errors = compile_errors(
        r"
        public class Counter { public int Value() { return 1; } }
        public int Main() {
            Counter counter = new Counter();
            Task<int> task = Task.Run(counter.Value);
            return task.Wait();
        }
        ",
    );
    assert!(
        !errors.is_empty(),
        "an instance method must not be accepted by Task.Run"
    );
}

#[test]
fn a_non_transferable_return_type_is_rejected() {
    let errors = compile_errors(
        r#"
        public string Text() { return "hi"; }
        public int Main() {
            Task<string> task = Task.Run(Text);
            return 0;
        }
        "#,
    );
    assert!(
        errors
            .iter()
            .any(|message| message.contains("cross a worker boundary")),
        "expected the non-transferable-result diagnostic, got {errors:?}"
    );
}

#[test]
fn a_decimal_return_type_is_rejected() {
    let errors = compile_errors(
        r"
        public decimal Compute() { return 1.5m; }
        public int Main() {
            Task<decimal> task = Task.Run(Compute);
            return 0;
        }
        ",
    );
    assert!(
        errors
            .iter()
            .any(|message| message.contains("`decimal` is reserved but not supported")),
        "expected the deferred-decimal diagnostic, got {errors:?}"
    );
}

#[test]
fn a_class_return_type_is_rejected() {
    let errors = compile_errors(
        r"
        public class Box { public Box() {} }
        public Box Make() { return new Box(); }
        public int Main() {
            Task<Box> task = Task.Run(Make);
            return 0;
        }
        ",
    );
    assert!(
        errors
            .iter()
            .any(|message| message.contains("cross a worker boundary")),
        "expected the non-transferable-result diagnostic for a class, got {errors:?}"
    );
}

#[test]
fn an_interface_return_type_is_rejected() {
    let errors = compile_errors(
        r"
        public interface IBox { int Get(); }
        public class Box : IBox { public Box() {} public int Get() { return 1; } }
        public IBox Make() { return new Box(); }
        public int Main() {
            Task<IBox> task = Task.Run(Make);
            return 0;
        }
        ",
    );
    assert!(
        errors
            .iter()
            .any(|message| message.contains("cross a worker boundary")),
        "expected the non-transferable-result diagnostic for an interface, got {errors:?}"
    );
}

#[test]
fn an_enum_return_type_is_rejected() {
    let errors = compile_errors(
        r"
        public enum Color { Red, Green, Blue }
        public Color Make() { return Color.Red; }
        public int Main() {
            Task<Color> task = Task.Run(Make);
            return 0;
        }
        ",
    );
    assert!(
        errors
            .iter()
            .any(|message| message.contains("cross a worker boundary")),
        "expected the non-transferable-result diagnostic for an enum, got {errors:?}"
    );
}

#[test]
fn a_struct_return_type_is_rejected() {
    let errors = compile_errors(
        r"
        public struct Point { public int x; public int y; }
        public Point Make() { return Point { x: 1, y: 2 }; }
        public int Main() {
            Task<Point> task = Task.Run(Make);
            return 0;
        }
        ",
    );
    assert!(
        errors
            .iter()
            .any(|message| message.contains("cross a worker boundary")),
        "expected the non-transferable-result diagnostic for a struct, got {errors:?}"
    );
}

#[test]
fn a_struct_method_can_produce_an_already_transferable_scalar_result() {
    let source = r"
        public struct Point {
            public int x;
            public int y;
            public int Sum() { return x + y; }
        }
        public int Compute() {
            Point point = Point { x: 20, y: 22 };
            return point.Sum();
        }
        public int Main() { return Task.Run(Compute).Wait(); }
    ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn generic_methods_do_not_disguise_non_transferable_worker_results() {
    let errors = compile_errors(
        r#"
        public class Tools {
            public Tools() {}
            public T Identity<T>(T value) { return value; }
        }
        public string Compute() { return new Tools().Identity<string>("not transferable"); }
        public int Main() { Task<string> task = Task.Run(Compute); return 0; }
        "#,
    );
    assert!(
        errors
            .iter()
            .any(|message| message.contains("cross a worker boundary")),
        "expected the non-transferable-result diagnostic, got {errors:?}"
    );

    let reference_struct = compile_errors(
        r#"
        public struct Holder { public string text; public int Length() { return text.Length; } }
        public Holder Compute() { return Holder { text: "not transferable" }; }
        public int Main() { Task<Holder> task = Task.Run(Compute); return 0; }
        "#,
    );
    assert!(
        reference_struct
            .iter()
            .any(|message| message.contains("cross a worker boundary")),
        "expected reference-bearing struct rejection, got {reference_struct:?}"
    );
}

#[test]
fn an_array_return_type_is_rejected() {
    let errors = compile_errors(
        r"
        public int[] Make() { return [1, 2, 3]; }
        public int Main() {
            Task<int[]> task = Task.Run(Make);
            return 0;
        }
        ",
    );
    assert!(
        errors
            .iter()
            .any(|message| message.contains("cross a worker boundary")),
        "expected the non-transferable-result diagnostic for an array, got {errors:?}"
    );
}

#[test]
fn a_nested_task_return_type_is_rejected() {
    // A zero-parameter static function whose declared return type is itself
    // `Task<T>` must be rejected by the transferability gate specifically
    // (`Task<Task<int>>` is not worker-transferable), independent of the
    // separate nested-concurrency diagnostic this shape also triggers
    // because `Wrapper`'s own body uses `Task.Run`.
    let errors = compile_errors(
        r"
        public int Compute() { return 1; }
        public Task<int> Wrapper() { return Task.Run(Compute); }
        public int Main() {
            Task<int> task = Task.Run(Wrapper);
            return 0;
        }
        ",
    );
    assert!(
        errors
            .iter()
            .any(|message| message.contains("cross a worker boundary")),
        "expected the non-transferable-result diagnostic for Task<Task<int>>, got {errors:?}"
    );
}

#[test]
fn task_int_is_incompatible_with_task_string() {
    let errors = compile_errors(
        r"
        public int Compute() { return 1; }
        public int Main() {
            Task<string> task = Task.Run(Compute);
            return 0;
        }
        ",
    );
    assert!(
        !errors.is_empty(),
        "`Task<int>` must not be assignable to a `Task<string>` variable"
    );
}

#[test]
fn sequential_programs_are_unaffected_by_task_support() {
    // A program that never touches Task.Run/Wait behaves identically to
    // before tasks existed: same result, and `module_uses_tasks` (see
    // `task_runtime.rs`) keeps this path from ever creating a task runtime.
    let source = "public int Run() { return 40 + 2; }";
    let compilation = compile(source).expect("valid program");
    assert_eq!(
        execute(&compilation.mir, "Run"),
        Ok(ExecutionValue::Int(42))
    );
}

#[test]
fn task_run_works_through_every_normal_entry_point() {
    // `aster run` (via aster-cli) calls exactly these functions. None of
    // them is a special "task-aware" API: Task.Run works through all of
    // them because `execute_resolved` detects task usage internally.
    let source = r"
        public int Compute() { return 42; }
        public int Main() {
            Task<int> task = Task.Run(Compute);
            return task.Wait();
        }
    ";
    let compilation = compile(source).expect("valid program");

    assert_eq!(
        execute(&compilation.mir, "Main"),
        Ok(ExecutionValue::Int(42))
    );
    let (value, stats) =
        execute_with_stats(&compilation.mir, "Main").expect("execute_with_stats succeeds");
    assert_eq!(value, ExecutionValue::Int(42));
    // The top-level entry's own context never allocates anything here;
    // `Task<T>` is a plain integer, not an arena allocation.
    assert_eq!(stats.total_allocations, 0);

    let entry_symbol = compilation
        .mir
        .functions
        .iter()
        .find(|function| function.name == "Main")
        .expect("Main is declared")
        .symbol;
    assert_eq!(
        execute_symbol(&compilation.mir, entry_symbol),
        Ok(ExecutionValue::Int(42))
    );
}

// --- Semantic identity: `Task` is a reserved intrinsic name (see
// `semantic::validate_no_reserved_type_names`). No class, struct, interface,
// or enum may be declared `Task`, so `Task.Run`/`Task<T>`/`.Wait()` are
// recognized structurally without ever consulting a user type table, and a
// user's own `Run`/`Wait` members on any other type keep working normally. ---

#[test]
fn a_class_named_task_is_rejected() {
    let errors = compile_errors("public class Task { public static int Run() { return 1; } }");
    assert!(
        errors.iter().any(|message| message.contains("reserved")),
        "expected a reserved-name diagnostic, got {errors:?}"
    );
}

#[test]
fn a_struct_named_task_is_rejected() {
    let errors = compile_errors("public struct Task { public int Value; }");
    assert!(
        errors.iter().any(|message| message.contains("reserved")),
        "expected a reserved-name diagnostic, got {errors:?}"
    );
}

#[test]
fn an_interface_named_task_is_rejected() {
    let errors = compile_errors("public interface Task { int Run(); }");
    assert!(
        errors.iter().any(|message| message.contains("reserved")),
        "expected a reserved-name diagnostic, got {errors:?}"
    );
}

#[test]
fn an_enum_named_task_is_rejected() {
    let errors = compile_errors("public enum Task { Idle, Running, }");
    assert!(
        errors.iter().any(|message| message.contains("reserved")),
        "expected a reserved-name diagnostic, got {errors:?}"
    );
}

#[test]
fn a_generic_class_template_named_task_is_rejected() {
    let errors =
        compile_errors("public class Task<T> { public T Value; public T Get() { return Value; } }");
    assert!(
        errors.iter().any(|message| message.contains("reserved")),
        "expected a reserved-name diagnostic, got {errors:?}"
    );
}

#[test]
fn a_namespace_qualified_declaration_cannot_fake_the_task_type() {
    let errors = compile_errors(
        r"
        namespace app;
        public class Task { public static int Run() { return 1; } }
        ",
    );
    assert!(
        errors.iter().any(|message| message.contains("reserved")),
        "a namespace header must not let a program redeclare `Task`, got {errors:?}"
    );
}

#[test]
fn a_user_static_method_named_run_on_another_type_does_not_activate_the_intrinsic() {
    let source = r"
        public class Worker { public static int Run() { return 5; } }
        public int Main() { return Worker.Run(); }
    ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(5)));
}

#[test]
fn a_wait_method_on_another_type_does_not_activate_the_intrinsic() {
    let source = r"
        public class Latch { public int Wait() { return 3; } }
        public int Main() { Latch latch = new Latch(); return latch.Wait(); }
    ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(3)));
}

#[test]
fn invalid_official_task_run_usage_is_a_controlled_diagnostic() {
    // No user `Task` type exists here, so this must resolve against the
    // official intrinsic and be rejected for its real shape violation
    // (two arguments) rather than silently doing something else.
    let errors = compile_errors(
        r"
        public int Compute() { return 1; }
        public int Main() {
            Task<int> task = Task.Run(Compute, Compute);
            return task.Wait();
        }
        ",
    );
    assert!(
        !errors.is_empty(),
        "Task.Run with the wrong argument count must be a controlled diagnostic"
    );
}

// --- Handle safety: no UB, no leak, no double interpretation. ---

#[test]
fn waiting_twice_on_the_same_variable_returns_the_same_result() {
    let source = r"
        public int Compute() { return 42; }
        public int Main() {
            Task<int> task = Task.Run(Compute);
            int a = task.Wait();
            int b = task.Wait();
            return a + b;
        }
    ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(84)));
}

#[test]
fn an_alias_of_the_same_task_can_be_waited_from_either_variable() {
    let source = r"
        public int Compute() { return 42; }
        public int Main() {
            Task<int> task = Task.Run(Compute);
            Task<int> alias = task;
            int a = task.Wait();
            int b = alias.Wait();
            return a + b;
        }
    ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(84)));
}

#[test]
fn a_task_created_and_never_awaited_does_not_hang_or_crash() {
    let source = r"
        public int Compute() { return 42; }
        public int Main() {
            Task<int> task = Task.Run(Compute);
            return 1;
        }
    ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(1)));
}

#[test]
fn many_tasks_abandoned_before_the_end_of_execution_do_not_hang_or_crash() {
    let source = r"
        public int Compute() { return 42; }
        public int Main() {
            Task<int> a = Task.Run(Compute);
            Task<int> b = Task.Run(Compute);
            Task<int> c = Task.Run(Compute);
            Task<int> d = Task.Run(Compute);
            return d.Wait();
        }
    ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn the_controlled_error_prefix_is_not_duplicated() {
    let source = r"
        public int Failing() {
            int[] values = new int[1];
            return values[5];
        }
        public int Main() {
            Task<int> task = Task.Run(Failing);
            return task.Wait();
        }
    ";
    let error = run(source, "Main").expect_err("the task's controlled error must propagate");
    let occurrences = error.matches("Aster runtime error").count();
    assert_eq!(
        occurrences, 1,
        "prefix must appear exactly once, got: {error}"
    );
}

#[test]
fn a_list_return_type_is_rejected() {
    // `List<T>` (Lote List A) is a native reference type like `Class`/`Array`,
    // never mapped by `values::primitive`, so it fails the same
    // `is_worker_transferable` gate with no List-specific exception needed.
    // `Make` recurses into itself instead of constructing a `List<int>`,
    // since List A exposes no constructor yet; this only needs to compile,
    // never to run.
    let errors = compile_errors(
        r"
        public List<int> Make() { return Make(); }
        public int Main() {
            Task<List<int>> task = Task.Run(Make);
            return 0;
        }
        ",
    );
    assert!(
        errors
            .iter()
            .any(|message| message.contains("cross a worker boundary")),
        "expected the non-transferable-result diagnostic for List<int>, got {errors:?}"
    );
}
