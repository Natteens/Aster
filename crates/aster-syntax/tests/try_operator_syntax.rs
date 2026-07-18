use aster_syntax::{Expression, ExpressionKind, Item, Statement, TokenKind, lex, parse};

fn first_return(source: &str) -> Expression {
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

fn first_variable_initializer(source: &str) -> Expression {
    let module = parse(lex(source).expect("source should lex")).expect("source should parse");
    let Item::Function(function) = &module.items[0] else {
        panic!("expected a leading function")
    };
    let Statement::Variable(declaration) = &function.body.as_ref().expect("body").statements[0]
    else {
        panic!("expected a variable declaration")
    };
    declaration
        .initializer
        .clone()
        .expect("variable has an initializer")
}

#[test]
fn lexes_question_token() {
    let tokens = lex("value?").expect("`?` should lex");
    assert_eq!(tokens[1].kind, TokenKind::Question);
}

#[test]
fn parses_postfix_try_after_call() {
    let value = first_return("public int F() { return fetch()?; }");
    let ExpressionKind::Try { operand } = &value.kind else {
        panic!("expected a postfix try")
    };
    assert!(matches!(operand.kind, ExpressionKind::Call { .. }));
}

#[test]
fn parses_try_after_member_access() {
    let value = first_return("public int F() { return holder.result?; }");
    let ExpressionKind::Try { operand } = &value.kind else {
        panic!("expected a postfix try")
    };
    assert!(matches!(operand.kind, ExpressionKind::Member { .. }));
}

#[test]
fn parses_try_inside_call_argument() {
    let value = first_return("public int F() { return apply(fetch()?); }");
    let ExpressionKind::Call { arguments, .. } = &value.kind else {
        panic!("expected an enclosing call")
    };
    assert!(matches!(arguments[0].kind, ExpressionKind::Try { .. }));
}

#[test]
fn try_binds_tighter_than_addition() {
    let value = first_return("public int F() { return fetch()? + 1; }");
    let ExpressionKind::Binary { left, right, .. } = &value.kind else {
        panic!("expected addition around the try")
    };
    assert!(matches!(left.kind, ExpressionKind::Try { .. }));
    assert!(matches!(right.kind, ExpressionKind::Literal(_)));
}

#[test]
fn parses_chained_try_as_postfix_association() {
    let value = first_return("public int F() { return validate(parse(text)?)?; }");
    let ExpressionKind::Try { operand } = &value.kind else {
        panic!("expected an outer try")
    };
    let ExpressionKind::Call { arguments, .. } = &operand.kind else {
        panic!("expected the validate call")
    };
    assert!(matches!(arguments[0].kind, ExpressionKind::Try { .. }));
}

#[test]
fn try_in_comparison_condition() {
    let value = first_return("public int F() { return validate()? == true; }");
    let ExpressionKind::Binary { left, .. } = &value.kind else {
        panic!("expected an equality comparison")
    };
    assert!(matches!(left.kind, ExpressionKind::Try { .. }));
}

#[test]
fn ternary_conditional_is_unaffected() {
    let value = first_return("public int F() { return flag ? 1 : 2; }");
    assert!(matches!(value.kind, ExpressionKind::Conditional { .. }));
}

#[test]
fn try_in_variable_initializer() {
    let value = first_variable_initializer("public int F() { int value = fetch()?; }");
    assert!(matches!(value.kind, ExpressionKind::Try { .. }));
}

#[test]
fn try_then_addition_in_variable_initializer() {
    let value = first_variable_initializer("public int F() { int next = fetch()? + 1; }");
    let ExpressionKind::Binary { left, .. } = &value.kind else {
        panic!("expected addition")
    };
    assert!(matches!(left.kind, ExpressionKind::Try { .. }));
}

#[test]
fn try_after_indexing() {
    let value = first_return("public int F() { return items[0]?; }");
    let ExpressionKind::Try { operand } = &value.kind else {
        panic!("expected a try after indexing")
    };
    assert!(matches!(operand.kind, ExpressionKind::Index { .. }));
}

#[test]
fn try_after_parenthesized_expression() {
    let value = first_return("public int F() { return (fetch())?; }");
    let ExpressionKind::Try { operand } = &value.kind else {
        panic!("expected a try after a parenthesized expression")
    };
    assert!(matches!(operand.kind, ExpressionKind::Call { .. }));
}

#[test]
fn ternary_with_try_in_consequent() {
    let value = first_return("public int F() { return condition ? fetch()? : fallback; }");
    let ExpressionKind::Conditional { when_true, .. } = &value.kind else {
        panic!("expected a conditional")
    };
    assert!(matches!(when_true.kind, ExpressionKind::Try { .. }));
}

#[test]
fn nested_ternary_still_parses() {
    let value = first_return("public int F() { return outer ? inner ? a : b : c; }");
    let ExpressionKind::Conditional { when_true, .. } = &value.kind else {
        panic!("expected an outer conditional")
    };
    assert!(matches!(when_true.kind, ExpressionKind::Conditional { .. }));
}

#[test]
fn disambiguation_is_whitespace_independent() {
    // No spaces: ternary stays a conditional, and a bare trailing `?` stays a try.
    let ternary = first_return("public int F() { return condition?first:second; }");
    assert!(matches!(ternary.kind, ExpressionKind::Conditional { .. }));

    let try_expression = first_variable_initializer("public int F() { int v = fetch()?; }");
    assert!(matches!(try_expression.kind, ExpressionKind::Try { .. }));
}

#[test]
fn null_conditional_is_a_controlled_syntax_error() {
    assert!(parse(lex("public int F() { return fetch()?.length; }").expect("lex")).is_err());
}

#[test]
fn null_coalescing_is_a_controlled_syntax_error() {
    assert!(parse(lex("public int F() { return fetch() ?? other; }").expect("lex")).is_err());
}
