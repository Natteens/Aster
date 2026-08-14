use aster_codegen_cranelift::{ExecutionValue, execute};
use aster_compiler::{compile, compile_without_mir_optimizer_for_research};

fn run(source: &str, function: &str) -> Result<ExecutionValue, String> {
    let compilation = compile(source).map_err(|diagnostics| format!("{diagnostics:#?}"))?;
    execute(&compilation.mir, function).map_err(|error| error.to_string())
}

fn run_baseline(source: &str, function: &str) -> Result<ExecutionValue, String> {
    let compilation = compile_without_mir_optimizer_for_research(source)
        .map_err(|diagnostics| format!("{diagnostics:#?}"))?;
    execute(&compilation.mir, function).map_err(|error| error.to_string())
}

#[test]
fn optimized_constant_control_executes_the_same_result() {
    let source = r"
        public int Run() {
            int seed = 10;
            int copy = seed;
            int result = copy + 2;
            if (3 < 4) { return result; }
            return 0;
        }
    ";
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(12)));
}

#[test]
fn dead_integer_division_and_bounds_access_still_fail() {
    let division = run(
        "public int Run() { int zero = 0; int unused = 10 / zero; return 1; }",
        "Run",
    )
    .expect_err("dead division must retain its controlled failure");
    assert!(division.contains("division by zero"), "{division}");

    let bounds = run(
        "public int Run() { int[] values = new int[1]; int unused = values[1]; return 1; }",
        "Run",
    )
    .expect_err("dead array access must retain its bounds failure");
    assert!(bounds.contains("array index"), "{bounds}");
}

#[test]
fn dead_call_and_allocation_retain_failure_behavior() {
    let call = run(
        "internal int Fail() { int zero = 0; return 1 / zero; } public int Run() { int unused = Fail(); return 1; }",
        "Run",
    )
    .expect_err("an unused call must still execute");
    assert!(call.contains("division by zero"), "{call}");

    let allocation = run(
        "public int Run() { int[] unused = new int[2147483647]; return 1; }",
        "Run",
    )
    .expect_err("an unused allocation must retain governor-visible failure");
    assert!(
        allocation.contains("execution memory limit"),
        "{allocation}"
    );
}

#[test]
fn first_error_order_is_unchanged_after_pure_work_disappears() {
    let error = run(
        "public int Run() { int pure = 1 + 2; int zero = 0; int[] values = new int[1]; int first = 10 / zero; int second = values[2]; return pure; }",
        "Run",
    )
    .expect_err("the first failing operation must remain first");
    assert!(error.contains("division by zero"), "{error}");
    assert!(!error.contains("array index"), "{error}");
}

#[test]
fn literal_float_folding_preserves_ieee_nan_behavior() {
    let source = r"
        public int Run() {
            double zero = 0.0d;
            double value = zero / zero;
            if (value == value) { return 1; }
            return 2;
        }
    ";
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(2)));
}

#[test]
fn loop_backedges_and_copy_mutation_match_unoptimized_execution() {
    let source = r"
        internal int Keep(int input) {
            int first = input;
            int saved = first;
            input = 10;
            return saved;
        }
        public int Run() {
            int value = 7;
            int index = 0;
            while (index < 4) {
                value = value + 1;
                index = index + 1;
            }
            for (int step = 0; step < 6; step += 1) {
                if (step % 2 == 0) {
                    value = value + 2;
                    continue;
                }
                value = value + 3;
            }
            int outer = 0;
            while (outer < 3) {
                int inner = 0;
                while (inner < 2) {
                    value = value + outer + inner;
                    inner = inner + 1;
                }
                outer = outer + 1;
            }
            return value + Keep(3);
        }
    ";
    let expected = Ok(ExecutionValue::Int(38));
    assert_eq!(run_baseline(source, "Run"), expected);
    assert_eq!(run(source, "Run"), expected);
}

#[test]
fn literal_float_edges_match_unoptimized_ieee_execution() {
    let source = r"
        public int Run() {
            double zero = 0.0d;
            double negativeZero = -zero;
            double negativeInfinity = 1.0d / negativeZero;
            double positiveInfinity = -negativeInfinity;
            double nan = positiveInfinity + negativeInfinity;
            if (negativeInfinity < zero && negativeZero == zero && nan != nan) {
                return 42;
            }
            return 0;
        }
    ";
    let expected = Ok(ExecutionValue::Int(42));
    assert_eq!(run_baseline(source, "Run"), expected);
    assert_eq!(run(source, "Run"), expected);
}

#[test]
fn dead_remainder_interface_and_collection_operations_still_fail() {
    let remainder = "public int Run() { int zero = 0; int unused = 10 % zero; return 1; }";
    for result in [run_baseline(remainder, "Run"), run(remainder, "Run")] {
        let error = result.expect_err("dead remainder must retain its controlled failure");
        assert!(error.contains("remainder by zero"), "{error}");
    }

    let interface = r"
        public interface IFailure { int Fail(); }
        public class Failure : IFailure {
            public Failure() { }
            public int Fail() { int zero = 0; return 1 / zero; }
        }
        public int Run() {
            IFailure failure = new Failure();
            int unused = failure.Fail();
            return 1;
        }
    ";
    for result in [run_baseline(interface, "Run"), run(interface, "Run")] {
        let error = result.expect_err("dead interface call must still execute");
        assert!(error.contains("division by zero"), "{error}");
    }

    let list = "public int Run() { List<int> values = new List<int>(); int unused = values.Get(0); return 1; }";
    for result in [run_baseline(list, "Run"), run(list, "Run")] {
        let error = result.expect_err("dead List.Get must retain bounds failure");
        assert!(error.contains("out of bounds"), "{error}");
    }
}
