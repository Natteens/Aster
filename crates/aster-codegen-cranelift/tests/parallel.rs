//! End-to-end tests for `Parallel.For`/`Parallel.ForEach`: chunked execution
//! on the existing worker pool, deterministic error selection, and the
//! host-owned scalar copies `ForEach` sends to workers.

use aster_codegen_cranelift::{ExecutionValue, execute, execute_with_stats};
use aster_compiler::compile;

fn run(source: &str) -> Result<ExecutionValue, String> {
    let compilation = compile(source).map_err(|diagnostics| format!("{diagnostics:#?}"))?;
    execute(&compilation.mir, "Main").map_err(|error| error.to_string())
}

#[test]
fn parallel_for_with_empty_range_runs_zero_jobs_and_succeeds() {
    let source = "public void Body(int index) { int[] a = new int[1]; int bad = a[index]; } \
         public int Main() { Parallel.For(5, 5, Body); return 42; }";
    assert_eq!(run(source), Ok(ExecutionValue::Int(42)));
}

#[test]
fn parallel_for_one_iteration_runs() {
    let source = "public void Body(int index) { } \
         public int Main() { Parallel.For(0, 1, Body); return 42; }";
    assert_eq!(run(source), Ok(ExecutionValue::Int(42)));
}

#[test]
fn parallel_for_many_iterations_runs() {
    let source = "public void Body(int index) { } \
         public int Main() { Parallel.For(0, 1000, Body); return 42; }";
    assert_eq!(run(source), Ok(ExecutionValue::Int(42)));
}

#[test]
fn parallel_for_negative_start_runs() {
    let source = "public void Body(int index) { } \
         public int Main() { Parallel.For(-500, 500, Body); return 42; }";
    assert_eq!(run(source), Ok(ExecutionValue::Int(42)));
}

#[test]
fn parallel_for_inverted_range_is_a_controlled_runtime_error() {
    let source = "public void Body(int index) { } \
         public int Main() { Parallel.For(10, 0, Body); return 42; }";
    let error = run(source).expect_err("end < start must fail");
    assert!(error.contains("before start"), "unexpected error: {error}");
}

#[test]
fn parallel_for_free_function_body_runs() {
    let source = "public void Body(int index) { } \
         public int Main() { Parallel.For(0, 10, Body); return 1; }";
    assert_eq!(run(source), Ok(ExecutionValue::Int(1)));
}

#[test]
fn parallel_for_static_method_body_runs() {
    let source = "public static class Jobs { public static void Body(int index) { } } \
         public int Main() { Parallel.For(0, 10, Jobs.Body); return 1; }";
    assert_eq!(run(source), Ok(ExecutionValue::Int(1)));
}

#[test]
fn parallel_for_body_error_is_propagated_with_its_logical_index() {
    // Every index but 7 is in bounds against a length-8 array; index 7
    // itself is the only one that overflows a length-7 slot it is
    // deliberately given, so the single controlled failure is unambiguous.
    let source = "public void Body(int index) { \
             int size = index == 7 ? 7 : 8; \
             int[] a = new int[size]; \
             int bad = a[index]; } \
         public int Main() { Parallel.For(0, 8, Body); return 1; }";
    let error = run(source).expect_err("index 7 must fail");
    assert!(error.contains("array index 7"), "unexpected error: {error}");
}

#[test]
fn parallel_for_selects_the_smallest_failing_index_deterministically() {
    // Indices 2, 5, and 9 all fail (each indexes a too-small array with its
    // own value); the smallest, 2, must always be the one reported,
    // regardless of which chunk or worker finishes first.
    let source = "public void Body(int index) { \
             if (index == 2 || index == 5 || index == 9) { \
                 int[] a = new int[1]; \
                 int bad = a[index]; \
             } } \
         public int Main() { Parallel.For(0, 12, Body); return 1; }";
    let error = run(source).expect_err("indices 2, 5, and 9 all fail");
    assert!(error.contains("array index 2"), "unexpected error: {error}");
}

#[test]
fn parallel_for_repeated_calls_reuse_the_pool() {
    let source = "public void Body(int index) { } \
         public int Main() { \
             Parallel.For(0, 100, Body); \
             Parallel.For(0, 100, Body); \
             Parallel.For(0, 100, Body); \
             return 3; }";
    assert_eq!(run(source), Ok(ExecutionValue::Int(3)));
}

#[test]
fn parallel_for_each_with_empty_array_runs_zero_jobs_and_succeeds() {
    let source = "public void Body(int value) { } \
         public int Main() { int[] values = new int[0]; Parallel.ForEach(values, Body); return 42; }";
    assert_eq!(run(source), Ok(ExecutionValue::Int(42)));
}

