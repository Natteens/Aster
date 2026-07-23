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

#[test]
fn foreach_is_typed_and_lowers_to_existing_array_cfg() {
    let compilation = aster_compiler::compile(
        "public int Run() { int[] values = [1, 2, 3]; int total = 0; foreach (int value in values) { total += value; } return total; }",
    )
    .expect("valid array foreach");
    let hir::Item::Function(function) = &compilation.hir.items[0] else {
        panic!("function");
    };
    assert!(matches!(
        function.body.as_ref().unwrap().statements[2],
        hir::Statement::ForEach { .. }
    ));
    let function = &compilation.mir.functions[0];
    assert!(
        function
            .blocks
            .iter()
            .any(|block| matches!(block.terminator, mir::Terminator::Branch { .. }))
    );
    assert!(
        function
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .any(|instruction| {
                matches!(
                    instruction,
                    mir::Instruction::Assign {
                        value: mir::Rvalue {
                            kind: mir::RvalueKind::ArrayLength(_),
                            ..
                        },
                        ..
                    }
                )
            })
    );
}

#[test]
fn foreach_over_a_list_is_typed_and_lowers_to_an_indexed_cfg() {
    // M3C: `List<T>` is now a valid `foreach` collection (M3B only accepted
    // arrays). Mirrors `foreach_is_typed_and_lowers_to_existing_array_cfg`
    // above, but confirms the version-checked shape `lower_foreach_over_list`
    // actually produces: a `ListLength` read, at least one `ListVersion`
    // read, and a `ListGet` (never `ArrayLength`/`Place::Index`).
    let compilation = aster_compiler::compile(
        "public int Run() { List<int> values = new List<int>(); values.Add(1); int total = 0; foreach (int value in values) { total += value; } return total; }",
    )
    .expect("valid list foreach");
    let function = &compilation.mir.functions[0];
    let instructions = function
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .collect::<Vec<_>>();
    assert!(instructions.iter().any(|instruction| matches!(
        instruction,
        mir::Instruction::Assign {
            value: mir::Rvalue {
                kind: mir::RvalueKind::ListLength(_),
                ..
            },
            ..
        }
    )));
    assert!(instructions.iter().any(|instruction| matches!(
        instruction,
        mir::Instruction::Assign {
            value: mir::Rvalue {
                kind: mir::RvalueKind::ListVersion(_),
                ..
            },
            ..
        }
    )));
    assert!(
        instructions
            .iter()
            .any(|instruction| matches!(instruction, mir::Instruction::ListGet { .. }))
    );
}

#[test]
fn foreach_diagnostics_preserve_array_only_and_readonly_rules() {
    for (source, message) in [
        (
            "public int Run() { int[] values = [1]; foreach (string value in values) { } return 0; }",
            "does not match array element type",
        ),
        (
            "public int Run() { string value = \"x\"; foreach (char item in value) { } return 0; }",
            "string` is not supported",
        ),
        (
            "public int Run() { int[] values = [1]; foreach (int value in values) { value = 2; } return 0; }",
            "foreach variable `value` is read-only",
        ),
        (
            "public struct Point { public int X; } public int Run() { Point[] values = [Point { X: 1 }]; foreach (Point value in values) { value.X = 2; } return 0; }",
            "foreach variable `value` is read-only",
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
