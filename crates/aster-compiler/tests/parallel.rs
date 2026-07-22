use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use aster_compiler::compile;

static NEXT_PROJECT_ID: AtomicU64 = AtomicU64::new(0);

struct TestProject {
    root: PathBuf,
}

impl TestProject {
    fn new() -> Self {
        let id = NEXT_PROJECT_ID.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "aster-nested-interface-{}-{id}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create test project");
        Self { root }
    }

    fn write(&self, relative: &str, source: &str) -> PathBuf {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create source directory");
        }
        fs::write(&path, source).expect("write source file");
        path
    }
}

impl Drop for TestProject {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).expect("remove test project");
    }
}

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

const BODY_INT: &str = "public void Body(int index) { }";
const BODY_LONG: &str = "public void BodyLong(long value) { }";
const BODY_BOOL: &str = "public void BodyBool(bool value) { }";
const BODY_DOUBLE: &str = "public void BodyDouble(double value) { }";

#[test]
fn parallel_for_with_empty_range_compiles() {
    assert_valid(&format!(
        "{BODY_INT} public int Main() {{ Parallel.For(5, 5, Body); return 0; }}"
    ));
}

#[test]
fn parallel_for_one_iteration_compiles() {
    assert_valid(&format!(
        "{BODY_INT} public int Main() {{ Parallel.For(0, 1, Body); return 0; }}"
    ));
}

#[test]
fn parallel_for_many_iterations_compiles() {
    assert_valid(&format!(
        "{BODY_INT} public int Main() {{ Parallel.For(0, 1000, Body); return 0; }}"
    ));
}

#[test]
fn parallel_for_negative_start_compiles() {
    assert_valid(&format!(
        "{BODY_INT} public int Main() {{ Parallel.For(-10, 10, Body); return 0; }}"
    ));
}

#[test]
fn parallel_for_inverted_range_compiles_and_fails_only_at_runtime() {
    // `end < start` is a controlled runtime error (section 16), not a compile diagnostic.
    assert_valid(&format!(
        "{BODY_INT} public int Main() {{ Parallel.For(10, 0, Body); return 0; }}"
    ));
}

#[test]
fn parallel_for_free_body_compiles() {
    assert_valid(&format!(
        "{BODY_INT} public int Main() {{ Parallel.For(0, 1, Body); return 0; }}"
    ));
}

#[test]
fn parallel_for_static_method_body_compiles() {
    assert_valid(
        "public static class Jobs { public static void Body(int index) { } } \
         public int Main() { Parallel.For(0, 1, Jobs.Body); return 0; }",
    );
}

#[test]
fn parallel_for_body_with_wrong_signature_is_rejected() {
    assert_error(
        "public void Body(int index, int extra) { } \
         public int Main() { Parallel.For(0, 1, Body); return 0; }",
        "no static method or free function with signature",
    );
}

#[test]
fn parallel_for_body_with_non_void_return_is_rejected() {
    assert_error(
        "public int Body(int index) { return index; } \
         public int Main() { Parallel.For(0, 1, Body); return 0; }",
        "no static method or free function with signature",
    );
}

#[test]
fn parallel_for_instance_method_body_is_rejected() {
    assert_error(
        "public class Jobs { public void Body(int index) { } } \
         public int Main() { Parallel.For(0, 1, Jobs.Body); return 0; }",
        "no static method or free function with signature",
    );
}

#[test]
fn parallel_for_non_function_body_is_rejected() {
    assert_error(
        "public int Main() { Parallel.For(0, 1, 5); return 0; }",
        "must directly name a static method or free function",
    );
}

#[test]
fn parallel_for_async_body_is_rejected() {
    assert_error(
        "public int Compute() { return 1; } \
         public async Task<int> Body(int index) { int v = await Task.Run(Compute); return v; } \
         public int Main() { Parallel.For(0, 1, Body); return 0; }",
        "no static method or free function with signature",
    );
}

#[test]
fn parallel_for_direct_nested_task_run_is_rejected() {
    assert_error(
        "public int Inner() { return 1; } \
         public void Body(int index) { Task.Run(Inner).Wait(); } \
         public int Main() { Parallel.For(0, 1, Body); return 0; }",
        "itself uses",
    );
}

#[test]
fn parallel_for_transitive_nested_concurrency_is_rejected() {
    assert_error(
        "public int Inner() { return 1; } \
         public void Helper() { Task.Run(Inner).Wait(); } \
         public void Body(int index) { Helper(); } \
         public int Main() { Parallel.For(0, 1, Body); return 0; }",
        "transitively calls",
    );
}

