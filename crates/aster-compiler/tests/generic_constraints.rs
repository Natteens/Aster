//! M7C first subset: interface-only generic constraints.
//!
//! Constraints are a generic-template contract. They are proven when a
//! specialization is requested and erased before semantic analysis, so nothing
//! here should ever observe a constraint after monomorphization.

use aster_syntax::Item;

const CONTRACTS: &str = "public interface IFirst { int First(); } \
     public interface ISecond { int Second(); } \
     public class Both : IFirst, ISecond { public Both() {} public int First() { return 40; } public int Second() { return 2; } } \
     public class OnlyFirst : IFirst { public OnlyFirst() {} public int First() { return 1; } } \
     public class Plain { public Plain() {} } ";

fn compile(source: &str) -> aster_compiler::Compilation {
    aster_compiler::compile(source).expect("valid constrained program")
}

fn diagnose(source: &str, expected: &str) {
    let diagnostics = aster_compiler::compile(source).expect_err("invalid constrained program");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains(expected)),
        "missing `{expected}` in {diagnostics:#?}"
    );
}

#[test]
fn a_class_that_lists_the_interface_satisfies_the_constraint() {
    let source = format!(
        "{CONTRACTS} public int Read<T>(T value) where T : IFirst {{ return value.First(); }} \
         public int Run() {{ return Read(new OnlyFirst()); }}"
    );
    compile(&source);
}

/// The required interface is itself an accepted argument, matching ASTER's
/// existing "same type is compatible" shape.
#[test]
fn the_interface_itself_satisfies_its_own_constraint() {
    let source = format!(
        "{CONTRACTS} public int Read<T>(T value) where T : IFirst {{ return value.First(); }} \
         public int Run() {{ IFirst held = new OnlyFirst(); return Read(held); }}"
    );
    compile(&source);
}

#[test]
fn a_class_without_the_interface_is_rejected_at_the_request() {
    let source = format!(
        "{CONTRACTS} public T Keep<T>(T value) where T : IFirst {{ return value; }} \
         public int Run() {{ Keep(new Plain()); return 0; }}"
    );
    diagnose(
        &source,
        "type argument `Plain` does not satisfy constraint `T: IFirst`",
    );
}

#[test]
fn multiple_constraints_are_all_required() {
    let satisfied = format!(
        "{CONTRACTS} public int Sum<T>(T value) where T : IFirst, ISecond {{ return value.First() + value.Second(); }} \
         public int Run() {{ return Sum(new Both()); }}"
    );
    compile(&satisfied);

    let unsatisfied = format!(
        "{CONTRACTS} public T Keep<T>(T value) where T : IFirst, ISecond {{ return value; }} \
         public int Run() {{ Keep(new OnlyFirst()); return 0; }}"
    );
    let diagnostics =
        aster_compiler::compile(&unsatisfied).expect_err("one constraint is unsatisfied");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message
            .contains("type argument `OnlyFirst` does not satisfy constraint `T: ISecond`")
    }));
    // The satisfied constraint must not also be reported.
    assert!(
        !diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("`T: IFirst`"))
    );
}

#[test]
fn separate_clauses_constrain_separate_parameters() {
    let source = format!(
        "{CONTRACTS} public int Build<T, U>(T left, U right) where T : IFirst where U : ISecond {{ return left.First() + right.Second(); }} \
         public int Run() {{ return Build(new OnlyFirst(), new Both()); }}"
    );
    compile(&source);

    let rejected = format!(
        "{CONTRACTS} public int Build<T, U>(T left, U right) where T : IFirst where U : ISecond {{ return left.First() + right.Second(); }} \
         public int Run() {{ return Build(new OnlyFirst(), new OnlyFirst()); }}"
    );
    diagnose(
        &rejected,
        "type argument `OnlyFirst` does not satisfy constraint `U: ISecond`",
    );
}

#[test]
fn malformed_constraints_have_specific_diagnostics() {
    for (constraint, expected) in [
        ("IMissing", "unknown constraint type `IMissing`"),
        ("int", "`int` is not an interface; it is a primitive type"),
        (
            "string",
            "`string` is not an interface; it is a primitive type",
        ),
        ("void", "`void` is not an interface; it is a built-in type"),
        (
            "List",
            "`List` is not an interface; it is a reserved built-in type",
        ),
        ("Plain", "`Plain` is not an interface; it is a class"),
        (
            "IFirst[]",
            "`IFirst[]` is not an interface; it is an array type",
        ),
        ("IFirst, IFirst", "duplicate constraint `IFirst`"),
    ] {
        let source = format!(
            "{CONTRACTS} public T Keep<T>(T value) where T : {constraint} {{ return value; }} \
             public int Run() {{ return 0; }}"
        );
        diagnose(&source, expected);
    }
}

