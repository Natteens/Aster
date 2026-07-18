use aster_syntax::{ExpressionKind, Item, Statement, TokenKind, Visibility, lex, parse};

#[test]
fn lexes_enum_and_switch_keywords() {
    let tokens = lex("enum switch case default").expect("enum keywords should lex");

    assert_eq!(tokens[0].kind, TokenKind::Enum);
    assert_eq!(tokens[1].kind, TokenKind::Switch);
    assert_eq!(tokens[2].kind, TokenKind::Case);
    assert_eq!(tokens[3].kind, TokenKind::Default);
    assert_eq!(tokens[4].kind, TokenKind::Eof);
}

#[test]
fn parses_simple_and_generic_payload_enums() {
    let source =
        "public enum Direction { North, South } public enum Option<T> { None, Some(T value) }";
    let module = parse(lex(source).expect("enum declarations should lex"))
        .expect("enum declarations should parse");

    let Item::Enum(direction) = &module.items[0] else {
        panic!("expected Direction enum")
    };
    assert_eq!(direction.visibility, Visibility::Public);
    assert!(direction.type_parameters.is_empty());
    assert_eq!(direction.cases.len(), 2);
    assert_eq!(direction.cases[0].name, "North");
    assert!(direction.cases[0].fields.is_empty());

    let Item::Enum(option) = &module.items[1] else {
        panic!("expected Option enum")
    };
    assert_eq!(option.type_parameters[0].name, "T");
    assert_eq!(option.cases.len(), 2);
    assert_eq!(option.cases[1].name, "Some");
    assert_eq!(option.cases[1].fields[0].type_ref.name, "T");
    assert_eq!(option.cases[1].fields[0].name, "value");
}

#[test]
fn parses_qualified_generic_variant_construction() {
    let source = "public enum Option<T> { None, Some(T value) } public Option<int> Make() { return Option<int>.Some(42); }";
    let module = parse(lex(source).expect("generic enum construction should lex"))
        .expect("generic enum construction should parse");
    let Item::Function(make) = &module.items[1] else {
        panic!("expected Make function")
    };
    let Statement::Return {
        value: Some(value), ..
    } = &make.body.as_ref().expect("function body").statements[0]
    else {
        panic!("expected returned variant construction")
    };
    let ExpressionKind::Call {
        callee, arguments, ..
    } = &value.kind
    else {
        panic!("expected variant call")
    };
    assert_eq!(arguments.len(), 1);
    let ExpressionKind::Member { object, name } = &callee.kind else {
        panic!("expected qualified variant member")
    };
    assert_eq!(name, "Some");
    assert!(matches!(
        &object.kind,
        ExpressionKind::Name(type_name) if type_name == "Option<int>"
    ));
}

#[test]
fn parses_short_qualified_and_default_switch_cases() {
    let source = "public enum Option<T> { None, Some(T value) } public int Read(Option<int> value) { switch (value) { case Some(number): return number; case Option.None: return 0; default: return -1; } }";
    let module = parse(lex(source).expect("switch should lex")).expect("switch should parse");
    let Item::Function(read) = &module.items[1] else {
        panic!("expected Read function")
    };
    let Statement::Switch { cases, default, .. } =
        &read.body.as_ref().expect("function body").statements[0]
    else {
        panic!("expected switch statement")
    };

    assert_eq!(cases.len(), 2);
    assert_eq!(cases[0].enum_name, None);
    assert_eq!(cases[0].case_name, "Some");
    assert_eq!(cases[0].bindings, ["number"]);
    assert_eq!(cases[0].body.statements.len(), 1);
    assert_eq!(cases[1].enum_name.as_deref(), Some("Option"));
    assert_eq!(cases[1].case_name, "None");
    assert!(cases[1].bindings.is_empty());
    assert_eq!(cases[1].body.statements.len(), 1);
    assert_eq!(default.as_ref().expect("default case").statements.len(), 1);
}

#[test]
fn legacy_module_and_import_keep_their_migration_diagnostics() {
    for (source, message, help) in [
        (
            "module app; public enum Status { Ready }",
            "`module` was replaced by `namespace`",
            "write `namespace name;` at the beginning of the file",
        ),
        (
            "import app.values; public enum Status { Ready }",
            "`import` was replaced by `using`",
            "write `using namespace.name;` before declarations",
        ),
    ] {
        let diagnostics =
            parse(lex(source).expect("legacy source should still lex")).expect_err("legacy syntax");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.message == message && diagnostic.help.as_deref() == Some(help)
        }));
    }
}
