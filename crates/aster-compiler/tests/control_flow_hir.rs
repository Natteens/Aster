use aster_compiler::{compile, hir};
use aster_diagnostics::Severity;

fn assert_valid(source: &str) -> aster_compiler::Compilation {
    compile(source).unwrap_or_else(|diagnostics| panic!("expected valid source: {diagnostics:#?}"))
}

fn assert_error(source: &str, expected: &str) {
    let diagnostics = compile(source).expect_err("source should be rejected");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains(expected)),
        "expected {expected:?}, got {diagnostics:#?}"
    );
}

#[test]
fn accepts_if_if_else_and_else_if() {
    assert_valid("public int Choose(bool value) { if (value) { return 1; } else { return 2; } }");
    assert_valid(
        "public int Choose(bool a, bool b) { if (a) { return 1; } else if (b) { return 2; } else { return 3; } }",
    );
    assert_valid("public void Check(bool value) { if (value) { Log(\"yes\"); } }");
}

#[test]
fn rejects_non_boolean_conditions() {
    assert_error(
        "public void Check() { if (1) {} }",
        "condition must be `bool`",
    );
    assert_error(
        "public void Check() { while (1) {} }",
        "condition must be `bool`",
    );
    assert_error(
        "public void Check() { for (; 1; ) {} }",
        "condition must be `bool`",
    );
}

#[test]
fn accepts_while_and_for() {
    assert_valid("public void Work(bool ready) { while (ready) { break; } }");
    assert_valid(
        "public void Work() { for (int index = 0; index < 10; index += 1) { continue; } }",
    );
}

#[test]
fn for_initializer_has_its_own_scope() {
    assert_error(
        "public void Work() { for (int index = 0; index < 1; index += 1) {} index = 2; }",
        "unknown name `index`",
    );
}

#[test]
fn blocks_have_lexical_scope() {
    assert_error(
        "public void Work(bool ready) { if (ready) { int value = 1; } value = 2; }",
        "unknown name `value`",
    );
}

#[test]
fn validates_break_and_continue_context() {
    assert_valid(
        "public void Work(bool ready) { while (ready) { break; } for (;;) { continue; } }",
    );
    assert_error("public void Work() { break; }", "only valid inside a loop");
    assert_error(
        "public void Work() { continue; }",
        "only valid inside a loop",
    );
}

#[test]
fn requires_return_on_every_reachable_path() {
    assert_valid("public int Value(bool ready) { if (ready) { return 1; } else { return 2; } }");
    assert_error(
        "public int Value(bool ready) { if (ready) { return 1; } }",
        "must return `int`",
    );
}

#[test]
fn reports_unreachable_code_as_warning() {
    let compilation = assert_valid("public int Value() { return 1; Log(\"never\"); }");
    assert!(compilation.diagnostics.iter().any(|diagnostic| {
        diagnostic.severity == Severity::Warning && diagnostic.message.contains("unreachable")
    }));
    let compilation =
        assert_valid("public void Work(bool ready) { while (ready) { break; Log(\"never\"); } }");
    assert!(
        compilation
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Warning)
    );
    let compilation = assert_valid("public void Work() { for (;;) { continue; Log(\"never\"); } }");
    assert!(
        compilation
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Warning)
    );
}

#[test]
fn lowers_functions_and_control_flow_to_hir() {
    let compilation = assert_valid(
        "public int Choose(bool ready) { if (ready) { return 1; } else { return 2; } }",
    );
    let hir::Item::Function(function) = &compilation.hir.items[0] else {
        panic!("expected HIR function");
    };
    assert_eq!(function.return_type, hir::Type::Int);
    assert!(matches!(
        function.body.as_ref().unwrap().statements[0],
        hir::Statement::If { .. }
    ));
}

#[test]
fn hir_names_reference_resolved_symbols_and_types() {
    let compilation =
        assert_valid("public int Identity(int value) { int copy = value; return copy; }");
    let hir::Item::Function(function) = &compilation.hir.items[0] else {
        panic!("expected HIR function");
    };
    let body = function.body.as_ref().unwrap();
    let hir::Statement::Variable(variable) = &body.statements[0] else {
        panic!("expected variable");
    };
    assert_eq!(variable.type_, hir::Type::Int);
    let hir::ExpressionKind::Symbol(parameter_reference) =
        variable.initializer.as_ref().unwrap().kind
    else {
        panic!("initializer should resolve to a symbol");
    };
    assert_eq!(parameter_reference, function.parameters[0].symbol);
    let hir::Statement::Return(Some(returned)) = &body.statements[1] else {
        panic!("expected return");
    };
    assert_eq!(returned.type_, hir::Type::Int);
    assert_eq!(returned.kind, hir::ExpressionKind::Symbol(variable.symbol));
}
