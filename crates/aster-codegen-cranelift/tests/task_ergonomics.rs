//! Vertical execution coverage for parameterized Task.Run, caller-side
//! `WaitAll`, and cooperative cancellation.

use std::fmt::Write as _;

use aster_codegen_cranelift::{ExecutionValue, execute};
use aster_compiler::compile;

fn run(source: &str) -> Result<ExecutionValue, String> {
    let compilation = compile(source).map_err(|diagnostics| format!("{diagnostics:#?}"))?;
    execute(&compilation.mir, "Main").map_err(|error| error.to_string())
}

fn compile_errors(source: &str) -> Vec<String> {
    compile(source).map_or_else(
        |diagnostics| {
            diagnostics
                .into_iter()
                .map(|diagnostic| diagnostic.message)
                .collect()
        },
        |_| Vec::new(),
    )
}

#[test]
fn task_arguments_are_evaluated_once_left_to_right() {
    let source = r"
        public class Counter {
            private int value;
            public Counter() { value = 0; }
            public int Next() { value = value + 1; return value; }
        }
        public int Join(int left, int right) { return left * 10 + right; }
        public int Main() {
            Counter counter = new Counter();
            Task<int> task = Task.Run(Join, counter.Next(), counter.Next());
            return task.Wait() * 10 + counter.Next();
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(123)));
}

#[test]
fn transferable_structs_and_enums_cross_by_value() {
    let source = r"
        public struct Inner { public int value; }
        public struct Pair { public Inner left; public long right; }
        public enum Choice { Value(int value), Empty }
        public int Read(Pair pair, Choice choice) {
            switch (choice) {
                case Value(value): return pair.left.value + (int)pair.right + value;
                case Empty: return pair.left.value + (int)pair.right;
            }
        }
        public int Main() {
            Pair pair = Pair { left: Inner { value: 10 }, right: 20L };
            Choice choice = Choice.Value(12);
            Task<int> task = Task.Run(Read, pair, choice);
            pair.left.value = 99;
            return task.Wait();
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(42)));
}

#[test]
fn every_scalar_width_uses_the_target_signature_abi() {
    let source = r"
        public int Mix(
            sbyte a, byte b, short c, ushort d, int e, uint f,
            long g, ulong h, float i, double j, bool k, char l
        ) {
            return (int)a + (int)b + (int)c + (int)d + e + (int)f
                + (int)g + (int)h + (int)i + (int)j + (k ? 1 : 0) + (int)l;
        }
        public int Main() {
            return Task.Run(
                Mix,
                (sbyte)-1, (byte)2, (short)3, (ushort)4, 5, 6u,
                7L, 8UL, 9.5f, 10.5d, true, 'A'
            ).Wait();
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(119)));
}

#[test]
fn reference_bearing_task_arguments_fail_before_mir() {
    let errors = compile_errors(
        r#"
        public struct Hidden { public string text; }
        public int Read(Hidden value) { return value.text.Length; }
        public int Main() {
            Hidden value = Hidden { text: "x" };
            return Task.Run(Read, value).Wait();
        }
        "#,
    );
    assert!(
        errors
            .iter()
            .any(|message| message.contains("cannot cross a worker boundary")),
        "unexpected diagnostics: {errors:?}"
    );
}

#[test]
fn wait_all_preserves_input_order_and_duplicate_handles() {
    let source = r"
        public int Echo(int value) { return value; }
        public int Main() {
            Task<int> first = Task.Run(Echo, 10);
            Task<int> second = Task.Run(Echo, 20);
            Task<int>[] tasks = [second, first, second];
            int[] values = Task.WaitAll(tasks);
            return values[0] + values[1] + values[2];
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(50)));
}

#[test]
fn wait_all_accepts_an_empty_array() {
    let source = r"
        public int Main() {
            Task<int>[] tasks = new Task<int>[0];
            int[] values = Task.WaitAll(tasks);
            return values.Length;
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(0)));
}

