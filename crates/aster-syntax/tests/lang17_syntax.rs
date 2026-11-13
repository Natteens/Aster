//! LANG-017: `do`-`while`, `%=`, block comments, exponent literals and digit
//! separators at the lexer and parser level.

use aster_syntax::{AssignmentOperator, ExpressionKind, Item, Statement, TokenKind, lex, parse};

fn kinds(source: &str) -> Vec<TokenKind> {
    lex(source)
        .expect("source lexes")
        .into_iter()
        .map(|token| token.kind)
        .filter(|kind| *kind != TokenKind::Eof)
        .collect()
}

fn lex_message(source: &str) -> String {
    let diagnostics = lex(source).expect_err("source must not lex");
    diagnostics[0].message.clone()
}

fn first_statement(body: &str) -> Statement {
    let source = format!("public void F() {{ {body} }}");
    let module = parse(lex(&source).expect("lexes")).expect("parses");
    let Item::Function(function) = &module.items[0] else {
        panic!("function expected")
    };
    function.body.as_ref().expect("body").statements[0].clone()
}

#[test]
fn do_while_parses_with_body_and_condition() {
    let Statement::DoWhile {
        body, condition, ..
    } = first_statement("do { x = x + 1; } while (x < 3);")
    else {
        panic!("do-while expected")
    };
    assert_eq!(body.statements.len(), 1);
    assert!(matches!(condition.kind, ExpressionKind::Binary { .. }));
}

#[test]
fn nested_do_while_parses() {
    let Statement::DoWhile { body, .. } =
        first_statement("do { do { break; } while (true); continue; } while (false);")
    else {
        panic!("do-while expected")
    };
    assert!(matches!(body.statements[0], Statement::DoWhile { .. }));
}

#[test]
fn do_while_requires_semicolon_while_and_block() {
    for source in [
        "public void F() { do { } while (true) }",
        "public void F() { do { } }",
        "public void F() { do x = 1; while (true); }",
        "public void F() { do { } while true; }",
    ] {
        assert!(
            lex(source).is_ok_and(|tokens| parse(tokens).is_err()),
            "{source}"
        );
    }
}

#[test]
fn percent_equal_is_one_token_and_a_remainder_assignment() {
    assert_eq!(
        kinds("a %= 2"),
        vec![
            TokenKind::Identifier("a".into()),
            TokenKind::PercentEqual,
            TokenKind::IntegerLiteral("2".into())
        ]
    );
    assert_eq!(kinds("a % = 2")[1], TokenKind::Percent);
    let Statement::Expression(expression) = first_statement("a %= 2;") else {
        panic!("expression statement expected")
    };
    let ExpressionKind::Assignment { operator, .. } = expression.kind else {
        panic!("assignment expected")
    };
    assert_eq!(operator, AssignmentOperator::RemainderAssign);
}

#[test]
fn block_comments_are_skipped_everywhere() {
    assert_eq!(kinds("/* a */"), vec![]);
    assert_eq!(
        kinds("a/* x */+/* y\nz */b"),
        vec![
            TokenKind::Identifier("a".into()),
            TokenKind::Plus,
            TokenKind::Identifier("b".into())
        ]
    );
    // Does not nest: the first `*/` closes it, and `//` inside is inert.
    assert_eq!(
        kinds("/* /* inner */ a // */\n"),
        vec![TokenKind::Identifier("a".into())]
    );
    assert_eq!(
        kinds("/**/ /***/ /* * / */ b"),
        vec![TokenKind::Identifier("b".into())]
    );
    assert_eq!(kinds("a /= 2")[1], TokenKind::SlashEqual);
}

#[test]
fn block_comment_marker_in_a_string_is_text() {
    assert_eq!(
        kinds("\"/* not a comment */\""),
        vec![TokenKind::StringLiteral("/* not a comment */".into())]
    );
    assert_eq!(
        kinds("// line /* still line\na"),
        vec![TokenKind::Identifier("a".into())]
    );
}

#[test]
fn unterminated_block_comment_is_a_diagnostic() {
    assert_eq!(
        lex_message("a /* never closed"),
        "unterminated block comment"
    );
    assert_eq!(lex_message("/*"), "unterminated block comment");
    // No nesting: the text after the first `*/` is ordinary code.
    assert_eq!(
        kinds("/* a /* b */ c */"),
        vec![
            TokenKind::Identifier("c".into()),
            TokenKind::Star,
            TokenKind::Slash
        ]
    );
    assert_eq!(lex_message("/*/"), "unterminated block comment");
}

#[test]
fn digit_separators_are_stripped_from_decimal_and_prefixed_literals() {
    assert_eq!(
        kinds("1_000_000"),
        vec![TokenKind::IntegerLiteral("1000000".into())]
    );
    assert_eq!(kinds("1__0"), vec![TokenKind::IntegerLiteral("10".into())]);
    assert_eq!(
        kinds("0xFF_FF"),
        vec![TokenKind::IntegerLiteral("65535".into())]
    );
    assert_eq!(
        kinds("0b1010_1010"),
        vec![TokenKind::IntegerLiteral("170".into())]
    );
    assert_eq!(
        kinds("0x_FF"),
        vec![TokenKind::IntegerLiteral("255".into())]
    );
    assert_eq!(kinds("0b_1"), vec![TokenKind::IntegerLiteral("1".into())]);
    assert_eq!(kinds("1_0L"), vec![TokenKind::LongLiteral("10".into())]);
    assert_eq!(
        kinds("0xFF_FFu"),
        vec![TokenKind::UIntLiteral("65535".into())]
    );
    assert_eq!(
        kinds("1_0.2_5"),
        vec![TokenKind::FloatLiteral("10.25".into())]
    );
    assert_eq!(
        kinds("1_0.2_5d"),
        vec![TokenKind::DoubleLiteral("10.25".into())]
    );
}

#[test]
fn exponent_makes_the_literal_real() {
    assert_eq!(kinds("1e3"), vec![TokenKind::FloatLiteral("1e3".into())]);
    assert_eq!(
        kinds("2.5E-3"),
        vec![TokenKind::FloatLiteral("2.5E-3".into())]
    );
    assert_eq!(kinds("1E+3"), vec![TokenKind::FloatLiteral("1E+3".into())]);
    assert_eq!(
        kinds("1_0e1_0"),
        vec![TokenKind::FloatLiteral("10e10".into())]
    );
    assert_eq!(kinds("1e3f"), vec![TokenKind::FloatLiteral("1e3".into())]);
    assert_eq!(kinds("1e3d"), vec![TokenKind::DoubleLiteral("1e3".into())]);
    assert_eq!(kinds("1e3m"), vec![TokenKind::DecimalLiteral("1e3".into())]);
    assert!(lex("1e3L").is_err());
    assert!(lex("1e3u").is_err());
}

#[test]
fn malformed_separators_and_exponents_are_rejected() {
    // `_1` is an identifier in the lexer, never a number.
    assert_eq!(kinds("_1"), vec![TokenKind::Identifier("_1".into())]);
    for source in [
        "1_", "1_.5", "1._5", "1.5_", "1_e3", "1e_3", "1e3_", "1e+_3", "1e", "1e+", "1e-", "1_f",
        "1_L", "0x_", "0xFF_", "0b1_", "0xFF_u", "0x", "1.5e",
    ] {
        assert!(lex(source).is_err(), "`{source}` must be rejected");
    }
    assert!(lex_message("1e").contains("exponent has no digits"));
    assert!(lex_message("1_").contains("digit separator"));
    assert!(lex_message("1e_3").contains("digit separator"));
}