#[test]
fn interface_dispatch_reaches_task_run() {
    let source = "public interface IWorker { void Execute(int value); } \
         public int Compute() { return 1; } \
         public class Worker : IWorker { public Worker() {} public void Execute(int value) { Task.Run(Compute); } } \
         public void Body(int index) { IWorker worker = new Worker(); worker.Execute(index); } \
         public int Main() { Parallel.For(0, 1, Body); return 0; }";
    let diagnostics = compile(source).expect_err("interface dispatch must expose Task.Run");
    let diagnostic = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.message.contains("through interface call"))
        .unwrap_or_else(|| panic!("missing interface path diagnostic: {diagnostics:#?}"));
    assert!(diagnostic.message.contains("Body"));
    assert!(diagnostic.message.contains("IWorker.Execute"));
    assert!(diagnostic.message.contains("Worker.Execute"));
    assert!(diagnostic.message.contains("Task.Run"));
}

#[test]
fn interface_dispatch_reaches_parallel_for() {
    assert_error(
        "public interface IWorker { void Execute(int value); } \
         public void Nested(int index) {} \
         public class Worker : IWorker { public Worker() {} public void Execute(int value) { Parallel.For(0, 1, Nested); } } \
         public void Body(int index) { IWorker worker = new Worker(); worker.Execute(index); } \
         public int Main() { Parallel.For(0, 1, Body); return 0; }",
        "concrete implementation `Worker.Execute`",
    );
}

#[test]
fn interface_dispatch_reaches_async_implementation() {
    assert_error(
        "public interface IWorker { Task<int> Execute(int value); } \
         public int Compute() { return 1; } \
         public class Worker : IWorker { public Worker() {} public async Task<int> Execute(int value) { int result = await Task.Run(Compute); return result; } } \
         public void Body(int index) { IWorker worker = new Worker(); worker.Execute(index); } \
         public int Main() { Parallel.For(0, 1, Body); return 0; }",
        "being an `async` function",
    );
}

#[test]
fn interface_implementation_helper_reaches_nested_concurrency() {
    assert_error(
        "public interface IWorker { void Execute(int value); } \
         public int Compute() { return 1; } \
         public void StartWork() { Task.Run(Compute); } \
         public class Worker : IWorker { public Worker() {} public void Execute(int value) { StartWork(); } } \
         public void Body(int index) { IWorker worker = new Worker(); worker.Execute(index); } \
         public int Main() { Parallel.For(0, 1, Body); return 0; }",
        "reaches `StartWork`, which uses `Task.Run`",
    );
}

#[test]
fn any_unsafe_interface_implementation_rejects_the_target() {
    assert_error(
        "public interface IWorker { void Execute(int value); } \
         public int Compute() { return 1; } \
         public class SafeWorker : IWorker { public SafeWorker() {} public void Execute(int value) {} } \
         public class UnsafeWorker : IWorker { public UnsafeWorker() {} public void Execute(int value) { Task.Run(Compute).Wait(); } } \
         public void Body(int index) { IWorker worker = new SafeWorker(); worker.Execute(index); } \
         public int Main() { Parallel.For(0, 1, Body); return 0; }",
        "UnsafeWorker.Execute",
    );
}

#[test]
fn all_sequential_interface_implementations_remain_valid() {
    assert_valid(
        "public interface IWorker { void Execute(int value); } \
         public class FirstWorker : IWorker { public FirstWorker() {} public void Execute(int value) {} } \
         public class SecondWorker : IWorker { public SecondWorker() {} public void Execute(int value) {} } \
         public void Body(int index) { IWorker worker = new FirstWorker(); worker.Execute(index); } \
         public int Main() { Parallel.For(0, 1, Body); return 0; }",
    );
}

#[test]
fn interface_dispatch_matches_the_complete_overload_signature() {
    assert_valid(
        "public interface IWorker { void Execute(int value); } \
         public int Compute() { return 1; } \
         public class Worker : IWorker { \
             public Worker() {} \
             public void Execute(int value) {} \
             public void Execute(string value) { Task.Run(Compute).Wait(); } \
         } \
         public void Body(int index) { IWorker worker = new Worker(); worker.Execute(index); } \
         public int Main() { Parallel.For(0, 1, Body); return 0; }",
    );
}

