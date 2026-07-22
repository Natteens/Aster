//! Lote 6D: combined, repeated, and interleaved stress across every
//! concurrency construct (`Task.Run`, async `await`, `Parallel.For`,
//! `Parallel.ForEach`, `Parallel.Reduce`, and plain sequential programs),
//! all sharing one compiled module so the suite exercises the runtime
//! repeatedly without re-parsing or re-analyzing source on every iteration.
//!
//! This file is deliberately one focused suite, not dozens of tiny files:
//! each test drives many iterations of a combined scenario rather than
//! asserting one narrow fact. Determinism checks never use `sleep`; where
//! ordering matters, the existing structural guarantees (chunk-index
//! ordering, smallest-logical-index error selection) are exercised at scale
//! instead of raced against a timer.

use aster_codegen_cranelift::{ExecutionValue, execute, execute_with_stats};
use aster_compiler::compile;
use aster_mir as mir;

/// One module covering every construct under stress, compiled exactly once
/// per test (`compile` is the expensive step; `execute`/`execute_with_stats`
/// each still JIT-prepare fresh per call, matching the public API's actual
/// contract — there is no public "prepare once, invoke many" entry point to
/// bypass, so per-call JIT is the correct, intended cost being stressed).
const STRESS_MODULE: &str = r"
    public int Compute() { return 1; }
    public int Failing() { int[] a = new int[1]; return a[5]; }

    public int AddValue(int accumulator, int value) { return accumulator + value; }
    public int AddPartial(int left, int right) { return left + right; }

    public void ForBody(int index) { }
    public void ForBodyFailingAtSeven(int index) {
        int size = index == 7 ? 7 : 8;
        int[] a = new int[size];
        int x = a[index];
    }

    public void ForEachBody(int value) { }

    public int[] Range(int count) {
        int[] values = new int[count];
        for (int i = 0; i < count; i++) { values[i] = i + 1; }
        return values;
    }

    public async Task<int> AsyncCalculate() {
        int value = await Task.Run(Compute);
        return value + 1;
    }

    public int RunTaskRunWait() { return Task.Run(Compute).Wait(); }

    public int RunAsyncWait() { return AsyncCalculate().Wait(); }

    public int RunParallelFor() { Parallel.For(0, 200, ForBody); return 1; }

    public int RunParallelForEmptyRange() { Parallel.For(9, 9, ForBody); return 1; }

    public int RunParallelForFailing() {
        Parallel.For(0, 8, ForBodyFailingAtSeven);
        return 1;
    }

    public int RunParallelForEach() {
        int[] values = Range(200);
        Parallel.ForEach(values, ForEachBody);
        return 1;
    }

    public int RunParallelForEachEmpty() {
        int[] values = new int[0];
        Parallel.ForEach(values, ForEachBody);
        return 1;
    }

    public int RunParallelReduceSmall() {
        int[] values = Range(10);
        return Parallel.Reduce(values, 0, AddValue, AddPartial);
    }

    public int RunParallelReduceLarger() {
        int[] values = Range(2000);
        return Parallel.Reduce(values, 0, AddValue, AddPartial);
    }

    public int RunParallelReduceEmpty() {
        int[] values = new int[0];
        return Parallel.Reduce(values, 99, AddValue, AddPartial);
    }

    public int RunSequential() { return 40 + 2; }

    public int RunAbandonedTaskAndAsync() {
        Task<int> plain = Task.Run(Compute);
        Task<int> asyncTask = AsyncCalculate();
        return 1;
    }
";

fn module() -> mir::Module {
    compile(STRESS_MODULE)
        .expect("the shared stress module compiles")
        .mir
}

fn run(module: &mir::Module, entry: &str) -> Result<ExecutionValue, String> {
    execute(module, entry).map_err(|error| error.to_string())
}

