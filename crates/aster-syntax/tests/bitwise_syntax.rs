use aster_syntax::{
    AssignmentOperator, BinaryOperator, Expression, ExpressionKind, Item, Literal, Statement,
    TokenKind, UnaryOperator, lex, parse,
};

fn return_value(source: &str) -> Expression {
    let module = parse(lex(source).expect("source should lex")).expect("source should parse");
    let Item::Function(function) = &module.items[0] else {
        panic!("expected a leading function")
    };
    let Statement::Return {
        value: Some(value), ..
    } = &function.body.as_ref().expect("function body").statements[0]
    else {
        panic!("expected a return statement")
    };
    value.clone()
}

fn parse_error(source: &str) -> bool {
    lex(source).is_ok_and(|tokens| parse(tokens).is_err())
}

fn binary_symbol(operator: BinaryOperator) -> &'static str {
    match operator {
        BinaryOperator::Multiply => "*",
        BinaryOperator::Divide => "/",
        BinaryOperator::Remainder => "%",
        BinaryOperator::Add => "+",
        BinaryOperator::Subtract => "-",
        BinaryOperator::Less => "<",
        BinaryOperator::LessEqual => "<=",
        BinaryOperator::Greater => ">",
        BinaryOperator::GreaterEqual => ">=",
        BinaryOperator::Equal => "==",
        BinaryOperator::NotEqual => "!=",
        BinaryOperator::LogicalAnd => "&&",
        BinaryOperator::LogicalOr => "||",
        BinaryOperator::BitwiseAnd => "&",
        BinaryOperator::BitwiseOr => "|",
        BinaryOperator::BitwiseXor => "^",
        BinaryOperator::ShiftLeft => "<<",
        BinaryOperator::ShiftRight => ">>",
        BinaryOperator::UnsignedShiftRight => ">>>",
    }
}

/// A fully parenthesized rendering that exposes the parse tree.
fn show(expression: &Expression) -> String {
    match &expression.kind {
        ExpressionKind::Binary {
            left,
            operator,
            right,
        } => format!(
            "({} {} {})",
            show(left),
            binary_symbol(*operator),
            show(right)
        ),
        ExpressionKind::Unary { operator, operand } => {
            let symbol = match operator {
                UnaryOperator::Not => "!",
                UnaryOperator::Negate => "-",
                UnaryOperator::BitwiseNot => "~",
            };
            format!("({symbol}{})", show(operand))
        }
        ExpressionKind::Name(name) => name.clone(),
        ExpressionKind::Literal(Literal::Integer(text)) => text.clone(),
        ExpressionKind::Cast { target, operand } => format!("(({}){})", target.name, show(operand)),
        ExpressionKind::Try { operand } => format!("({}?)", show(operand)),
        ExpressionKind::Call { .. } => "call".to_owned(),
        other => panic!("unexpected expression in a precedence test: {other:?}"),
    }
}

fn tree(expression: &str) -> String {
    show(&return_value(&format!(
        "public int F() {{ return {expression}; }}"
    )))
}

#[test]
fn lexes_the_new_operators_and_never_merges_right_angle_brackets() {
    let kinds = |source: &str| -> Vec<TokenKind> {
        lex(source)
            .expect("source should lex")
            .into_iter()
            .map(|token| token.kind)
            .take_while(|kind| *kind != TokenKind::Eof)
            .collect()
    };
    assert_eq!(
        kinds("& | ^ ~ << &= |= ^= <<="),
        [
            TokenKind::Ampersand,
            TokenKind::Pipe,
            TokenKind::Caret,
            TokenKind::Tilde,
            TokenKind::LessLess,
            TokenKind::AmpersandEqual,
            TokenKind::PipeEqual,
            TokenKind::CaretEqual,
            TokenKind::LessLessEqual,
        ]
    );
    // `&&` and `||` are unchanged.
    assert_eq!(
        kinds("a && b || c"),
        [
            TokenKind::Identifier("a".into()),
            TokenKind::AndAnd,
            TokenKind::Identifier("b".into()),
            TokenKind::OrOr,
            TokenKind::Identifier("c".into()),
        ]
    );
    // Right angle brackets stay separate so nested generics can close.
    assert_eq!(
        kinds(">>>"),
        [TokenKind::Greater, TokenKind::Greater, TokenKind::Greater]
    );
    assert_eq!(kinds(">>="), [TokenKind::Greater, TokenKind::GreaterEqual]);
    assert_eq!(
        kinds(">>>="),
        [
            TokenKind::Greater,
            TokenKind::Greater,
            TokenKind::GreaterEqual
        ]
    );
}

