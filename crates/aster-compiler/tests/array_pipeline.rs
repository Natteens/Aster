use aster_hir as hir;
use aster_mir as mir;

#[test]
fn arrays_are_typed_in_hir_and_explicit_in_mir() {
    let compilation = aster_compiler::compile(
        "public int Run() { int[] values = [1, 2, 3]; values[1] += 4; return values.Length; }",
    )
    .expect("valid arrays");
    let hir::Item::Function(function) = &compilation.hir.items[0] else {
        panic!("function")
    };
    let body = function.body.as_ref().unwrap();
    let hir::Statement::Variable(variable) = &body.statements[0] else {
        panic!("variable")
    };
    assert_eq!(variable.type_, hir::Type::Array(Box::new(hir::Type::Int)));
    assert!(matches!(
        variable.initializer.as_ref().unwrap().kind,
        hir::ExpressionKind::ArrayLiteral(_)
    ));
    let function = &compilation.mir.functions[0];
    assert!(
        function
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .any(|instruction| matches!(
                instruction,
                mir::Instruction::AllocateArray {
                    element_type: mir::Type::Int,
                    region: mir::AllocationRegion::Temporary,
                    ..
                }
            ))
    );
    assert!(
        function
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .any(|instruction| matches!(
                instruction,
                mir::Instruction::Assign {
                    target: mir::Place::Index { .. },
                    ..
                }
            ))
    );
}

#[test]
fn array_diagnostics_are_specific() {
    for (source, message) in [
        (
            "public int Run() { int[] a = [1]; return a[true]; }",
            "array index must have type `int`",
        ),
        (
            "public int Run() { int[] a = [1]; a.Length = 2; return 0; }",
            "array Length is read-only",
        ),
        (
            "public int Run() { int[] a = [1]; return a.Missing; }",
            "array has no member `Missing`",
        ),
        (
            "public int Run() { int[] a = [1]; return a[-1]; }",
            "array index cannot be negative",
        ),
        (
            "public int Run() { int[] a = [1]; a[0] = false; return 0; }",
            "expected `int`, found `bool`",
        ),
        (
            "public int Run() { int[] a; return a.Length; }",
            "used before initialization",
        ),
        (
            "public int Run() { string[] a = new string[2]; return a.Length; }",
            "has no non-null default value",
        ),
    ] {
        let diagnostics = aster_compiler::compile(source).expect_err("source must be rejected");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains(message)),
            "missing `{message}` in {diagnostics:#?}"
        );
    }
}
