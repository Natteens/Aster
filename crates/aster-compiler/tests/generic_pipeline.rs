use aster_hir as hir;

#[test]
fn generic_functions_are_concrete_before_hir_and_reused() {
    let source = "public T Identity<T>(T value) { return value; } public int Run() { int a = Identity(20); int b = Identity<int>(22); return a + b; }";
    let compilation = aster_compiler::compile(source).expect("generic program");
    let instances = compilation.hir.items.iter().filter(|item| matches!(item, hir::Item::Function(function) if function.name.starts_with("Identity#"))).count();
    assert_eq!(instances, 1);
    assert!(compilation.mir.functions.iter().all(|function| {
        function
            .parameters
            .iter()
            .all(|parameter| parameter.type_ != hir::Type::Unknown)
    }));
}

#[test]
fn generic_diagnostics_are_specific() {
    for (source, expected) in [
        (
            "public T Make<T>() { T value; return value; } public int Run() { return Make(); }",
            "cannot infer type parameter `T`",
        ),
        (
            "public T Pick<T>(T value) { return value; } public int Run() { return Pick<int, long>(1); }",
            "expects 1 type argument",
        ),
        (
            "public T Choose<T>(T a, T b) { return a; } public int Run() { return Choose(1, 2L); }",
            "conflicting inference",
        ),
        (
            "public U Bad<T>(T value) { return value; } public int Run() { return Bad(1); }",
            "unknown type `U`",
        ),
        (
            "public T Grow<T>(T value) { T[] values = [value]; return Grow(values); } public int Run() { return Grow(1); }",
            "recursively creates a different specialization",
        ),
        (
            "public T Bad<T, T>(T value) { return value; } public int Run() { return Bad(1); }",
            "duplicate type parameter",
        ),
        (
            "public int Plain(int value) { return value; } public int Run() { return Plain<int>(1); }",
            "is not a generic function",
        ),
    ] {
        let diagnostics = aster_compiler::compile(source).expect_err("invalid generic program");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains(expected)),
            "missing `{expected}` in {diagnostics:#?}"
        );
    }
}

#[test]
fn substitution_handles_multiple_parameters_through_nested_expressions() {
    let source = "public U Select<T, U>(bool condition, T first, U second) { T[] firsts = [first]; U[] seconds = [second]; seconds[0] = second; return condition ? seconds[0] : second; } public long Run() { return Select<int, long>(true, 1, 42L); }";
    let compilation = aster_compiler::compile(source).expect("two parameter specialization");
    let specialized = compilation.hir.items.iter().find_map(|item| match item {
        hir::Item::Function(function) if function.name.starts_with("Select#") => Some(function),
        _ => None,
    });
    let specialized = specialized.expect("specialized function");
    assert_eq!(specialized.return_type, hir::Type::Long);
    assert!(
        specialized
            .parameters
            .iter()
            .all(|parameter| parameter.type_ != hir::Type::Unknown)
    );
}
