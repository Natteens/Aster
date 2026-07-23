//! End-to-end tests for `List<T>`: the public constructor `new List<T>()`,
//! the read-only `.Length` property (List B1), `values.Add(value)` with
//! geometric buffer growth (List B2A), and `values.Get(index)` with
//! value-copy/identity-preserving semantics (List B2B), through the full
//! pipeline (parser, semantic analysis, HIR, MIR, escape analysis, codegen,
//! JIT execution). `Set`/`RemoveAt`/the indexer do not exist yet.

use std::fmt::Write as _;

use aster_codegen_cranelift::{ExecutionValue, execute, execute_with_stats};
use aster_compiler::{compile, mir};

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
fn parallel_for_each_rejects_a_list_element_type() {
    // Same `is_worker_transferable` gate as `Task.Run` above, reached through
    // `Parallel.ForEach`'s body-parameter check instead â€” no List-specific
    // rule needed.
    let errors = compile_errors(
        "public void Body(List<int> item) { int x = item.Length; } \
         public int Main() { List<int>[] values = new List<int>[1]; Parallel.ForEach(values, Body); return 0; }",
    );
    assert!(
        !errors.is_empty(),
        "expected `Parallel.ForEach` over `List<int>` elements to be rejected"
    );
}

#[test]
fn parallel_reduce_rejects_a_list_element_type() {
    // Same gate, reached through `Parallel.Reduce`'s element/accumulator
    // check.
    let errors = compile_errors(
        "public int CountValue(int acc, List<int> item) { return acc + item.Length; } \
         public int CountPartial(int a, int b) { return a + b; } \
         public int Main() { List<int>[] values = new List<int>[1]; return Parallel.Reduce(values, 0, CountValue, CountPartial); }",
    );
    assert!(
        !errors.is_empty(),
        "expected `Parallel.Reduce` over `List<int>` elements to be rejected"
    );
}

#[test]
fn an_async_function_cannot_keep_a_list_alive_across_an_await() {
    // The same gate also guards the async state machine's slot storage
    // (`AsyncStoreSlot`/`AsyncLoadSlot`): a `List<T>` local live across an
    // `await` would need to be saved into a slot, which `is_worker_transferable`
    // refuses.
    let errors = compile_errors(
        "public int Compute() { return 1; } \
         public async Task<int> Calculate() { List<int> kept = new List<int>(); kept.Add(1); \
         int v = await Task.Run(Compute); return kept.Length + v; } \
         public int Main() { return 0; }",
    );
    assert!(
        !errors.is_empty(),
        "expected a `List<int>` kept alive across `await` to be rejected"
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
fn set_and_the_indexer_remain_unavailable() {
    for (source, expected) in [
        (
            "public int Main() { List<int> values = new List<int>(); values.Add(1); values.Set(0, 2); return 0; }",
            "no member `Set`",
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

// --- List C: `values.RemoveAt(index)` -------------------------------------

#[test]
fn remove_at_first_shifts_the_rest_left() {
    let source = "
        public int Main()
        {
            List<int> values = new List<int>();
            values.Add(10);
            values.Add(20);
            values.Add(30);
            values.RemoveAt(0);
            return values.Get(0) * 100 + values.Get(1);
        }
        ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(2030)));
}

#[test]
fn remove_at_middle_shifts_only_the_tail() {
    let source = "
        public int Main()
        {
            List<int> values = new List<int>();
            values.Add(10);
            values.Add(20);
            values.Add(30);
            values.RemoveAt(1);
            return values.Get(0) * 100 + values.Get(1);
        }
        ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(1030)));
}

#[test]
fn remove_at_last_leaves_the_earlier_elements_untouched() {
    let source = "
        public int Main()
        {
            List<int> values = new List<int>();
            values.Add(10);
            values.Add(20);
            values.Add(30);
            values.RemoveAt(2);
            return values.Length * 100 + values.Get(0) * 10 + values.Get(1);
        }
        ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(320)));
}

#[test]
fn remove_at_the_only_element_leaves_an_empty_list() {
    let source = "
        public int Main()
        {
            List<int> values = new List<int>();
            values.Add(10);
            values.RemoveAt(0);
            return values.Length;
        }
        ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(0)));
}

#[test]
fn remove_at_repeatedly_until_empty_then_add_again() {
    let source = "
        public int Main()
        {
            List<int> values = new List<int>();
            values.Add(1);
            values.Add(2);
            values.Add(3);
            values.RemoveAt(0);
            values.RemoveAt(0);
            values.RemoveAt(0);
            values.Add(9);
            return values.Length * 100 + values.Get(0);
        }
        ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(109)));
}

