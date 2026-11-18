//! LANG-012: interface inheritance (`interface IB : IA, IC`). Programs run
//! through the whole pipeline (semantic analysis, HIR, MIR, Cranelift).
//! Negative programs are controlled compile-time diagnostics.

use std::sync::atomic::{AtomicU64, Ordering};

use aster_codegen_cranelift::{ExecutionValue, execute};
use aster_compiler::{compile, compile_project};

fn run_int(source: &str) -> i32 {
    let compilation = compile(source).unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"));
    match execute(&compilation.mir, "Run") {
        Ok(ExecutionValue::Int(value)) => value,
        other => panic!("expected an int result, found {other:?}"),
    }
}

fn rejects(source: &str, expected: &str) {
    match compile(source) {
        Ok(_) => panic!("expected a compile-time diagnostic for:\n{source}"),
        Err(diagnostics) => assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains(expected)),
            "expected a diagnostic containing `{expected}`, found {diagnostics:#?}"
        ),
    }
}

/// Programs that use `as` need the official `Option`.
fn run_project_int(source: &str) -> i32 {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "aster-interface-inheritance-{}-{id}.aster",
        std::process::id()
    ));
    std::fs::write(&path, format!("using aster.core;\n{source}")).expect("write project");
    let compiled = compile_project(&path).map_err(|diagnostics| format!("{diagnostics:#?}"));
    std::fs::remove_file(&path).ok();
    let compilation = compiled.expect("project compiles").compilation;
    match execute(&compilation.mir, "Run") {
        Ok(ExecutionValue::Int(value)) => value,
        other => panic!("expected an int result, found {other:?}"),
    }
}

const TOWER: &str = r"
    public interface IA { int A(); }
    public interface IB : IA { int B(); }
    public interface IC : IB { int C(); }
    public class Impl : IC {
        public Impl() {}
        public int A() { return 1; }
        public int B() { return 20; }
        public int C() { return 300; }
    }
";

#[test]
fn a_base_interface_member_is_callable_through_the_derived_interface() {
    let source = format!(
        "{TOWER} public int Run() {{ IC value = new Impl(); return value.A() + value.B() + value.C(); }}"
    );
    assert_eq!(run_int(&source), 321);
}

#[test]
fn a_derived_interface_value_converts_to_every_base_interface() {
    let source = format!(
        "{TOWER}
        internal int ViaA(IA a) {{ return a.A(); }}
        internal int ViaB(IB b) {{ return b.A() + b.B() + ViaA(b); }}
        public int Run() {{
            IC c = new Impl();
            IB b = c;
            IA a = b;
            IA direct = c;
            return ViaB(c) + a.A() * 1000 + direct.A() * 10000;
        }}"
    );
    // ViaB: 1 + 20 + 1 = 22.
    assert_eq!(run_int(&source), 22 + 1000 + 10000);
}

#[test]
fn a_class_converts_to_a_base_interface_it_names_only_through_a_derived_one() {
    let source = format!("{TOWER} public int Run() {{ IA a = new Impl(); return a.A(); }}");
    assert_eq!(run_int(&source), 1);
}

#[test]
fn multiple_base_interfaces_and_a_diamond_share_the_common_member() {
    let source = r"
        public interface IRoot { int Root(); }
        public interface ILeft : IRoot { int Left(); }
        public interface IRight : IRoot { int Right(); }
        public interface IBoth : ILeft, IRight { int Both(); }
        public class Impl : IBoth {
            public Impl() {}
            public int Root() { return 1; }
            public int Left() { return 20; }
            public int Right() { return 300; }
            public int Both() { return 4000; }
        }
        internal int ViaRoot(IRoot r) { return r.Root(); }
        internal int ViaRight(IRight r) { return r.Right() + ViaRoot(r); }
        public int Run() {
            IBoth both = new Impl();
            ILeft left = both;
            IRight right = both;
            return both.Root() + left.Left() + ViaRight(right) + both.Both();
        }";
    assert_eq!(run_int(source), 1 + 20 + 301 + 4000);
}

#[test]
fn an_identical_member_redeclared_by_the_derived_interface_is_the_same_member() {
    let source = r"
        public interface IA { int A(); }
        public interface IB : IA { int A(); int B(); }
        public class Impl : IB {
            public Impl() {}
            public int A() { return 5; }
            public int B() { return 7; }
        }
        public int Run() { IB b = new Impl(); IA a = b; return b.A() + a.A() + b.B(); }";
    assert_eq!(run_int(source), 17);
}