#[test]
fn recursive_interface_dispatch_terminates_when_sequential() {
    assert_valid(
        "public interface IWorker { void Execute(int value); } \
         public class Worker : IWorker { \
             public Worker() {} \
             public void Execute(int value) { if (value > 0) { IWorker next = new Worker(); next.Execute(value - 1); } } \
         } \
         public void Body(int index) { IWorker worker = new Worker(); worker.Execute(index); } \
         public int Main() { Parallel.For(0, 1, Body); return 0; }",
    );
}

#[test]
fn interface_nested_concurrency_diagnostic_is_deterministic_and_unique() {
    let source = "public interface IWorker { void Execute(int value); } \
         public int Compute() { return 1; } \
         public class First : IWorker { public First() {} public void Execute(int value) { Task.Run(Compute).Wait(); } } \
         public class Second : IWorker { public Second() {} public void Execute(int value) { Task.Run(Compute).Wait(); } } \
         public void Body(int index) { IWorker worker = new First(); worker.Execute(index); } \
         public int Main() { Parallel.For(0, 1, Body); return 0; }";
    let messages = || {
        compile(source)
            .expect_err("nested concurrency must be rejected")
            .into_iter()
            .filter(|diagnostic| diagnostic.message.contains("through interface call"))
            .map(|diagnostic| diagnostic.message)
            .collect::<Vec<_>>()
    };
    let first = messages();
    let second = messages();
    assert_eq!(first, second);
    assert_eq!(first.len(), 1, "one logical path should report once");
}

#[test]
fn task_run_target_is_protected_through_interface_dispatch() {
    assert_error(
        "public interface IWorker { void Execute(int value); } \
         public void Nested(int index) {} \
         public class Worker : IWorker { public Worker() {} public void Execute(int value) { Parallel.For(0, 1, Nested); } } \
         public int Body() { IWorker worker = new Worker(); worker.Execute(0); return 1; } \
         public int Main() { return Task.Run(Body).Wait(); }",
        "`Task.Run` target `Body`",
    );
}

#[test]
fn monomorphized_interface_dispatch_uses_concrete_symbols() {
    assert_error(
        "public interface IWorker<T> { void Execute(T value); } \
         public int Compute() { return 1; } \
         public class Worker<T> : IWorker<T> { public Worker() {} public void Execute(T value) { Task.Run(Compute).Wait(); } } \
         public void Body(int index) { IWorker<int> worker = new Worker<int>(); worker.Execute(index); } \
         public int Main() { Parallel.For(0, 1, Body); return 0; }",
        "concrete implementation",
    );
}

#[test]
fn interface_implementation_in_another_namespace_is_reached() {
    let project = TestProject::new();
    let root = project.write(
        "main.aster",
        "using aster.core; using workers; \
         public void Body(int index) { IWorker worker = new Worker(); worker.Execute(index); } \
         public int Main() { Parallel.For(0, 1, Body); return 0; }",
    );
    project.write(
        "workers/worker.aster",
        "namespace workers; using aster.core; \
         public interface IWorker { void Execute(int value); } \
         public int Compute() { return 1; } \
         public class Worker : IWorker { public Worker() {} public void Execute(int value) { Task.Run(Compute); } }",
    );
    let diagnostics = aster_compiler::compile_project(&root)
        .expect_err("cross-namespace implementation must be inspected");
    assert!(
        diagnostics.iter().any(|diagnostic| {
            diagnostic
                .diagnostic
                .message
                .contains("workers::Worker.Execute")
        }),
        "missing concrete cross-namespace implementation: {diagnostics:#?}"
    );
}

#[test]
fn an_unrelated_wait_method_does_not_trigger_nested_concurrency() {
    assert_valid(
        "public class Probe { public void Wait() { } } \
         public void Body(int index) { Probe probe = new Probe(); probe.Wait(); } \
         public int Main() { Parallel.For(0, 1, Body); return 0; }",
    );
}

#[test]
fn sequential_recursion_in_a_parallel_body_remains_valid() {
    assert_valid(
        "public void Body(int index) { if (index > 0) { Body(index - 1); } } \
         public int Main() { Parallel.For(0, 1, Body); return 0; }",
    );
}

#[test]
fn mutual_recursion_reaching_concurrency_is_rejected_without_looping() {
    assert_error(
        "public int Inner() { return 1; } \
         public void First() { Second(); } \
         public void Second() { First(); Task.Run(Inner).Wait(); } \
         public void Body(int index) { First(); } \
         public int Main() { Parallel.For(0, 1, Body); return 0; }",
        "transitively calls",
    );
}

