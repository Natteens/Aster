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

#[test]
fn parallel_reduce_is_not_available() {
    let source = "public int Body(int a, int b) { return a + b; } \
         public int Main() { return Parallel.Reduce(0, 10, Body); }";
    assert!(
        compile(source).is_err(),
        "Parallel.Reduce must not compile in this version"
    );
}
