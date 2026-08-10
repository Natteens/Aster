use aster_syntax::{ExpressionKind, Item, Member, Statement, TypeParameter, lex, parse};

fn constraints(parameter: &TypeParameter) -> Vec<&str> {
    parameter
        .constraints
        .iter()
        .map(|constraint| constraint.name.as_str())
        .collect()
}

#[test]
fn parses_generic_function_and_inferred_and_explicit_calls() {
    let source = "public T Pick<T>(T value) { return value; } public int Run() { int a = Pick(1); return Pick<int>(a); }";
    let module = parse(lex(source).expect("lexing")).expect("generic syntax");
    let Item::Function(template) = &module.items[0] else {
        panic!("template")
    };
    assert_eq!(template.type_parameters[0].name, "T");
    let Item::Function(run) = &module.items[1] else {
        panic!("run")
    };
    let Statement::Return {
        value: Some(call), ..
    } = &run.body.as_ref().unwrap().statements[1]
    else {
        panic!("return")
    };
    let ExpressionKind::Call { type_arguments, .. } = &call.kind else {
        panic!("call")
    };
    assert_eq!(type_arguments[0].name, "int");
}

#[test]
fn parses_generic_classes_structs_interfaces_and_nested_uses() {
    let source = "public interface IValue<T> { T Get(); } public class Box<T> : IValue<T> { private T value; public Box(T value) { this.value = value; } public T Get() { return value; } } public struct Pair<T, U> { public T first; public U second; } public int Run() { Box<int> box = new Box<int>(42); Pair<Box<int>, string[]> pair = Pair<Box<int>, string[]> { first: box, second: [\"Aster\"] }; return pair.first.Get(); }";
    let module = parse(lex(source).expect("lexing")).expect("generic type syntax");
    let Item::Interface(interface) = &module.items[0] else {
        panic!("interface")
    };
    assert_eq!(interface.type_parameters[0].name, "T");
    let Item::Class(class) = &module.items[1] else {
        panic!("class")
    };
    assert_eq!(class.interfaces[0].name, "IValue<T>");
    let Item::Struct(pair) = &module.items[2] else {
        panic!("pair")
    };
    assert_eq!(pair.type_parameters.len(), 2);
    let Item::Function(run) = &module.items[3] else {
        panic!("run")
    };
    let Statement::Variable(variable) = &run.body.as_ref().unwrap().statements[1] else {
        panic!("pair variable")
    };
    let aster_syntax::VariableKind::Explicit(type_ref) = &variable.kind else {
        panic!("explicit type")
    };
    assert_eq!(type_ref.name, "Pair<Box<int>,string[]>");
}

#[test]
fn parses_where_clauses_on_every_generic_declaration_kind() {
    let source = "public interface IFirst { int A(); } public interface ISecond { int B(); } \
         public T Free<T>(T value) where T : IFirst { return value; } \
         public class Box<T> where T : IFirst, ISecond { private T value; public Box(T value) { this.value = value; } } \
         public class Tagged<T> : IFirst where T : ISecond { public int A() { return 1; } } \
         public struct Holder<T> where T : IFirst { public int count; } \
         public interface IKeep<T> where T : IFirst { int Count(); } \
         public enum Slot<T> where T : ISecond { Empty, Full(T value), }";
    let module = parse(lex(source).expect("lexing")).expect("where clauses");

    let Item::Function(free) = &module.items[2] else {
        panic!("free function")
    };
    assert_eq!(constraints(&free.type_parameters[0]), ["IFirst"]);

    let Item::Class(box_) = &module.items[3] else {
        panic!("class")
    };
    assert_eq!(constraints(&box_.type_parameters[0]), ["IFirst", "ISecond"]);

    // A `where` clause sits after the interface list, so both can coexist.
    let Item::Class(tagged) = &module.items[4] else {
        panic!("class with interfaces")
    };
    assert_eq!(tagged.interfaces[0].name, "IFirst");
    assert_eq!(constraints(&tagged.type_parameters[0]), ["ISecond"]);

    let Item::Struct(holder) = &module.items[5] else {
        panic!("struct")
    };
    assert_eq!(constraints(&holder.type_parameters[0]), ["IFirst"]);

    let Item::Interface(keep) = &module.items[6] else {
        panic!("interface")
    };
    assert_eq!(constraints(&keep.type_parameters[0]), ["IFirst"]);

    let Item::Enum(slot) = &module.items[7] else {
        panic!("enum")
    };
    assert_eq!(constraints(&slot.type_parameters[0]), ["ISecond"]);
}