#[test]
fn non_interface_declaration_kinds_are_each_named() {
    for (declaration, constraint, expected) in [
        (
            "public struct Point { public int x; }",
            "Point",
            "`Point` is not an interface; it is a struct",
        ),
        (
            "public enum Colour { Red, Green, }",
            "Colour",
            "`Colour` is not an interface; it is an enum",
        ),
    ] {
        let source = format!(
            "{declaration} public T Keep<T>(T value) where T : {constraint} {{ return value; }} \
             public int Run() {{ return 0; }}"
        );
        diagnose(&source, expected);
    }
}

#[test]
fn closed_and_self_referential_generic_interface_constraints_are_nominal() {
    let source = "public interface IBox<T> { T Get(); } \
         public interface IComparable<T> { int CompareTo(T other); } \
         public class IntBox : IBox<int>, IComparable<IntBox> { public IntBox() {} public int Get() { return 42; } public int CompareTo(IntBox other) { return 0; } } \
         public T Closed<T>(T value) where T : IBox<int> { return value; } \
         public T Self<T>(T value) where T : IComparable<T> { return value; } \
         public int Run() { IntBox box = Closed(new IntBox()); IntBox same = Self(box); return same.Get(); }";
    compile(source);

    let wrong_closed = "public interface IBox<T> { T Get(); } \
         public class TextBox : IBox<string> { public TextBox() {} public string Get() { return \"x\"; } } \
         public T Keep<T>(T value) where T : IBox<int> { return value; } \
         public int Run() { Keep(new TextBox()); return 0; }";
    diagnose(
        wrong_closed,
        "type argument `TextBox` does not satisfy constraint `T: IBox<int>`",
    );

    let wrong_self = "public interface IComparable<T> { int CompareTo(T other); } \
         public class Other { public Other() {} } \
         public class Value : IComparable<Other> { public Value() {} public int CompareTo(Other other) { return 0; } } \
         public T Keep<T>(T value) where T : IComparable<T> { return value; } \
         public int Run() { Keep(new Value()); return 0; }";
    diagnose(
        wrong_self,
        "type argument `Value` does not satisfy constraint `T: IComparable<Value>`",
    );
}

#[test]
fn nested_closed_constraints_and_multiple_specializations_remain_nominal() {
    let accepted = "public interface IBox<T> { T Get(); } \
         public interface IMarked<T> { int Mark(); } \
         public interface IComparable<T> { int CompareTo(T other); } \
         public class Value : IMarked<IBox<int>>, IComparable<Value> { public Value() {} public int Mark() { return 42; } public int CompareTo(Value other) { return 0; } } \
         public T Keep<T>(T value) where T : IMarked<IBox<int>>, IComparable<T> { return value; } \
         public int Run() { return Keep(new Value()).Mark(); }";
    compile(accepted);

    let rejected = "public interface IBox<T> { T Get(); } \
         public interface IMarked<T> { int Mark(); } \
         public interface IComparable<T> { int CompareTo(T other); } \
         public class Value : IMarked<IBox<string>>, IComparable<Value> { public Value() {} public int Mark() { return 42; } public int CompareTo(Value other) { return 0; } } \
         public T Keep<T>(T value) where T : IMarked<IBox<int>>, IComparable<T> { return value; } \
         public int Run() { Keep(new Value()); return 0; }";
    let diagnostics = aster_compiler::compile(rejected).expect_err("wrong nested specialization");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message
            .contains("does not satisfy constraint `T: IMarked<IBox<int>>`")
    }));
    assert!(!diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message
            .contains("does not satisfy constraint `T: IComparable<Value>`")
    }));
}

#[test]
fn open_or_wrong_arity_generic_interface_constraints_are_rejected() {
    for (constraint, expected) in [
        (
            "IBox",
            "generic interface constraint `IBox` expects 1 type argument(s), found 0",
        ),
        (
            "IBox<int, string>",
            "generic interface constraint `IBox` expects 1 type argument(s), found 2",
        ),
    ] {
        let source = format!(
            "public interface IBox<T> {{ T Get(); }} public T Keep<T>(T value) where T : {constraint} {{ return value; }} public int Run() {{ return 0; }}"
        );
        diagnose(&source, expected);
    }
}

