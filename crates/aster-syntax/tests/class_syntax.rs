use aster_syntax::{ExpressionKind, Item, Member, Statement, lex, parse};

#[test]
fn parses_constructor_new_methods_and_this() {
    let source = "public class C { private int value; public C(int value) { this.value = value; } public int Get() { return value; } } public int Run() { C c = new C(1); return c.Get(); }";
    let module = parse(lex(source).expect("lexing")).expect("class syntax");
    let Item::Class(class) = &module.items[0] else {
        panic!("class")
    };
    let Member::Method(constructor) = &class.members[1] else {
        panic!("constructor")
    };
    assert!(constructor.constructor);
    let body = constructor.body.as_ref().unwrap();
    let Statement::Expression(assignment) = &body.statements[0] else {
        panic!("assignment")
    };
    let ExpressionKind::Assignment { target, .. } = &assignment.kind else {
        panic!("assignment")
    };
    assert!(
        matches!(target.kind, ExpressionKind::Member { ref object, .. } if matches!(object.kind, ExpressionKind::This))
    );
    let Item::Function(run) = &module.items[1] else {
        panic!("run")
    };
    let Statement::Variable(variable) = &run.body.as_ref().unwrap().statements[0] else {
        panic!("variable")
    };
    assert!(matches!(
        variable.initializer.as_ref().unwrap().kind,
        ExpressionKind::NewObject { .. }
    ));
}
