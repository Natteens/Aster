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
            "`decimal` is reserved but not supported",
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
fn list_has_no_member_operations_beyond_length_add_get_and_remove_at() {
    for (source, expected) in [
        (
            "public int Run(List<int> values) { values.Set(0, 1); return 0; }",
            "no member `Set`",
        ),
        (
            "public int Run(List<int> values) { return values[0]; }",
            "cannot be indexed",
        ),
    ] {
        let diagnostics = aster_compiler::compile(source).expect_err("member not yet implemented");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains(expected)),
            "missing `{expected}` in {diagnostics:#?}"
        );
    }
}

// --- List B1: `new List<T>()` and `.Length` -----------------------------

#[test]
fn new_list_constructs_an_empty_list_with_zero_length() {
    let source = "public int Run() { List<int> values = new List<int>(); return values.Length; }";
    let compilation =
        aster_compiler::compile(source).expect("`new List<T>()` and `.Length` compile");
    assert!(!format!("{:#?}", compilation.module).contains("Unknown"));
}

#[test]
fn new_list_constructor_diagnostics_are_specific() {
    for (source, expected) in [
        (
            "public int Run() { List<int> values = new List<int>(10); return 0; }",
            "takes no arguments",
        ),
        (
            "public int Run() { List<int> values = new List<int>(1, 2); return 0; }",
            "takes no arguments",
        ),
        (
            "public int Run() { List values = new List(); return 0; }",
            "`List` expects 1 type argument, found 0",
        ),
        (
            "public int Run() { List<int, string> values = new List<int, string>(); return 0; }",
            "`List` expects 1 type argument, found 2",
        ),
        (
            "public int Run() { List<void> values = new List<void>(); return 0; }",
            "`List<void>` is not supported",
        ),
        (
            "public int Run() { List<decimal> values = new List<decimal>(); return 0; }",
            "`decimal` is reserved but not supported",
        ),
    ] {
        let diagnostics = aster_compiler::compile(source).expect_err("invalid `new List<T>()`");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains(expected)),
            "missing `{expected}` in {diagnostics:#?}"
        );
    }
}

#[test]
fn new_list_constructs_every_required_element_type() {
    for source in [
        "public int Run() { List<int> values = new List<int>(); return values.Length; }",
        "public int Run() { List<string> values = new List<string>(); return values.Length; }",
        "public class Widget { public Widget() {} } public int Run() { List<Widget> values = new List<Widget>(); return values.Length; }",
        "public interface IJob { int Run(); } public int Consume() { List<IJob> values = new List<IJob>(); return values.Length; }",
        "public int Run() { List<int[]> values = new List<int[]>(); return values.Length; }",
        "public struct Point { public int x; public int y; } public int Run() { List<Point> values = new List<Point>(); return values.Length; }",
        "public enum Color { Red, Green } public int Run() { List<Color> values = new List<Color>(); return values.Length; }",
        "public class Box<T> { private T value; public Box(T value) { this.value = value; } public T Get() { return value; } } public int Run() { List<Box<int>> values = new List<Box<int>>(); return values.Length; }",
        "public int Run() { List<List<int>> values = new List<List<int>>(); return values.Length; }",
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
fn a_type_reserved_for_list_can_never_be_declared_to_shadow_the_builtin() {
    // `List` is globally reserved (see `validate_no_reserved_type_names`), so
    // there is no way for a user declaration to ever exist under that name in
    // the first place; this is the strongest possible form of "a fake `List`
    // does not activate the built-in constructor".
    let diagnostics = aster_compiler::compile("public class List { public List() {} }")
        .expect_err("`List` is reserved");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message
            .contains("reserved for the built-in `List<T>`")
    }));
}

#[test]
fn list_length_cannot_be_assigned_and_capacity_does_not_exist() {
    for (source, expected) in [
        (
            "public int Run(List<int> values) { values.Length = 10; return 0; }",
            "list Length is read-only",
        ),
        (
            "public int Run(List<int> values) { return values.Capacity; }",
            "list has no member `Capacity`",
        ),
        (
            "public int Run(List<int> values) { return values.Count; }",
            "list has no member `Count`",
        ),
    ] {
        let diagnostics = aster_compiler::compile(source).expect_err("must be rejected");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains(expected)),
            "missing `{expected}` in {diagnostics:#?}"
        );
    }
}

