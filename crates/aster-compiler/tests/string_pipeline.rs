use aster_compiler::compile;
use aster_hir as hir;
use aster_mir as mir;

fn messages(source: &str) -> Vec<String> {
    compile(source)
        .expect_err("source should be rejected")
        .into_iter()
        .map(|diagnostic| diagnostic.message)
        .collect()
}

#[test]
fn strings_reach_typed_hir_and_mir() {
    let compilation = compile(
        r#"
        public int Run(string name) {
            string message = "Olá, " + name;
            message += "!";
            return message.Length;
        }
        "#,
    )
    .expect("string operations should compile");

    let hir::Item::Function(function) = &compilation.hir.items[0] else {
        panic!("expected function HIR");
    };
    let body = function.body.as_ref().expect("function body");
    assert!(body.statements.iter().any(|statement| {
        matches!(
            statement,
            hir::Statement::Return(Some(hir::Expression {
                kind: hir::ExpressionKind::StringLength(_),
                type_: hir::Type::Int,
            }))
        )
    }));

    let function = &compilation.mir.functions[0];
    let intrinsics = function
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| {
            let mir::Instruction::CallIntrinsic { intrinsic, .. } = instruction else {
                return None;
            };
            Some(*intrinsic)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        intrinsics
            .iter()
            .filter(|intrinsic| **intrinsic == mir::Intrinsic::StringConcat)
            .count(),
        2
    );
    assert!(intrinsics.contains(&mir::Intrinsic::StringLength));
}

#[test]
fn string_diagnostics_reject_implicit_text_conversion_and_length_assignment() {
    assert!(
        messages(r#"public string Bad() { return "value=" + 42; }"#)
            .iter()
            .any(|message| message.contains("requires two `string` operands"))
    );
    assert!(
        messages(r#"public void Bad() { string value = "x"; value.Length = 1; }"#)
            .iter()
            .any(|message| message.contains("string Length is read-only"))
    );
    assert!(
        messages(r#"public int Bad() { string value = "x"; return value[0]; }"#)
            .iter()
            .any(|message| message.contains("type `string` cannot be indexed"))
    );
}