/// Cycle through every construct many times on one compiled module,
/// asserting each entry's exact expected outcome every time. A single
/// contaminated `ExecutionContext`, a leaked pool, or a misrouted outcome
/// would eventually surface as a wrong value or an unexpected error on some
/// later iteration, not necessarily the first.
#[test]
fn interleaved_operations_across_many_repetitions_do_not_contaminate_each_other() {
    let module = module();
    let entries: [(&str, Result<ExecutionValue, &str>); 10] = [
        ("RunTaskRunWait", Ok(ExecutionValue::Int(1))),
        ("RunAsyncWait", Ok(ExecutionValue::Int(2))),
        ("RunParallelFor", Ok(ExecutionValue::Int(1))),
        ("RunParallelForEmptyRange", Ok(ExecutionValue::Int(1))),
        ("RunParallelForEach", Ok(ExecutionValue::Int(1))),
        ("RunParallelForEachEmpty", Ok(ExecutionValue::Int(1))),
        ("RunParallelReduceSmall", Ok(ExecutionValue::Int(55))),
        ("RunParallelReduceEmpty", Ok(ExecutionValue::Int(99))),
        ("RunSequential", Ok(ExecutionValue::Int(42))),
        ("RunAbandonedTaskAndAsync", Ok(ExecutionValue::Int(1))),
    ];

    for round in 0..6 {
        for (entry, expected) in &entries {
            let result = run(&module, entry);
            match expected {
                Ok(value) => assert_eq!(
                    result,
                    Ok(value.clone()),
                    "round {round}, entry {entry}: unexpected result"
                ),
                Err(substring) => {
                    let error = result.expect_err("this entry must fail");
                    assert!(
                        error.contains(substring),
                        "round {round}, entry {entry}: unexpected error {error}"
                    );
                }
            }
        }
    }
}

/// A controlled runtime error, on either `Parallel.For` or `Parallel.Reduce`,
/// interleaved between many successful executions of every other construct,
/// must never affect a neighboring call.
#[test]
fn controlled_errors_interleaved_with_successes_remain_isolated() {
    let module = module();
    for round in 0..12 {
        assert_eq!(run(&module, "RunSequential"), Ok(ExecutionValue::Int(42)));
        assert_eq!(
            run(&module, "RunParallelReduceSmall"),
            Ok(ExecutionValue::Int(55))
        );

        let error = run(&module, "RunParallelForFailing")
            .expect_err("index 7's deliberately undersized array must fail");
        assert!(
            error.contains("array index 7"),
            "round {round}: unexpected error {error}"
        );

        assert_eq!(run(&module, "RunTaskRunWait"), Ok(ExecutionValue::Int(1)));
        assert_eq!(
            run(&module, "RunParallelForEach"),
            Ok(ExecutionValue::Int(1)),
            "round {round}: a neighboring ForEach must not be affected by the earlier failure"
        );
    }
}

/// Many abandoned `Task.Run` and async handles, across many independent
/// top-level executions, must never hang, panic, or leak into a later call.
#[test]
fn many_abandoned_handles_across_many_executions_do_not_hang_or_leak() {
    let module = module();
    for _ in 0..20 {
        assert_eq!(
            run(&module, "RunAbandonedTaskAndAsync"),
            Ok(ExecutionValue::Int(1))
        );
    }
    // The runtime must still be perfectly usable afterward.
    assert_eq!(run(&module, "RunSequential"), Ok(ExecutionValue::Int(42)));
}

/// Empty ranges and empty arrays across every parallel construct must
/// succeed without running any body/operator, repeatedly.
#[test]
fn empty_ranges_and_arrays_succeed_across_every_construct_repeatedly() {
    let module = module();
    for _ in 0..12 {
        assert_eq!(
            run(&module, "RunParallelForEmptyRange"),
            Ok(ExecutionValue::Int(1))
        );
        assert_eq!(
            run(&module, "RunParallelForEachEmpty"),
            Ok(ExecutionValue::Int(1))
        );
        assert_eq!(
            run(&module, "RunParallelReduceEmpty"),
            Ok(ExecutionValue::Int(99))
        );
    }
}

/// Small and larger loads over the same `Parallel.Reduce` operators must
/// keep producing the mathematically exact sum (`n * (n + 1) / 2`), proving
/// every element is folded exactly once regardless of chunk count.
#[test]
fn small_and_larger_reduce_loads_produce_the_exact_sum() {
    let module = module();
    assert_eq!(
        run(&module, "RunParallelReduceSmall"),
        Ok(ExecutionValue::Int(55))
    );
    // 1..=2000 summed.
    assert_eq!(
        run(&module, "RunParallelReduceLarger"),
        Ok(ExecutionValue::Int(2_001_000))
    );
}

