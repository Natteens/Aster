use std::collections::HashSet;

use aster_hir as hir;
use aster_syntax::{Item, Member};

fn compile(source: &str) -> aster_compiler::Compilation {
    aster_compiler::compile(source).expect("valid generic method program")
}

fn diagnose(source: &str, expected: &str) {
    let diagnostics = aster_compiler::compile(source).expect_err("invalid generic method program");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains(expected)),
        "missing `{expected}` in {diagnostics:#?}"
    );
}

#[test]
fn generic_methods_are_concrete_before_hir_and_reuse_identical_requests() {
    let source = "public class Tools { public Tools() {} public T Identity<T>(T value) { return value; } } \
                  public int Run() { Tools tools = new Tools(); int a = tools.Identity(20); int b = tools.Identity<int>(22); string text = tools.Identity(\"Aster\"); return a + b + (text.Length - 5); }";
    let compilation = compile(source);
    let methods = compilation
        .hir
        .items
        .iter()
        .filter_map(|item| match item {
            hir::Item::Class(class) if class.name == "Tools" => Some(&class.methods),
            _ => None,
        })
        .flatten()
        .filter(|method| method.name.contains("#method#Identity#"))
        .collect::<Vec<_>>();
    assert_eq!(methods.len(), 2);
    assert!(
        methods
            .iter()
            .any(|method| method.return_type == hir::Type::Int)
    );
    assert!(
        methods
            .iter()
            .any(|method| method.return_type == hir::Type::String)
    );
    assert!(compilation.module.items.iter().all(|item| match item {
        Item::Class(value) | Item::Struct(value) | Item::Interface(value) => {
            value.type_parameters.is_empty()
                && value.members.iter().all(|member| {
                    !matches!(member, Member::Method(method) if !method.type_parameters.is_empty())
                })
        }
        Item::Enum(value) => value.type_parameters.is_empty(),
        Item::Function(value) => value.type_parameters.is_empty(),
        Item::Variable(_) => true,
    }));
}

#[test]
fn a_generic_owner_and_generic_method_compose() {
    let source = "public class Box<T> { private T stored; public Box(T stored) { this.stored = stored; } \
                  public U Choose<U>(U value) { return value; } public T Get() { return stored; } } \
                  public int Run() { Box<string> box = new Box<string>(\"Aster\"); return box.Choose<int>(42); }";
    compile(source);
}

#[test]
fn specialization_identity_distinguishes_declarations_owners_and_arguments() {
    let source = "public class Left { public Left() {} public T Identity<T>(T value) { return value; } } \
                  public class Right { public Right() {} public T Identity<T>(T value) { return value; } } \
                  public class Box<T> { public Box() {} public U Choose<U>(U value) { return value; } } \
                  public int Run() { Left left = new Left(); Right right = new Right(); \
                  Box<int> ints = new Box<int>(); Box<string> strings = new Box<string>(); \
                  int a = left.Identity<int>(1); int b = right.Identity<int>(2); \
                  string c = ints.Choose<string>(\"a\"); string d = ints.Choose(\"b\"); \
                  int e = ints.Choose<int>(3); string f = strings.Choose<string>(\"c\"); \
                  return a + b + e + c.Length + d.Length + f.Length + 33; }";
    let compilation = compile(source);
    let methods = compilation
        .hir
        .items
        .iter()
        .filter_map(|item| match item {
            hir::Item::Class(class) => Some(
                class
                    .methods
                    .iter()
                    .filter(|method| method.name.contains("#method#"))
                    .map(move |method| (class.name.clone(), method.name.clone())),
            ),
            _ => None,
        })
        .flatten()
        .collect::<Vec<_>>();
    assert_eq!(methods.len(), 5, "{methods:#?}");
    assert_eq!(
        methods
            .iter()
            .map(|(_, method)| method)
            .collect::<HashSet<_>>()
            .len(),
        5,
        "specialized callable names collided: {methods:#?}"
    );
    assert_eq!(
        methods
            .iter()
            .filter(|(owner, _)| owner == "Box<int>")
            .count(),
        2
    );
    assert_eq!(
        methods
            .iter()
            .filter(|(owner, _)| owner == "Box<string>")
            .count(),
        1
    );
}

#[test]
fn owner_and_method_parameters_substitute_together_in_constraints() {
    let accepted = "public interface ILink<T, U> { int Read(); } \
                    public class IntLink : ILink<int, IntLink> { public IntLink() {} public int Read() { return 42; } } \
                    public class Owner<T> { public Owner() {} public U Keep<U>(U value) where U : ILink<T, U> { return value; } } \
                    public int Run() { Owner<int> owner = new Owner<int>(); IntLink a = owner.Keep(new IntLink()); IntLink b = owner.Keep<IntLink>(a); return b.Read(); }";
    compile(accepted);

    let rejected = "public interface ILink<T, U> { int Read(); } \
                    public class IntLink : ILink<int, IntLink> { public IntLink() {} public int Read() { return 42; } } \
                    public class Owner<T> { public Owner() {} public U Keep<U>(U value) where U : ILink<T, U> { return value; } } \
                    public int Run() { Owner<string> owner = new Owner<string>(); owner.Keep(new IntLink()); return 0; }";
    diagnose(
        rejected,
        "does not satisfy constraint `U: ILink<string,IntLink>`",
    );

    let nested = "public interface IWrap<T> { int Read(); } \
                  public class Nested : IWrap<List<int>> { public Nested() {} public int Read() { return 42; } } \
                  public class Owner<T> { public Owner() {} public U Keep<U>(U value) where U : IWrap<T> { return value; } } \
                  public int Run() { Owner<List<int>> owner = new Owner<List<int>>(); return owner.Keep(new Nested()).Read(); }";
    compile(nested);
}

