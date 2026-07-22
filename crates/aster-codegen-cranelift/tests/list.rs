//! End-to-end tests for `List<T>`: the public constructor `new List<T>()`,
//! the read-only `.Length` property (List B1), `values.Add(value)` with
//! geometric buffer growth (List B2A), and `values.Get(index)` with
//! value-copy/identity-preserving semantics (List B2B), through the full
//! pipeline (parser, semantic analysis, HIR, MIR, escape analysis, codegen,
//! JIT execution). `Set`/`RemoveAt`/the indexer do not exist yet.

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

// --- List B2B: `values.Get(index)` ---------------------------------------

#[test]
fn get_returns_first_middle_and_last_elements() {
    let source = "
        public int Main()
        {
            List<int> values = new List<int>();
            values.Add(10);
            values.Add(20);
            values.Add(30);
            return values.Get(0) * 100 + values.Get(1) * 10 + values.Get(2);
        }
        ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(1230)));
}

#[test]
fn get_works_after_several_growths() {
    let mut source =
        String::from("public int Main() { List<int> values = new List<int>(); int total = 0;");
    for i in 0..20 {
        let _ = write!(source, "values.Add({i});");
    }
    for i in 0..20 {
        let _ = write!(source, "total = total + values.Get({i});");
    }
    source.push_str("return total; }");
    assert_eq!(run(&source, "Main"), Ok(ExecutionValue::Int((0..20).sum())));
}

#[test]
fn repeated_get_calls_are_consistent() {
    let source = "
        public int Main()
        {
            List<int> values = new List<int>();
            values.Add(42);
            int a = values.Get(0);
            int b = values.Get(0);
            int c = values.Get(0);
            return a + b + c;
        }
        ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(126)));
}

#[test]
fn two_independent_lists_read_independently() {
    let source = "
        public int Main()
        {
            List<int> a = new List<int>();
            List<int> b = new List<int>();
            a.Add(1);
            b.Add(2);
            b.Add(3);
            return a.Get(0) * 100 + b.Get(1);
        }
        ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(103)));
}

#[test]
fn an_alias_reads_the_same_content() {
    let source = "
        public int Main()
        {
            List<int> a = new List<int>();
            a.Add(7);
            List<int> b = a;
            return b.Get(0);
        }
        ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(7)));
}

#[test]
fn get_does_not_change_length() {
    let source = "
        public int Main()
        {
            List<int> values = new List<int>();
            values.Add(1);
            values.Add(2);
            values.Get(0);
            values.Get(1);
            return values.Length;
        }
        ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(2)));
}

#[test]
fn get_and_add_work_for_every_required_element_type() {
    for (source, expected) in [
        (
            "public int Main() { List<int> v = new List<int>(); v.Add(11); return v.Get(0); }",
            ExecutionValue::Int(11),
        ),
        (
            "public long Main() { List<long> v = new List<long>(); v.Add(11L); return v.Get(0); }",
            ExecutionValue::Long(11),
        ),
        (
            "public uint Main() { List<uint> v = new List<uint>(); v.Add(11U); return v.Get(0); }",
            ExecutionValue::UInt(11),
        ),
        (
            "public ulong Main() { List<ulong> v = new List<ulong>(); v.Add(11UL); return v.Get(0); }",
            ExecutionValue::ULong(11),
        ),
        (
            "public float Main() { List<float> v = new List<float>(); v.Add(1.5f); return v.Get(0); }",
            ExecutionValue::float(1.5),
        ),
        (
            "public double Main() { List<double> v = new List<double>(); v.Add(1.5d); return v.Get(0); }",
            ExecutionValue::double(1.5),
        ),
        (
            "public bool Main() { List<bool> v = new List<bool>(); v.Add(true); return v.Get(0); }",
            ExecutionValue::Bool(true),
        ),
        (
            "public char Main() { List<char> v = new List<char>(); v.Add('x'); return v.Get(0); }",
            ExecutionValue::Char('x'),
        ),
        (
            "public string Main() { List<string> v = new List<string>(); v.Add(\"hi\"); return v.Get(0); }",
            ExecutionValue::String("hi".to_owned()),
        ),
    ] {
        assert_eq!(run(source, "Main"), Ok(expected), "`{source}`");
    }
}

#[test]
fn get_returns_the_same_class_identity_as_add() {
    // Mutating the field through the value `Get` returned must be observed
    // through the original reference too — proof of shared identity, since
    // a copy would leave the original untouched.
    let source = "
        public class Box { public int value; public Box(int value) { this.value = value; } }
        public int Main()
        {
            Box box = new Box(1);
            List<Box> values = new List<Box>();
            values.Add(box);
            Box loaded = values.Get(0);
            loaded.value = 99;
            return box.value;
        }
        ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(99)));
}

#[test]
fn get_preserves_interface_dispatch() {
    let source = "
        public interface IShape { int Area(); }
        public class Square : IShape { public int side; public Square(int side) { this.side = side; } public int Area() { return side * side; } }
        public int Main()
        {
            IShape shape = new Square(4);
            List<IShape> values = new List<IShape>();
            values.Add(shape);
            IShape loaded = values.Get(0);
            return loaded.Area();
        }
        ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(16)));
}

