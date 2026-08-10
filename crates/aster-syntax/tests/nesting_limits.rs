use aster_syntax::{MAX_SOURCE_NESTING, lex, parse};

fn function_with_return(expression: &str) -> String {
    format!("public int Main() {{ return {expression}; }}")
}

fn nesting_diagnostic(source: &str) -> Vec<aster_diagnostics::Diagnostic> {
    parse(lex(source).expect("generated source lexes")).expect_err("nesting must be rejected")
}

#[test]
fn ordinary_and_boundary_parentheses_are_controlled() {
    let ordinary = function_with_return("((((1))))");
    parse(lex(&ordinary).expect("ordinary source lexes")).expect("ordinary nesting parses");

    // The function body and root expression consume two positions in the one
    // combined source-nesting budget. Pin active depths 63, 64, and 65 rather
    // than only testing a distant pathological input.
    for active_depth in [MAX_SOURCE_NESTING - 1, MAX_SOURCE_NESTING] {
        let parentheses = active_depth - 2;
        let accepted = function_with_return(&format!(
            "{}1{}",
            "(".repeat(parentheses),
            ")".repeat(parentheses)
        ));
        parse(lex(&accepted).expect("boundary source lexes")).unwrap_or_else(|diagnostics| {
            panic!("active depth {active_depth} must parse: {diagnostics:#?}")
        });
    }

    let rejected_depth = MAX_SOURCE_NESTING + 1;
    let rejected_parentheses = rejected_depth - 2;
    let rejected = function_with_return(&format!(
        "{}1{}",
        "(".repeat(rejected_parentheses),
        ")".repeat(rejected_parentheses)
    ));
    let diagnostics = nesting_diagnostic(&rejected);
    let expected = format!("source nesting exceeds the compiler limit of {MAX_SOURCE_NESTING}");
    assert_eq!(
        diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.message == expected)
            .count(),
        1
    );
    assert!(diagnostics[0].span.start > 0);
}

#[test]
fn excessive_unary_and_postfix_nesting_are_rejected() {
    let unary = function_with_return(&format!("{}true", "!".repeat(MAX_SOURCE_NESTING * 4)));
    let unary_diagnostics = nesting_diagnostic(&unary);
    assert!(
        unary_diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("source nesting exceeds"))
    );

    let members = format!("value{}", ".value".repeat(MAX_SOURCE_NESTING * 4));
    let member_source = format!(
        "public class Node {{ public Node value; }} public int Read(Node value) {{ return {members}.value; }}"
    );
    let member_diagnostics = nesting_diagnostic(&member_source);
    assert!(
        member_diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("source nesting exceeds"))
    );
}

#[test]
fn malformed_deep_generic_syntax_is_a_controlled_diagnostic() {
    let nested = format!(
        "{}int{}",
        "Box<".repeat(MAX_SOURCE_NESTING * 2),
        ">".repeat(MAX_SOURCE_NESTING * 2 - 1)
    );
    let source = format!("public {nested} Read() {{ return value; }}");
    let diagnostics = nesting_diagnostic(&source);
    assert!(!diagnostics.is_empty());
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("source nesting exceeds"))
    );
}

#[test]
fn generic_type_nesting_uses_the_same_63_64_65_boundary() {
    for active_depth in [MAX_SOURCE_NESTING - 1, MAX_SOURCE_NESTING] {
        let wrappers = active_depth - 1;
        let nested = format!("{}int{}", "Box<".repeat(wrappers), ">".repeat(wrappers));
        let source = format!("public {nested} Read() {{ return value; }}");
        parse(lex(&source).expect("boundary generic source lexes")).unwrap_or_else(|diagnostics| {
            panic!("generic active depth {active_depth} must parse: {diagnostics:#?}")
        });
    }

    let rejected_depth = MAX_SOURCE_NESTING + 1;
    let wrappers = rejected_depth - 1;
    let nested = format!("{}int{}", "Box<".repeat(wrappers), ">".repeat(wrappers));
    let source = format!("public {nested} Read() {{ return value; }}");
    let diagnostics = nesting_diagnostic(&source);
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.message
            == format!("source nesting exceeds the compiler limit of {MAX_SOURCE_NESTING}")
    }));
}
