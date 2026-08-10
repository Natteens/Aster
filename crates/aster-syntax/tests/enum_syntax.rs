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
fn parses_restricted_enum_switch_expression() {
    let source = "public enum Message { Quit, Move(int x, int y), } public int Read(Message message) { return message switch { Quit => 0, Move(x, y) => x + y, }; }";
    let module = parse(lex(source).expect("switch expression should lex"))
        .expect("switch expression should parse");
    let Item::Function(read) = &module.items[1] else {
        panic!("expected Read function")
    };
    let Statement::Return {
        value: Some(value), ..
    } = &read.body.as_ref().expect("function body").statements[0]
    else {
        panic!("expected return")
    };
    let ExpressionKind::Switch {
        value: selected,
        cases,
        default,
    } = &value.kind
    else {
        panic!("expected switch expression")
    };
    assert!(matches!(&selected.kind, ExpressionKind::Name(name) if name == "message"));
    assert_eq!(cases.len(), 2);
    assert_eq!(cases[0].case_name, "Quit");
    assert_eq!(cases[1].bindings, ["x", "y"]);
    assert!(default.is_none());
}

#[test]
fn switch_expression_requires_fat_arrows_and_commas() {
    let missing_arrow = parse(
        lex("public enum E { A } public int Read(E value) { return value switch { A: 1 }; }")
            .expect("source lexes"),
    )
    .expect_err("colon is not an expression arm arrow");
    assert!(
        missing_arrow
            .iter()
            .any(|diagnostic| diagnostic.message.contains("expected `=>`"))
    );

    let missing_comma = parse(lex(
        "public enum E { A, B } public int Read(E value) { return value switch { A => 1 B => 2 }; }",
    ).expect("source lexes")).expect_err("arms require commas");
    assert!(
        missing_comma
            .iter()
            .any(|diagnostic| diagnostic.message.contains("expected `,`"))
    );
}

#[test]
fn parses_default_and_switch_expression_composition() {
    let source = "public enum E { A, B } public int Take(int value) { return value; } public int Read(E value) { return Take(value switch { A => 42, default => 0, }); }";
    let module = parse(lex(source).expect("composed switch expression lexes"))
        .expect("default and function-argument composition parse");
    let Item::Function(read) = &module.items[2] else {
        panic!("expected Read function")
    };
    let Statement::Return {
        value: Some(value), ..
    } = &read.body.as_ref().expect("function body").statements[0]
    else {
        panic!("expected return")
    };
    let ExpressionKind::Call { arguments, .. } = &value.kind else {
        panic!("expected composed function call")
    };
    let ExpressionKind::Switch { default, .. } = &arguments[0].kind else {
        panic!("expected switch argument")
    };
    assert!(default.is_some());
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

#[test]
fn rejects_unreachable_switch_expression_arm_after_default() {
    let source = "public enum E { A, B } public int Read(E value) { return value switch { default => 0, A => 1, }; }";
    let diagnostics = parse(lex(source).expect("source should lex")).expect_err("unreachable arm");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.message == "switch expression arm after `default` is unreachable"
            && diagnostic.help.as_deref() == Some("move `default` to the final arm")
    }));
}
