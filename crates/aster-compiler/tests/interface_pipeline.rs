use aster_hir as hir;
use aster_mir as mir;

const VALID: &str = "public interface IScore { int Value(); } public class Counter : IScore { private int score; public Counter(int score) { this.score = score; } public int Value() { return score; } } internal int Read(IScore item) { return item.Value(); } public int Run() { Counter counter = new Counter(42); return Read(counter); }";

#[test]
fn interface_symbols_upcasts_and_dispatch_reach_hir_and_mir() {
    let compilation = aster_compiler::compile(VALID).expect("valid interface program");
    let hir::Item::Interface(interface) = &compilation.hir.items[0] else {
        panic!("interface")
    };
    let hir::Item::Class(class) = &compilation.hir.items[1] else {
        panic!("class")
    };
    assert_eq!(class.interfaces, [interface.symbol]);
    assert!(compilation.hir.items.iter().any(|item| {
        let hir::Item::Function(function) = item else {
            return false;
        };
        function.body.as_ref().is_some_and(|body| {
            body.statements.iter().any(|statement| {
                matches!(
                    statement,
                    hir::Statement::Return(Some(hir::Expression {
                        kind: hir::ExpressionKind::Call { arguments, .. },
                        ..
                    })) if arguments.iter().any(|argument| matches!(argument.kind, hir::ExpressionKind::UpcastInterface { .. }))
                )
            })
        })
    }));

    assert_eq!(compilation.mir.interfaces.len(), 1);
    assert_eq!(compilation.mir.interface_implementations.len(), 1);
    assert!(compilation.mir.functions.iter().any(|function| {
        function
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .any(|instruction| matches!(instruction, mir::Instruction::CallInterface { .. }))
    }));
    assert!(compilation.mir.functions.iter().any(|function| {
        function
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .any(|instruction| {
                matches!(
                    instruction,
                    mir::Instruction::Assign {
                        value: mir::Rvalue {
                            kind: mir::RvalueKind::MakeInterface { .. },
                            ..
                        },
                        ..
                    }
                )
            })
    }));
}

#[test]
fn invalid_interface_programs_have_specific_diagnostics() {
    for (source, expected) in [
        (
            "public interface I { int Value(); } public class C : I { public C() {} }",
            "does not implement required method `I.Value`",
        ),
        (
            "public interface I { int Value(); } public class C : I { public C() {} public bool Value() { return true; } }",
            "does not match interface `I`",
        ),
        (
            "public interface I { int Value(); } public class C : I { public C() {} private int Value() { return 1; } }",
            "must be public",
        ),
        (
            "public class A { public A() {} } public class B : A { public B() {} }",
            "not an interface",
        ),
        (
            "public interface I { int Value(); } public class C : I, I { public C() {} public int Value() { return 1; } }",
            "more than once",
        ),
        (
            "public interface I { int Value(); } public class C { public C() {} public int Value() { return 1; } } public int Read(I value) { return value.Value(); } public int Run() { return Read(new C()); }",
            "expected `I`, found `C`",
        ),
        (
            "public interface I { int Value(); } public int Run() { I value = new I(); return 0; }",
            "`new` requires a class",
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