#[test]
fn list_length_evaluates_its_receiver_exactly_once() {
    let source = "public List<int> Make() { return new List<int>(); } \
                  public int Run() { return Make().Length; }";
    let compilation = aster_compiler::compile(source).expect("`Make().Length` compiles");
    let run = compilation
        .mir
        .functions
        .iter()
        .find(|function| function.name == "Run" && function.owner.is_none())
        .expect("Run is declared");
    let call_count = run
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter(|instruction| matches!(instruction, aster_mir::Instruction::Call { .. }))
        .count();
    assert_eq!(call_count, 1, "`Make()` must be evaluated exactly once");
}

#[test]
fn a_class_field_named_length_still_uses_ordinary_member_resolution() {
    // The `List<T>.Length` intrinsic is gated on the receiver's type being
    // `Type::List(_)`; any other type's own `Length` member (field, property,
    // or method) must keep resolving through the normal path, unaffected.
    let source = "public class Widget { public int Length; public Widget() { Length = 5; } } \
                  public int Run() { Widget widget = new Widget(); return widget.Length; }";
    aster_compiler::compile(source).expect("a class field named `Length` is unaffected by List");
}

// --- List B2A: `values.Add(value)` -------------------------------------

#[test]
fn list_add_accepts_a_compatible_value() {
    let source = "public int Run() { List<int> values = new List<int>(); values.Add(1); return values.Length; }";
    aster_compiler::compile(source).expect("List<int>.Add(int) is well-formed");
}

#[test]
fn list_add_returns_void() {
    let source =
        "public int Run() { List<int> values = new List<int>(); int x = values.Add(1); return x; }";
    let diagnostics = aster_compiler::compile(source).expect_err("Add returns void, not int");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("void")),
        "missing a void-related mismatch in {diagnostics:#?}"
    );
}

#[test]
fn list_add_diagnostics_are_specific() {
    for (source, expected) in [
        (
            "public int Run() { List<int> values = new List<int>(); values.Add(); return 0; }",
            "expects 1 argument, found 0",
        ),
        (
            "public int Run() { List<int> values = new List<int>(); values.Add(1, 2); return 0; }",
            "expects 1 argument, found 2",
        ),
        (
            "public int Run() { List<int> values = new List<int>(); values.Add(\"text\"); return 0; }",
            "expected",
        ),
    ] {
        let diagnostics = aster_compiler::compile(source).expect_err("invalid `Add` usage");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains(expected)),
            "missing `{expected}` in {diagnostics:#?}"
        );
    }
}

#[test]
fn an_ordinary_add_method_on_a_class_still_resolves_normally() {
    let source = "public class Accumulator { public int total; public Accumulator() {} \
                  public void Add(int value) { total = total + value; } } \
                  public int Run() { Accumulator accumulator = new Accumulator(); \
                  accumulator.Add(10); return accumulator.total; }";
    aster_compiler::compile(source).expect("a class's own `Add` method is unaffected by List");
}

#[test]
fn a_struct_with_an_add_method_still_resolves_normally() {
    let source = "public struct Wrapper { public int total; } \
                  public int AddTo(Wrapper wrapper, int value) { return wrapper.total + value; } \
                  public int Run() { Wrapper wrapper = Wrapper { total: 1 }; return AddTo(wrapper, 2); }";
    aster_compiler::compile(source)
        .expect("an unrelated free function named `Add`-like is unaffected");
}

