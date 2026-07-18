use aster_syntax::{ExpressionKind, Item, Statement, lex, parse};

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
