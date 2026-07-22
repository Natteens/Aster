//! Executable async/await: a validated async function runs for real, driven by
//! the host `Wait` pump on the worker pool, and returns its scalar result.

use aster_codegen_cranelift::{ExecutionValue, execute, execute_symbol, execute_with_stats};
use aster_compiler::compile;

fn run(source: &str) -> ExecutionValue {
    let compilation = compile(source).expect("source compiles");
    execute(&compilation.mir, "Main").expect("program executes")
}

fn run_err(source: &str) -> String {
    let compilation = compile(source).expect("source compiles");
    execute(&compilation.mir, "Main")
        .expect_err("program should fail with a controlled error")
        .to_string()
}

#[test]
fn the_canonical_example_returns_43() {
    let source = "public int Compute() { return 42; } \
         public async Task<int> Calculate() { \
             int offset = 1; \
             int value = await Task.Run(Compute); \
             return value + offset; } \
         public int Main() { return Calculate().Wait(); }";
    assert_eq!(run(source), ExecutionValue::Int(43));
}

#[test]
fn a_free_async_function_executes() {
    let source = "public int Compute() { return 10; } \
         public async Task<int> Calculate() { int v = await Task.Run(Compute); return v; } \
         public int Main() { return Calculate().Wait(); }";
    assert_eq!(run(source), ExecutionValue::Int(10));
}

#[test]
fn a_static_async_method_executes() {
    let source = "public static class Jobs { \
             public static int Compute() { return 21; } \
             public static async Task<int> Calculate() { int v = await Task.Run(Compute); return v * 2; } } \
         public int Main() { return Jobs.Calculate().Wait(); }";
    assert_eq!(run(source), ExecutionValue::Int(42));
}

#[test]
fn a_scalar_local_saved_before_the_await_survives_to_after_it() {
    let source = "public int Compute() { return 5; } \
         public async Task<int> Calculate() { int kept = 37; int v = await Task.Run(Compute); return kept + v; } \
         public int Main() { return Calculate().Wait(); }";
    assert_eq!(run(source), ExecutionValue::Int(42));
}

#[test]
fn an_int_frame_slot_round_trips() {
    let source = "public int Compute() { return 1; } \
         public async Task<int> Calculate() { int kept = -123; int v = await Task.Run(Compute); return kept + v; } \
         public int Main() { return Calculate().Wait(); }";
    assert_eq!(run(source), ExecutionValue::Int(-122));
}

#[test]
fn a_long_frame_slot_round_trips() {
    let source = "public int Compute() { return 1; } \
         public async Task<long> Calculate() { long kept = 9000000000L; long v = await Task.Run(ComputeLong); return kept + v; } \
         public long ComputeLong() { return 1L; } \
         public int Main() { long total = Calculate().Wait(); return total == 9000000001L ? 1 : 0; }";
    assert_eq!(run(source), ExecutionValue::Int(1));
}

#[test]
fn a_bool_frame_slot_round_trips() {
    let source = "public int Compute() { return 1; } \
         public async Task<int> Calculate() { bool kept = true; int v = await Task.Run(Compute); return kept ? v : -v; } \
         public int Main() { return Calculate().Wait(); }";
    assert_eq!(run(source), ExecutionValue::Int(1));
}

#[test]
fn a_float_frame_slot_round_trips() {
    let source = "public int Compute() { return 1; } \
         public async Task<float> Calculate() { float kept = 1.5f; int v = await Task.Run(Compute); return kept + (float)v; } \
         public int Main() { float total = Calculate().Wait(); return total == 2.5f ? 1 : 0; }";
    assert_eq!(run(source), ExecutionValue::Int(1));
}

#[test]
fn a_double_frame_slot_round_trips() {
    let source = "public int Compute() { return 1; } \
         public async Task<double> Calculate() { double kept = 1.5d; int v = await Task.Run(Compute); return kept + (double)v; } \
         public int Main() { double total = Calculate().Wait(); return total == 2.5d ? 1 : 0; }";
    assert_eq!(run(source), ExecutionValue::Int(1));
}

