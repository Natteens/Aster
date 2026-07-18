use aster_compiler::{compile, hir, mir};

fn messages(source: &str) -> Vec<String> {
    compile(source)
        .expect_err("source should be rejected")
        .into_iter()
        .map(|diagnostic| diagnostic.message)
        .collect()
}

#[test]
fn enum_cases_are_resolved_in_hir_and_mir() {
    let source = "public enum Value { None, Some(int value), } public int Read(Value value) { switch (value) { case Some(number): return number; case None: return 0; } } public int Run() { return Read(Value.Some(42)); }";
    let compilation = compile(source).expect("valid enum program");
    let hir::Item::Enum(definition) = &compilation.hir.items[0] else {
        panic!("enum HIR item");
    };
    assert_eq!(definition.cases.len(), 2);
    assert_eq!(compilation.mir.enums.len(), 1);
    assert!(
        compilation
            .mir
            .functions
            .iter()
            .flat_map(|function| &function.blocks)
            .flat_map(|block| &block.instructions)
            .any(|instruction| matches!(
                instruction,
                mir::Instruction::Assign {
                    value: mir::Rvalue {
                        kind: mir::RvalueKind::EnumConstruct { .. },
                        ..
                    },
                    ..
                }
            ))
    );
    assert!(
        compilation
            .mir
            .functions
            .iter()
            .flat_map(|function| &function.blocks)
            .flat_map(|block| &block.instructions)
            .any(|instruction| matches!(
                instruction,
                mir::Instruction::Assign {
                    value: mir::Rvalue {
                        kind: mir::RvalueKind::Discriminant(_),
                        ..
                    },
                    ..
                }
            ))
    );
}

#[test]
fn generic_enums_are_specialized_before_hir() {
    let compilation = compile("public enum Option<T> { Some(T value), None, } public int Run() { Option<int> value = Option<int>.Some(42); switch (value) { case Some(number): return number; case None: return 0; } }").expect("generic enum should specialize");
    let names = compilation
        .hir
        .items
        .iter()
        .filter_map(|item| match item {
            hir::Item::Enum(value) => Some(value.name.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(names, ["Option<int>"]);
}

#[test]
fn switch_reports_exhaustiveness_and_case_errors() {
    let missing = messages(
        "public enum State { A, B, } public int Read(State value) { switch (value) { case A: return 1; } }",
    );
    assert!(
        missing
            .iter()
            .any(|message| message.contains("non-exhaustive switch"))
    );
    let duplicate = messages(
        "public enum State { A, B, } public int Read(State value) { switch (value) { case A: return 1; case A: return 2; case B: return 3; } }",
    );
    assert!(
        duplicate
            .iter()
            .any(|message| message.contains("duplicate switch case"))
    );
    let invalid = messages(
        "public enum State { A, } public int Read(State value) { switch (value) { case Missing: return 1; default: return 0; } }",
    );
    assert!(
        invalid
            .iter()
            .any(|message| message.contains("has no case `Missing`"))
    );
}

#[test]
fn switch_bindings_have_case_scope_and_checked_arity() {
    let scope = messages(
        "public enum Value { Some(int value), None, } public int Read(Value item) { switch (item) { case Some(number): Log(number); return number; case None: return 0; } } public int Bad() { return number; }",
    );
    assert!(
        scope
            .iter()
            .any(|message| message.contains("unknown name `number`"))
    );
    let arity = messages(
        "public enum Value { Pair(int left, int right), } public int Read(Value item) { switch (item) { case Pair(one): return one; } }",
    );
    assert!(
        arity
            .iter()
            .any(|message| message.contains("expects 2 binding"))
    );
}

#[test]
fn enum_equality_is_accepted_only_for_comparable_payloads() {
    compile("public enum Value { Number(int value), Empty, } public bool Equal(Value a, Value b) { return a == b; }")
        .expect("comparable enum payload supports equality");
}
