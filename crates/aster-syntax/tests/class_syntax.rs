use aster_syntax::{ExpressionKind, Item, Member, Statement, lex, parse};

#[test]
fn parses_constructor_new_methods_and_this() {
    let source = "public class C { private int value; public C(int value) { this.value = value; } public int Get() { return value; } } public int Run() { C c = new C(1); return c.Get(); }";
    let module = parse(lex(source).expect("lexing")).expect("class syntax");
    let Item::Class(class) = &module.items[0] else {
        panic!("class")
    };
    let Member::Method(constructor) = &class.members[1] else {
        panic!("constructor")
    };
    assert!(constructor.constructor);
    let body = constructor.body.as_ref().unwrap();
    let Statement::Expression(assignment) = &body.statements[0] else {
        panic!("assignment")
    };
    let ExpressionKind::Assignment { target, .. } = &assignment.kind else {
        panic!("assignment")
    };
    assert!(
        matches!(target.kind, ExpressionKind::Member { ref object, .. } if matches!(object.kind, ExpressionKind::This))
    );
    let Item::Function(run) = &module.items[1] else {
        panic!("run")
    };
    let Statement::Variable(variable) = &run.body.as_ref().unwrap().statements[0] else {
        panic!("variable")
    };
    assert!(matches!(
        variable.initializer.as_ref().unwrap().kind,
        ExpressionKind::NewObject { .. }
    ));
}

mod inheritance_syntax {
    use aster_syntax::{Item, Member, lex, parse};

    fn module(source: &str) -> Result<aster_syntax::Module, Vec<String>> {
        parse(lex(source).expect("lexes")).map_err(|d| d.into_iter().map(|d| d.message).collect())
    }

    #[test]
    fn parses_base_list_modifiers_and_base_initializer() {
        let parsed = module(
            "public abstract class A { protected A(int x) {} } \
             public sealed class B : A, IThing { public B() : base(1 + 2, x: 3) {} }",
        )
        .expect("parses");
        let Item::Class(a) = &parsed.items[0] else {
            panic!()
        };
        assert!(a.is_abstract && !a.is_sealed);
        let Item::Class(b) = &parsed.items[1] else {
            panic!()
        };
        assert!(b.is_sealed && !b.is_abstract);
        assert_eq!(b.interfaces.len(), 2);
        let Member::Method(ctor) = &b.members[0] else {
            panic!()
        };
        assert_eq!(
            ctor.base_call.as_ref().expect("base call").arguments.len(),
            2
        );
    }

    #[test]
    fn rejects_virtual_members_base_identifier_and_modifier_misuse() {
        for (source, expected) in [
            (
                "class A { virtual int M() { return 1; } }",
                "`virtual` members are not supported yet",
            ),
            (
                "class A { override int M() { return 1; } }",
                "`override` members are not supported yet",
            ),
            ("class A { int base; }", "`base` is a reserved word"),
            ("abstract sealed class A {}", "cannot be both"),
            ("abstract struct S {}", "valid only on a class"),
            ("class A { A() : this() {} }", "constructor chaining"),
        ] {
            let errors = module(source).expect_err(source);
            assert!(
                errors.iter().any(|m| m.contains(expected)),
                "{source}: {errors:?}"
            );
        }
    }

    #[test]
    fn abstract_and_sealed_stay_usable_as_identifiers() {
        assert!(module("int abstract = 1; int Use() { return abstract; }").is_ok());
    }
}
