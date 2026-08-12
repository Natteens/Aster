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
            .filter(|intrinsic| {
                matches!(
                    **intrinsic,
                    mir::Intrinsic::StringConcat | mir::Intrinsic::StringConcatTemporary
                )
            })
            .count(),
        2
    );
    assert_eq!(
        intrinsics
            .iter()
            .filter_map(|intrinsic| intrinsic.string_allocation_region())
            .collect::<Vec<_>>(),
        vec![
            mir::AllocationRegion::Temporary,
            mir::AllocationRegion::Temporary,
        ]
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

#[test]
fn static_string_concat_chains_lower_to_one_join() {
    let compilation = compile(
        r#"
        public string Run() {
            string a = "a";
            string b = "b";
            string c = "c";
            string d = "d";
            string e = "e";
            return ((a + b) + (c + d)) + e;
        }
        "#,
    )
    .expect("static string chain should compile");

    let calls = compilation.mir.functions[0]
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| {
            let mir::Instruction::CallIntrinsic {
                intrinsic,
                arguments,
                ..
            } = instruction
            else {
                return None;
            };
            Some((*intrinsic, arguments.len()))
        })
        .collect::<Vec<_>>();

    assert_eq!(
        calls
            .iter()
            .filter(|(intrinsic, _)| {
                matches!(
                    intrinsic,
                    mir::Intrinsic::StringConcat | mir::Intrinsic::StringConcatTemporary
                )
            })
            .count(),
        0
    );
    assert_eq!(
        calls
            .iter()
            .filter(|(intrinsic, arguments)| {
                matches!(
                    intrinsic,
                    mir::Intrinsic::StringJoin | mir::Intrinsic::StringJoinTemporary
                ) && *arguments == 5
            })
            .count(),
        1
    );
}

#[test]
fn concat_with_effectful_or_interpolated_leaves_keeps_pairwise_order() {
    let compilation = compile(
        r#"
        public string A() { return "a"; }
        public string B() { return "b"; }
        public string C() { return "c"; }
        public string Calls() { return A() + B() + C(); }
        public string Mixed(string value) { return "a" + $"{value}" + "c"; }
        "#,
    )
    .expect("ordered string expressions should compile");

    for name in ["Calls", "Mixed"] {
        let function = compilation
            .mir
            .functions
            .iter()
            .find(|function| function.name == name)
            .expect("function exists");
        let concat_count = function
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .filter(|instruction| {
                matches!(
                    instruction,
                    mir::Instruction::CallIntrinsic {
                        intrinsic: mir::Intrinsic::StringConcat
                            | mir::Intrinsic::StringConcatTemporary,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(concat_count, 2, "{name} must retain pairwise ordering");
    }
}

#[test]
fn literal_and_large_static_chains_compile_without_intermediate_concats() {
    let literals = compile(r#"public string Run() { return "a" + "b" + "c"; }"#)
        .expect("literal chain compiles");
    assert!(
        literals.mir.functions[0]
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .all(|instruction| !matches!(
                instruction,
                mir::Instruction::CallIntrinsic {
                    intrinsic: mir::Intrinsic::StringConcat | mir::Intrinsic::StringConcatTemporary,
                    ..
                }
            ))
    );

    let mut source = String::from("public string Run() { string p = \"x\"; return p");
    for _ in 1..32 {
        source.push_str(" + p");
    }
    source.push_str("; }");
    let large = compile(&source).expect("large static chain compiles");
    assert!(
        large.mir.functions[0]
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .any(|instruction| matches!(
                instruction,
                mir::Instruction::CallIntrinsic {
                    intrinsic: mir::Intrinsic::StringJoin | mir::Intrinsic::StringJoinTemporary,
                    arguments,
                    ..
                } if arguments.len() == 32
            ))
    );
}
