use aster_hir as hir;
use aster_mir as mir;

const VALID: &str = "public class Counter { private int value; public Counter(int initial) { value = initial; } public int Get() { return this.value; } } public int Run() { Counter c = new Counter(4); return c.Get(); }";

#[test]
fn class_symbols_receivers_and_allocation_reach_hir_and_mir() {
    let compilation = aster_compiler::compile(VALID).expect("valid class");
    let hir::Item::Class(class) = &compilation.hir.items[0] else {
        panic!("class")
    };
    assert!(class.methods[0].constructor);
    assert!(matches!(
        class.methods[0].parameters[0].type_,
        hir::Type::Class(_)
    ));
    assert_eq!(compilation.mir.classes.len(), 1);
    assert!(
        compilation
            .mir
            .functions
            .iter()
            .any(|function| function.constructor)
    );
    assert!(
        compilation
            .mir
            .functions
            .iter()
            .flat_map(|function| &function.blocks)
            .flat_map(|block| &block.instructions)
            .any(|instruction| matches!(
                instruction,
                mir::Instruction::AllocateObject {
                    region: mir::AllocationRegion::Temporary,
                    ..
                }
            ))
    );
    assert!(
        compilation
            .mir
            .functions
            .iter()
            .flat_map(|function| &function.blocks)
            .flat_map(|block| &block.instructions)
            .any(|instruction| matches!(
                instruction,
                mir::Instruction::Assign {
                    target: mir::Place::ObjectField { .. },
                    ..
                }
            ))
    );
}

#[test]
fn classes_without_a_declared_constructor_get_an_implicit_default_one() {
    let source = "public class Counter { private int value = 40; public int Run() { Increment(); Increment(); return value; } private void Increment() { value = value + 1; } } public int Go() { Counter counter = new Counter(); return counter.Run(); }";
    let compilation = aster_compiler::compile(source).expect("default constructor is implicit");
    assert!(
        compilation
            .mir
            .functions
            .iter()
            .any(|function| function.constructor && function.name == "Counter"),
        "synthesized constructor must reach MIR"
    );
}

#[test]
fn field_initializer_constructing_an_object_reaches_a_synthesized_constructor() {
    let source = "public class Dependency { public int Get() { return 42; } } public class Service { private Dependency dependency = new Dependency(); } public int Run() { Service service = new Service(); return 42; }";
    let compilation =
        aster_compiler::compile(source).expect("field initializer construction must compile");
    assert!(
        compilation
            .mir
            .functions
            .iter()
            .any(|function| function.constructor && function.name == "Service"),
        "synthesized constructor must reach MIR"
    );
}

#[test]
fn static_context_calling_an_instance_method_reports_one_focused_diagnostic() {
    let source = "public class Program { public static int Main() { return Run(); } private int Run() { return 1; } }";
    let diagnostics = aster_compiler::compile(source).expect_err("static context must fail");
    assert_eq!(
        diagnostics.len(),
        1,
        "no duplicate overload error: {diagnostics:#?}"
    );
    assert!(diagnostics[0].message.contains("requires an object"));
}

#[test]
fn invalid_class_programs_have_specific_diagnostics() {
    for (source, expected) in [
        (
            "public struct S { public int x; } public int Run() { S s = new S(); return 0; }",
            "`new` requires a class",
        ),
        (
            "public class C { public C(int x) {} } public int Run() { C c = new C(); return 0; }",
            "missing required argument `x`",
        ),
        (
            "public class C { public C(int x) {} } public int Run() { C c = new C(false); return 0; }",
            "expected `int`, found `bool`",
        ),
        (
            "public class C { private int x; public C() {} } public int Run() { C c = new C(); return c.x; }",
            "field `C.x` is private",
        ),
        (
            "public class C { public C() {} private int Get() { return 1; } } public int Run() { C c = new C(); return c.Get(); }",
            "method `C.Get` is private",
        ),
        (
            "public class C { public C() {} } public int Run() { C c = new C(); return c.Missing(); }",
            "has no method `Missing`",
        ),
        (
            "public int Run() { return this.value; }",
            "`this` is valid only inside",
        ),
        (
            "public class C { private int[] values; public C(bool set) { if (set) { values = [1]; } } } public int Run() { return 0; }",
            "does not initialize field `values`",
        ),
        (
            "public class C { private int[] values; public C(bool stop) { if (stop) { return; } values = [1]; } } public int Run() { return 0; }",
            "returns before field `values` is initialized",
        ),
        (
            "public class C { private int[] values; public C(int[] input) { int length = values.Length; values = input; } } public int Run() { return 0; }",
            "used before initialization",
        ),
        (
            "public class A { public A() {} } public class B { public B() {} } public int Run() { A a = new B(); return 0; }",
            "expected `A`, found `B`",
        ),
        (
            "public class C { public C() {} public int Get() { return 1; } } public int Run() { return Get(); }",
            "instance method `Get` requires an object",
        ),
        (
            "public class C { private Missing value = new Missing(); public C() {} } public int Run() { return 0; }",
            "unknown type `Missing`",
        ),
    ] {
        let diagnostics = aster_compiler::compile(source).expect_err("must fail");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains(expected)),
            "missing `{expected}` in {diagnostics:#?}"
        );
    }
}
