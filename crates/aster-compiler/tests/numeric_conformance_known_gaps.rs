//! Reproducers for confirmed disagreements that are intentionally not fixed in
//! the test-only conformance change. Each test is `#[ignore]`d with the reason
//! and passes once the corresponding correction lands; remove the attribute
//! with that correction.

use aster_compiler::{compile, hir};

fn returned_expression(source: &str) -> hir::Expression {
    let compilation = compile(source).expect("valid source");
    let hir::Item::Function(function) = &compilation.hir.items[0] else {
        panic!("expected a function");
    };
    function
        .body
        .as_ref()
        .expect("body")
        .statements
        .iter()
        .find_map(|statement| match statement {
            hir::Statement::Return(Some(expression)) => Some(expression.clone()),
            _ => None,
        })
        .expect("return statement")
}

/// Semantic analysis types `c ? a : b` with two `byte` branches as `int`
/// (`promoted_numeric` in `semantic/general/expressions.rs`: observable because
/// `var x = c ? a : b; byte y = x;` is rejected with "expected `byte`, found
/// `int`", and overload resolution picks the `int` overload). HIR lowering
/// types the same expression `byte` because `conditional_type` in
/// `hir_lowering/types.rs` returns the shared branch type before consulting the
/// promotion table. Runtime values stay correct today because the HIR inserts a
/// conversion at each use, but HIR and semantic analysis disagree on the type.
#[test]
#[ignore = "known gap: HIR types a conditional with equal small-integer branches as the narrow type, semantic analysis as int"]
fn equal_small_integer_conditional_branches_are_typed_int_in_hir() {
    for name in ["sbyte", "byte", "short", "ushort"] {
        let expression = returned_expression(&format!(
            "public int Pick(bool c, {name} a, {name} b) {{ return c ? a : b; }}"
        ));
        let conditional = match &expression.kind {
            hir::ExpressionKind::Convert { operand } => operand,
            _ => &expression,
        };
        assert!(matches!(
            conditional.kind,
            hir::ExpressionKind::Conditional { .. }
        ));
        assert_eq!(conditional.type_, hir::Type::Int, "{name}");
    }
}