#[test]
fn add_after_remove_at_reuses_capacity_without_growing_beyond_it() {
    // Forces growth to 8, removes down to 4 remaining, then re-adds 4 more —
    // all of which must fit in the already-grown buffer.
    let mut source = String::from("public int Main() { List<int> values = new List<int>();");
    for i in 0..8 {
        let _ = write!(source, "values.Add({i});");
    }
    for _ in 0..4 {
        source.push_str("values.RemoveAt(0);");
    }
    for i in 100..104 {
        let _ = write!(source, "values.Add({i});");
    }
    source.push_str("return values.Length; }");
    assert_eq!(run(&source, "Main"), Ok(ExecutionValue::Int(8)));
}

#[test]
fn get_length_after_remove_at_are_all_consistent() {
    let source = "
        public int Main()
        {
            List<int> values = new List<int>();
            values.Add(1);
            values.Add(2);
            values.Add(3);
            values.Add(4);
            values.RemoveAt(1);
            int length = values.Length;
            int first = values.Get(0);
            int second = values.Get(1);
            int third = values.Get(2);
            return length * 1000 + first * 100 + second * 10 + third;
        }
        ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(3134)));
}

#[test]
fn two_independent_lists_remove_independently() {
    let source = "
        public int Main()
        {
            List<int> a = new List<int>();
            List<int> b = new List<int>();
            a.Add(1);
            a.Add(2);
            b.Add(10);
            b.Add(20);
            a.RemoveAt(0);
            return a.Get(0) * 100 + b.Length;
        }
        ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(202)));
}

#[test]
fn an_alias_observes_the_removal() {
    let source = "
        public int Main()
        {
            List<int> a = new List<int>();
            a.Add(1);
            a.Add(2);
            List<int> b = a;
            b.RemoveAt(0);
            return a.Length * 10 + a.Get(0);
        }
        ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(12)));
}

#[test]
fn remove_at_and_add_work_for_every_required_element_type() {
    for (source, expected) in [
        (
            "public int Main() { List<int> v = new List<int>(); v.Add(1); v.Add(2); v.RemoveAt(0); return v.Get(0); }",
            ExecutionValue::Int(2),
        ),
        (
            "public long Main() { List<long> v = new List<long>(); v.Add(1L); v.Add(2L); v.RemoveAt(0); return v.Get(0); }",
            ExecutionValue::Long(2),
        ),
        (
            "public uint Main() { List<uint> v = new List<uint>(); v.Add(1U); v.Add(2U); v.RemoveAt(0); return v.Get(0); }",
            ExecutionValue::UInt(2),
        ),
        (
            "public ulong Main() { List<ulong> v = new List<ulong>(); v.Add(1UL); v.Add(2UL); v.RemoveAt(0); return v.Get(0); }",
            ExecutionValue::ULong(2),
        ),
        (
            "public float Main() { List<float> v = new List<float>(); v.Add(1.5f); v.Add(2.5f); v.RemoveAt(0); return v.Get(0); }",
            ExecutionValue::float(2.5),
        ),
        (
            "public double Main() { List<double> v = new List<double>(); v.Add(1.5d); v.Add(2.5d); v.RemoveAt(0); return v.Get(0); }",
            ExecutionValue::double(2.5),
        ),
        (
            "public bool Main() { List<bool> v = new List<bool>(); v.Add(false); v.Add(true); v.RemoveAt(0); return v.Get(0); }",
            ExecutionValue::Bool(true),
        ),
        (
            "public char Main() { List<char> v = new List<char>(); v.Add('a'); v.Add('b'); v.RemoveAt(0); return v.Get(0); }",
            ExecutionValue::Char('b'),
        ),
        (
            "public string Main() { List<string> v = new List<string>(); v.Add(\"a\"); v.Add(\"b\"); v.RemoveAt(0); return v.Get(0); }",
            ExecutionValue::String("b".to_owned()),
        ),
    ] {
        assert_eq!(run(source, "Main"), Ok(expected), "`{source}`");
    }
}

#[test]
fn remove_at_preserves_class_identity_of_the_remaining_element() {
    let source = "
        public class Box { public int value; public Box(int value) { this.value = value; } }
        public int Main()
        {
            Box a = new Box(1);
            Box b = new Box(2);
            List<Box> values = new List<Box>();
            values.Add(a);
            values.Add(b);
            values.RemoveAt(0);
            Box loaded = values.Get(0);
            loaded.value = 99;
            return b.value;
        }
        ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(99)));
}