#[test]
fn overload_identity_does_not_taint_a_sequential_overload() {
    assert_valid(
        "public int Inner() { return 1; } \
         public void Helper() { Task.Run(Inner).Wait(); } \
         public void Helper(int value) { } \
         public void Body(int index) { Helper(index); } \
         public int Main() { Parallel.For(0, 1, Body); return 0; }",
    );
}

#[test]
fn parallel_for_error_points_at_the_target_and_the_reason() {
    let source = "public int Inner() { return 1; } \
         public void Body(int index) { Task.Run(Inner).Wait(); } \
         public int Main() { Parallel.For(0, 1, Body); return 0; }";
    let diagnostics = compile(source).expect_err("nested concurrency is rejected");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("Body")
                && diagnostic.message.contains("Task<T>.Wait")),
        "diagnostic should name the target and the reason, got {diagnostics:#?}"
    );
}

#[test]
fn parallel_for_each_with_empty_array_compiles() {
    assert_valid(&format!(
        "{BODY_INT} public int Main() {{ int[] values = new int[0]; Parallel.ForEach(values, Body); return 0; }}"
    ));
}

#[test]
fn parallel_for_each_int_array_compiles() {
    assert_valid(&format!(
        "{BODY_INT} public int Main() {{ int[] values = [1, 2, 3]; Parallel.ForEach(values, Body); return 0; }}"
    ));
}

#[test]
fn parallel_for_each_long_array_compiles() {
    assert_valid(&format!(
        "{BODY_LONG} public int Main() {{ long[] values = [1L, 2L]; Parallel.ForEach(values, BodyLong); return 0; }}"
    ));
}

#[test]
fn parallel_for_each_bool_array_compiles() {
    assert_valid(&format!(
        "{BODY_BOOL} public int Main() {{ bool[] values = [true, false]; Parallel.ForEach(values, BodyBool); return 0; }}"
    ));
}

#[test]
fn parallel_for_each_double_array_compiles() {
    assert_valid(&format!(
        "{BODY_DOUBLE} public int Main() {{ double[] values = [1.0d, 2.0d]; Parallel.ForEach(values, BodyDouble); return 0; }}"
    ));
}

#[test]
fn parallel_for_each_string_array_is_rejected() {
    assert_error(
        "public void Body(string value) { } \
         public int Main() { string[] values = [\"a\"]; Parallel.ForEach(values, Body); return 0; }",
        "requires a scalar element type",
    );
}

#[test]
fn parallel_for_each_class_array_is_rejected() {
    assert_error(
        "public class Thing { } \
         public void Body(Thing value) { } \
         public int Main() { Thing[] values = new Thing[1]; Parallel.ForEach(values, Body); return 0; }",
        "requires a scalar element type",
    );
}

#[test]
fn parallel_for_each_decimal_array_is_rejected() {
    // `decimal` is numeric but has no backend ABI yet, so `decimal[]` must be
    // rejected the same way a reference-typed array is, not silently accepted
    // because it is a recognized numeric type.
    assert_error(
        "public void Body(decimal value) { } \
         public int Main() { decimal[] values = [1.5m]; Parallel.ForEach(values, Body); return 0; }",
        "requires a scalar element type",
    );
}

#[test]
fn parallel_for_each_interface_array_is_rejected() {
    assert_error(
        "public interface IThing { int Get(); } \
         public class Thing : IThing { public Thing() {} public int Get() { return 1; } } \
         public void Body(IThing value) { } \
         public int Main() { IThing[] values = new IThing[1]; Parallel.ForEach(values, Body); return 0; }",
        "requires a scalar element type",
    );
}

#[test]
fn parallel_for_each_enum_array_is_rejected() {
    assert_error(
        "public enum Color { Red, Green, Blue } \
         public void Body(Color value) { } \
         public int Main() { Color[] values = new Color[1]; Parallel.ForEach(values, Body); return 0; }",
        "requires a scalar element type",
    );
}

#[test]
fn parallel_for_each_rejects_a_body_whose_parameter_type_does_not_match_the_element_type() {
    // Both `int` and `long` are worker-transferable on their own; the body's
    // parameter must still match the array's concrete element type exactly.
    assert_error(
        &format!(
            "{BODY_LONG} public int Main() {{ int[] values = [1, 2, 3]; Parallel.ForEach(values, BodyLong); return 0; }}"
        ),
        "no static method or free function with signature",
    );
}

#[test]
fn parallel_for_each_struct_array_is_rejected() {
    assert_error(
        "public struct Point { public int x; public int y; } \
         public void Body(Point value) { } \
         public int Main() { Point[] values = new Point[1]; Parallel.ForEach(values, Body); return 0; }",
        "requires a scalar element type",
    );
}