/// Repeated, independent top-level executions of every concurrent construct
/// must not grow `used_bytes`/`reserved_bytes` linearly: each call's own
/// `ExecutionContext` is dropped when that call returns, so metrics must be
/// stable call over call, not accumulate.
#[test]
fn repeated_executions_of_every_construct_do_not_accumulate_memory() {
    let module = module();
    for entry in [
        "RunTaskRunWait",
        "RunAsyncWait",
        "RunParallelFor",
        "RunParallelForEach",
        "RunParallelReduceSmall",
        "RunSequential",
    ] {
        let mut expected_used_bytes = None;
        let mut expected_reserved_bytes = None;
        for iteration in 0..10 {
            let (_, stats) = execute_with_stats(&module, entry)
                .unwrap_or_else(|error| panic!("{entry} iteration {iteration} failed: {error}"));
            // Each call gets a brand new `ExecutionContext`: since the same
            // code always allocates the same objects in the same order, both
            // fields must be exactly stable call over call, not merely
            // bounded — any drift would mean a previous call's state is
            // somehow visible to this one.
            let used = *expected_used_bytes.get_or_insert(stats.used_bytes);
            assert_eq!(
                stats.used_bytes, used,
                "{entry} iteration {iteration}: used_bytes must stay stable, not accumulate"
            );
            let reserved = *expected_reserved_bytes.get_or_insert(stats.reserved_bytes);
            assert_eq!(
                stats.reserved_bytes, reserved,
                "{entry} iteration {iteration}: reserved_bytes must stay stable, not accumulate"
            );
        }
    }
}

/// A fully sequential program, run many times between concurrent
/// executions, must never gain a pool, a completion queue, or any other
/// concurrency infrastructure it does not need: its metrics must be
/// identical to running it in complete isolation.
#[test]
fn sequential_programs_stay_unaffected_between_concurrent_calls() {
    let module = module();
    let (isolated_value, isolated_stats) =
        execute_with_stats(&module, "RunSequential").expect("sequential entry runs alone");

    for round in 0..8 {
        // Sandwich the sequential call between concurrent ones.
        assert_eq!(
            run(&module, "RunParallelReduceSmall"),
            Ok(ExecutionValue::Int(55))
        );
        let (value, stats) = execute_with_stats(&module, "RunSequential")
            .unwrap_or_else(|error| panic!("round {round}: sequential call failed: {error}"));
        assert_eq!(value, isolated_value);
        assert_eq!(
            stats, isolated_stats,
            "round {round}: sequential metrics must match the isolated baseline exactly"
        );
        assert_eq!(run(&module, "RunParallelFor"), Ok(ExecutionValue::Int(1)));
    }
}

/// A deterministic (seed-free, no RNG dependency) longer stress run for
/// manual/local use. Cycles a fixed, larger sequence of every construct,
/// including deliberately failing ones at fixed positions, and asserts the
/// exact same outcomes a much larger number of times. Not part of the
/// default CI run.
#[test]
#[ignore = "long-running concurrency stress"]
fn long_running_combined_stress() {
    const ITERATIONS: usize = 2_000;

    let module = module();
    let sequence: [&str; 8] = [
        "RunTaskRunWait",
        "RunAsyncWait",
        "RunParallelFor",
        "RunParallelForEach",
        "RunParallelReduceSmall",
        "RunParallelReduceLarger",
        "RunSequential",
        "RunAbandonedTaskAndAsync",
    ];
    for iteration in 0..ITERATIONS {
        let entry = sequence[iteration % sequence.len()];
        let expected = match entry {
            "RunTaskRunWait"
            | "RunParallelFor"
            | "RunParallelForEach"
            | "RunAbandonedTaskAndAsync" => ExecutionValue::Int(1),
            "RunAsyncWait" => ExecutionValue::Int(2),
            "RunParallelReduceSmall" => ExecutionValue::Int(55),
            "RunParallelReduceLarger" => ExecutionValue::Int(2_001_000),
            "RunSequential" => ExecutionValue::Int(42),
            _ => unreachable!("sequence lists only the entries handled above"),
        };
        assert_eq!(
            run(&module, entry),
            Ok(expected),
            "iteration {iteration} ({entry}) diverged from its deterministic expectation"
        );
        // Deliberately failing calls interleaved at a fixed cadence, proving
        // isolation holds across thousands of independent executions.
        if iteration % 37 == 0 {
            let error = run(&module, "RunParallelForFailing")
                .expect_err("the fixed-position failing call must still fail");
            assert!(error.contains("array index 7"));
        }
    }
}
