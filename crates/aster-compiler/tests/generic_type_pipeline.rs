use aster_hir as hir;

#[test]
fn generic_types_are_concrete_before_hir_and_reused() {
    let source = "public class Box<T> { private T value; public Box(T value) { this.value = value; } public T Get() { return value; } } public int Run() { Box<int> first = new Box<int>(20); Box<int> second = new Box<int>(22); return first.Get() + second.Get(); }";
    let compilation = aster_compiler::compile(source).expect("generic class");
    let boxes = compilation
        .hir
        .items
        .iter()
        .filter(|item| matches!(item, hir::Item::Class(value) if value.name == "Box<int>"))
        .count();
    assert_eq!(boxes, 1);
    assert!(format!("{:#?}", compilation.hir).contains("Box<int>"));
    assert!(!format!("{:#?}", compilation.mir).contains("Unknown"));
}

#[test]
fn generic_type_diagnostics_are_specific() {
    for (source, expected) in [
        (
            "public class Box<T> { public Box(T value) {} } public int Run() { Box value; return 0; }",
            "expects 1 type argument",
        ),
        (
            "public class Box<T> { public Box(T value) {} } public int Run() { Box<int, long> value; return 0; }",
            "expects 1 type argument",
        ),
        (
            "public class Plain { public Plain() {} } public int Run() { Plain<int> value; return 0; }",
            "is not generic",
        ),
        (
            "public class Box<T> { public Box(T value) {} } public int Run() { Box<int> first = new Box<int>(1); Box<long> second = first; return 0; }",
            "expected `Box<long>`, found `Box<int>`",
        ),
        (
            "public class Bad<T, T> { public Bad(T value) {} } public int Run() { Bad<int, int> value; return 0; }",
            "duplicate type parameter",
        ),
        (
            "public class Grow<T> { public Grow<Grow<T>> next; public Grow() {} } public int Run() { Grow<int> value = new Grow<int>(); return 0; }",
            "infinitely expanding specialization",
        ),
    ] {
        let diagnostics = aster_compiler::compile(source).expect_err("invalid generic type");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains(expected)),
            "missing `{expected}` in {diagnostics:#?}"
        );
    }
}

#[test]
fn nested_generic_arrays_are_concrete_in_all_declaration_positions() {
    let source = "public struct Pair<T, U> { public T First; public U Second; } public class Box<T> { private T value; public Box(T value) { this.value = value; } public T Value { get { return value; } private set { value = value; } } public T Get() { return value; } } public Box<Pair<int, long>[]> Echo(Box<Pair<int, long>[]> value) { return value; } public int Run() { Pair<int, long>[] pairs = [Pair<int, long> { First: 42, Second: 1L }]; Box<Pair<int, long>[]> boxed = new Box<Pair<int, long>[]>(pairs); Box<Pair<int, long>[]> echoed = Echo(boxed); return (int)echoed.Value[0].First; }";
    let compilation = aster_compiler::compile(source).expect("nested generic arrays");
    let module = format!("{:#?}", compilation.module);
    assert!(module.contains("Pair<int,long>[]"));
    assert!(module.contains("Box<Pair<int,long>[]>"));
    assert!(!module.contains("name: \"T\""));
    assert!(!module.contains("name: \"U\""));
}
