use aster_syntax::{ExpressionKind, Item, Statement, VariableKind, lex, parse};

#[test]
fn parses_array_types_literals_creation_and_indexing() {
    let source = "public int Run() { int[] a = [1, 2]; int[] b = new int[3]; b[0] = a[1]; return b.Length; }";
    let module = parse(lex(source).expect("lexing")).expect("array syntax should parse");
    let Item::Function(function) = &module.items[0] else {
        panic!("function")
    };
    let body = function.body.as_ref().unwrap();
    let Statement::Variable(first) = &body.statements[0] else {
        panic!("variable")
    };
    assert!(matches!(&first.kind, VariableKind::Explicit(type_) if type_.name == "int[]"));
    assert!(matches!(
        first.initializer.as_ref().unwrap().kind,
        ExpressionKind::ArrayLiteral(_)
    ));
    let Statement::Variable(second) = &body.statements[1] else {
        panic!("variable")
    };
    assert!(matches!(
        second.initializer.as_ref().unwrap().kind,
        ExpressionKind::NewArray { .. }
    ));
    let Statement::Expression(assignment) = &body.statements[2] else {
        panic!("assignment")
    };
    assert!(matches!(assignment.kind, ExpressionKind::Assignment { .. }));
}
