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
fn every_string_method_signature_reaches_typed_hir_and_mir() {
    let compilation = compile(
        r#"
        public int Run(string text) {
            bool contains = text.Contains("a");
            bool starts = text.StartsWith("a");
            bool ends = text.EndsWith("z");
            int index = text.IndexOf("b");
            string tail = text.Substring(1);
            string middle = text.Substring(1, 2);
            return index + tail.Length + middle.Length;
        }
        "#,
    )
    .expect("all string operations compile");

    let hir::Item::Function(function) = &compilation.hir.items[0] else {
        panic!("expected function HIR");
    };
    let operations = function
        .body
        .as_ref()
        .expect("function body")
        .statements
        .iter()
        .filter_map(|statement| {
            let hir::Statement::Variable(variable) = statement else {
                return None;
            };
            let hir::ExpressionKind::StringOperation { operation, .. } =
                &variable.initializer.as_ref()?.kind
            else {
                return None;
            };
            Some(*operation)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        operations,
        vec![
            hir::StringOperation::Contains,
            hir::StringOperation::StartsWith,
            hir::StringOperation::EndsWith,
            hir::StringOperation::IndexOf,
            hir::StringOperation::SubstringFrom,
            hir::StringOperation::SubstringRange,
        ]
    );

    let intrinsics = compilation.mir.functions[0]
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| {
            let mir::Instruction::CallIntrinsic { intrinsic, .. } = instruction else {
                return None;
            };
            matches!(
                intrinsic,
                mir::Intrinsic::StringContains
                    | mir::Intrinsic::StringStartsWith
                    | mir::Intrinsic::StringEndsWith
                    | mir::Intrinsic::StringIndexOf
                    | mir::Intrinsic::StringSubstringFrom
                    | mir::Intrinsic::StringSubstringFromTemporary
                    | mir::Intrinsic::StringSubstringRange
                    | mir::Intrinsic::StringSubstringRangeTemporary
            )
            .then_some(*intrinsic)
        })
        .collect::<Vec<_>>();
    assert_eq!(intrinsics.len(), 6);
    assert_eq!(
        intrinsics
            .iter()
            .filter_map(|intrinsic| intrinsic.string_allocation_region())
            .collect::<Vec<_>>(),
        vec![
            mir::AllocationRegion::Temporary,
            mir::AllocationRegion::Temporary
        ]
    );
}

#[test]
fn string_methods_reject_wrong_arity_and_argument_types() {
    for (source, expected) in [
        (
            r"public bool Bad(string text) { return text.Contains(); }",
            "expects 1 argument, found 0",
        ),
        (
            r#"public bool Bad(string text) { return text.Contains("a", "b"); }"#,
            "expects 1 argument, found 2",
        ),
        (
            r"public bool Bad(string text) { return text.Contains(1); }",
            "expected `string`, found `int`",
        ),
        (
            r#"public bool Bad(string text) { return text.StartsWith("a", "b"); }"#,
            "expects 1 argument, found 2",
        ),
        (
            r"public bool Bad(string text) { return text.EndsWith(1); }",
            "expected `string`, found `int`",
        ),
        (
            r"public int Bad(string text) { return text.IndexOf(); }",
            "expects 1 argument, found 0",
        ),
        (
            r"public int Bad(string text) { return text.IndexOf(1); }",
            "expected `string`, found `int`",
        ),
        (
            r"public string Bad(string text) { return text.Substring(); }",
            "expects 1 or 2 arguments, found 0",
        ),
        (
            r#"public string Bad(string text) { return text.Substring("1"); }"#,
            "expected `int`, found `string`",
        ),
        (
            r#"public string Bad(string text) { return text.Substring(0, "1"); }"#,
            "expected `int`, found `string`",
        ),
        (
            r"public string Bad(string text) { return text.Substring(0, 1, 2); }",
            "expects 1 or 2 arguments, found 3",
        ),
    ] {
        assert!(
            messages(source)
                .iter()
                .any(|message| message.contains(expected)),
            "missing `{expected}` for {source}"
        );
    }
}

#[test]
fn same_named_user_methods_keep_normal_resolution() {
    let compilation = compile(
        r#"
        public class TextProbe {
            public int Contains(int value) { return value + 1; }
            public string Substring(string value) { return value; }
        }
        public int Run() {
            TextProbe probe = new TextProbe();
            string value = probe.Substring("ok");
            return probe.Contains(41) + value.Length - 2;
        }
        "#,
    )
    .expect("user methods resolve normally");
    assert!(compilation.mir.functions.iter().all(|function| {
        function.blocks.iter().all(|block| {
            block.instructions.iter().all(|instruction| {
                !matches!(
                    instruction,
                    mir::Instruction::CallIntrinsic {
                        intrinsic: mir::Intrinsic::StringContains
                            | mir::Intrinsic::StringSubstringFrom
                            | mir::Intrinsic::StringSubstringFromTemporary,
                        ..
                    }
                )
            })
        })
    }));
}

#[test]
fn substring_escape_analysis_classifies_the_result_independently() {
    let compilation = compile(
        r#"
        internal int Local(string value) {
            string slice = value.Substring(1, 2);
            return slice.Length;
        }
        internal string Returned(string value) {
            return value.Substring(1);
        }
        public int Run() { return Local("abcd") + Returned("abcd").Length; }
        "#,
    )
    .expect("substring escape cases compile");

    let regions = |name: &str| {
        compilation
            .mir
            .functions
            .iter()
            .find(|function| function.name == name)
            .expect("function exists")
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .filter_map(|instruction| {
                let mir::Instruction::CallIntrinsic { intrinsic, .. } = instruction else {
                    return None;
                };
                intrinsic.string_allocation_region()
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(regions("Local"), vec![mir::AllocationRegion::Temporary]);
    assert_eq!(regions("Returned"), vec![mir::AllocationRegion::Persistent]);
}

#[test]
fn concurrency_still_rejects_string_results() {
    assert!(
        messages(
            r#"
        public string Slice() { return "aster".Substring(1); }
        public int Run() { Task<string> task = Task.Run(Slice); return 0; }
        "#
        )
        .iter()
        .any(|message| message.contains("cannot cross a worker boundary"))
    );
}