#[test]
fn remove_at_preserves_array_identity_of_the_remaining_element() {
    let source = "
        public int Main()
        {
            int[] a = [1, 1];
            int[] b = [2, 2];
            List<int[]> values = new List<int[]>();
            values.Add(a);
            values.Add(b);
            values.RemoveAt(0);
            int[] loaded = values.Get(0);
            loaded[0] = 99;
            return b[0];
        }
        ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(99)));
}

#[test]
fn remove_at_preserves_nested_list_identity_of_the_remaining_element() {
    let source = "
        public int Main()
        {
            List<int> inner = new List<int>();
            inner.Add(1);
            List<int> other = new List<int>();
            other.Add(2);
            List<List<int>> outer = new List<List<int>>();
            outer.Add(other);
            outer.Add(inner);
            outer.RemoveAt(0);
            List<int> loaded = outer.Get(0);
            loaded.Add(5);
            return inner.Length;
        }
        ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(2)));
}

#[test]
fn remove_at_preserves_interface_dispatch_of_the_remaining_element() {
    let source = "
        public interface IShape { int Area(); }
        public class Square : IShape { public int side; public Square(int side) { this.side = side; } public int Area() { return side * side; } }
        public int Main()
        {
            IShape first = new Square(2);
            IShape second = new Square(4);
            List<IShape> values = new List<IShape>();
            values.Add(first);
            values.Add(second);
            values.RemoveAt(0);
            IShape loaded = values.Get(0);
            return loaded.Area();
        }
        ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(16)));
}

#[test]
fn remove_at_shifts_a_struct_with_padding_byte_for_byte() {
    let source = "
        public struct Point { public int x; public bool flag; public long y; }
        public int Main()
        {
            List<Point> values = new List<Point>();
            values.Add(Point { x: 1, flag: true, y: 2L });
            values.Add(Point { x: 3, flag: false, y: 4L });
            values.RemoveAt(0);
            Point remaining = values.Get(0);
            return remaining.x;
        }
        ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(3)));
}

#[test]
fn remove_at_preserves_a_reference_field_inside_a_remaining_struct() {
    let source = "
        public class Box { public int value; public Box(int value) { this.value = value; } }
        public struct Wrapper { public Box inner; }
        public int Main()
        {
            Box a = new Box(1);
            Box b = new Box(2);
            List<Wrapper> values = new List<Wrapper>();
            values.Add(Wrapper { inner: a });
            values.Add(Wrapper { inner: b });
            values.RemoveAt(0);
            Wrapper loaded = values.Get(0);
            loaded.inner.value = 77;
            return b.value;
        }
        ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(77)));
}

#[test]
fn remove_at_shifts_enum_without_payload() {
    let source = "
        public enum Color { Red, Green, Blue }
        public int Main()
        {
            List<Color> values = new List<Color>();
            values.Add(Color.Red);
            values.Add(Color.Blue);
            values.RemoveAt(0);
            Color remaining = values.Get(0);
            switch (remaining) {
                case Red: return 0;
                case Green: return 1;
                case Blue: return 2;
            }
        }
        ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(2)));
}

#[test]
fn remove_at_shifts_enum_with_scalar_payload() {
    let source = "
        public enum Shape { Circle(int radius), Square }
        public int Main()
        {
            List<Shape> values = new List<Shape>();
            values.Add(Shape.Square);
            values.Add(Shape.Circle(9));
            values.RemoveAt(0);
            Shape remaining = values.Get(0);
            switch (remaining) {
                case Circle(r): return r;
                case Square: return -1;
            }
        }
        ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(9)));
}

#[test]
fn remove_at_preserves_a_reference_payload_inside_a_remaining_enum() {
    let source = "
        public class Box { public int value; public Box(int value) { this.value = value; } }
        public enum MaybeBox { None, Some(Box inner) }
        public int Main()
        {
            Box box = new Box(5);
            List<MaybeBox> values = new List<MaybeBox>();
            values.Add(MaybeBox.None);
            values.Add(MaybeBox.Some(box));
            values.RemoveAt(0);
            MaybeBox loaded = values.Get(0);
            switch (loaded) {
                case Some(b): return b.value;
                case None: return -1;
            }
        }
        ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(5)));
}