#[test]
fn sequential_module_does_not_reference_parallel() {
    assert_valid("public int Main() { return 1 + 1; }");
}

#[test]
fn a_normal_function_named_for_or_foreach_does_not_activate_the_intrinsic() {
    assert_valid(
        "public static class Utils { public static int For(int a, int b) { return a + b; } } \
         public int Main() { return Utils.For(1, 2); }",
    );
}

#[test]
fn class_named_parallel_is_rejected() {
    assert_error(
        "public class Parallel { }",
        "reserved for the intrinsic concurrency system",
    );
}

#[test]
fn struct_named_parallel_is_rejected() {
    assert_error(
        "public struct Parallel { public int x; }",
        "reserved for the intrinsic concurrency system",
    );
}

#[test]
fn interface_named_parallel_is_rejected() {
    assert_error(
        "public interface Parallel { int Run(); }",
        "reserved for the intrinsic concurrency system",
    );
}

#[test]
fn enum_named_parallel_is_rejected() {
    assert_error(
        "public enum Parallel { A }",
        "reserved for the intrinsic concurrency system",
    );
}

// --- Parallel.Reduce ---------------------------------------------------

const ADD_VALUE: &str =
    "public int AddValue(int accumulator, int value) { return accumulator + value; }";
const ADD_PARTIAL: &str = "public int AddPartial(int left, int right) { return left + right; }";

#[test]
fn parallel_reduce_valid_signature_compiles() {
    assert_valid(&format!(
        "{ADD_VALUE} {ADD_PARTIAL} \
         public int Main() {{ int[] values = [1, 2, 3, 4, 5]; return Parallel.Reduce(values, 0, AddValue, AddPartial); }}"
    ));
}

#[test]
fn parallel_reduce_free_functions_compile() {
    assert_valid(
        "public int AddValue(int accumulator, int value) { return accumulator + value; } \
         public int AddPartial(int left, int right) { return left + right; } \
         public int Main() { int[] values = [1, 2, 3]; return Parallel.Reduce(values, 0, AddValue, AddPartial); }",
    );
}

#[test]
fn parallel_reduce_result_can_be_used_in_a_further_expression() {
    assert_valid(&format!(
        "{ADD_VALUE} {ADD_PARTIAL} \
         public int Main() {{ int[] values = [1, 2, 3]; return Parallel.Reduce(values, 0, AddValue, AddPartial) + 1; }}"
    ));
}

#[test]
fn parallel_reduce_wrong_argument_count_is_rejected() {
    assert_error(
        &format!(
            "{ADD_VALUE} {ADD_PARTIAL} \
             public int Main() {{ int[] values = [1]; return Parallel.Reduce(values, 0, AddValue); }}"
        ),
        "expects exactly 4 arguments",
    );
}

#[test]
fn parallel_reduce_non_array_first_argument_is_rejected() {
    assert_error(
        &format!(
            "{ADD_VALUE} {ADD_PARTIAL} public int Main() {{ return Parallel.Reduce(5, 0, AddValue, AddPartial); }}"
        ),
        "requires an array argument",
    );
}

#[test]
fn parallel_reduce_non_transferable_element_is_rejected_string() {
    assert_error(
        "public int AddValue(int accumulator, string value) { return accumulator + value.Length; } \
         public int AddPartial(int left, int right) { return left + right; } \
         public int Main() { string[] values = [\"a\"]; return Parallel.Reduce(values, 0, AddValue, AddPartial); }",
        "requires a scalar element type",
    );
}

#[test]
fn parallel_reduce_non_transferable_element_is_rejected_class() {
    assert_error(
        "public class Thing { } \
         public int AddValue(int accumulator, Thing value) { return accumulator; } \
         public int AddPartial(int left, int right) { return left + right; } \
         public int Main() { Thing[] values = new Thing[1]; return Parallel.Reduce(values, 0, AddValue, AddPartial); }",
        "requires a scalar element type",
    );
}

#[test]
fn parallel_reduce_non_transferable_element_is_rejected_interface() {
    assert_error(
        "public interface IThing { int Get(); } \
         public class Thing : IThing { public Thing() {} public int Get() { return 1; } } \
         public int AddValue(int accumulator, IThing value) { return accumulator; } \
         public int AddPartial(int left, int right) { return left + right; } \
         public int Main() { IThing[] values = new IThing[1]; return Parallel.Reduce(values, 0, AddValue, AddPartial); }",
        "requires a scalar element type",
    );
}

