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
    fn rejects_base_identifier_and_modifier_misuse() {
        for (source, expected) in [
            ("class A { int base; }", "`base` is a reserved word"),
            ("abstract sealed class A {}", "cannot be both"),
            ("abstract struct S {}", "valid only on a class"),
            ("class A { A() : this() {} }", "constructor chaining"),
            (
                "class A { sealed override int M() { return 1; } }",
                "`sealed override` is not supported",
            ),
            (
                "class A { override sealed int M() { return 1; } }",
                "`sealed override` is not supported",
            ),
            (
                "class A { sealed int M() { return 1; } }",
                "`sealed` applies to classes",
            ),
            (
                "class A { virtual override int M() { return 1; } }",
                "cannot be combined",
            ),
            (
                "class A { virtual virtual int M() { return 1; } }",
                "duplicate `virtual` modifier",
            ),
            (
                "class A { virtual A() {} }",
                "constructors cannot be `virtual`",
            ),
            ("class A { virtual int x; }", "fields cannot be `virtual`"),
            (
                "interface I { virtual int M(); }",
                "interface members cannot have dispatch modifiers",
            ),
            (
                "abstract class A { abstract int M() { return 1; } }",
                "expected `;`",
            ),
            (
                "class A { int M() { return base; } }",
                "`base` must be followed by a member access",
            ),
        ] {
            let errors = module(source).expect_err(source);
            assert!(
                errors.iter().any(|m| m.contains(expected)),
                "{source}: {errors:?}"
            );
        }
    }

    #[test]
    fn parses_virtual_override_and_abstract_members() {
        use aster_syntax::{ExpressionKind, MemberDispatch, Statement};
        let parsed = module(
            "abstract class A { public virtual int V() { return 1; } public abstract int Z(int x); \
             public abstract int P { get; set; } public virtual int Q { get { return 2; } } } \
             class B : A { public override int V() { return base.V() + 1; } \
             public override int Z(int x) { return x; } public override int P { get { return base.P; } } }",
        )
        .expect("virtual syntax");
        let Item::Class(base) = &parsed.items[0] else {
            panic!()
        };
        let dispatch = base
            .members
            .iter()
            .map(|member| match member {
                Member::Method(method) => (method.name.clone(), method.dispatch),
                Member::Property(property) => (property.name.clone(), property.dispatch),
                Member::Field(_) => panic!(),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            dispatch,
            vec![
                ("V".to_owned(), MemberDispatch::Virtual),
                ("Z".to_owned(), MemberDispatch::Abstract),
                ("P".to_owned(), MemberDispatch::Abstract),
                ("Q".to_owned(), MemberDispatch::Virtual),
            ]
        );
        let Member::Method(abstract_method) = &base.members[1] else {
            panic!()
        };
        assert!(abstract_method.body.is_none());
        let Member::Property(abstract_property) = &base.members[2] else {
            panic!()
        };
        assert!(abstract_property.getter.is_some() && abstract_property.setter.is_some());
        let Item::Class(derived) = &parsed.items[1] else {
            panic!()
        };
        let Member::Method(overriding) = &derived.members[0] else {
            panic!()
        };
        assert_eq!(overriding.dispatch, MemberDispatch::Override);
        let Statement::Return {
            value: Some(value), ..
        } = &overriding.body.as_ref().unwrap().statements[0]
        else {
            panic!()
        };
        let ExpressionKind::Binary { left, .. } = &value.kind else {
            panic!()
        };
        let ExpressionKind::Call { callee, .. } = &left.kind else {
            panic!()
        };
        let ExpressionKind::Member { object, .. } = &callee.kind else {
            panic!()
        };
        assert!(matches!(object.kind, ExpressionKind::Base));
    }

    #[test]
    fn dispatch_modifiers_stay_usable_as_identifiers() {
        assert!(
            module("class A { int virtual; int Use() { return virtual; } }").is_ok(),
            "an ordinary field named `virtual` keeps parsing"
        );
    }

    #[test]
    fn abstract_and_sealed_stay_usable_as_identifiers() {
        assert!(module("int abstract = 1; int Use() { return abstract; }").is_ok());
    }
}
