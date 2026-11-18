//! T-OBJ-5 (object model B2): constructor overloads and `this(...)` chaining,
//! executed through the whole pipeline. The compile-time rules and negative
//! cases live in `aster-compiler/tests/constructor_overloads.rs`.
//!
//! Order is made observable with a `Trace` that appends one digit per event
//! (`value = value * 10 + digit`), so the expected number spells the order.

use aster_codegen_cranelift::{ExecutionValue, execute};
use aster_compiler::compile;

fn run_int(source: &str) -> i32 {
    let compilation = compile(source).unwrap_or_else(|diagnostics| {
        panic!("expected the program to compile, got {diagnostics:#?}")
    });
    match execute(&compilation.mir, "Run") {
        Ok(ExecutionValue::Int(value)) => value,
        other => panic!("expected an int result, found {other:?}"),
    }
}

const TRACE: &str = r"
    public class Trace {
        public int value;
        public Trace() {}
        public int Mark(int digit) { value = value * 10 + digit; return digit; }
    }
";

#[test]
fn new_selects_the_constructor_by_arity_and_by_parameter_type() {
    let source = r#"
        public class P {
            public int v;
            public P() { v = 1; }
            public P(int a) { v = 2; }
            public P(long a) { v = 3; }
            public P(string s) { v = 4; }
            public P(int a, int b) { v = 5; }
        }
        public int Run() {
            long wide = 9;
            P p0 = new P();
            P p1 = new P(1);
            P p2 = new P(wide);
            P p3 = new P("x");
            P p4 = new P(1, 2);
            return p0.v * 10000 + p1.v * 1000 + p2.v * 100 + p3.v * 10 + p4.v;
        }
    "#;
    assert_eq!(run_int(source), 12_345);
}

#[test]
fn a_delegating_constructor_runs_its_target_first_and_then_its_own_body() {
    // Chain of three: the arguments of `this(...)` are evaluated before the
    // target runs (3, 4), the base constructor of the last target runs once
    // (2), then the bodies unwind from the innermost out (5, 6, 7).
    let source = format!(
        r"{TRACE}
        public class Base {{
            public int tag;
            public Base(Trace t) {{ tag = 1; t.Mark(1); }}
            public Base(Trace t, int x) {{ tag = x; t.Mark(2); }}
        }}
        public class D : Base {{
            public int f = 5;
            public D(Trace t) : this(t, t.Mark(3)) {{ t.Mark(7); }}
            public D(Trace t, int a) : this(t, a, t.Mark(4)) {{ t.Mark(6); }}
            public D(Trace t, int a, int b) : base(t, a + b) {{ t.Mark(5); }}
        }}
        public int Run() {{
            Trace t = new Trace();
            D d = new D(t);
            return t.value;
        }}"
    );
    assert_eq!(run_int(&source), 342_567);
}

#[test]
fn the_chain_reaches_the_selected_base_overload_and_keeps_inherited_state() {
    let source = format!(
        r"{TRACE}
        public class Base {{
            public int tag;
            public Base(Trace t) {{ tag = 1; t.Mark(1); }}
            public Base(Trace t, int x) {{ tag = x; t.Mark(2); }}
        }}
        public class D : Base {{
            public int f = 5;
            public D(Trace t) : this(t, 10) {{ f = f + 1; }}
            public D(Trace t, int a) : base(t, a) {{ f = f * 2; }}
        }}
        public int Run() {{
            Trace t = new Trace();
            D d = new D(t);
            return d.tag * 1000 + d.f * 10 + t.value;
        }}"
    );
    // The initializer `f = 5` runs once (in `D(Trace, int)`), then `f = 10`,
    // then the delegating body makes it 11; `Base(Trace, int)` marked 2.
    assert_eq!(run_int(&source), 10_000 + 110 + 2);
}

#[test]
fn the_base_constructor_runs_exactly_once_through_a_delegation() {
    let source = format!(
        r"{TRACE}
        public class Base {{
            public Base(Trace t) {{ t.Mark(1); }}
        }}
        public class E : Base {{
            public E(Trace t) : this(t, 1) {{ t.Mark(3); }}
            public E(Trace t, int x) : base(t) {{ t.Mark(2); }}
        }}
        public int Run() {{
            Trace t = new Trace();
            E e = new E(t);
            return t.value;
        }}"
    );
    assert_eq!(run_int(&source), 123);
}

#[test]
fn field_initializers_run_once_in_the_constructor_that_does_not_delegate() {
    // If a delegating constructor ran the initializer again after its target,
    // `a` would be reset to 5 before the delegating body. 5 -> 6 -> 62.
    let source = r"
        public class C {
            public int a = 5;
            public C() { a = a + 1; }
            public C(int x) : this() { a = a * 10 + x; }
        }
        public int Run() {
            C delegating = new C(2);
            C plain = new C();
            return delegating.a * 100 + plain.a;
        }
    ";
    assert_eq!(run_int(source), 6_200 + 6);
}

#[test]
fn get_only_auto_properties_are_assignable_in_delegating_constructors() {
    let source = r"
        public class P {
            public int W { get; }
            public int H { get; }
            public P() : this(2, 3) {}
            public P(int w, int h) { W = w; H = h; }
            public P(int s) : this() { W = W + s; }
        }
        public int Run() {
            P p = new P(10);
            return p.W * 100 + p.H;
        }
    ";
    assert_eq!(run_int(source), 1_203);
}

#[test]
fn defaulted_parameters_and_named_arguments_work_through_this() {
    let source = r"
        public class C {
            public int a;
            public int b;
            public C(int a, int b = 9) { this.a = a; this.b = b; }
            public C() : this(b: 4, a: 1) {}
        }
        public int Run() {
            C one = new C();
            C two = new C(7);
            return one.a * 1000 + one.b * 100 + two.a * 10 + two.b;
        }
    ";
    assert_eq!(run_int(source), 1_000 + 400 + 70 + 9);
}

#[test]
fn a_private_overload_is_only_reachable_through_this() {
    let source = r"
        public class C {
            public int v;
            private C(int a, int b) { v = a + b; }
            public C(int a) : this(a, 100) {}
        }
        public int Run() { C c = new C(5); return c.v; }
    ";
    assert_eq!(run_int(source), 105);
}