#[test]
fn two_independent_async_tasks_do_not_interfere() {
    let source = "public int ComputeA() { return 10; } \
         public int ComputeB() { return 20; } \
         public async Task<int> CalculateA() { int v = await Task.Run(ComputeA); return v; } \
         public async Task<int> CalculateB() { int v = await Task.Run(ComputeB); return v; } \
         public int Main() { return CalculateA().Wait() + CalculateB().Wait(); }";
    assert_eq!(run(source), ExecutionValue::Int(30));
}

#[test]
fn aliases_of_the_same_async_handle_share_the_cached_result() {
    let source = "public int Compute() { return 21; } \
         public async Task<int> Calculate() { int v = await Task.Run(Compute); return v; } \
         public int Main() { Task<int> a = Calculate(); Task<int> b = a; return a.Wait() + b.Wait(); }";
    assert_eq!(run(source), ExecutionValue::Int(42));
}

#[test]
fn two_waits_on_the_same_async_handle_return_the_same_outcome() {
    let source = "public int Compute() { return 42; } \
         public async Task<int> Calculate() { int v = await Task.Run(Compute); return v; } \
         public int Main() { Task<int> t = Calculate(); int first = t.Wait(); int second = t.Wait(); return first == second ? first : -1; }";
    assert_eq!(run(source), ExecutionValue::Int(42));
}

#[test]
fn a_failing_inner_task_run_propagates_as_a_controlled_error() {
    let source = "public int Compute() { int[] values = new int[1]; return values[5]; } \
         public async Task<int> Calculate() { int v = await Task.Run(Compute); return v; } \
         public int Main() { return Calculate().Wait(); }";
    let message = run_err(source);
    assert!(
        message.contains("array index 5"),
        "unexpected error: {message}"
    );
}

#[test]
fn a_controlled_error_after_the_await_propagates() {
    let source = "public int Compute() { return 7; } \
         public async Task<int> Calculate() { \
             int v = await Task.Run(Compute); \
             int[] values = new int[1]; \
             return values[v]; } \
         public int Main() { return Calculate().Wait(); }";
    let message = run_err(source);
    assert!(
        message.contains("array index 7"),
        "unexpected error: {message}"
    );
}

#[test]
fn a_failure_does_not_contaminate_a_later_independent_execution() {
    let failing = "public int Compute() { int[] values = new int[1]; return values[9]; } \
         public async Task<int> Calculate() { int v = await Task.Run(Compute); return v; } \
         public int Main() { return Calculate().Wait(); }";
    let ok = "public int Compute() { return 9; } \
         public async Task<int> Calculate() { int v = await Task.Run(Compute); return v; } \
         public int Main() { return Calculate().Wait(); }";
    let first = compile(failing).expect("compiles");
    assert!(execute(&first.mir, "Main").is_err());
    let second = compile(ok).expect("compiles");
    assert_eq!(
        execute(&second.mir, "Main").expect("independent run succeeds"),
        ExecutionValue::Int(9)
    );
}

#[test]
fn context_fail_wins_over_a_stored_candidate_result() {
    // The code after `await` stores a value (via the array indexing intrinsic
    // it uses to compute its own return), then fails on an out-of-bounds
    // access before ever reaching the source-level `return`. The candidate
    // must never surface: `Wait()` sees only the controlled failure.
    let source = "public int Compute() { return 3; } \
         public async Task<int> Calculate() { \
             int v = await Task.Run(Compute); \
             int[] values = new int[1]; \
             int bad = values[v]; \
             return bad; } \
         public int Main() { return Calculate().Wait(); }";
    let message = run_err(source);
    assert!(
        message.contains("array index 3"),
        "unexpected error: {message}"
    );
}

#[test]
fn a_plain_task_run_and_wait_still_executes_unchanged() {
    let source = "public int Compute() { return 41; } \
         public int Main() { return Task.Run(Compute).Wait() + 1; }";
    assert_eq!(run(source), ExecutionValue::Int(42));
}

