//! T-LANG-8 (LANG-008): `new T(args) { A = 1 }` syntax.

use aster_syntax::{ExpressionKind, Item, Statement, lex, parse};

fn initializer(source: &str) -> ExpressionKind {
    let module = parse(lex(source).expect("lexes")).expect("parses");
    let Item::Function(run) = &module.items[0] else {
        panic!("function")
    };
    let Statement::Variable(variable) = &run.body.as_ref().unwrap().statements[0] else {
        panic!("variable")
    };
    variable.initializer.as_ref().unwrap().kind.clone()
}

fn error(source: &str) -> Vec<String> {
    parse(lex(source).expect("lexes"))
        .expect_err("must not parse")
        .into_iter()
        .map(|diagnostic| diagnostic.message)
        .collect()
}

#[test]
fn parses_members_in_source_order_after_arguments() {
    let ExpressionKind::ObjectInitializer { creation, members } =
        initializer("public void Run() { var c = new C(1, 2) { X = 3, Y = 4 + 5, }; }")
    else {
        panic!("object initializer")
    };
    assert!(matches!(
        creation.kind,
        ExpressionKind::NewObject { ref arguments, .. } if arguments.len() == 2
    ));
    let names: Vec<_> = members.iter().map(|member| member.name.as_str()).collect();
    assert_eq!(names, ["X", "Y"]);
}

#[test]
fn parses_target_typed_and_empty_initializers() {
    assert!(matches!(
        initializer("public void Run() { C c = new() { X = 1 }; }"),
        ExpressionKind::ObjectInitializer { .. }
    ));
    assert!(matches!(
        initializer("public void Run() { var c = new C { }; }"),
        ExpressionKind::ObjectInitializer { ref members, .. } if members.is_empty()
    ));
}

#[test]
fn a_creation_without_braces_is_unchanged() {
    assert!(matches!(
        initializer("public void Run() { var c = new C(1); }"),
        ExpressionKind::NewObject { .. }
    ));
}

#[test]
fn nested_and_collection_initializers_are_not_supported_yet() {
    assert!(
        error("public void Run() { var c = new C() { P = { X = 1 } }; }")
            .iter()
            .any(|message| message.contains("not supported yet"))
    );
    assert!(
        error("public void Run() { var c = new C() { A, B }; }")
            .iter()
            .any(|message| message.contains("expected `=`"))
    );
}
