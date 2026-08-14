use aster_compiler::{compile, hir, mir};

const POSITION: &str = "public struct Position { public int x; public int y; }";

fn messages(source: &str) -> Vec<String> {
    compile(source)
        .expect_err("source should be rejected")
        .into_iter()
        .map(|diagnostic| diagnostic.message)
        .collect()
}

#[test]
fn hir_resolves_struct_and_field_symbols() {
    let source = format!(
        "{POSITION} public int Run() {{ Position p = Position {{ y: 2, x: 1 }}; return p.x; }}"
    );
    let compilation = compile(&source).expect("valid struct");
    let hir::Item::Struct(definition) = &compilation.hir.items[0] else {
        panic!("struct");
    };
    let hir::Item::Function(function) = &compilation.hir.items[1] else {
        panic!("function");
    };
    let hir::Statement::Variable(variable) = &function.body.as_ref().unwrap().statements[0] else {
        panic!("variable");
    };
    let hir::ExpressionKind::StructLiteral {
        struct_symbol,
        fields,
    } = &variable.initializer.as_ref().unwrap().kind
    else {
        panic!("literal");
    };
    assert_eq!(*struct_symbol, definition.symbol);
    assert_eq!(fields.len(), 2);
    assert!(fields.iter().all(|value| {
        definition
            .fields
            .iter()
            .any(|field| field.symbol == value.field)
    }));
}

#[test]
fn mir_preserves_struct_definitions_aggregates_and_field_places() {
    let source = format!(
        "{POSITION} public int Run() {{ Position p = Position {{ x: 1, y: 2 }}; p.x = 3; return p.x; }}"
    );
    let compilation = compile(&source).expect("valid struct");
    assert_eq!(compilation.mir.structs.len(), 1);
    assert_eq!(compilation.mir.structs[0].fields.len(), 2);
    let function = &compilation.mir.functions[0];
    assert!(
        function
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .any(|instruction| matches!(
                instruction,
                mir::Instruction::Assign {
                    value: mir::Rvalue {
                        kind: mir::RvalueKind::Aggregate(_),
                        ..
                    },
                    ..
                }
            ))
    );
    assert!(
        function
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .any(|instruction| matches!(
                instruction,
                mir::Instruction::Assign {
                    target: mir::Place::Field { .. },
                    ..
                }
            ))
    );
}

#[test]
fn rejects_missing_duplicate_unknown_and_private_fields() {
    assert!(
        messages(&format!(
            "{POSITION} public void F() {{ Position p = Position {{ x: 1 }}; }}"
        ))
        .iter()
        .any(|m| m.contains("missing field `y`"))
    );
    assert!(
        messages(&format!(
            "{POSITION} public void F() {{ Position p = Position {{ x: 1, x: 2, y: 3 }}; }}"
        ))
        .iter()
        .any(|m| m.contains("more than once"))
    );
    assert!(
        messages(&format!(
            "{POSITION} public void F() {{ Position p = Position {{ x: 1, y: 2, z: 3 }}; }}"
        ))
        .iter()
        .any(|m| m.contains("no field `z`"))
    );
    assert!(messages("public struct Secret { int value; } public void F() { Secret s = Secret { value: 1 }; }").iter().any(|m| m.contains("not public")));
    assert!(
        messages(&format!(
            "{POSITION} public void F() {{ Position p = Position {{ x: true, y: 2 }}; }}"
        ))
        .iter()
        .any(|message| message.contains("expected `int`, found `bool`"))
    );
}

#[test]
fn rejects_recursive_struct_layout_and_accepts_structural_equality() {
    assert!(
        messages("public struct Node { public Node next; }")
            .iter()
            .any(|m| m.contains("recursive struct layout"))
    );
    aster_compiler::compile(&format!(
        "{POSITION} public bool Equal(Position a, Position b) {{ return a == b; }}"
    ))
    .expect("comparable structs support structural equality");
}

#[test]
fn struct_methods_lower_with_an_ordinary_by_value_receiver_and_direct_call() {
    let source = "public struct Position { public int x; public int Read() { return this.x; } } \
                  public int Run() { Position value = Position { x: 42 }; return value.Read(); }";
    let compilation = compile(source).expect("executable struct method pipeline");
    let hir::Item::Struct(definition) = &compilation.hir.items[0] else {
        panic!("struct definition");
    };
    let method = &definition.methods[0];
    assert_eq!(method.parameters.len(), 1);
    assert_eq!(method.parameters[0].name, "this");
    assert_eq!(
        method.parameters[0].type_,
        hir::Type::User(definition.symbol)
    );
    let caller = compilation
        .mir
        .functions
        .iter()
        .find(|function| function.name == "Run")
        .expect("caller MIR");
    assert!(
        caller
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .any(
                |instruction| matches!(instruction, mir::Instruction::Call { arguments, .. }
            if matches!(arguments.as_slice(), [mir::Operand { type_: mir::Type::User(_), .. }]))
            )
    );
}

#[test]
fn struct_method_receiver_visibility_and_arguments_are_checked_semantically() {
    for (source, expected) in [
        (
            "public struct Value { private int Hidden() { return 1; } } public int Run() { Value value = Value {}; return value.Hidden(); }",
            "method `Value.Hidden` is private",
        ),
        (
            "public struct Value { public int Add(int value) { return value; } } public int Run() { Value receiver = Value {}; return receiver.Add(); }",
            "expected 1 argument(s), found 0",
        ),
        (
            "public struct Value { public int Read() { return 1; } } public int Run() { return 42.Read(); }",
            "`int` has no method `Read`",
        ),
    ] {
        let diagnostics = messages(source);
        assert!(
            diagnostics.iter().any(|message| message.contains(expected)),
            "missing `{expected}` for `{source}`; diagnostics: {diagnostics:?}"
        );
    }
}