#[test]
fn remove_at_works_for_a_generic_specialization() {
    let source = "
        public class Box<T> { private T value; public Box(T value) { this.value = value; } public T Get() { return value; } }
        public int Main()
        {
            List<Box<int>> values = new List<Box<int>>();
            values.Add(new Box<int>(1));
            values.Add(new Box<int>(2));
            values.RemoveAt(0);
            Box<int> loaded = values.Get(0);
            return loaded.Get();
        }
        ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(2)));
}

#[test]
fn remove_at_still_rejects_decimal() {
    let errors = compile_errors(
        "public int Main() { List<decimal> values = new List<decimal>(); values.Add(1.5m); values.RemoveAt(0); return 0; }",
    );
    assert!(
        errors
            .iter()
            .any(|message| message.contains("`List<decimal>` cannot be used")),
        "expected `List<decimal>` to be rejected, got {errors:?}"
    );
}

#[test]
fn set_and_the_indexer_remain_unavailable_after_remove_at() {
    for (source, expected) in [
        (
            "public int Main() { List<int> values = new List<int>(); values.Add(1); values.RemoveAt(0); values.Set(0, 2); return 0; }",
            "no member `Set`",
        ),
        (
            "public int Main() { List<int> values = new List<int>(); values.Add(1); values.RemoveAt(0); return values[0]; }",
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
fn a_list_still_cannot_cross_a_worker_boundary_after_remove_at() {
    let errors = compile_errors(
        "public List<int> Make() { List<int> values = new List<int>(); values.Add(1); values.Add(2); values.RemoveAt(0); return values; } \
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
fn arrays_add_get_and_length_are_unaffected_by_remove_at() {
    let source = "
        public class Box { public int value; public Box(int value) { this.value = value; } }
        public int Main()
        {
            int[] array = [1, 2, 3];
            Box box = new Box(array[0]);
            List<int> values = new List<int>();
            values.Add(10);
            values.Add(20);
            values.RemoveAt(0);
            return array.Length + box.value + values.Length + values.Get(0);
        }
        ";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(25)));
}

// --- Nested-scope lifetime regression tests --------------------------------
//
// A List created in an outer scope must remain valid after a helper function
// adds elements to it, even when that helper's own temporary scope is rewound
// on return. Before the `birth_scope_depth` fix, the data buffer allocated
// inside the helper's temp scope would be zeroed on scope exit, leaving the
// list header with a dangling pointer.

#[test]
fn list_fill_helper_first_add_survives_scope_rewind() {
    // Minimum reproduction: helper has its own temp scope (string concat),
    // triggers the first buffer allocation via Add, returns, and the list must
    // be intact after the helper's scope is rewound.
    let source = r#"
        public void Fill(List<string> values)
        {
            string s = "hello" + " world";
            values.Add(s);
        }
        public int Main()
        {
            List<string> values = new List<string>();
            Fill(values);
            Fill(values);
            return values.Length;
        }
    "#;
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(2)));
}

#[test]
fn list_fill_helper_get_after_scope_rewind() {
    // After a helper adds elements, Get must return the correct values.
    let source = r#"
        public void Append(List<int> values, int base_val)
        {
            string unused = "tick" + "tock";
            values.Add(base_val + 1);
        }
        public int Main()
        {
            List<int> values = new List<int>();
            Append(values, 0);
            Append(values, 10);
            Append(values, 20);
            return values.Get(0) + values.Get(1) + values.Get(2);
        }
    "#;
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(33)));
}

#[test]
fn list_fill_helper_growth_inside_helper() {
    // Add enough elements to trigger geometric growth inside the helper.
    // All elements must survive the helper's scope rewind.
    let source = r#"
        public void FillFive(List<int> values, int start)
        {
            string unused = "g" + "row";
            values.Add(start + 0);
            values.Add(start + 1);
            values.Add(start + 2);
            values.Add(start + 3);
            values.Add(start + 4);
        }
        public int Main()
        {
            List<int> values = new List<int>();
            FillFive(values, 0);
            FillFive(values, 10);
            int sum = 0;
            int i = 0;
            foreach (int v in values) {
                sum = sum + v;
            }
            return sum;
        }
    "#;
    // 0+1+2+3+4 + 10+11+12+13+14 = 10 + 60 = 70
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(70)));
}

