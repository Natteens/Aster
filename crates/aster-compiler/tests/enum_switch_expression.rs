use std::sync::atomic::{AtomicU64, Ordering};

use aster_compiler::{ProjectCompilation, compile, compile_project, hir, mir};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

fn compile_with_stdlib(source: &str) -> ProjectCompilation {
    let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "aster-switch-expression-{}-{id}.aster",
        std::process::id()
    ));
    std::fs::write(&path, source).expect("write temporary source");
    let result = compile_project(&path).expect("project with official standard library compiles");
    std::fs::remove_file(path).ok();
    result
}

fn messages(source: &str) -> Vec<String> {
    compile(source)
        .expect_err("source should be rejected")
        .into_iter()
        .map(|diagnostic| diagnostic.message)
        .collect()
}

#[test]
fn resolves_switch_expression_to_typed_hir_and_cfg_mir() {
    let compilation = compile(
        "public enum Message { Quit, Move(int x, int y) } public int Read(Message message) { return message switch { Quit => 0, Move(x, y) => x + y, }; }",
    )
    .expect("valid switch expression compiles");
    let read = compilation
        .hir
        .items
        .iter()
        .find_map(|item| match item {
            hir::Item::Function(function) if function.name == "Read" => Some(function),
            _ => None,
        })
        .expect("Read HIR");
    let hir::Statement::Return(Some(expression)) = &read.body.as_ref().expect("body").statements[0]
    else {
        panic!("switch expression return")
    };
    let hir::ExpressionKind::Switch { cases, .. } = &expression.kind else {
        panic!("typed switch expression HIR")
    };
    assert_eq!(expression.type_, hir::Type::Int);
    assert_eq!(cases.len(), 2);
    assert_eq!(cases[1].bindings.len(), 2);
    assert!(format!("{:#?}", compilation.hir).contains("Switch"));

    let read = compilation
        .mir
        .functions
        .iter()
        .find(|function| function.name == "Read")
        .expect("Read MIR");
    assert!(
        read.blocks
            .iter()
            .any(|block| matches!(block.terminator, mir::Terminator::Branch { .. }))
    );
    assert!(
        read.blocks
            .iter()
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
    let return_block = read
        .blocks
        .iter()
        .find(|block| matches!(block.terminator, mir::Terminator::Return(_)))
        .expect("switch result joins before return");
    let result_local = match &return_block.terminator {
        mir::Terminator::Return(Some(mir::Operand {
            kind: mir::OperandKind::Copy(mir::Place::Local(local)),
            ..
        })) => *local,
        other => panic!("typed result local return, got {other:?}"),
    };
    assert_eq!(
        read.locals
            .iter()
            .find(|local| local.id == result_local)
            .expect("result local")
            .type_,
        mir::Type::Int
    );
    assert_eq!(
        read.blocks
            .iter()
            .filter(|block| {
                matches!(block.terminator, mir::Terminator::Goto(target) if target == return_block.id)
            })
            .count(),
        2
    );
    let mir_dump = format!("{:#?}", compilation.mir);
    assert!(mir_dump.contains("Discriminant"));
    assert!(mir_dump.contains("Branch"));
    assert!(mir_dump.contains("Goto"));
}

#[test]
fn validates_switch_expression_patterns_exhaustiveness_and_result_types() {
    for (source, expected) in [
        (
            "public int Read(int value) { return value switch { Anything => 1, }; }",
            "requires an enum value",
        ),
        (
            "public enum E { A, B } public int Read(E value) { return value switch { A => 1, }; }",
            "non-exhaustive switch",
        ),
        (
            "public enum E { A, B } public int Read(E value) { return value switch { A => 1, A => 2, B => 3, }; }",
            "duplicate switch case",
        ),
        (
            "public enum E { Pair(int left, int right) } public int Read(E value) { return value switch { Pair(one) => one, }; }",
            "expects 2 binding",
        ),
        (
            "public enum E { A, B } public int Read(E value) { return value switch { A => 1, B => \"bad\", }; }",
            "incompatible types",
        ),
        (
            "public enum E { A } public enum Other { A } public int Read(E value) { return value switch { Other.A => 1, }; }",
            "belongs to `Other`, not `E`",
        ),
        (
            "public enum E { A } public int Read(E value) { return value switch { Missing => 1, }; }",
            "has no case `Missing`",
        ),
        (
            "public enum E { A, B } public void Stop() {} public int Read(E value) { return value switch { A => Stop(), B => Stop(), }; }",
            "must produce a value",
        ),
    ] {
        assert!(
            messages(source)
                .iter()
                .any(|message| message.contains(expected)),
            "expected diagnostic containing {expected:?}"
        );
    }
}

#[test]
fn official_option_result_and_common_type_are_supported() {
    let compilation = compile_with_stdlib(
        "using aster.core; public enum E { A, B } public long Promote(E value) { return value switch { A => 1, B => 2L, }; } public int ReadOption(Option<int> value) { return value switch { Some(item) => item, None => 0, }; } public int ReadResult(Result<int, string> value) { return value switch { Ok(item) => item, Error(error) => error.Length, }; }",
    );
    let promote = compilation
        .compilation
        .hir
        .items
        .iter()
        .find_map(|item| match item {
            hir::Item::Function(function) if function.name == "Promote" => Some(function),
            _ => None,
        })
        .expect("Promote HIR");
    let hir::Statement::Return(Some(expression)) =
        &promote.body.as_ref().expect("body").statements[0]
    else {
        panic!("switch return")
    };
    assert_eq!(expression.type_, hir::Type::Long);
}

#[test]
fn payload_bindings_are_scoped_and_generic_enums_are_concrete() {
    let scope = messages(
        "public enum E { Some(int value), None } public int Read(E item) { int result = item switch { Some(value) => value, None => 0, }; return value; }",
    );
    assert!(
        scope
            .iter()
            .any(|message| message.contains("unknown name `value`"))
    );

    let compilation = compile(
        "public enum Option<T> { Some(T value), None } public int Read(Option<int> item) { return item switch { Some(value) => value, None => 0, }; }",
    )
    .expect("generic enum switch expression specializes");
    assert!(compilation.hir.items.iter().any(|item| matches!(
        item,
        hir::Item::Enum(definition) if definition.name == "Option<int>"
    )));

    let compilation = compile(
        "public enum Choice<T> { Some(T value), None } public T Read<T>(Choice<T> item, T fallback) { return item switch { Some(value) => value, None => fallback, }; } public int Run() { return Read<int>(Choice<int>.Some(42), 0); }",
    )
    .expect("generic function switch expression specializes");
    assert!(compilation.hir.items.iter().any(|item| matches!(
        item,
        hir::Item::Function(function) if function.name.starts_with("Read#")
    )));
}