#[test]
fn list_add_evaluates_receiver_then_argument_exactly_once_each() {
    let source = "public List<int> GetList() { return new List<int>(); } \
                  public int GetValue() { return 1; } \
                  public int Run() { GetList().Add(GetValue()); return 0; }";
    let compilation = aster_compiler::compile(source).expect("GetList().Add(GetValue()) compiles");
    let run = compilation
        .mir
        .functions
        .iter()
        .find(|function| function.name == "Run" && function.owner.is_none())
        .expect("Run is declared");
    let get_list = compilation
        .mir
        .functions
        .iter()
        .find(|function| function.name == "GetList" && function.owner.is_none())
        .expect("GetList is declared")
        .symbol;
    let get_value = compilation
        .mir
        .functions
        .iter()
        .find(|function| function.name == "GetValue" && function.owner.is_none())
        .expect("GetValue is declared")
        .symbol;
    let calls = run
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            aster_mir::Instruction::Call { function, .. } => Some(*function),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        calls
            .iter()
            .filter(|&&function| function == get_list)
            .count(),
        1,
        "GetList() must be evaluated exactly once: {calls:?}"
    );
    assert_eq!(
        calls
            .iter()
            .filter(|&&function| function == get_value)
            .count(),
        1,
        "GetValue() must be evaluated exactly once: {calls:?}"
    );
    let list_position = calls.iter().position(|&function| function == get_list);
    let value_position = calls.iter().position(|&function| function == get_value);
    assert!(
        list_position < value_position,
        "GetList() must be called before GetValue(): {calls:?}"
    );
}

// --- List B2B: `values.Get(index)` --------------------------------------

#[test]
fn list_get_accepts_a_valid_call_and_returns_the_element_type() {
    let source = "public int Run() { List<int> values = new List<int>(); values.Add(1); \
                  int first = values.Get(0); return first; }";
    aster_compiler::compile(source).expect("List<int>.Get(int) returns int");
}

#[test]
fn list_get_diagnostics_are_specific() {
    for (source, expected) in [
        (
            "public int Run() { List<int> values = new List<int>(); return values.Get(); }",
            "expects 1 argument, found 0",
        ),
        (
            "public int Run() { List<int> values = new List<int>(); return values.Get(0, 1); }",
            "expects 1 argument, found 2",
        ),
        (
            "public int Run() { List<int> values = new List<int>(); return values.Get(1L); }",
            "requires an `int` index",
        ),
        (
            "public int Run() { List<int> values = new List<int>(); return values.Get(\"0\"); }",
            "requires an `int` index",
        ),
    ] {
        let diagnostics = aster_compiler::compile(source).expect_err("invalid `Get` usage");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains(expected)),
            "missing `{expected}` in {diagnostics:#?}"
        );
    }
}

#[test]
fn an_ordinary_get_method_on_a_class_still_resolves_normally() {
    let source = "public class Store { public int value; public Store() {} \
                  public int Get() { return value; } } \
                  public int Run() { Store store = new Store(); return store.Get(); }";
    aster_compiler::compile(source).expect("a class's own `Get` method is unaffected by List");
}

#[test]
fn a_free_function_named_get_from_still_resolves_normally() {
    let source = "public struct Wrapper { public int total; } \
                  public int GetFrom(Wrapper wrapper) { return wrapper.total; } \
                  public int Run() { Wrapper wrapper = Wrapper { total: 5 }; return GetFrom(wrapper); }";
    aster_compiler::compile(source).expect("an unrelated free function is unaffected by List.Get");
}

#[test]
fn list_get_evaluates_receiver_then_index_exactly_once_each() {
    let source = "public List<int> GetList() { List<int> values = new List<int>(); values.Add(1); return values; } \
                  public int GetIndex() { return 0; } \
                  public int Run() { return GetList().Get(GetIndex()); }";
    let compilation = aster_compiler::compile(source).expect("GetList().Get(GetIndex()) compiles");
    let run = compilation
        .mir
        .functions
        .iter()
        .find(|function| function.name == "Run" && function.owner.is_none())
        .expect("Run is declared");
    let get_list = compilation
        .mir
        .functions
        .iter()
        .find(|function| function.name == "GetList" && function.owner.is_none())
        .expect("GetList is declared")
        .symbol;
    let get_index = compilation
        .mir
        .functions
        .iter()
        .find(|function| function.name == "GetIndex" && function.owner.is_none())
        .expect("GetIndex is declared")
        .symbol;
    let calls = run
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            aster_mir::Instruction::Call { function, .. } => Some(*function),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        calls
            .iter()
            .filter(|&&function| function == get_list)
            .count(),
        1,
        "GetList() must be evaluated exactly once: {calls:?}"
    );
    assert_eq!(
        calls
            .iter()
            .filter(|&&function| function == get_index)
            .count(),
        1,
        "GetIndex() must be evaluated exactly once: {calls:?}"
    );
    let list_position = calls.iter().position(|&function| function == get_list);
    let index_position = calls.iter().position(|&function| function == get_index);
    assert!(
        list_position < index_position,
        "GetList() must be called before GetIndex(): {calls:?}"
    );
}