#[test]
fn get_returns_the_same_array_identity_as_add() {
    let source = "
        public int Main()
        {
            int[] array = [1, 2, 3];
            List<int[]> values = new List<int[]>();
            values.Add(array);
            int[] loaded = values.Get(0);
            loaded[0] = 99;
            return array[0];
        }
        ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(99)));
}

#[test]
fn get_returns_the_same_nested_list_identity_as_add() {
    let source = "
        public int Main()
        {
            List<int> inner = new List<int>();
            inner.Add(5);
            List<List<int>> outer = new List<List<int>>();
            outer.Add(inner);
            List<int> loaded = outer.Get(0);
            loaded.Add(6);
            return inner.Length;
        }
        ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(2)));
}

#[test]
fn get_produces_a_struct_copy_not_a_reference_to_the_slot() {
    let source = "
        public struct Point { public int x; public bool flag; public long y; }
        public int Main()
        {
            List<Point> values = new List<Point>();
            values.Add(Point { x: 1, flag: true, y: 2L });
            Point copy = values.Get(0);
            copy.x = 99;
            Point again = values.Get(0);
            return again.x;
        }
        ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(1)));
}

#[test]
fn get_preserves_a_reference_field_inside_a_struct() {
    let source = "
        public class Box { public int value; public Box(int value) { this.value = value; } }
        public struct Wrapper { public Box inner; }
        public int Main()
        {
            Box box = new Box(10);
            List<Wrapper> values = new List<Wrapper>();
            values.Add(Wrapper { inner: box });
            Wrapper loaded = values.Get(0);
            loaded.inner.value = 55;
            return box.value;
        }
        ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(55)));
}

#[test]
fn get_preserves_enum_tag_and_scalar_payload() {
    let source = "
        public enum Shape { Circle(int radius), Square }
        public int Main()
        {
            List<Shape> values = new List<Shape>();
            values.Add(Shape.Circle(7));
            values.Add(Shape.Square);
            Shape first = values.Get(0);
            switch (first) {
                case Circle(r): return r;
                case Square: return -1;
            }
        }
        ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(7)));
}

#[test]
fn get_preserves_a_reference_payload_inside_an_enum() {
    let source = "
        public class Box { public int value; public Box(int value) { this.value = value; } }
        public enum MaybeBox { None, Some(Box inner) }
        public int Main()
        {
            Box box = new Box(3);
            List<MaybeBox> values = new List<MaybeBox>();
            values.Add(MaybeBox.Some(box));
            MaybeBox loaded = values.Get(0);
            switch (loaded) {
                case Some(b): return b.value;
                case None: return -1;
            }
        }
        ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(3)));
}

#[test]
fn get_works_for_a_generic_specialization() {
    let source = "
        public class Box<T> { private T value; public Box(T value) { this.value = value; } public T Get() { return value; } }
        public int Main()
        {
            List<Box<int>> values = new List<Box<int>>();
            values.Add(new Box<int>(9));
            Box<int> loaded = values.Get(0);
            return loaded.Get();
        }
        ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(9)));
}

#[test]
fn a_helper_returning_get_of_a_parameter_keeps_the_reference_valid() {
    let source = "
        public class Box { public int value; public Box(int value) { this.value = value; } }
        public Box Read(List<Box> values) { return values.Get(0); }
        public int Main()
        {
            List<Box> values = new List<Box>();
            values.Add(new Box(21));
            Box loaded = Read(values);
            return loaded.value;
        }
        ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(21)));
}

#[test]
fn get_and_add_still_reject_decimal() {
    let errors = compile_errors(
        "public int Main() { List<decimal> values = new List<decimal>(); values.Add(1.5m); return 0; }",
    );
    assert!(
        errors
            .iter()
            .any(|message| message.contains("`List<decimal>` cannot be used")),
        "expected `List<decimal>` to be rejected, got {errors:?}"
    );
}

#[test]
fn set_remove_at_and_the_indexer_remain_unavailable() {
    for (source, expected) in [
        (
            "public int Main() { List<int> values = new List<int>(); values.Add(1); values.Set(0, 2); return 0; }",
            "no member `Set`",
        ),
        (
            "public int Main() { List<int> values = new List<int>(); values.Add(1); values.RemoveAt(0); return 0; }",
            "no member `RemoveAt`",
        ),
        (
            "public int Main() { List<int> values = new List<int>(); values.Add(1); return values[0]; }",
            "cannot be indexed",
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
fn a_value_read_through_a_zero_arg_helper_still_cannot_cross_a_worker_boundary() {
    let errors = compile_errors(
        "public class Box { public int value; public Box(int value) { this.value = value; } } \
         public List<Box> MakeList() { List<Box> values = new List<Box>(); values.Add(new Box(1)); return values; } \
         public Box ReadFirst() { return MakeList().Get(0); } \
         public int Main() { Task<Box> task = Task.Run(ReadFirst); return 0; }",
    );
    assert!(
        errors
            .iter()
            .any(|message| message.contains("cross a worker boundary")),
        "expected the non-transferable-result diagnostic, got {errors:?}"
    );
}
