//! T-LANG-8 (LANG-008): object initializers executed through the whole
//! pipeline.

use aster_codegen_cranelift::{ExecutionValue, execute};
use aster_compiler::compile;

fn run_int(source: &str) -> i32 {
    let compilation = compile(source)
        .unwrap_or_else(|diagnostics| panic!("expected a valid program: {diagnostics:#?}"));
    match execute(&compilation.mir, "Run") {
        Ok(ExecutionValue::Int(value)) => value,
        other => panic!("expected an int result, found {other:?}"),
    }
}

/// A trace appends one decimal digit per event, so the final value spells the
/// order in which events happened.
const TRACE: &str = r"
    public class Trace
    {
        public int Value;
        public Trace() {}
        public int Mark(int digit) { Value = Value * 10 + digit; return digit; }
    }
";

#[test]
fn arguments_then_constructor_then_members_left_to_right() {
    assert_eq!(
        run_int(&format!(
            r"{TRACE}
            public class Widget
            {{
                public int A;
                public int B {{ get; set; }}
                public int C {{ get; init; }}
                public Widget(int first, Trace trace) {{ trace.Mark(2); }}
            }}
            public int Run()
            {{
                Trace trace = new Trace();
                Widget widget = new Widget(trace.Mark(1), trace) {{
                    A = trace.Mark(3), C = trace.Mark(4), B = trace.Mark(5) }};
                return trace.Value * 1000 + widget.A * 100 + widget.B * 10 + widget.C;
            }}
        "
        )),
        12345 * 1000 + 3 * 100 + 5 * 10 + 4
    );
}

#[test]
fn the_parameterless_form_and_empty_initializer_work() {
    assert_eq!(
        run_int(
            r"
            public class P
            {
                public int X;
                public int Y { get; init; }
                public P() { X = 100; }
            }
            public int Run()
            {
                P a = new P { Y = 5 };
                P b = new P { };
                P c = new P() { X = 7, Y = 1 };
                return a.X + a.Y + b.X + c.X * 1000 + c.Y;
            }
        "
        ),
        100 + 5 + 100 + 7000 + 1
    );
}

#[test]
fn a_target_typed_initializer_and_an_initializer_as_an_argument() {
    assert_eq!(
        run_int(
            r"
            public class P { public int X; public int Y { get; init; } public P() {} }
            public int Sum(P p) { return p.X + p.Y; }
            public int Run()
            {
                P p = new() { X = 3, Y = 4 };
                return Sum(p) * 100 + Sum(new P { X = 1, Y = 2 });
            }
        "
        ),
        703
    );
}

#[test]
fn a_member_value_may_itself_be_an_initializer_and_may_use_earlier_locals() {
    assert_eq!(
        run_int(
            r"
            public class Inner { public int V; public Inner() {} }
            public class Outer { public Inner Child; public int N; public Outer() { Child = new Inner(); } }
            public int Run()
            {
                int seed = 6;
                Outer o = new Outer { Child = new Inner { V = seed + 1 }, N = seed };
                return o.Child.V * 10 + o.N;
            }
        "
        ),
        76
    );
}

#[test]
fn a_virtual_property_assignment_dispatches_to_the_override() {
    assert_eq!(
        run_int(
            r"
            public class Base
            {
                public virtual int Level { get; set; }
                public Base() {}
            }
            public class Derived : Base
            {
                private int stored;
                public override int Level
                {
                    get { return stored; }
                    set { stored = value * 2; }
                }
                public Derived() {}
            }
            public int Run()
            {
                Base b = new Derived { Level = 21 };
                return b.Level;
            }
        "
        ),
        42
    );
}

#[test]
fn an_inherited_member_is_assignable_through_a_derived_creation() {
    assert_eq!(
        run_int(
            r"
            public class Base { public int Id { get; init; } public int Tag; public Base() {} }
            public class Derived : Base { public int Extra; public Derived() {} }
            public int Run()
            {
                Derived d = new Derived { Id = 1, Tag = 2, Extra = 3 };
                return d.Id * 100 + d.Tag * 10 + d.Extra;
            }
        "
        ),
        123
    );
}

#[test]
fn an_initializer_inside_a_loop_creates_independent_objects() {
    assert_eq!(
        run_int(
            r"
            public class P { public int X; public P() {} }
            public int Run()
            {
                int total = 0;
                for (int i = 1; i <= 10; i += 1)
                {
                    P p = new P { X = i };
                    total += p.X;
                }
                return total;
            }
        "
        ),
        55
    );
}

/// Under reference counting (`--features mem-exp-c6`) an initializer that
/// stores reference-typed members must keep the counts balanced: objects
/// created in a loop are released, those still referenced survive.
#[cfg(feature = "mem-exp-rc-core")]
#[test]
fn reference_typed_members_keep_reference_counts_balanced() {
    let source = r"
        public class Leaf { public int V; public Leaf() {} }
        public class Holder { public Leaf Item; public Leaf Other; public Holder() { Item = new Leaf(); Other = new Leaf(); } }
        public int Run()
        {
            Leaf kept = new Leaf { V = 7 };
            int sum = 0;
            for (int i = 0; i < 100; i += 1)
            {
                Holder h = new Holder { Item = new Leaf { V = i }, Other = kept };
                sum += h.Item.V + h.Other.V;
            }
            return sum;
        }
    ";
    let compilation = compile(source).expect("source compiles");
    let (value, stats) =
        aster_codegen_cranelift::execute_with_stats(&compilation.mir, "Run").expect("executes");
    assert_eq!(value, ExecutionValue::Int(4950 + 700));
    assert!(
        stats.object_heap.released_blocks >= 99,
        "initialized objects were not released: {:?}",
        stats.object_heap
    );
}