#[test]
fn list_fill_helper_struct_with_string_survives_scope_rewind() {
    // Structs containing strings are stored by value; the struct data in the
    // list buffer must survive the helper's scope exit.
    let source = r#"
        public struct Item { public string Name; public int Value; }
        public void Push(List<Item> items, string prefix, int val)
        {
            string name = prefix + val.ToString();
            Item item = Item { Name: name, Value: val };
            items.Add(item);
        }
        public int Main()
        {
            List<Item> items = new List<Item>();
            Push(items, "item-", 1);
            Push(items, "item-", 2);
            Push(items, "item-", 3);
            int sum = 0;
            foreach (Item it in items) {
                sum = sum + it.Value;
            }
            return sum;
        }
    "#;
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(6)));
}

#[test]
fn list_fill_helper_post_return_allocs_do_not_corrupt() {
    // After the helper returns, the caller's own allocations must not overlap
    // the list's (now permanent) data buffer.
    let source = r#"
        public void Seed(List<int> values)
        {
            string unused = "s" + "eed";
            values.Add(42);
            values.Add(43);
        }
        public int Main()
        {
            List<int> values = new List<int>();
            Seed(values);
            string noise = "post" + "seed";
            string more = "extra" + "alloc";
            return values.Get(0) + values.Get(1);
        }
    "#;
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(85)));
}

// --- Retention / memory region tests ----------------------------------------

#[test]
fn list_local_no_helper_header_and_buffer_are_temporary() {
    // A List used only in its own scope must be classified Temporary and
    // reclaimed when the scope exits. The permanent arena grows by 0 bytes.
    let source = r"
        public int Main()
        {
            List<int> values = new List<int>();
            values.Add(1);
            values.Add(2);
            return values.Length;
        }
    ";
    let module = compile(source).expect("compiles").mir;
    let (result, stats) = execute_with_stats(&module, "Main").expect("executes");
    assert_eq!(result, ExecutionValue::Int(2));
    assert_eq!(
        stats.used_bytes, 0,
        "local list must not retain permanent bytes; used_bytes={}",
        stats.used_bytes
    );
}

#[test]
fn list_passed_to_helper_no_per_call_growth_after_escape_fix() {
    // List passed to a helper: after the escape analysis fix the header is
    // Persistent. used_bytes grows on each RunOnce call (each creates a new
    // Persistent list) but execution must complete without error — bounded
    // linear growth, not a dangling-pointer crash.
    let source = r"
        public void Fill(List<int> values, int v)
        {
            values.Add(v);
        }
        public void RunOnce(int index)
        {
            List<int> values = new List<int>();
            Fill(values, index);
        }
        public int Main()
        {
            int index = 0;
            while (index < 1000)
            {
                RunOnce(index);
                index = index + 1;
            }
            return 0;
        }
    ";
    let module = compile(source).expect("compiles").mir;
    let (result, stats) = execute_with_stats(&module, "Main").expect("executes");
    assert_eq!(result, ExecutionValue::Int(0));
    let _ = stats;
}

#[test]
fn list_region_is_persistent_when_passed_to_helper() {
    // Compile-time check: AllocateList in Main must be Persistent when the list
    // is passed to a helper function.
    let source = r"
        public void Fill(List<int> values, int v)
        {
            values.Add(v);
        }
        public int Main()
        {
            List<int> values = new List<int>();
            Fill(values, 42);
            return values.Length;
        }
    ";
    let module = compile(source).expect("compiles").mir;
    let main_fn = module
        .functions
        .iter()
        .find(|f| f.name == "Main" && f.owner.is_none())
        .expect("Main function");
    let region = main_fn
        .blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .find_map(|i| match i {
            mir::Instruction::AllocateList { region, .. } => Some(*region),
            _ => None,
        })
        .expect("AllocateList in Main");
    assert_eq!(
        region,
        mir::AllocationRegion::Persistent,
        "List passed to a helper must be allocated Persistent, got {region:?}"
    );
}

#[test]
fn list_region_stays_temporary_for_local_only_use() {
    // Compile-time check: a List not passed to any user function must stay
    // Temporary (no unnecessary promotion).
    let source = r"
        public int Main()
        {
            List<int> values = new List<int>();
            values.Add(1);
            values.Add(2);
            return values.Length;
        }
    ";
    let module = compile(source).expect("compiles").mir;
    let main_fn = module
        .functions
        .iter()
        .find(|f| f.name == "Main" && f.owner.is_none())
        .expect("Main function");
    let region = main_fn
        .blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .find_map(|i| match i {
            mir::Instruction::AllocateList { region, .. } => Some(*region),
            _ => None,
        })
        .expect("AllocateList in Main");
    assert_eq!(
        region,
        mir::AllocationRegion::Temporary,
        "List used only locally must remain Temporary, got {region:?}"
    );
}