#[test]
fn parallel_reduce_non_transferable_element_is_rejected_struct() {
    assert_error(
        "public struct Point { public int x; public int y; } \
         public int AddValue(int accumulator, Point value) { return accumulator; } \
         public int AddPartial(int left, int right) { return left + right; } \
         public int Main() { Point[] values = new Point[1]; return Parallel.Reduce(values, 0, AddValue, AddPartial); }",
        "requires a scalar element type",
    );
}

#[test]
fn parallel_reduce_non_transferable_element_is_rejected_enum() {
    assert_error(
        "public enum Color { Red, Green, Blue } \
         public int AddValue(int accumulator, Color value) { return accumulator; } \
         public int AddPartial(int left, int right) { return left + right; } \
         public int Main() { Color[] values = new Color[1]; return Parallel.Reduce(values, 0, AddValue, AddPartial); }",
        "requires a scalar element type",
    );
}

#[test]
fn parallel_reduce_decimal_element_is_rejected() {
    // `decimal` is numeric but not worker-transferable (Lote 6B): it must not
    // be silently accepted just because it "looks scalar."
    assert_error(
        "public decimal AddValue(decimal accumulator, decimal value) { return accumulator + value; } \
         public decimal AddPartial(decimal left, decimal right) { return left + right; } \
         public decimal Main() { decimal[] values = [1.0m]; return Parallel.Reduce(values, 0.0m, AddValue, AddPartial); }",
        "requires a scalar element type",
    );
}

#[test]
fn parallel_reduce_task_element_is_rejected() {
    assert_error(
        "public int Compute() { return 1; } \
         public int AddValue(int accumulator, Task<int> value) { return accumulator; } \
         public int AddPartial(int left, int right) { return left + right; } \
         public int Main() { Task<int>[] values = new Task<int>[1]; return Parallel.Reduce(values, 0, AddValue, AddPartial); }",
        "requires a scalar element type",
    );
}

#[test]
fn parallel_reduce_non_transferable_identity_is_rejected_class() {
    assert_error(
        "public class Box { public Box() {} } \
         public Box AddValue(Box accumulator, int value) { return accumulator; } \
         public Box AddPartial(Box left, Box right) { return left; } \
         public int Main() { int[] values = [1]; Box identity = new Box(); Parallel.Reduce(values, identity, AddValue, AddPartial); return 0; }",
        "cannot cross a worker boundary",
    );
}

#[test]
fn parallel_reduce_decimal_identity_is_rejected() {
    assert_error(
        "public decimal AddValue(decimal accumulator, int value) { return accumulator; } \
         public decimal AddPartial(decimal left, decimal right) { return left; } \
         public decimal Main() { int[] values = [1]; return Parallel.Reduce(values, 0.0m, AddValue, AddPartial); }",
        "cannot cross a worker boundary",
    );
}

#[test]
fn parallel_reduce_accumulate_wrong_arity_is_rejected() {
    assert_error(
        &format!(
            "public int AddValue(int accumulator) {{ return accumulator; }} {ADD_PARTIAL} \
             public int Main() {{ int[] values = [1]; return Parallel.Reduce(values, 0, AddValue, AddPartial); }}"
        ),
        "no static method or free function with signature",
    );
}

#[test]
fn parallel_reduce_accumulate_wrong_parameter_type_is_rejected() {
    assert_error(
        &format!(
            "public int AddValue(int accumulator, long value) {{ return accumulator; }} {ADD_PARTIAL} \
             public int Main() {{ int[] values = [1]; return Parallel.Reduce(values, 0, AddValue, AddPartial); }}"
        ),
        "no static method or free function with signature",
    );
}

#[test]
fn parallel_reduce_accumulate_wrong_return_type_is_rejected() {
    assert_error(
        &format!(
            "public long AddValue(int accumulator, int value) {{ return accumulator; }} {ADD_PARTIAL} \
             public int Main() {{ int[] values = [1]; return Parallel.Reduce(values, 0, AddValue, AddPartial); }}"
        ),
        "no static method or free function with signature",
    );
}

#[test]
fn parallel_reduce_combine_wrong_signature_is_rejected() {
    assert_error(
        &format!(
            "{ADD_VALUE} public int AddPartial(int left, long right) {{ return left; }} \
             public int Main() {{ int[] values = [1]; return Parallel.Reduce(values, 0, AddValue, AddPartial); }}"
        ),
        "no static method or free function with signature",
    );
}

