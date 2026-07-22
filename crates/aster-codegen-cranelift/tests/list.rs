//! End-to-end tests for `List<T>` (List B1): the public constructor
//! `new List<T>()` and the read-only `.Length` property, through the full
//! pipeline (parser, semantic analysis, HIR, MIR, escape analysis, codegen,
//! JIT execution). No other member (`Add`/`Get`/`Set`/`RemoveAt`) exists yet.

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
fn add_get_set_and_remove_at_remain_unavailable() {
    for (source, expected) in [
        (
            "public int Main() { List<int> values = new List<int>(); values.Add(1); return 0; }",
            "no member `Add`",
        ),
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