#[test]
fn constraints_are_enforced_on_generic_classes_and_structs() {
    let source = format!(
        "{CONTRACTS} public class Box<T> where T : IFirst {{ private T item; public Box(T item) {{ this.item = item; }} public int Read() {{ return item.First(); }} }} \
         public struct Holder<T> where T : ISecond {{ public int count; }} \
         public int Run() {{ Box<OnlyFirst> box = new Box<OnlyFirst>(new OnlyFirst()); Holder<Both> holder = Holder<Both> {{ count: 41 }}; return box.Read() + holder.count; }}"
    );
    compile(&source);

    let rejected = format!(
        "{CONTRACTS} public class Box<T> where T : IFirst {{ private T item; public Box(T item) {{ this.item = item; }} }} \
         public int Run() {{ Box<Plain> box = new Box<Plain>(new Plain()); return 0; }}"
    );
    diagnose(
        &rejected,
        "type argument `Plain` does not satisfy constraint `T: IFirst`",
    );

    let rejected = format!(
        "{CONTRACTS} public struct Holder<T> where T : ISecond {{ public T value; }} \
         public int Run() {{ Holder<Plain> holder = Holder<Plain> {{ value: new Plain() }}; return 0; }}"
    );
    diagnose(
        &rejected,
        "type argument `Plain` does not satisfy constraint `T: ISecond`",
    );
}

#[test]
fn constraints_are_enforced_on_generic_enums() {
    let source = format!(
        "{CONTRACTS} public enum Slot<T> where T : IFirst {{ Empty, Full(T value), }} \
         public int Run() {{ Slot<OnlyFirst> slot = Slot<OnlyFirst>.Full(new OnlyFirst()); switch (slot) {{ case Full(value): return value.First(); case Empty: return 0; }} }}"
    );
    compile(&source);

    let rejected = format!(
        "{CONTRACTS} public enum Slot<T> where T : IFirst {{ Empty, Full(T value), }} \
         public int Run() {{ Slot<Plain> slot = Slot<Plain>.Empty; return 0; }}"
    );
    diagnose(
        &rejected,
        "type argument `Plain` does not satisfy constraint `T: IFirst`",
    );
}

/// A generic interface may carry a `where` clause. It cannot be instantiated
/// through a constrained argument today for unrelated reasons, so this pins
/// parsing and well-formedness only.
#[test]
fn generic_interfaces_accept_and_validate_where_clauses() {
    let valid = format!(
        "{CONTRACTS} public interface IKeep<T> where T : IFirst {{ int Count(); }} public int Run() {{ return 0; }}"
    );
    compile(&valid);

    let invalid = format!(
        "{CONTRACTS} public interface IKeep<T> where T : Plain {{ int Count(); }} public int Run() {{ return 0; }}"
    );
    diagnose(&invalid, "`Plain` is not an interface; it is a class");
}

#[test]
fn a_generated_specialization_keeps_the_interface_relation_it_was_declared_with() {
    let source = format!(
        "{CONTRACTS} public class Wrapper<T> : IFirst {{ private T item; public Wrapper(T item) {{ this.item = item; }} public int First() {{ return 42; }} }} \
         public int Take<U>(U value) where U : IFirst {{ return value.First(); }} \
         public int Run() {{ Wrapper<int> wrapped = new Wrapper<int>(1); return Take(wrapped); }}"
    );
    compile(&source);
}

#[test]
fn nested_specialization_proves_each_constrained_request() {
    let source = format!(
        "{CONTRACTS} public class Box<T> where T : IFirst {{ private T item; public Box(T item) {{ this.item = item; }} public int Read() {{ return item.First(); }} }} \
         public int Outer<U>(U value) where U : IFirst {{ Box<U> inner = new Box<U>(value); return inner.Read(); }} \
         public int Run() {{ return Outer(new OnlyFirst()); }}"
    );
    compile(&source);

    // The inner request is proven too, through the substituted argument.
    let rejected = format!(
        "{CONTRACTS} public class Box<T> where T : ISecond {{ private T item; public Box(T item) {{ this.item = item; }} }} \
         public int Outer<U>(U value) where U : IFirst {{ Box<U> inner = new Box<U>(value); return 0; }} \
         public int Run() {{ return Outer(new OnlyFirst()); }}"
    );
    diagnose(
        &rejected,
        "type argument `OnlyFirst` does not satisfy constraint `T: ISecond`",
    );
}