#[test]
fn a_derived_class_inherits_the_base_interface_tables_of_its_base_class() {
    let source = r"
        public interface IA { int A(); }
        public interface IB : IA { int B(); }
        public class Base : IB {
            public Base() {}
            public virtual int A() { return 1; }
            public int B() { return 2; }
        }
        public class Derived : Base {
            public Derived() {}
            public override int A() { return 10; }
        }
        public int Run() {
            Base b = new Derived();
            IB ib = b;
            IA ia = ib;
            return ia.A() + ib.B();
        }";
    assert_eq!(run_int(source), 12);
}

#[test]
fn is_with_a_base_interface_is_true_through_inheritance() {
    let source = format!(
        "{TOWER}
        public interface IUnrelated {{ int U(); }}
        public class Plain {{ public Plain() {{}} }}
        internal int Test(Plain plain) {{
            int bits = 0;
            if (plain is IA) {{ bits = bits + 1; }}
            return bits;
        }}
        public int Run() {{
            Impl impl = new Impl();
            int bits = 0;
            if (impl is IA) {{ bits = bits + 1; }}
            if (impl is IB) {{ bits = bits + 2; }}
            if (impl is IC) {{ bits = bits + 4; }}
            if (impl is IUnrelated) {{ bits = bits + 8; }}
            IC top = impl;
            if (top is IA a) {{ bits = bits + 100 * a.A(); }}
            return bits;
        }}"
    );
    assert_eq!(run_int(&source), 107);
}

#[test]
fn as_with_a_base_interface_yields_a_callable_value() {
    let source = format!(
        "{TOWER}
        public int Run() {{
            IC top = new Impl();
            int total = 0;
            switch (top as IA) {{
                case Some(a): total = total + a.A();
                case None: total = total + 1000;
            }}
            Impl impl = new Impl();
            switch (impl as IB) {{
                case Some(b): total = total + b.B();
                case None: total = total + 5000;
            }}
            return total;
        }}"
    );
    assert_eq!(run_project_int(&source), 21);
}

#[test]
fn generic_interfaces_can_extend_closed_generic_interfaces() {
    let source = r"
        public interface IValue<T> { T Value(); }
        public interface IScored : IValue<int> { int Score(); }
        public class Item : IScored {
            public Item() {}
            public int Value() { return 6; }
            public int Score() { return 70; }
        }
        public int Run() {
            IScored scored = new Item();
            IValue<int> value = scored;
            return value.Value() + scored.Score();
        }";
    assert_eq!(run_int(source), 76);
}

#[test]
fn a_missing_inherited_member_is_an_error() {
    rejects(
        r"
        public interface IA { int A(); }
        public interface IB : IA { int B(); }
        public class Impl : IB { public Impl() {} public int B() { return 2; } }
        public int Run() { return 0; }",
        "does not implement required method",
    );
}

#[test]
fn an_interface_cycle_is_an_error() {
    rejects(
        r"
        public interface IA : IB { int A(); }
        public interface IB : IA { int B(); }
        public int Run() { return 0; }",
        "interface inheritance cycle",
    );
    rejects(
        "public interface ISelf : ISelf { int A(); } public int Run() { return 0; }",
        "interface inheritance cycle",
    );
}

#[test]
fn conflicting_inherited_signatures_are_an_error() {
    rejects(
        r"
        public interface IA { int Get(); }
        public interface IB { string Get(); }
        public interface IC : IA, IB { int C(); }
        public int Run() { return 0; }",
        "conflicting declarations of `Get`",
    );
}

#[test]
fn an_interface_base_must_be_an_interface() {
    rejects(
        r"
        public class K { public K() {} }
        public interface IA : K { int A(); }
        public int Run() { return 0; }",
        "is not an interface",
    );
    rejects(
        "public interface IA : Missing { int A(); } public int Run() { return 0; }",
        "unknown interface `Missing`",
    );
}

#[test]
fn a_base_interface_is_not_implicitly_a_derived_interface() {
    rejects(
        r"
        public interface IA { int A(); }
        public interface IB : IA { int B(); }
        public class OnlyA : IA { public OnlyA() {} public int A() { return 1; } }
        public int Run() { IA a = new OnlyA(); IB b = a; return 0; }",
        "expected `IB`, found `IA`",
    );
}

#[cfg(feature = "mem-exp-c6")]
#[test]
fn interface_upcasts_run_under_the_counting_configuration() {
    let source = format!(
        "{TOWER}
        internal int ViaA(IA a) {{ return a.A(); }}
        public int Run() {{
            int total = 0;
            for (int i = 0; i < 50; i++) {{
                IC c = new Impl();
                IB b = c;
                total = total + ViaA(b) + b.B();
            }}
            return total;
        }}"
    );
    assert_eq!(run_int(&source), 50 * 21);
}
