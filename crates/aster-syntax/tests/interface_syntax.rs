use aster_syntax::{Item, lex, parse};

#[test]
fn parses_nominal_interface_lists_on_classes() {
    let source = "public interface IReadable { int Read(); } public interface IWritable { void Write(int value); } public class Device : IReadable, IWritable { public Device() {} public int Read() { return 0; } public void Write(int value) {} }";
    let module = parse(lex(source).expect("lexing")).expect("interface syntax");
    let Item::Class(class) = &module.items[2] else {
        panic!("class")
    };
    assert_eq!(
        class
            .interfaces
            .iter()
            .map(|interface| interface.name.as_str())
            .collect::<Vec<_>>(),
        ["IReadable", "IWritable"]
    );
}

#[test]
fn rejects_interface_lists_on_value_types() {
    let source = "public interface IValue { int Value(); } public struct Number : IValue { public int value; }";
    let diagnostics = parse(lex(source).expect("lexing")).expect_err("struct list must fail");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message
            .contains("only classes can declare implemented interfaces")
    }));
}