#[test]
fn parallel_for_each_delivers_the_exact_int_value_to_the_worker() {
    // The body deliberately indexes an out-of-bounds array using the
    // received value: the resulting error message names that exact value,
    // proving it crossed the ABI intact (width and signedness both).
    let source = "public void Body(int value) { int[] a = new int[1]; int bad = a[value]; } \
         public int Main() { int[] values = [3]; Parallel.ForEach(values, Body); return 0; }";
    let error = run(source).expect_err("value 3 must trigger an out-of-bounds access");
    assert!(error.contains("array index 3"), "unexpected error: {error}");
}

#[test]
fn parallel_for_each_delivers_the_exact_long_value_to_the_worker() {
    let source = "public void Body(long value) { int[] a = new int[1]; int bad = a[(int)value]; } \
         public int Main() { long[] values = [5L]; Parallel.ForEach(values, Body); return 0; }";
    let error = run(source).expect_err("value 5 must trigger an out-of-bounds access");
    assert!(error.contains("array index 5"), "unexpected error: {error}");
}

#[test]
fn parallel_for_each_delivers_the_exact_bool_value_to_the_worker() {
    let source = "public void Body(bool value) { \
             int index = value ? 6 : 4; \
             int[] a = new int[1]; \
             int bad = a[index]; } \
         public int Main() { bool[] values = [true]; Parallel.ForEach(values, Body); return 0; }";
    let error = run(source).expect_err("true must select index 6");
    assert!(error.contains("array index 6"), "unexpected error: {error}");
}

#[test]
fn parallel_for_each_delivers_the_exact_double_value_to_the_worker() {
    let source = "public void Body(double value) { int[] a = new int[1]; int bad = a[(int)value]; } \
         public int Main() { double[] values = [9.0d]; Parallel.ForEach(values, Body); return 0; }";
    let error = run(source).expect_err("value 9.0 must trigger an out-of-bounds access");
    assert!(error.contains("array index 9"), "unexpected error: {error}");
}

#[test]
fn parallel_for_each_int_array_runs_cleanly_when_every_value_is_valid() {
    let source = "public void Body(int value) { } \
         public int Main() { int[] values = [1, 2, 3, 4, 5]; Parallel.ForEach(values, Body); return 5; }";
    assert_eq!(run(source), Ok(ExecutionValue::Int(5)));
}

#[test]
fn parallel_for_each_selects_the_smallest_failing_array_position_not_value() {
    // Position 0 holds the larger value (9) but fails at the smaller
    // *position*; position 1 holds the smaller value (1) but fails at the
    // larger position. Selecting by position means position 0's failure
    // (value 9) must win, proving the ordering is positional, not by value.
    let source = "public void Body(int value) { int[] a = new int[1]; int bad = a[value]; } \
         public int Main() { int[] values = [9, 1]; Parallel.ForEach(values, Body); return 0; }";
    let error = run(source).expect_err("both positions fail");
    assert!(error.contains("array index 9"), "unexpected error: {error}");
}

#[test]
fn parallel_for_each_repeated_calls_reuse_the_pool() {
    let source = "public void Body(int value) { } \
         public int Main() { \
             int[] values = [1, 2, 3]; \
             Parallel.ForEach(values, Body); \
             Parallel.ForEach(values, Body); \
             return 2; }";
    assert_eq!(run(source), Ok(ExecutionValue::Int(2)));
}

#[test]
fn a_sequential_module_still_executes_normally_without_parallel() {
    let source = "public int Main() { return 40 + 2; }";
    assert_eq!(run(source), Ok(ExecutionValue::Int(42)));
}

#[test]
fn many_chunks_across_repeated_calls_do_not_accumulate_memory_in_the_caller() {
    // `Main`'s own `ExecutionContext` never allocates on behalf of any chunk
    // (each chunk gets its own, on its own worker); after many repeated
    // `Parallel.For` calls, `Main`'s own usage is still zero.
    let source = "public void ForBody(int index) { } \
         public int Main() { \
             for (int round = 0; round < 20; round++) { \
                 Parallel.For(0, 200, ForBody); \
             } \
             return 1; }";
    let compilation = compile(source).expect("compiles");
    let (value, stats) = execute_with_stats(&compilation.mir, "Main").expect("executes");
    assert_eq!(value, ExecutionValue::Int(1));
    assert_eq!(
        stats.used_bytes, 0,
        "no chunk allocation ever lands in Main's own context"
    );
}

#[test]
fn errors_and_successes_interleave_across_many_separate_executions() {
    for round in 0..20 {
        let source = if round % 2 == 0 {
            "public void Body(int index) { } \
             public int Main() { Parallel.For(0, 50, Body); return 1; }"
                .to_owned()
        } else {
            "public void Body(int index) { int[] a = new int[0]; int bad = a[index]; } \
             public int Main() { Parallel.For(0, 1, Body); return 1; }"
                .to_owned()
        };
        let result = run(&source);
        if round % 2 == 0 {
            assert_eq!(
                result,
                Ok(ExecutionValue::Int(1)),
                "round {round} should succeed"
            );
        } else {
            let error = result.expect_err("failing round must fail");
            assert!(
                error.contains("array index 0"),
                "round {round}: unexpected error {error}"
            );
        }
    }
}