#[test]
fn wait_all_covers_one_two_eight_and_sixty_four_tasks() {
    for count in [1_usize, 2, 8, 64] {
        let mut source =
            String::from("public int Keep(int value) { return value; } public int Main() { ");
        for index in 0..count {
            write!(source, "Task<int> task{index} = Task.Run(Keep, {index}); ")
                .expect("writing to a String cannot fail");
        }
        source.push_str("int[] values = Task.WaitAll([");
        for index in 0..count {
            if index != 0 {
                source.push_str(", ");
            }
            write!(source, "task{index}").expect("writing to a String cannot fail");
        }
        source.push_str("]); int total = 0; for (int i = 0; i < values.Length; i++) { total += values[i]; } return total; }");
        let expected = i32::try_from(count * (count - 1) / 2).expect("small checksum");
        assert_eq!(
            run(&source),
            Ok(ExecutionValue::Int(expected)),
            "task count {count}"
        );
    }
}

#[test]
fn wait_all_reconstructs_every_transferable_scalar_result() {
    let cases = [
        ("bool", "true", ExecutionValue::Bool(true)),
        ("char", "'A'", ExecutionValue::Char('A')),
        ("sbyte", "(sbyte)-7", ExecutionValue::SByte(-7)),
        ("byte", "(byte)7", ExecutionValue::Byte(7)),
        ("short", "(short)-70", ExecutionValue::Short(-70)),
        ("ushort", "(ushort)70", ExecutionValue::UShort(70)),
        ("int", "-700", ExecutionValue::Int(-700)),
        ("uint", "700u", ExecutionValue::UInt(700)),
        ("long", "-7000L", ExecutionValue::Long(-7000)),
        ("ulong", "7000UL", ExecutionValue::ULong(7000)),
        ("float", "7.5f", ExecutionValue::Float(7.5)),
        ("double", "7.5d", ExecutionValue::Double(7.5)),
    ];
    for (type_, literal, expected) in cases {
        let source = format!(
            "public {type_} Keep({type_} value) {{ return value; }} \
             public {type_} Main() {{ Task<{type_}> task = Task.Run(Keep, {literal}); return Task.WaitAll([task])[0]; }}"
        );
        assert_eq!(run(&source), Ok(expected), "scalar type {type_}");
    }
}

#[test]
fn wait_all_selects_the_lowest_input_failure_not_completion_order() {
    let source = r"
        public int FailAt(int index) {
            int[] values = new int[1];
            return values[index];
        }
        public int Main() {
            Task<int> later = Task.Run(FailAt, 7);
            Task<int> earlier = Task.Run(FailAt, 3);
            Task<int>[] tasks = [earlier, later];
            int[] values = Task.WaitAll(tasks);
            return values.Length;
        }
    ";
    let error = run(source).expect_err("the composed task group must fail");
    assert!(error.contains("array index 3"), "unexpected error: {error}");
}

#[test]
fn wait_all_failure_selection_is_stable_when_workers_finish_in_reverse_order() {
    let source = r"
        public int FailAfter(int index, int delay) {
            int value = 0;
            while (value < delay) { value += 1; }
            int[] values = new int[1];
            return values[index];
        }
        public int Main() {
            Task<int> slowFirst = Task.Run(FailAfter, 3, 200000);
            Task<int> fastSecond = Task.Run(FailAfter, 7, 0);
            return Task.WaitAll([slowFirst, fastSecond])[0];
        }
    ";
    let compilation = compile(source).expect("reverse-completion source compiles");
    for _ in 0..24 {
        let error = execute(&compilation.mir, "Main")
            .expect_err("the lowest input failure must be reported")
            .to_string();
        assert!(error.contains("array index 3"), "unexpected error: {error}");
    }
}

