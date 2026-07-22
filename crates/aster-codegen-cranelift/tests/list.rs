//! End-to-end tests for `List<T>`: the public constructor `new List<T>()`,
//! the read-only `.Length` property (List B1), and `values.Add(value)` with
//! geometric buffer growth (List B2A), through the full pipeline (parser,
//! semantic analysis, HIR, MIR, escape analysis, codegen, JIT execution).
//! `Get`/`Set`/`RemoveAt` do not exist yet.

use std::fmt::Write as _;

use aster_codegen_cranelift::{ExecutionValue, execute};
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
fn a_freshly_constructed_list_has_zero_length() {
    let source = "
        public int Main()
        {
            List<int> values = new List<int>();
            return values.Length;
        }
        ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(0)));
}

#[test]
fn construction_and_length_work_for_every_required_element_type() {
    for source in [
        "public int Main() { List<int> values = new List<int>(); return values.Length; }",
        "public int Main() { List<long> values = new List<long>(); return values.Length; }",
        "public int Main() { List<string> values = new List<string>(); return values.Length; }",
        "public class Widget { public Widget() {} } \
         public int Main() { List<Widget> values = new List<Widget>(); return values.Length; }",
        "public interface IJob { int Run(); } \
         public int Main() { List<IJob> values = new List<IJob>(); return values.Length; }",
        "public int Main() { List<int[]> values = new List<int[]>(); return values.Length; }",
        "public struct Point { public int x; public int y; } \
         public int Main() { List<Point> values = new List<Point>(); return values.Length; }",
        "public enum Color { Red, Green } \
         public int Main() { List<Color> values = new List<Color>(); return values.Length; }",
        "public class Box<T> { private T value; public Box(T value) { this.value = value; } public T Get() { return value; } } \
         public int Main() { List<Box<int>> values = new List<Box<int>>(); return values.Length; }",
        "public int Main() { List<List<int>> values = new List<List<int>>(); return values.Length; }",
    ] {
        assert_eq!(
            run(source, "Main"),
            Ok(ExecutionValue::Int(0)),
            "`{source}` did not construct a zero-length list"
        );
    }
}

#[test]
fn a_list_of_decimal_is_still_rejected() {
    let errors = compile_errors(
        "public int Main() { List<decimal> values = new List<decimal>(); return values.Length; }",
    );
    assert!(
        errors
            .iter()
            .any(|message| message.contains("`List<decimal>` cannot be used")),
        "expected `List<decimal>` to be rejected, got {errors:?}"
    );
}

#[test]
fn get_set_and_remove_at_remain_unavailable() {
    for (source, expected) in [
        (
            "public int Main() { List<int> values = new List<int>(); return values.Get(0); }",
            "no member `Get`",
        ),
        (
            "public int Main() { List<int> values = new List<int>(); values.Set(0, 1); return 0; }",
            "no member `Set`",
        ),
        (
            "public int Main() { List<int> values = new List<int>(); values.RemoveAt(0); return 0; }",
            "no member `RemoveAt`",
        ),
    ] {
        let errors = compile_errors(source);
        assert!(
            errors.iter().any(|message| message.contains(expected)),
            "expected `{expected}` in {errors:?}"
        );
    }
}

// --- List B2A: `values.Add(value)` --------------------------------------

#[test]
fn one_add_makes_length_one() {
    let source = "
        public int Main()
        {
            List<int> values = new List<int>();
            values.Add(10);
            return values.Length;
        }
        ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(1)));
}

#[test]
fn three_adds_make_length_three() {
    let source = "
        public int Main()
        {
            List<int> values = new List<int>();
            values.Add(10);
            values.Add(20);
            values.Add(30);
            return values.Length;
        }
        ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(3)));
}

#[test]
fn many_adds_grow_through_several_capacity_doublings() {
    // Forces growth through 0->4->8->16->32 with a single expression per
    // add (no loops in the source language available here), so this checks
    // the final `Length` after crossing every doubling boundary at least
    // once, matching the runtime-level growth-progression tests.
    let mut source = String::from("public int Main() { List<int> values = new List<int>();");
    for i in 0..20 {
        let _ = write!(source, "values.Add({i});");
    }
    source.push_str("return values.Length; }");
    assert_eq!(run(&source, "Main"), Ok(ExecutionValue::Int(20)));
}