#[test]
fn bitwise_and_shift_precedence_follows_csharp() {
    assert_eq!(tree("a | b ^ c & d"), "(a | (b ^ (c & d)))");
    assert_eq!(tree("a & b == c"), "(a & (b == c))");
    assert_eq!(tree("a == b & c != d"), "((a == b) & (c != d))");
    assert_eq!(tree("a << 1 + 2"), "(a << (1 + 2))");
    assert_eq!(tree("a + 1 << 2"), "((a + 1) << 2)");
    assert_eq!(tree("a < b << c"), "(a < (b << c))");
    assert_eq!(tree("a << b < c"), "((a << b) < c)");
    assert_eq!(tree("a >> b >> c"), "((a >> b) >> c)");
    assert_eq!(tree("a >>> 1 >> 2 << 3"), "(((a >>> 1) >> 2) << 3)");
    assert_eq!(tree("a || b | c && d"), "(a || ((b | c) && d))");
    assert_eq!(tree("a | b && c ^ d"), "((a | b) && (c ^ d))");
    assert_eq!(tree("~a & b"), "((~a) & b)");
    assert_eq!(tree("-a << 1"), "((-a) << 1)");
    assert_eq!(tree("~-a"), "(~(-a))");
    assert_eq!(tree("(int)~a"), "((int)(~a))");
    assert_eq!(tree("a * b << c"), "((a * b) << c)");
}

#[test]
fn right_shift_and_comparison_operators_stay_distinct() {
    assert_eq!(tree("a > b"), "(a > b)");
    assert_eq!(tree("a >= b"), "(a >= b)");
    assert_eq!(tree("a >> b > c"), "((a >> b) > c)");
    assert_eq!(tree("a > b >> c"), "(a > (b >> c))");
    assert_eq!(tree("a >> b >= c"), "((a >> b) >= c)");
    assert_eq!(tree("a >>> b >= c"), "((a >>> b) >= c)");
    assert_eq!(tree("a < b"), "(a < b)");
    assert_eq!(tree("a <= b"), "(a <= b)");
}

#[test]
fn compound_bitwise_assignments_parse_to_their_operators() {
    for (text, expected) in [
        ("&=", AssignmentOperator::BitwiseAndAssign),
        ("|=", AssignmentOperator::BitwiseOrAssign),
        ("^=", AssignmentOperator::BitwiseXorAssign),
        ("<<=", AssignmentOperator::ShiftLeftAssign),
        (">>=", AssignmentOperator::ShiftRightAssign),
        (">>>=", AssignmentOperator::UnsignedShiftRightAssign),
    ] {
        let value = return_value(&format!("public int F() {{ return x {text} 1 << 2; }}"));
        let ExpressionKind::Assignment {
            operator, value, ..
        } = &value.kind
        else {
            panic!("expected an assignment for `{text}`")
        };
        assert_eq!(*operator, expected, "{text}");
        // The right-hand side is a full expression.
        assert_eq!(show(value), "(1 << 2)", "{text}");
    }
    // Compound assignment is right-associative like `=`.
    let nested = return_value("public int F() { return x |= y &= 3; }");
    let ExpressionKind::Assignment { value, .. } = &nested.kind else {
        panic!("expected an assignment")
    };
    assert!(matches!(value.kind, ExpressionKind::Assignment { .. }));
}

#[test]
fn nested_generics_still_parse_next_to_shift_expressions() {
    let source = "
        public int F(int c)
        {
            List<List<int>> a = new List<List<int>>();
            Dictionary<string, List<int>> d = new Dictionary<string, List<int>>();
            List<List<List<int>>> deep = new List<List<List<int>>>();
            int b = c >> 1;
            int e = c >>> 2;
            int f = c >> 1 >> 1;
            bool g = c < b >> 2;
            b >>= 1;
            b >>>= 1;
            return Pick<List<int>>(a) >> 1;
        }
    ";
    let module = parse(lex(source).expect("lexes")).expect("nested generics and shifts parse");
    assert_eq!(module.items.len(), 1);
}

#[test]
fn shifts_require_adjacent_angle_brackets() {
    assert!(parse_error(
        "public int F(int a, int b) { return a > > b; }"
    ));
    assert!(parse_error(
        "public int F(int a, int b) { return a >> > b; }"
    ));
    assert!(parse_error(
        "public int F(int a, int b) { return a >>>> b; }"
    ));
    assert!(parse_error(
        "public int F(int a, int b) { a > >= b; return a; }"
    ));
    assert!(parse_error(
        "public int F(int a, int b) { a >> >= b; return a; }"
    ));
}

#[test]
fn postfix_try_composes_with_bitwise_operators() {
    assert_eq!(tree("fetch()? & 3"), "((call?) & 3)");
    assert_eq!(tree("fetch()? | 3"), "((call?) | 3)");
    assert_eq!(tree("fetch()? ^ 3"), "((call?) ^ 3)");
    assert_eq!(tree("fetch()? << 2"), "((call?) << 2)");
    assert_eq!(tree("fetch()? >> 2"), "((call?) >> 2)");
    assert_eq!(tree("fetch()? >>> 2"), "((call?) >>> 2)");
    // A trailing `?` followed by `~` still starts a conditional consequent.
    assert!(matches!(
        return_value("public int F(bool c) { return c ? ~a : b; }").kind,
        ExpressionKind::Conditional { .. }
    ));
}

#[test]
fn lone_ampersand_and_pipe_are_operators_not_lexical_errors() {
    let tokens = lex("a & b | c").expect("single `&` and `|` now lex");
    assert!(
        tokens
            .iter()
            .any(|token| token.kind == TokenKind::Ampersand)
    );
    assert!(tokens.iter().any(|token| token.kind == TokenKind::Pipe));
    // A dangling operator is a controlled parse error.
    assert!(parse_error("public int F(int a) { return a &; }"));
    assert!(parse_error("public int F(int a) { return | a; }"));
}
