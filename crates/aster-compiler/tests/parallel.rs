use aster_compiler::compile;

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