#[test]
fn parallel_reduce_instance_method_accumulate_is_rejected() {
    let source = format!(
        "public class Ops {{ public int AddValue(int accumulator, int value) {{ return accumulator + value; }} }} {ADD_PARTIAL} \
         public int Main() {{ Ops ops = new Ops(); int[] values = [1]; return Parallel.Reduce(values, 0, ops.AddValue, AddPartial); }}"
    );
    assert!(
        compile(&source).is_err(),
        "an instance method must not be accepted as Accumulate"
    );
}

#[test]
fn parallel_reduce_instance_method_combine_is_rejected() {
    let source = format!(
        "{ADD_VALUE} public class Ops {{ public int AddPartial(int left, int right) {{ return left + right; }} }} \
         public int Main() {{ Ops ops = new Ops(); int[] values = [1]; return Parallel.Reduce(values, 0, AddValue, ops.AddPartial); }}"
    );
    assert!(
        compile(&source).is_err(),
        "an instance method must not be accepted as Combine"
    );
}

#[test]
fn parallel_reduce_async_accumulate_is_rejected() {
    assert_error(
        &format!(
            "public int Compute() {{ return 1; }} \
             public async Task<int> AddValue(int accumulator, int value) {{ int v = await Task.Run(Compute); return accumulator + v; }} \
             {ADD_PARTIAL} \
             public int Main() {{ int[] values = [1]; return Parallel.Reduce(values, 0, AddValue, AddPartial); }}"
        ),
        "no static method or free function with signature",
    );
}

#[test]
fn parallel_reduce_direct_nested_task_run_in_accumulate_is_rejected() {
    assert_error(
        &format!(
            "public int Compute() {{ return 1; }} \
             public int AddValue(int accumulator, int value) {{ Task.Run(Compute); return accumulator + value; }} \
             {ADD_PARTIAL} \
             public int Main() {{ int[] values = [1]; return Parallel.Reduce(values, 0, AddValue, AddPartial); }}"
        ),
        "itself uses",
    );
}

#[test]
fn parallel_reduce_direct_nested_parallel_reduce_in_combine_is_rejected() {
    assert_error(
        &format!(
            "{ADD_VALUE} \
             public int AddPartial(int left, int right) {{ int[] inner = [1]; return Parallel.Reduce(inner, left, AddValue, AddPartial); }} \
             public int Main() {{ int[] values = [1, 2]; return Parallel.Reduce(values, 0, AddValue, AddPartial); }}"
        ),
        "itself uses",
    );
}

#[test]
fn parallel_reduce_transitive_nested_concurrency_in_accumulate_is_rejected() {
    assert_error(
        &format!(
            "public int Inner() {{ return 1; }} \
             public void Helper() {{ Task.Run(Inner).Wait(); }} \
             public int AddValue(int accumulator, int value) {{ Helper(); return accumulator + value; }} \
             {ADD_PARTIAL} \
             public int Main() {{ int[] values = [1]; return Parallel.Reduce(values, 0, AddValue, AddPartial); }}"
        ),
        "transitively calls",
    );
}

#[test]
fn parallel_reduce_transitive_nested_parallel_for_in_combine_is_rejected() {
    assert_error(
        &format!(
            "public void Body(int index) {{ }} \
             public void Helper() {{ Parallel.For(0, 1, Body); }} \
             {ADD_VALUE} \
             public int AddPartial(int left, int right) {{ Helper(); return left + right; }} \
             public int Main() {{ int[] values = [1, 2]; return Parallel.Reduce(values, 0, AddValue, AddPartial); }}"
        ),
        "transitively calls",
    );
}

#[test]
fn parallel_reduce_interface_dispatch_reaching_task_run_is_rejected() {
    let source = format!(
        "public interface IWorker {{ int Combine(int left, int right); }} \
         public int Compute() {{ return 1; }} \
         public class Worker : IWorker {{ public Worker() {{}} public int Combine(int left, int right) {{ Task.Run(Compute); return left + right; }} }} \
         public int AddPartial(int left, int right) {{ IWorker worker = new Worker(); return worker.Combine(left, right); }} \
         {ADD_VALUE} \
         public int Main() {{ int[] values = [1, 2]; return Parallel.Reduce(values, 0, AddValue, AddPartial); }}"
    );
    let diagnostics = compile(&source).expect_err("interface dispatch must expose Task.Run");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("through interface call")),
        "missing interface path diagnostic: {diagnostics:#?}"
    );
}