#[test]
fn duplicate_failed_and_cancelled_handles_remain_safe_and_deterministic() {
    let failed = r"
        public int Fail() { int[] values = new int[1]; return values[4]; }
        public int Value() { return 1; }
        public int Main() {
            Task<int> task = Task.Run(Fail);
            return Task.WaitAll([task, Task.Run(Value), task])[0];
        }
    ";
    let error = run(failed).expect_err("duplicate failed handle must fail once, safely");
    assert!(error.contains("array index 4"), "unexpected error: {error}");

    let cancelled = r"
        public int Work(int limit) {
            int value = 0;
            while (value < limit) {
                if (Task.IsCancellationRequested()) { return value; }
                value += 1;
            }
            return value;
        }
        public int Main() {
            Task<int> task = Task.Run(Work, 100000000);
            if (!task.Cancel()) { return -1; }
            return Task.WaitAll([task, task])[0];
        }
    ";
    let error = run(cancelled).expect_err("duplicate cancelled handle must stay cancelled");
    assert!(
        error.contains("task was cancelled"),
        "unexpected error: {error}"
    );
}

#[test]
fn wait_all_reports_a_real_failure_before_cancellation() {
    let source = r"
        public int Work(int limit) {
            int value = 0;
            while (value < limit) {
                if (Task.IsCancellationRequested()) { return value; }
                value = value + 1;
            }
            return value;
        }
        public int Fail() {
            int[] values = new int[1];
            return values[4];
        }
        public int Main() {
            Task<int> cancelled = Task.Run(Work, 100000000);
            if (!cancelled.Cancel()) { return -1; }
            Task<int> failed = Task.Run(Fail);
            int[] values = Task.WaitAll([cancelled, failed]);
            return values.Length;
        }
    ";
    let error = run(source).expect_err("a real task failure must win over cancellation");
    assert!(error.contains("array index 4"), "unexpected error: {error}");
}

#[test]
fn cancellation_query_is_false_outside_a_task() {
    assert_eq!(
        run("public int Main() { return Task.IsCancellationRequested() ? 1 : 0; }"),
        Ok(ExecutionValue::Int(0))
    );
}

#[test]
fn cancellation_state_is_isolated_between_tasks() {
    let source = r"
        public bool Requested() { return Task.IsCancellationRequested(); }
        public int Cancellable(int limit) {
            int value = 0;
            while (value < limit) {
                if (Requested()) { return value; }
                value += 1;
            }
            return value;
        }
        public int Probe() { return Requested() ? 1 : 0; }
        public int Main() {
            Task<int> cancelled = Task.Run(Cancellable, 100000000);
            if (!cancelled.Cancel()) { return -1; }
            return Task.Run(Probe).Wait();
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(0)));
}

#[test]
fn cancel_after_completion_does_not_change_the_result() {
    let source = r"
        public int Value() { return 42; }
        public int Main() {
            Task<int> task = Task.Run(Value);
            int value = task.Wait();
            bool accepted = task.Cancel();
            return accepted ? 0 : value;
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(42)));
}

#[test]
fn accepted_cancellation_is_observed_as_a_controlled_wait_failure() {
    let source = r"
        public int Work(int limit) {
            int value = 0;
            while (value < limit) {
                if (Task.IsCancellationRequested()) { return value; }
                value = value + 1;
            }
            return value;
        }
        public int Main() {
            Task<int> task = Task.Run(Work, 100000000);
            if (!task.Cancel()) { return task.Wait(); }
            return task.Wait();
        }
    ";
    let error = run(source).expect_err("accepted cancellation must be terminal");
    assert!(
        error.contains("task was cancelled"),
        "unexpected error: {error}"
    );
}

#[test]
fn awaited_parameterized_task_uses_the_same_transfer_frame() {
    let source = r"
        public int Add(int left, int right) { return left + right; }
        public async Task<int> Later() {
            int value = await Task.Run(Add, 20, 22);
            return value;
        }
        public int Main() { return Later().Wait(); }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(42)));
}