#[test]
fn a_satisfied_specialization_is_cached_and_reused() {
    let source = format!(
        "{CONTRACTS} public int Read<T>(T value) where T : IFirst {{ return value.First(); }} \
         public int Run() {{ OnlyFirst a = new OnlyFirst(); OnlyFirst b = new OnlyFirst(); return Read(a) + Read(b) + Read<OnlyFirst>(a); }}"
    );
    let compilation = compile(&source);
    let instances = compilation
        .hir
        .items
        .iter()
        .filter(|item| {
            matches!(item, aster_compiler::hir::Item::Function(function) if function.name.starts_with("Read#"))
        })
        .count();
    assert_eq!(instances, 1);
}

/// A cached satisfied request must never hide a later unsatisfied one, and two
/// separate bad sites must each be reported.
#[test]
fn every_unsatisfied_request_site_is_reported() {
    let source = format!(
        "{CONTRACTS} public T Keep<T>(T value) where T : IFirst {{ return value; }} \
         public int Run() {{ OnlyFirst good = new OnlyFirst(); Plain bad = new Plain(); Keep(good); Keep(bad); Keep<Plain>(bad); return 0; }}"
    );
    let diagnostics = aster_compiler::compile(&source).expect_err("two bad request sites");
    let unsatisfied = diagnostics
        .iter()
        .filter(|diagnostic| {
            diagnostic
                .message
                .contains("type argument `Plain` does not satisfy constraint `T: IFirst`")
        })
        .collect::<Vec<_>>();
    assert_eq!(unsatisfied.len(), 2, "{diagnostics:#?}");
    assert!(unsatisfied[0].span.start < unsatisfied[1].span.start);
}

#[test]
fn an_unsatisfied_request_reports_the_request_span() {
    let source = format!(
        "{CONTRACTS} public T Keep<T>(T value) where T : IFirst {{ return value; }} \
         public int Run() {{ Keep(new Plain()); return 0; }}"
    );
    let diagnostics = aster_compiler::compile(&source).expect_err("unsatisfied constraint");
    let diagnostic = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.message.contains("does not satisfy constraint"))
        .expect("constraint diagnostic");
    assert_eq!(
        &source[diagnostic.span.start..diagnostic.span.end],
        "Keep(new Plain())"
    );
}

#[test]
fn template_diagnostics_are_emitted_in_source_order() {
    let source = "public class Plain { public Plain() {} } \
         public T A<T>(T v) where T : int { return v; } \
         public T B<T>(T v) where T : IMissing { return v; } \
         public T C<T>(T v) where T : Plain { return v; } \
         public int Run() { return 0; }";
    let diagnostics = aster_compiler::compile(source).expect_err("three bad templates");
    let spans = diagnostics
        .iter()
        .map(|diagnostic| diagnostic.span.start)
        .collect::<Vec<_>>();
    let mut sorted = spans.clone();
    sorted.sort_unstable();
    assert_eq!(spans, sorted, "{diagnostics:#?}");
    assert_eq!(diagnostics.len(), 3);
}

/// M7C deliberately does not make `where` mandatory. Member access on an
/// unconstrained parameter stays legal and remains checked per specialization.
/// Closing this needs open-template semantic validation, which is out of scope.
#[test]
fn unconstrained_member_access_remains_specialization_checked() {
    let accepted = format!(
        "{CONTRACTS} public int Read<T>(T value) {{ return value.First(); }} \
         public int Run() {{ return Read(new OnlyFirst()); }}"
    );
    compile(&accepted);

    let rejected = format!(
        "{CONTRACTS} public int Read<T>(T value) {{ return value.First(); }} \
         public int Run() {{ return Read(new Plain()); }}"
    );
    diagnose(&rejected, "type `Plain` has no method `First`");
}

#[test]
fn no_type_parameter_or_constraint_survives_specialization() {
    let source = format!(
        "{CONTRACTS} public int Read<T>(T value) where T : IFirst {{ return value.First(); }} \
         public class Box<T> where T : IFirst {{ private T item; public Box(T item) {{ this.item = item; }} }} \
         public enum Slot<T> where T : IFirst {{ Empty, Full(T value), }} \
         public int Run() {{ Box<OnlyFirst> box = new Box<OnlyFirst>(new OnlyFirst()); Slot<OnlyFirst> slot = Slot<OnlyFirst>.Empty; return Read(new OnlyFirst()); }}"
    );
    let compilation = compile(&source);
    for item in &compilation.module.items {
        let parameters = match item {
            Item::Class(value) | Item::Struct(value) | Item::Interface(value) => {
                &value.type_parameters
            }
            Item::Enum(value) => &value.type_parameters,
            Item::Function(value) => &value.type_parameters,
            Item::Variable(_) => continue,
        };
        assert!(
            parameters.is_empty(),
            "open type parameter survived monomorphization in `{item:?}`"
        );
    }
}