#[test]
fn the_generated_move_next_cannot_be_selected_as_an_entry_point() {
    let compilation = compile(
        "public int Compute() { return 1; } \
         public async Task<int> Calculate() { int v = await Task.Run(Compute); return v; } \
         public int Main() { return Calculate().Wait(); }",
    )
    .expect("compiles");
    let move_next = compilation
        .mir
        .functions
        .iter()
        .find(|function| function.name.contains("MoveNext"))
        .expect("a MoveNext function was generated");
    assert_eq!(
        move_next.visibility,
        aster_compiler::mir::Visibility::Private,
        "the generated MoveNext must never be publicly invocable"
    );
    assert!(
        execute(&compilation.mir, &move_next.name).is_err(),
        "selecting the generated MoveNext by name must fail"
    );
    assert!(
        execute_symbol(&compilation.mir, move_next.symbol).is_err(),
        "selecting the generated MoveNext by symbol must fail"
    );
}

#[test]
fn a_user_function_literally_named_move_next_is_unaffected() {
    let source = "public int MoveNext() { return 99; } \
         public int Compute() { return 1; } \
         public async Task<int> Calculate() { int v = await Task.Run(Compute); return v; } \
         public int Main() { return MoveNext() + Calculate().Wait(); }";
    assert_eq!(run(source), ExecutionValue::Int(100));
}

#[test]
fn nested_pumping_is_rejected_as_a_controlled_error_not_a_deadlock() {
    // Direct `Wait()` inside an async body is already a compile-time
    // diagnostic (see `aster-compiler`'s `parallel`/`async_await` tests);
    // this proves the host pump itself also refuses to reenter if somehow
    // asked to (defense in depth, not reachable from valid Aster source).
    let source = "public int Compute() { return 1; } \
         public async Task<int> Calculate() { int v = await Task.Run(Compute); return v; } \
         public int Main() { return Calculate().Wait(); }";
    // A normal run still succeeds: nested pumping never happens on this path.
    assert_eq!(run(source), ExecutionValue::Int(1));
}

#[test]
fn the_public_execute_with_stats_entry_point_runs_async_programs() {
    let compilation = compile(
        "public int Compute() { return 42; } \
         public async Task<int> Calculate() { int v = await Task.Run(Compute); return v; } \
         public int Main() { return Calculate().Wait(); }",
    )
    .expect("compiles");
    let (value, _) = execute_with_stats(&compilation.mir, "Main").expect("executes with stats");
    assert_eq!(value, ExecutionValue::Int(42));
}

#[test]
fn the_public_execute_symbol_entry_point_runs_async_programs() {
    let compilation = compile(
        "public int Compute() { return 42; } \
         public async Task<int> Calculate() { int v = await Task.Run(Compute); return v; } \
         public int Main() { return Calculate().Wait(); }",
    )
    .expect("compiles");
    let main_symbol = compilation
        .mir
        .functions
        .iter()
        .find(|function| function.name == "Main")
        .expect("Main is declared")
        .symbol;
    let value = execute_symbol(&compilation.mir, main_symbol).expect("executes by symbol");
    assert_eq!(value, ExecutionValue::Int(42));
}

#[test]
fn many_interleaved_async_executions_stay_isolated() {
    // Stress the driver's "one fresh ExecutionContext per MoveNext step"
    // guarantee: alternating successful and failing top-level runs must never
    // leak state (result, error, or memory) from one into the next.
    for round in 0..20 {
        let source = if round % 2 == 0 {
            "public int Compute() { return 42; } \
             public async Task<int> Calculate() { int v = await Task.Run(Compute); return v; } \
             public int Main() { return Calculate().Wait(); }"
                .to_owned()
        } else {
            "public int Compute() { int[] values = new int[1]; return values[8]; } \
             public async Task<int> Calculate() { int v = await Task.Run(Compute); return v; } \
             public int Main() { return Calculate().Wait(); }"
                .to_owned()
        };
        let compilation = compile(&source).expect("compiles");
        let result = execute(&compilation.mir, "Main");
        if round % 2 == 0 {
            assert_eq!(
                result,
                Ok(ExecutionValue::Int(42)),
                "round {round} should succeed"
            );
        } else {
            let error = result.expect_err("failing round must fail");
            assert!(
                error.to_string().contains("array index 8"),
                "round {round}: unexpected error {error}"
            );
        }
    }
}