#[test]
fn static_and_inferred_generic_targets_have_concrete_worker_identities() {
    let source = r"
        public T Identity<T>(T value) { return value; }
        public class Tools {
            public static T Keep<T>(T value) { return value; }
        }
        public int Main() {
            Task<int> generic = Task.Run(Identity, 20);
            Task<int> staticMethod = Task.Run(Tools.Keep, 22);
            return generic.Wait() + staticMethod.Wait();
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(42)));
}

#[test]
fn generic_specialization_cannot_hide_a_reference_bearing_field() {
    let errors = compile_errors(
        r#"
        public struct Payload<T> { public T value; }
        public int Read<T>(Payload<T> value) { return 1; }
        public int Main() {
            Payload<string> value = Payload<string> { value: "hidden" };
            return Task.Run(Read, value).Wait();
        }
        "#,
    );
    assert!(
        errors
            .iter()
            .any(|message| message.contains("cannot cross a worker boundary")),
        "unexpected diagnostics: {errors:?}"
    );
}

#[test]
fn a_task_remains_waitable_after_wait_all() {
    let source = r"
        public int Value(int value) { return value; }
        public int Main() {
            Task<int> task = Task.Run(Value, 21);
            int[] group = Task.WaitAll([task, task]);
            return group[0] + task.Wait();
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(42)));
}

#[test]
fn cancellation_of_an_async_task_before_its_first_pump_is_deterministic() {
    let source = r"
        public int Value() { return 42; }
        public async Task<int> Later() { return await Task.Run(Value); }
        public int Main() {
            Task<int> task = Later();
            if (!task.Cancel()) { return task.Wait(); }
            return task.Wait();
        }
    ";
    let error = run(source).expect_err("the pending async task must be cancelled");
    assert!(
        error.contains("task was cancelled"),
        "unexpected error: {error}"
    );
}

#[test]
fn an_async_resume_uses_the_outer_tasks_control_not_stale_worker_state() {
    let source = r"
        public int Value() { return 42; }
        public int Work(int limit) {
            int value = 0;
            while (value < limit) {
                if (Task.IsCancellationRequested()) { return value; }
                value += 1;
            }
            return value;
        }
        public bool Requested() { return Task.IsCancellationRequested(); }
        public async Task<int> Later() {
            int value = await Task.Run(Value);
            return Requested() ? -1 : value;
        }
        public int Main() {
            Task<int> cancelled = Task.Run(Work, 100000000);
            if (!cancelled.Cancel()) { return -2; }
            return Later().Wait();
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(42)));
}

#[test]
fn nested_generic_reference_shapes_are_rejected_before_hir() {
    let hidden_types = [
        ("string", ""),
        ("int[]", ""),
        ("List<int>", ""),
        ("Dictionary<int, int>", ""),
        ("Box", "public class Box { public Box() {} }"),
        (
            "IBox",
            "public interface IBox { public int Value(); } public class Box : IBox { public Box() {} public int Value() { return 1; } }",
        ),
        ("Task<int>", ""),
    ];
    for (hidden, declarations) in hidden_types {
        let source = format!(
            "{declarations} \
             public struct Inner<T> {{ public T value; }} \
             public struct Outer<T> {{ public Inner<T> inner; }} \
             public int Read<T>(Outer<T> value) {{ return 1; }} \
             public int Main(Outer<{hidden}> input) {{ return Task.Run(Read, input).Wait(); }}"
        );
        let errors = compile_errors(&source);
        assert!(
            errors
                .iter()
                .any(|message| message.contains("cannot cross a worker boundary")),
            "hidden type {hidden} unexpectedly crossed: {errors:?}"
        );
    }

    let enum_errors = compile_errors(
        r"
        public enum Hidden<T> { Value(T value), Empty }
        public int Read<T>(Hidden<T> value) { return 1; }
        public int Main(Hidden<string> input) { return Task.Run(Read, input).Wait(); }
        ",
    );
    assert!(
        enum_errors
            .iter()
            .any(|message| message.contains("cannot cross a worker boundary")),
        "reference-bearing enum unexpectedly crossed: {enum_errors:?}"
    );
}

