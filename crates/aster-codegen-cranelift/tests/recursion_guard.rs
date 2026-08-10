use aster_codegen_cranelift::{ExecutionValue, execute};
use aster_compiler::compile;
use aster_runtime::ASTER_CALL_DEPTH_LIMIT;

fn run(source: &str, entry: &str) -> Result<ExecutionValue, String> {
    let compilation = compile(source).map_err(|diagnostics| format!("{diagnostics:#?}"))?;
    execute(&compilation.mir, entry).map_err(|error| error.to_string())
}

fn assert_controlled_depth_error(error: &str) {
    assert!(error.contains("Aster runtime error:"), "{error}");
    assert!(
        error.contains("call depth exceeds the supported limit"),
        "{error}"
    );
    for forbidden in [
        "overflowed its stack",
        "panicked",
        "SIGABRT",
        "thread 'main'",
    ] {
        assert!(
            !error.contains(forbidden),
            "unexpected host failure in: {error}"
        );
    }
}

#[test]
fn moderate_direct_and_mutual_recursion_execute_normally() {
    let source = r"
        public int Direct(int remaining) {
            return remaining == 0 ? 0 : Direct(remaining - 1) + 1;
        }
        public int Even(int remaining) {
            return remaining == 0 ? 0 : Odd(remaining - 1) + 1;
        }
        public int Odd(int remaining) {
            return remaining == 0 ? 0 : Even(remaining - 1) + 1;
        }
        public int Main() { return Direct(128) + Even(128); }
    ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(256)));
}

#[test]
fn direct_and_indirect_recursion_beyond_the_limit_fail_closed() {
    let direct = r"
        public int Recurse(int remaining) {
            return remaining == 0 ? 0 : Recurse(remaining - 1) + 1;
        }
        public int Main() { return Recurse(2000); }
    ";
    assert_controlled_depth_error(&run(direct, "Main").expect_err("depth must be bounded"));

    let indirect = r"
        public int First(int remaining) {
            return remaining == 0 ? 0 : Second(remaining - 1) + 1;
        }
        public int Second(int remaining) {
            return remaining == 0 ? 0 : First(remaining - 1) + 1;
        }
        public int Main() { return First(2000); }
    ";
    assert_controlled_depth_error(&run(indirect, "Main").expect_err("depth must be bounded"));

    let interface_indirect = r"
        public interface ILoop { int Recurse(int remaining); }
        public class Loop : ILoop {
            public Loop() {}
            public int Recurse(int remaining) {
                ILoop next = this;
                return remaining == 0 ? 0 : next.Recurse(remaining - 1) + 1;
            }
        }
        public int Main() { return new Loop().Recurse(2000); }
    ";
    assert_controlled_depth_error(
        &run(interface_indirect, "Main").expect_err("interface recursion must be bounded"),
    );
}

#[test]
fn direct_recursion_pins_limit_minus_one_limit_and_limit_plus_one() {
    for active_calls in [ASTER_CALL_DEPTH_LIMIT - 1, ASTER_CALL_DEPTH_LIMIT] {
        let remaining = active_calls - 1;
        let source = format!(
            "public int Recurse(int remaining) {{
                 return remaining == 0 ? 1 : Recurse(remaining - 1) + 1;
             }}
             public int Main() {{ return Recurse({remaining}); }}"
        );
        assert_eq!(
            run(&source, "Main"),
            Ok(ExecutionValue::Int(
                i32::try_from(active_calls).expect("call limit fits int")
            )),
            "{active_calls} active ASTER calls must be accepted"
        );
    }

    let remaining = ASTER_CALL_DEPTH_LIMIT;
    let source = format!(
        "public int Recurse(int remaining) {{
             return remaining == 0 ? 1 : Recurse(remaining - 1) + 1;
         }}
         public int Main() {{ return Recurse({remaining}); }}"
    );
    assert_controlled_depth_error(
        &run(&source, "Main").expect_err("limit plus one must be rejected"),
    );
}

#[test]
fn failed_reference_return_is_not_observed_by_callers() {
    let source = r"
        public class Value {
            public int Number;
            public Value(int number) { Number = number; }
        }
        public Value Recurse(int remaining) {
            return remaining == 0 ? new Value(42) : Recurse(remaining - 1);
        }
        public int Main() { return Recurse(2000).Number; }
    ";
    assert_controlled_depth_error(
        &run(source, "Main").expect_err("a failed reference return must unwind safely"),
    );
}

#[test]
fn task_run_uses_the_same_controlled_recursion_limit() {
    let source = r"
        public int Recurse(int remaining) {
            return remaining == 0 ? 0 : Recurse(remaining - 1) + 1;
        }
        public int Worker() { return Recurse(2000); }
        public int Main() { return Task.Run(Worker).Wait(); }
    ";
    assert_controlled_depth_error(&run(source, "Main").expect_err("worker depth must be bounded"));
}

#[test]
fn task_run_uses_the_same_exact_boundary() {
    let accepted_remaining = ASTER_CALL_DEPTH_LIMIT - 1;
    let accepted = format!(
        "public int Recurse(int remaining) {{
             return remaining == 0 ? 1 : Recurse(remaining - 1) + 1;
         }}
         public int Worker() {{ return Recurse({accepted_remaining}); }}
         public int Main() {{ return Task.Run(Worker).Wait(); }}"
    );
    assert_eq!(
        run(&accepted, "Main"),
        Ok(ExecutionValue::Int(
            i32::try_from(ASTER_CALL_DEPTH_LIMIT).expect("call limit fits int")
        ))
    );

    let rejected_remaining = ASTER_CALL_DEPTH_LIMIT;
    let rejected = format!(
        "public int Recurse(int remaining) {{
             return remaining == 0 ? 1 : Recurse(remaining - 1) + 1;
         }}
         public int Worker() {{ return Recurse({rejected_remaining}); }}
         public int Main() {{ return Task.Run(Worker).Wait(); }}"
    );
    assert_controlled_depth_error(
        &run(&rejected, "Main").expect_err("worker limit plus one must be rejected"),
    );
}

#[test]
fn a_failed_execution_does_not_poison_a_later_context() {
    let source = r"
        public int Recurse(int remaining) {
            return remaining == 0 ? 42 : Recurse(remaining - 1);
        }
        public int TooDeep() { return Recurse(2000); }
        public int Healthy() { return Recurse(10); }
    ";
    assert_controlled_depth_error(&run(source, "TooDeep").expect_err("depth must be bounded"));
    assert_eq!(run(source, "Healthy"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn recursion_and_allocation_failures_do_not_contaminate_fresh_contexts() {
    let safe_remaining = ASTER_CALL_DEPTH_LIMIT - 16;
    let source = format!(
        "public int Recurse(int remaining) {{
             int[] scratch = new int[1];
             int observed = scratch.Length;
             return remaining == 0 ? 42 : Recurse(remaining - 1);
         }}
         public int NearLimit() {{ return Recurse({safe_remaining}); }}
         public int TooDeep() {{ return Recurse({ASTER_CALL_DEPTH_LIMIT}); }}
         public int HealthyAllocation() {{
             int[] values = new int[4];
             return values.Length + 38;
         }}"
    );

    assert_eq!(run(&source, "NearLimit"), Ok(ExecutionValue::Int(42)));
    assert_controlled_depth_error(
        &run(&source, "TooDeep").expect_err("over-depth allocation recursion must fail"),
    );
    assert_eq!(
        run(&source, "HealthyAllocation"),
        Ok(ExecutionValue::Int(42))
    );
}
