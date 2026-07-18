use aster_syntax::{ExpressionKind, Item, Statement, VariableKind, lex, parse};

#[test]
fn parses_named_struct_literal_with_field_spans() {
    let source = "public void Use() { Position value = Position { x: 10, y: 20 }; }";
    let module = parse(lex(source).expect("lexes")).expect("parses");
    let Item::Function(function) = &module.items[0] else {
        panic!("expected function");
    };
    let Statement::Variable(variable) = &function.body.as_ref().unwrap().statements[0] else {
        panic!("expected variable");
    };
    assert!(matches!(variable.kind, VariableKind::Explicit(ref ty) if ty.name == "Position"));
    let ExpressionKind::StructLiteral { type_name, fields } =
        &variable.initializer.as_ref().unwrap().kind
    else {
        panic!("expected struct literal");
    };
    assert_eq!(type_name, "Position");
    assert_eq!(
        fields
            .iter()
            .map(|field| field.name.as_str())
            .collect::<Vec<_>>(),
        ["x", "y"]
    );
    assert_eq!(&source[fields[0].span.start..fields[0].span.end], "x: 10");
}
