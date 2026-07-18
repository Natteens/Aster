use aster_syntax::{Item, Member, lex, parse};

#[test]
fn parses_static_methods_field_initializers_and_properties() {
    let source = "public class Player { private int health = 100; public Player() {} public static int Limit(int value) { return value; } public int Health { get { return health; } private set { health = value; } } }";
    let module = parse(lex(source).expect("lexing")).expect("ergonomic class syntax");
    let Item::Class(class) = &module.items[0] else {
        panic!("class")
    };
    let Member::Field(field) = &class.members[0] else {
        panic!("field")
    };
    assert!(field.initializer.is_some());
    let Member::Method(method) = &class.members[2] else {
        panic!("static method")
    };
    assert!(method.is_static);
    let Member::Property(property) = &class.members[3] else {
        panic!("property")
    };
    assert!(property.getter.is_some());
    assert!(property.setter.is_some());
}

#[test]
fn rejects_static_fields_and_static_constructors() {
    for source in [
        "public class C { public static int value; public C() {} }",
        "public class C { public static C() {} }",
    ] {
        assert!(parse(lex(source).expect("lexing")).is_err());
    }
}

#[test]
fn parses_a_static_class_and_rejects_other_static_types() {
    let module = parse(
        lex("public static class Math { public static int One() { return 1; } }").expect("lexing"),
    )
    .expect("static class syntax");
    let Item::Class(class) = &module.items[0] else {
        panic!("class")
    };
    assert!(class.is_static);

    for source in [
        "public static struct Bad {}",
        "public static interface Bad {}",
        "public static int Bad() { return 0; }",
        "static var bad = 0;",
        "static const int Bad = 0;",
        "static enum Bad { A, B }",
        "static Bad() {}",
    ] {
        assert!(parse(lex(source).expect("lexing")).is_err());
    }
}