#[test]
fn parallel_reduce_inside_async_function_is_rejected() {
    assert_error(
        &format!(
            "public int Compute() {{ return 1; }} {ADD_VALUE} {ADD_PARTIAL} \
             public int RunReduce() {{ int[] values = [1]; return Parallel.Reduce(values, 0, AddValue, AddPartial); }} \
             public async Task<int> Calculate() {{ int value = await Task.Run(Compute); return value + RunReduce(); }}"
        ),
        "which uses `Parallel.Reduce`",
    );
}

#[test]
fn parallel_reduce_overload_is_chosen_by_complete_signature() {
    // Two `AddValue` overloads exist; only the `(int, int) -> int` one
    // matches the array's `int` element and the identity's `int` type.
    assert_valid(&format!(
        "public int AddValue(int accumulator, int value) {{ return accumulator + value; }} \
         public long AddValue(long accumulator, long value) {{ return accumulator + value; }} \
         {ADD_PARTIAL} \
         public int Main() {{ int[] values = [1, 2, 3]; return Parallel.Reduce(values, 0, AddValue, AddPartial); }}"
    ));
}

#[test]
fn parallel_reduce_ambiguous_overload_is_rejected() {
    // `Main` is itself a method of `Ops`, which also declares a static
    // `AddValue` with the identical signature as the free function: the bare
    // name resolves to both, so `Accumulate` is ambiguous.
    assert_error(
        "public int AddValue(int accumulator, int value) { return accumulator + value; } \
         public class Ops { \
             public static int AddValue(int accumulator, int value) { return accumulator + value; } \
             public static int AddPartial(int left, int right) { return left + right; } \
             public static int Main() { int[] values = [1]; return Parallel.Reduce(values, 0, AddValue, AddPartial); } \
         }",
        "ambiguous",
    );
}

#[test]
fn a_type_named_parallel_by_a_user_namespace_does_not_activate_reduce() {
    // `Parallel` is reserved (see `validate_no_reserved_type_names`), so this
    // is only a negative-shape sanity check: an unrelated `Reduce` method on
    // another type must never be mistaken for the intrinsic.
    assert_valid(
        "public static class Utils { public static int Reduce(int a, int b) { return a + b; } } \
         public int Main() { return Utils.Reduce(1, 2); }",
    );
}

#[test]
fn different_element_and_accumulator_types_are_accepted() {
    // `TElement` (`long`) and `TAccumulator` (`int`) may differ, as long as
    // both are worker-transferable and the signatures are exact.
    assert_valid(
        "public int CountValue(int accumulator, long value) { return accumulator + 1; } \
         public int CountPartial(int left, int right) { return left + right; } \
         public int Main() { long[] values = [10L, 20L, 30L]; return Parallel.Reduce(values, 0, CountValue, CountPartial); }",
    );
}

#[test]
fn parallel_reduce_array_and_identity_are_evaluated_once_left_to_right() {
    let compilation = compile(&format!(
        "public int[] GetValues() {{ return [1, 2, 3]; }} \
         public int GetIdentity() {{ return 0; }} \
         {ADD_VALUE} {ADD_PARTIAL} \
         public int Main() {{ return Parallel.Reduce(GetValues(), GetIdentity(), AddValue, AddPartial); }}"
    ))
    .expect("valid program");
    let main = compilation
        .mir
        .functions
        .iter()
        .find(|function| function.name == "Main" && function.owner.is_none())
        .expect("Main is lowered to MIR");
    let calls: Vec<&aster_compiler::mir::SymbolId> = main
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            aster_compiler::mir::Instruction::Call { function, .. } => Some(function),
            _ => None,
        })
        .collect();
    let get_values = compilation
        .mir
        .functions
        .iter()
        .find(|function| function.name == "GetValues")
        .expect("GetValues is declared")
        .symbol;
    let get_identity = compilation
        .mir
        .functions
        .iter()
        .find(|function| function.name == "GetIdentity")
        .expect("GetIdentity is declared")
        .symbol;
    assert_eq!(
        calls
            .iter()
            .filter(|symbol| ***symbol == get_values)
            .count(),
        1,
        "the array expression must be evaluated exactly once"
    );
    assert_eq!(
        calls
            .iter()
            .filter(|symbol| ***symbol == get_identity)
            .count(),
        1,
        "the identity expression must be evaluated exactly once"
    );
    let values_index = calls
        .iter()
        .position(|symbol| **symbol == get_values)
        .unwrap();
    let identity_index = calls
        .iter()
        .position(|symbol| **symbol == get_identity)
        .unwrap();
    assert!(
        values_index < identity_index,
        "the array must be evaluated before the identity (left to right)"
    );
}
