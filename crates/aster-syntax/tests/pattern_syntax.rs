//! T-LANG-4b: parsing of `is T x` type patterns and `when` switch guards.

use aster_syntax::{
    BinaryOperator, Expression, ExpressionKind, Item, Statement, TokenKind, lex, parse,
};

fn function_statements(source: &str) -> Vec<Statement> {
    let module = parse(lex(source).expect("source lexes")).expect("source parses");
    let Item::Function(function) = &module.items[0] else {
        panic!("expected a function")
    };
    function
        .body
        .as_ref()
        .expect("function body")
        .statements
        .clone()
}

fn returned(source: &str) -> Expression {
    let Statement::Return {
        value: Some(value), ..
    } = function_statements(source).remove(0)
    else {
        panic!("expected a return statement")
    };
    value
}

#[test]
fn is_and_when_are_not_keywords() {
    let tokens = lex("is when").expect("lexes");
    assert!(matches!(&tokens[0].kind, TokenKind::Identifier(name) if name == "is"));
    assert!(matches!(&tokens[1].kind, TokenKind::Identifier(name) if name == "when"));
}

#[test]
fn parses_a_type_pattern_with_and_without_a_binding() {
    let with_binding = returned("public bool F(Shape s) { return s is Circle c; }");
    let ExpressionKind::TypePattern {
        value,
        target,
        binding,
    } = with_binding.kind
    else {
        panic!("expected a type pattern")
    };
    assert!(matches!(value.kind, ExpressionKind::Name(ref name) if name == "s"));
    assert_eq!(target.name, "Circle");
    assert_eq!(binding.expect("binding").name, "c");

    let bare = returned("public bool F(Shape s) { return s is Circle; }");
    let ExpressionKind::TypePattern { binding, .. } = bare.kind else {
        panic!("expected a type pattern")
    };
    assert!(binding.is_none());
}

#[test]
fn type_pattern_binds_tighter_than_logical_operators_and_looser_than_arithmetic() {
    let expression =
        returned("public bool F(Shape s, int n) { return s is Circle c && n + 1 > 2; }");
    let ExpressionKind::Binary {
        left,
        operator: BinaryOperator::LogicalAnd,
        right,
    } = expression.kind
    else {
        panic!("expected `&&` at the root")
    };
    assert!(matches!(left.kind, ExpressionKind::TypePattern { .. }));
    assert!(matches!(
        right.kind,
        ExpressionKind::Binary {
            operator: BinaryOperator::Greater,
            ..
        }
    ));
}

#[test]
fn parses_generic_pattern_targets() {
    let generic = returned("public bool F(Shape s) { return s is Box<int> b; }");
    let ExpressionKind::TypePattern { target, .. } = generic.kind else {
        panic!("expected a type pattern")
    };
    assert_eq!(target.name, "Box<int>");
}

#[test]
fn parses_guards_on_statement_and_expression_cases() {
    let statements = function_statements(
        "public int F(int v) { switch (v) { case 1 when v > 0: return 1; case Red when v > 2: return 2; default: return 0; } }",
    );
    let Statement::Switch { cases, .. } = &statements[0] else {
        panic!("expected a switch statement")
    };
    assert!(cases[0].guard.is_some());
    assert!(cases[1].guard.is_some());
    assert_eq!(
        cases[1].case_name, "Red",
        "a guarded name keeps the enum pattern"
    );

    let expression = returned(
        "public int F(Message m) { return m switch { Move(x, y) when x > y => 1, Quit => 2, 3 when true => 3, }; }",
    );
    let ExpressionKind::Switch { cases, .. } = expression.kind else {
        panic!("expected a switch expression")
    };
    assert_eq!(cases[0].bindings, ["x", "y"]);
    assert!(cases[0].guard.is_some());
    assert!(cases[1].guard.is_none());
    assert!(cases[2].guard.is_some());
}

#[test]
fn a_guard_may_contain_a_type_pattern() {
    let statements = function_statements(
        "public int F(int v, Shape s) { switch (v) { case 1 when s is Circle c && c.radius > 0: return 1; default: return 0; } }",
    );
    let Statement::Switch { cases, .. } = &statements[0] else {
        panic!("expected a switch statement")
    };
    let guard = cases[0].guard.as_ref().expect("guard");
    assert!(matches!(
        guard.kind,
        ExpressionKind::Binary {
            operator: BinaryOperator::LogicalAnd,
            ..
        }
    ));
}

#[test]
fn the_formatter_keeps_patterns_and_guards_stable() {
    use aster_syntax::format::format_source;
    let source = "public int F(Shape s,int n)\n{\n    if (s is  Circle c&&c.radius>1)\n    {\n        return 1;\n    }\n    switch (n)\n    {\n        case 1  when n>0:\n            return 2;\n        default:\n            return 0;\n    }\n}\n";
    let formatted = format_source(source).expect("formats");
    assert!(
        formatted.contains("s is Circle c && c.radius > 1"),
        "{formatted}"
    );
    assert!(formatted.contains("case 1 when n > 0:"), "{formatted}");
    assert_eq!(format_source(&formatted).expect("formats again"), formatted);
}

#[test]
fn parses_a_checked_downcast_with_as() {
    let cast = returned("public bool F(Shape s) { return s as Circle; }");
    let ExpressionKind::TypeAs { value, target } = cast.kind else {
        panic!("expected a type `as` expression")
    };
    assert!(matches!(value.kind, ExpressionKind::Name(ref name) if name == "s"));
    assert_eq!(target.name, "Circle");
}

#[test]
fn as_binds_like_is_and_is_not_a_keyword() {
    let tokens = lex("as").expect("lexes");
    assert!(matches!(&tokens[0].kind, TokenKind::Identifier(name) if name == "as"));
    // Looser than arithmetic, tighter than `&&`: the left operand of `&&` is the `as`.
    let expression =
        returned("public bool F(Shape s, bool b) { return s as Circle != null && b; }");
    assert!(matches!(
        expression.kind,
        ExpressionKind::Binary {
            operator: BinaryOperator::LogicalAnd,
            ..
        }
    ));
}

#[test]
fn the_formatter_keeps_as_expressions_stable() {
    use aster_syntax::format::format_source;
    let source = "public int F(Shape s)\n{\n    var o = s  as   Circle;\n    return 0;\n}\n";
    let formatted = format_source(source).expect("formats");
    assert!(formatted.contains("s as Circle"), "{formatted}");
    assert_eq!(format_source(&formatted).expect("formats again"), formatted);
}
