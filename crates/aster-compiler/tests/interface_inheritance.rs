//! LANG-012: semantic rules and lowering of interface inheritance.

use aster_compiler::{compile, hir, mir};

const TOWER: &str = r"
    public interface IA { int A(); }
    public interface IB : IA { int B(); }
    public interface IC { int C(); }
    public interface ID : IB, IC { int D(); }
    public class Impl : ID {
        public Impl() {}
        public int A() { return 1; }
        public int B() { return 2; }
        public int C() { return 3; }
        public int D() { return 4; }
    }
";

fn messages(source: &str) -> Vec<String> {
    match compile(source) {
        Ok(_) => panic!("expected a compile-time diagnostic for:\n{source}"),
        Err(diagnostics) => diagnostics
            .into_iter()
            .map(|diagnostic| diagnostic.message)
            .collect(),
    }
}

fn rejects(source: &str, expected: &str) {
    let messages = messages(source);
    assert!(
        messages.iter().any(|message| message.contains(expected)),
        "expected a diagnostic containing `{expected}`, found {messages:#?}"
    );
}

#[test]
fn a_class_gets_a_table_for_every_transitive_base_interface() {
    let compilation = compile(TOWER).expect("valid interface inheritance");
    let name = |symbol: mir::SymbolId| {
        compilation
            .mir
            .interfaces
            .iter()
            .find(|interface| interface.symbol == symbol)
            .map(|interface| interface.name.clone())
            .expect("interface")
    };
    let mut implemented = compilation
        .mir
        .interface_implementations
        .iter()
        .map(|implementation| name(implementation.interface))
        .collect::<Vec<_>>();
    implemented.sort();
    assert_eq!(implemented, ["IA", "IB", "IC", "ID"]);
}

#[test]
fn a_derived_interface_declares_the_inherited_members_once() {
    let compilation = compile(TOWER).expect("valid interface inheritance");
    let methods = |interface: &str| {
        let definition = compilation
            .mir
            .interfaces
            .iter()
            .find(|definition| definition.name == interface)
            .expect("interface");
        let mut names = definition
            .methods
            .iter()
            .map(|method| method.name.clone())
            .collect::<Vec<_>>();
        names.sort();
        names
    };
    assert_eq!(methods("IA"), ["A"]);
    assert_eq!(methods("IB"), ["A", "B"]);
    assert_eq!(methods("ID"), ["A", "B", "C", "D"]);
}

#[test]
fn a_derived_to_base_interface_conversion_lowers_to_an_interface_lookup() {
    let source =
        format!("{TOWER} public int Run() {{ ID d = new Impl(); IA a = d; return a.A(); }}");
    let compilation = compile(&source).expect("valid conversion");
    let lookups = compilation
        .mir
        .functions
        .iter()
        .filter(|function| function.name == "Run")
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .filter(|instruction| {
            matches!(
                instruction,
                mir::Instruction::Assign {
                    value: mir::Rvalue {
                        kind: mir::RvalueKind::InterfaceLookup { .. },
                        ..
                    },
                    ..
                }
            )
        })
        .count();
    assert_eq!(lookups, 1);
    assert!(
        compilation.hir.items.iter().any(|item| {
            matches!(item, hir::Item::Function(function) if function.name == "Run")
        })
    );
}

#[test]
fn a_class_listing_a_base_interface_again_is_accepted_without_duplicate_tables() {
    let source = r"
        public interface IA { int A(); }
        public interface IB : IA { int B(); }
        public class Impl : IB, IA {
            public Impl() {}
            public int A() { return 1; }
            public int B() { return 2; }
        }";
    let compilation = compile(source).expect("redundant listing is accepted");
    assert_eq!(compilation.mir.interface_implementations.len(), 2);
}

#[test]
fn a_derived_class_does_not_repeat_the_tables_of_its_base_class() {
    let source = r"
        public interface IA { int A(); }
        public interface IB : IA { int B(); }
        public class Base : IB {
            public Base() {}
            public int A() { return 1; }
            public int B() { return 2; }
        }
        public class Derived : Base { public Derived() {} }";
    compile(source).expect("derived class inherits the interface tables");
}

#[test]
fn missing_members_of_every_level_are_reported() {
    rejects(
        r"
        public interface IA { int A(); }
        public interface IB : IA { int B(); }
        public class Impl : IB { public Impl() {} public int B() { return 2; } }",
        "does not implement required method `IA.A`",
    );
    rejects(
        r"
        public interface IA { int A(); }
        public interface IB : IA { int B(); }
        public class Impl : IB { public Impl() {} public int A() { return 1; } }",
        "does not implement required method `IB.B`",
    );
}

#[test]
fn cycles_duplicates_and_bad_bases_are_errors() {
    rejects(
        r"
        public interface IA : IB { int A(); }
        public interface IB : IC { int B(); }
        public interface IC : IA { int C(); }",
        "interface inheritance cycle",
    );
    rejects(
        "public interface IA { int A(); } public interface IB : IA, IA { int B(); }",
        "listed more than once",
    );
    rejects(
        "public struct S { public int x; } public interface IB : S { int B(); }",
        "is not an interface",
    );
}

#[test]
fn conflicting_signatures_are_errors_but_overloads_are_distinct_members() {
    rejects(
        r"
        public interface IA { int Get(); }
        public interface IB { long Get(); }
        public interface IC : IA, IB { }",
        "conflicting declarations of `Get`",
    );
    compile(
        r"
        public interface IA { int Get(); }
        public interface IB { int Get(int x); }
        public interface IC : IA, IB { }
        public class Impl : IC {
            public Impl() {}
            public int Get() { return 1; }
            public int Get(int x) { return x; }
        }",
    )
    .expect("overloads with different parameters are distinct members");
}

#[test]
fn the_implicit_conversion_only_goes_toward_a_base_interface() {
    rejects(
        r"
        public interface IA { int A(); }
        public interface IB : IA { int B(); }
        internal int Take(IB b) { return b.B(); }
        internal int Pass(IA a) { return Take(a); }",
        "expected `IB`, found `IA`",
    );
    compile(
        r"
        public interface IA { int A(); }
        public interface IB : IA { int B(); }
        internal int Take(IA a) { return a.A(); }
        internal int Pass(IB b) { return Take(b); }",
    )
    .expect("derived to base interface is implicit");
}

#[test]
fn is_with_a_base_interface_of_a_sealed_class_is_statically_valid() {
    let source = r"
        public interface IA { int A(); }
        public interface IB : IA { int B(); }
        public sealed class Impl : IB {
            public Impl() {}
            public int A() { return 1; }
            public int B() { return 2; }
        }
        internal bool Test(Impl impl) { return impl is IA; }";
    compile(source).expect("a sealed class implementing IB implements IA");
}