// --- List C: `values.RemoveAt(index)` -----------------------------------

#[test]
fn list_remove_at_accepts_a_valid_call_and_returns_void() {
    let source = "public int Run() { List<int> values = new List<int>(); values.Add(1); \
                  values.RemoveAt(0); return values.Length; }";
    aster_compiler::compile(source).expect("List<int>.RemoveAt(int) is well-formed");
}

#[test]
fn list_remove_at_diagnostics_are_specific() {
    for (source, expected) in [
        (
            "public int Run() { List<int> values = new List<int>(); values.Add(1); values.RemoveAt(); return 0; }",
            "expects 1 argument, found 0",
        ),
        (
            "public int Run() { List<int> values = new List<int>(); values.Add(1); values.RemoveAt(0, 1); return 0; }",
            "expects 1 argument, found 2",
        ),
        (
            "public int Run() { List<int> values = new List<int>(); values.Add(1); values.RemoveAt(1L); return 0; }",
            "requires an `int` index",
        ),
        (
            "public int Run() { List<int> values = new List<int>(); values.Add(1); values.RemoveAt(\"0\"); return 0; }",
            "requires an `int` index",
        ),
    ] {
        let diagnostics = aster_compiler::compile(source).expect_err("invalid `RemoveAt` usage");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains(expected)),
            "missing `{expected}` in {diagnostics:#?}"
        );
    }
}

#[test]
fn an_ordinary_remove_at_method_on_a_class_still_resolves_normally() {
    let source = "public class Store { public int value; public Store() {} \
                  public void RemoveAt(int index) { value = index; } } \
                  public int Run() { Store store = new Store(); store.RemoveAt(3); return store.value; }";
    aster_compiler::compile(source).expect("a class's own `RemoveAt` method is unaffected by List");
}

#[test]
fn a_free_function_named_remove_at_like_still_resolves_normally() {
    let source = "public struct Wrapper { public int total; } \
                  public int RemoveFrom(Wrapper wrapper) { return wrapper.total; } \
                  public int Run() { Wrapper wrapper = Wrapper { total: 9 }; return RemoveFrom(wrapper); }";
    aster_compiler::compile(source)
        .expect("an unrelated free function is unaffected by List.RemoveAt");
}

#[test]
fn list_remove_at_evaluates_receiver_then_index_exactly_once_each() {
    let source = "public List<int> GetList() { List<int> values = new List<int>(); values.Add(1); values.Add(2); return values; } \
                  public int GetIndex() { return 0; } \
                  public int Run() { GetList().RemoveAt(GetIndex()); return 0; }";
    let compilation =
        aster_compiler::compile(source).expect("GetList().RemoveAt(GetIndex()) compiles");
    let run = compilation
        .mir
        .functions
        .iter()
        .find(|function| function.name == "Run" && function.owner.is_none())
        .expect("Run is declared");
    let get_list = compilation
        .mir
        .functions
        .iter()
        .find(|function| function.name == "GetList" && function.owner.is_none())
        .expect("GetList is declared")
        .symbol;
    let get_index = compilation
        .mir
        .functions
        .iter()
        .find(|function| function.name == "GetIndex" && function.owner.is_none())
        .expect("GetIndex is declared")
        .symbol;
    let calls = run
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            aster_mir::Instruction::Call { function, .. } => Some(*function),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        calls
            .iter()
            .filter(|&&function| function == get_list)
            .count(),
        1,
        "GetList() must be evaluated exactly once: {calls:?}"
    );
    assert_eq!(
        calls
            .iter()
            .filter(|&&function| function == get_index)
            .count(),
        1,
        "GetIndex() must be evaluated exactly once: {calls:?}"
    );
    let list_position = calls.iter().position(|&function| function == get_list);
    let index_position = calls.iter().position(|&function| function == get_index);
    assert!(
        list_position < index_position,
        "GetList() must be called before GetIndex(): {calls:?}"
    );
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
