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
fn expanding_type_specialization_reports_the_recursive_type_span() {
    let source = "public class Grow<T> { public Grow<Grow<T>> next; public Grow() {} } public int Run() { Grow<int> value = new Grow<int>(); return 0; }";
    let diagnostics = aster_compiler::compile(source).expect_err("expansion must be rejected");
    let diagnostic = diagnostics
        .iter()
        .find(|diagnostic| {
            diagnostic
                .message
                .contains("infinitely expanding specialization")
        })
        .expect("expansion diagnostic");

    assert_eq!(
        &source[diagnostic.span.start..diagnostic.span.end],
        "Grow<Grow<T>> "
    );
}

// --- List<T>: identity, layout, and structural validation (foundation only;
// no constructor, no member access, no iteration exist yet) --------------

#[test]
fn list_of_int_is_recognized_as_a_parameter_and_return_type() {
    let source = "public int Count(List<int> values) { return 0; } \
                  public List<int> Echo(List<int> values) { return values; }";
    let compilation = aster_compiler::compile(source).expect("List<int> is a recognized type");
    let module = format!("{:#?}", compilation.module);
    assert!(module.contains("List<int>"));
    assert!(!module.contains("Unknown"));
}

#[test]
fn list_diagnostics_are_specific() {
    for (source, expected) in [
        (
            "public int Count(List value) { return 0; }",
            "`List` expects 1 type argument, found 0",
        ),
        (
            "public int Count(List<int, long> value) { return 0; }",
            "`List` expects 1 type argument, found 2",
        ),
        (
            "public int Count(List<void> value) { return 0; }",
            "`List<void>` is not supported",
        ),
        (
            "public int Count(List<decimal> value) { return 0; }",
            "`List<decimal>` cannot be used until `decimal` is executable",
        ),
        ("public class List { }", "cannot be declared as a"),
        (
            "public struct List { public int x; }",
            "cannot be declared as a",
        ),
        (
            "public interface List { int Run(); }",
            "cannot be declared as a",
        ),
        ("public enum List { A }", "cannot be declared as a"),
        (
            "public class List<T> { public List(T value) {} }",
            "cannot be declared as a generic",
        ),
    ] {
        let diagnostics = aster_compiler::compile(source).expect_err("invalid `List<T>` usage");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains(expected)),
            "missing `{expected}` in {diagnostics:#?}"
        );
    }
}

#[test]
fn list_accepts_string_class_struct_interface_enum_array_and_nested_list_elements() {
    for source in [
        "public int Count(List<string> values) { return 0; }",
        "public class Widget { public Widget() {} } public int Count(List<Widget> values) { return 0; }",
        "public struct Point { public int x; public int y; } public int Count(List<Point> values) { return 0; }",
        "public interface IJob { int Run(); } public int Count(List<IJob> values) { return 0; }",
        "public enum Color { Red, Green } public int Count(List<Color> values) { return 0; }",
        "public int Count(List<int[]> values) { return 0; }",
        "public int Count(List<List<int>> values) { return 0; }",
    ] {
        let compilation = aster_compiler::compile(source).unwrap_or_else(|diagnostics| {
            panic!("expected `{source}` to compile: {diagnostics:#?}")
        });
        assert!(
            !format!("{:#?}", compilation.module).contains("Unknown"),
            "`{source}` left an Unknown type behind"
        );
    }
}

#[test]
fn list_of_int_is_not_assignable_to_int() {
    let source = "public int Count(List<int> values) { return 0; } public int Run(int value) { return Count(value); }";
    let diagnostics = aster_compiler::compile(source).expect_err("int is not a List<int>");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("List<int>")),
        "missing a `List<int>` type mismatch in {diagnostics:#?}"
    );
}

#[test]
fn list_of_int_is_not_assignable_to_list_of_long() {
    let source = "public int Count(List<long> values) { return 0; } \
                  public int Run(List<int> values) { return Count(values); }";
    let diagnostics = aster_compiler::compile(source).expect_err("List<int> is not a List<long>");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("List<long>")
                && diagnostic.message.contains("List<int>")),
        "missing a List<long>/List<int> type mismatch in {diagnostics:#?}"
    );
}

#[test]
fn list_has_no_public_constructor_yet() {
    let source = "public int Run() { List<int> values = new List<int>(); return 0; }";
    aster_compiler::compile(source).expect_err("`new List<T>()` must still be rejected");
}

#[test]
fn list_has_no_member_operations_yet() {
    let source = "public int Run(List<int> values) { values.Add(1); return values.Length; }";
    aster_compiler::compile(source).expect_err("List<T> exposes no members in this foundation");
}

#[test]
fn arrays_and_generics_are_unaffected_by_list() {
    let source = "public class Box<T> { private T value; public Box(T value) { this.value = value; } public T Get() { return value; } } \
                  public int Run() { int[] values = [1, 2]; Box<int> boxed = new Box<int>(values[0]); return boxed.Get() + values[1]; }";
    aster_compiler::compile(source).expect("arrays and generics keep working alongside List<T>");
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