#[test]
fn overloads_and_generic_specializations_keep_distinct_task_targets() {
    let source = r"
        public int Pick(int value) { return value + 1; }
        public long Pick(long value) { return value + 2L; }
        public T Keep<T>(T value) { return value; }
        public int Main() {
            Task<int> intPick = Task.Run(Pick, 19);
            Task<long> longPick = Task.Run(Pick, 20L);
            Task<int> genericInt = Task.Run(Keep, 1);
            Task<long> genericLong = Task.Run(Keep, 1L);
            return intPick.Wait() + (int)longPick.Wait()
                + genericInt.Wait() + (int)genericLong.Wait() - 2;
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(42)));
}

#[test]
fn zero_argument_task_run_keeps_the_no_payload_mir_shape() {
    let compilation = compile(
        "public int Compute() { return 42; } public int Main() { return Task.Run(Compute).Wait(); }",
    )
    .expect("parameterless Task.Run compiles");
    let arguments = compilation
        .mir
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .find_map(|instruction| match instruction {
            aster_mir::Instruction::CallIntrinsic {
                intrinsic: aster_mir::Intrinsic::TaskRun,
                arguments,
                ..
            } => Some(arguments),
            _ => None,
        })
        .expect("TaskRun reaches MIR");
    assert_eq!(
        arguments.len(),
        1,
        "only the callable crosses the no-arg MIR path"
    );
}

#[test]
fn parameterized_task_run_does_not_bypass_nested_concurrency_rules() {
    let errors = compile_errors(
        r"
        public int Inner(int value) { return value; }
        public int Outer(int value) { return Task.Run(Inner, value).Wait(); }
        public int Main() { return Task.Run(Outer, 42).Wait(); }
        ",
    );
    assert!(
        errors
            .iter()
            .any(|message| message.contains("nested") || message.contains("Task.Run")),
        "unexpected diagnostics: {errors:?}"
    );
}

#[test]
fn parameterized_pool_wait_all_and_cancellation_stress_stay_deterministic() {
    let parameterized = r"
        public int Keep(int value) { return value; }
        public int Main() {
            int total = 0;
            int i = 0;
            while (i < 10000) {
                total += Task.Run(Keep, i).Wait();
                i += 1;
            }
            return total;
        }
    ";
    assert_eq!(run(parameterized), Ok(ExecutionValue::Int(49_995_000)));

    let groups = r"
        public int Keep(int value) { return value; }
        public int Main() {
            int total = 0;
            int group = 0;
            while (group < 100) {
                Task<int> a = Task.Run(Keep, 0);
                Task<int> b = Task.Run(Keep, 1);
                Task<int> c = Task.Run(Keep, 2);
                Task<int> d = Task.Run(Keep, 3);
                Task<int> e = Task.Run(Keep, 4);
                Task<int> f = Task.Run(Keep, 5);
                Task<int> g = Task.Run(Keep, 6);
                Task<int> h = Task.Run(Keep, 7);
                int[] values = Task.WaitAll([a, b, c, d, e, f, g, h]);
                for (int i = 0; i < values.Length; i++) { total += values[i]; }
                group += 1;
            }
            return total;
        }
    ";
    assert_eq!(run(groups), Ok(ExecutionValue::Int(2_800)));

    let cancellations = r"
        public int Work(int limit) {
            int value = 0;
            while (value < limit) {
                if (Task.IsCancellationRequested()) { return value; }
                value += 1;
            }
            return value;
        }
        public int Main() {
            int accepted = 0;
            int i = 0;
            while (i < 500) {
                Task<int> task = Task.Run(Work, 100000000);
                if (task.Cancel()) { accepted += 1; }
                i += 1;
            }
            return accepted;
        }
    ";
    assert_eq!(run(cancellations), Ok(ExecutionValue::Int(500)));
}