#[test]
fn parses_one_where_clause_per_constrained_type_parameter() {
    let source = "public interface IFirst { int A(); } public interface ISecond { int B(); } \
         public T Build<T, U, V>(T left, U middle, V right) where T : IFirst where V : ISecond { return left; }";
    let module = parse(lex(source).expect("lexing")).expect("multiple clauses");
    let Item::Function(build) = &module.items[2] else {
        panic!("function")
    };
    assert_eq!(constraints(&build.type_parameters[0]), ["IFirst"]);
    assert!(build.type_parameters[1].constraints.is_empty());
    assert_eq!(constraints(&build.type_parameters[2]), ["ISecond"]);
}

/// A signature-only interface member accepts the same trailing clause. It is
/// still rejected later by the unchanged "no standalone generic methods" rule;
/// this only pins the grammar.
#[test]
fn parses_a_where_clause_on_a_signature_only_interface_member() {
    let source = "public interface IFirst { int A(); } public interface IUse { int Run<T>(T value) where T : IFirst; }";
    let module = parse(lex(source).expect("lexing")).expect("signature-only clause");
    let Item::Interface(declaration) = &module.items[1] else {
        panic!("interface")
    };
    let Member::Method(method) = &declaration.members[0] else {
        panic!("method")
    };
    assert!(method.body.is_none());
    assert_eq!(constraints(&method.type_parameters[0]), ["IFirst"]);
}

#[test]
fn malformed_where_clauses_have_specific_diagnostics() {
    for (source, expected) in [
        (
            "public interface I { int A(); } public T F<T>(T v) where : I { return v; }",
            "expected identifier, found `:`",
        ),
        (
            "public interface I { int A(); } public T F<T>(T v) where T I { return v; }",
            "expected `:`, found identifier",
        ),
        (
            "public interface I { int A(); } public T F<T>(T v) where T : { return v; }",
            "expected type, found `{`",
        ),
        (
            "public interface I { int A(); } public T F<T>(T v) where T : I, { return v; }",
            "expected type, found `{`",
        ),
        (
            "public interface I { int A(); } public T F<T>(T v) where U : I { return v; }",
            "unknown type parameter `U` in `where` clause",
        ),
        (
            "public interface I { int A(); } public int F(int v) where T : I { return v; }",
            "unknown type parameter `T` in `where` clause",
        ),
        (
            "public interface I { int A(); } public T F<T>(T v) where T : I where T : I { return v; }",
            "duplicate `where` clause for type parameter `T`",
        ),
        (
            "public interface I { int A(); } public class B<T> where U : I { }",
            "unknown type parameter `U` in `where` clause",
        ),
    ] {
        let diagnostics = parse(lex(source).expect("lexing")).expect_err("malformed clause");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains(expected)),
            "missing `{expected}` in {diagnostics:#?}"
        );
    }
}

/// `where` is contextual. It is only a clause opener between a declaration
/// header and its body, so ordinary declarations named `where` keep parsing.
#[test]
fn where_remains_a_usable_ordinary_identifier() {
    let source = "public int where = 5; \
         public int Where() { int where = 3; return where; } \
         public class Holder { private int where; public Holder() { where = 1; } public int Get(int where) { return where; } }";
    let module = parse(lex(source).expect("lexing")).expect("`where` as an identifier");
    let Item::Variable(global) = &module.items[0] else {
        panic!("global")
    };
    assert_eq!(global.name, "where");
    let Item::Class(holder) = &module.items[2] else {
        panic!("class")
    };
    let Member::Field(field) = &holder.members[0] else {
        panic!("field")
    };
    assert_eq!(field.name, "where");
}