#[test]
fn construction_and_add_work_for_every_required_element_type() {
    for source in [
        "public int Main() { List<int> v = new List<int>(); v.Add(1); return v.Length; }",
        "public int Main() { List<long> v = new List<long>(); v.Add(1L); return v.Length; }",
        "public int Main() { List<float> v = new List<float>(); v.Add(1.0f); return v.Length; }",
        "public int Main() { List<double> v = new List<double>(); v.Add(1.0d); return v.Length; }",
        "public int Main() { List<bool> v = new List<bool>(); v.Add(true); return v.Length; }",
        "public int Main() { List<string> v = new List<string>(); v.Add(\"hi\"); return v.Length; }",
        "public class Widget { public Widget() {} } \
         public int Main() { List<Widget> v = new List<Widget>(); v.Add(new Widget()); return v.Length; }",
        "public interface IJob { int Run(); } \
         public class Job : IJob { public Job() {} public int Run() { return 1; } } \
         public int Main() { List<IJob> v = new List<IJob>(); IJob job = new Job(); v.Add(job); return v.Length; }",
        "public int Main() { List<int[]> v = new List<int[]>(); v.Add([1, 2]); return v.Length; }",
        "public struct Point { public int x; public bool flag; public long y; } \
         public int Main() { List<Point> v = new List<Point>(); v.Add(Point { x: 1, flag: true, y: 2L }); return v.Length; }",
        "public enum Color { Red, Green } \
         public int Main() { List<Color> v = new List<Color>(); v.Add(Color.Red); return v.Length; }",
        "public enum Shape { Circle(int radius), Square } \
         public int Main() { List<Shape> v = new List<Shape>(); v.Add(Shape.Circle(5)); return v.Length; }",
        "public class Box<T> { private T value; public Box(T value) { this.value = value; } public T Get() { return value; } } \
         public int Main() { List<Box<int>> v = new List<Box<int>>(); v.Add(new Box<int>(1)); return v.Length; }",
        "public int Main() { List<List<int>> v = new List<List<int>>(); v.Add(new List<int>()); return v.Length; }",
    ] {
        assert_eq!(
            run(source, "Main"),
            Ok(ExecutionValue::Int(1)),
            "`{source}` did not add its element"
        );
    }
}

#[test]
fn a_list_of_decimal_add_is_still_rejected() {
    let errors = compile_errors(
        "public int Main() { List<decimal> values = new List<decimal>(); values.Add(1.5m); return values.Length; }",
    );
    assert!(
        errors
            .iter()
            .any(|message| message.contains("`List<decimal>` cannot be used")),
        "expected `List<decimal>` to be rejected, got {errors:?}"
    );
}

#[test]
fn two_independent_lists_do_not_share_state() {
    let source = "
        public int Main()
        {
            List<int> a = new List<int>();
            List<int> b = new List<int>();
            a.Add(1);
            a.Add(2);
            b.Add(10);
            return a.Length * 100 + b.Length;
        }
        ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(201)));
}

#[test]
fn an_alias_modifies_the_same_list() {
    let source = "
        public int Main()
        {
            List<int> a = new List<int>();
            List<int> b = a;
            a.Add(1);
            b.Add(2);
            return a.Length;
        }
        ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(2)));
}

#[test]
fn repeated_calls_do_not_contaminate_later_lists() {
    let source = "
        public List<int> MakeWithOne() { List<int> values = new List<int>(); values.Add(1); return values; }
        public int Main()
        {
            List<int> first = MakeWithOne();
            List<int> second = MakeWithOne();
            first.Add(2);
            return first.Length * 10 + second.Length;
        }
        ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(21)));
}

#[test]
fn a_temporary_local_list_grows_correctly() {
    let source = "
        public int Main()
        {
            List<int> values = new List<int>();
            values.Add(1);
            values.Add(2);
            values.Add(3);
            values.Add(4);
            values.Add(5);
            return values.Length;
        }
        ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(5)));
}

#[test]
fn a_persistent_returned_list_grows_correctly() {
    let source = "
        public List<int> Make()
        {
            List<int> values = new List<int>();
            values.Add(1);
            values.Add(2);
            values.Add(3);
            values.Add(4);
            values.Add(5);
            return values;
        }
        public int Main() { return Make().Length; }
        ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(5)));
}

#[test]
fn a_constructed_list_still_cannot_cross_a_worker_boundary_via_task_run() {
    // Now that `new List<T>()` exists, this exercises the same fail-closed
    // `is_worker_transferable` gate (see
    // `aster-codegen-cranelift/tests/task_run.rs::a_list_return_type_is_rejected`)
    // through the real constructor instead of a recursive-function workaround.
    let errors = compile_errors(
        "public List<int> Make() { return new List<int>(); } \
         public int Main() { Task<List<int>> task = Task.Run(Make); return 0; }",
    );
    assert!(
        errors
            .iter()
            .any(|message| message.contains("cross a worker boundary")),
        "expected the non-transferable-result diagnostic, got {errors:?}"
    );
}

#[test]
fn arrays_and_objects_are_unaffected_by_list_construction() {
    let source = "
        public class Box { public int value; public Box(int value) { this.value = value; } }
        public int Main()
        {
            int[] values = [1, 2, 3];
            Box box = new Box(values[0]);
            List<int> list = new List<int>();
            return values.Length + box.value + list.Length;
        }
        ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(4)));
}
