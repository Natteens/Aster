use aster_hir as hir;
use aster_mir as mir;

#[test]
fn static_properties_overloads_and_equality_reach_hir_and_mir() {
    let source = "public struct P { public int x; } public class C { private int value = 40; public C() {} public int Value { get { return value; } private set { value = value; } } public void Add(int amount) { Value += amount; } public static int Pick(int value) { return value; } public static long Pick(long value) { return value; } } public int Run() { C c = new C(); c.Add(2); P a = P { x: 1 }; P b = P { x: 1 }; if (a == b) { return C.Pick(c.Value); } return 0; }";
    let compilation = aster_compiler::compile(source).expect("valid ergonomics program");
    let hir::Item::Class(class) = &compilation.hir.items[1] else {
        panic!("class")
    };
    assert!(class.methods.len() >= 6, "accessors are real HIR functions");
    assert!(
        compilation
            .mir
            .functions
            .iter()
            .any(|function| function.name == "Pick")
    );
    assert!(
        compilation
            .mir
            .functions
            .iter()
            .flat_map(|function| &function.blocks)
            .flat_map(|block| &block.instructions)
            .any(|instruction| matches!(instruction, mir::Instruction::Call { .. }))
    );
}

#[test]
fn ergonomic_errors_are_specific() {
    let cases = [
        (
            "public class C { public C() {} public static int Bad() { return this.value; } private int value; }",
            "instance method",
        ),
        (
            "public class C { private int value; public C() {} public static int Bad() { return value; } }",
            "unknown name `value`",
        ),
        (
            "public class C { private int value = this.Read(); public C() {} public int Read() { return 1; } }",
            "instance method",
        ),
        (
            "public class C { public C() {} public int Value { get { return 1; } } } public int Run() { C c = new C(); c.Value = 2; return 0; }",
            "read-only",
        ),
        (
            "public class C { private int value; public C() { value = 1; } public int Value { get { return value; } private set { value = value; } } } public int Run() { C c = new C(); c.Value = 2; return 0; }",
            "setter for property `C.Value` is private",
        ),
        ("public int Run() { return value; }", "unknown name `value`"),
        (
            "public int Pick(long value) { return 1; } public int Pick(double value) { return 2; } public int Run() { return Pick(1); }",
            "ambiguous",
        ),
    ];
    for (source, expected) in cases {
        let diagnostics = aster_compiler::compile(source).expect_err("invalid program");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains(expected)),
            "missing `{expected}` in {diagnostics:#?}"
        );
    }
}