#[test]
fn generic_method_constraints_accept_closed_self_referential_interfaces() {
    let source = "public interface IComparable<T> { int CompareTo(T other); } \
                  public class Number : IComparable<Number> { public Number() {} public int CompareTo(Number other) { return 0; } } \
                  public class Tools { public Tools() {} public T Keep<T>(T value) where T : IComparable<T> { return value; } } \
                  public int Run() { Tools tools = new Tools(); Number value = tools.Keep(new Number()); return 42; }";
    compile(source);

    let rejected = "public interface IComparable<T> { int CompareTo(T other); } \
                    public class Plain { public Plain() {} } \
                    public class Tools { public Tools() {} public T Keep<T>(T value) where T : IComparable<T> { return value; } } \
                    public int Run() { Tools tools = new Tools(); tools.Keep(new Plain()); return 0; }";
    diagnose(
        rejected,
        "type argument `Plain` does not satisfy constraint `T: IComparable<Plain>`",
    );
}

#[test]
fn explicit_type_arguments_select_a_generic_method_beside_a_non_generic_overload() {
    let source = "public class Tools { public Tools() {} public int Pick(int value) { return 0; } \
                  public T Pick<T>(T value) { return value; } } \
                  public int Run() { Tools tools = new Tools(); return tools.Pick<int>(42); }";
    compile(source);

    let ambiguous = "public class Tools { public Tools() {} public int Pick(int value) { return value; } \
                     public T Pick<T>(T value) { return value; } } \
                     public int Run() { Tools tools = new Tools(); return tools.Pick(42); }";
    diagnose(ambiguous, "call to method `Tools.Pick` is ambiguous");
}

#[test]
fn generic_method_overloads_use_their_closed_parameter_signature() {
    let source = "public class Tools { public Tools() {} \
                  public T Pick<T>(T value, int marker) { return value; } \
                  public T Pick<T>(T value, string marker) { return value; } } \
                  public int Run() { Tools tools = new Tools(); return tools.Pick(40, \"inferred\") + tools.Pick<int>(2, 0); }";
    let compilation = compile(source);
    let methods = compilation
        .hir
        .items
        .iter()
        .filter_map(|item| match item {
            hir::Item::Class(class) if class.name == "Tools" => Some(&class.methods),
            _ => None,
        })
        .flatten()
        .filter(|method| method.name.contains("#method#Pick#"))
        .collect::<Vec<_>>();
    assert_eq!(methods.len(), 2);
    assert_ne!(methods[0].name, methods[1].name);
}

#[test]
fn generic_method_diagnostics_cover_arity_inference_and_recursive_expansion() {
    for (source, expected) in [
        (
            "public class Tools { public Tools() {} public T Make<T>() { T value; return value; } } public int Run() { return new Tools().Make(); }",
            "cannot infer type parameter `T` for generic method `Tools.Make`",
        ),
        (
            "public class Tools { public Tools() {} public T Keep<T>(T value) { return value; } } public int Run() { return new Tools().Keep<int, long>(1); }",
            "has no overload with 2 type argument(s)",
        ),
        (
            "public class Tools { public Tools() {} public T Same<T>(T left, T right) { return left; } } public int Run() { new Tools().Same(1, \"wrong\"); return 0; }",
            "conflicting inference for `T`",
        ),
        (
            "public class Tools { public Tools() {} public T Grow<T>(T value) { T[] values = [value]; return Grow(values); } } public int Run() { return new Tools().Grow(1); }",
            "recursively creates a different specialization",
        ),
        (
            "public class Box<T> { public static U Keep<U>(U value) { return value; } } public int Run() { return Box<int>.Keep<int>(42); }",
            "static methods on generic types are not implemented",
        ),
    ] {
        diagnose(source, expected);
    }
}

#[test]
fn failed_method_requests_do_not_poison_later_requests_or_compilations() {
    let invalid = "public interface IValue<T> { T Get(); } \
                   public class Good : IValue<int> { public Good() {} public int Get() { return 42; } } \
                   public class Bad { public Bad() {} } \
                   public class Tools { public Tools() {} public T Keep<T>(T value) where T : IValue<int> { return value; } } \
                   public int Run() { Tools tools = new Tools(); tools.Keep(new Bad()); Good good = tools.Keep(new Good()); tools.Keep<Bad>(new Bad()); return good.Get(); }";
    let diagnostics = aster_compiler::compile(invalid).expect_err("two invalid request sites");
    assert_eq!(
        diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.message.contains("does not satisfy constraint"))
            .count(),
        2,
        "{diagnostics:#?}"
    );

    let recursive = "public class Tools { public Tools() {} public T Grow<T>(T value) { T[] values = [value]; return Grow(values); } } public int Run() { return new Tools().Grow(1); }";
    diagnose(recursive, "recursively creates a different specialization");

    let valid = "public interface IValue<T> { T Get(); } \
                 public class Good : IValue<int> { public Good() {} public int Get() { return 42; } } \
                 public class Tools { public Tools() {} public T Keep<T>(T value) where T : IValue<int> { return value; } } \
                 public int Run() { return new Tools().Keep(new Good()).Get(); }";
    compile(valid);
}
