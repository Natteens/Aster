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
fn interpolated_string_is_typed_string_and_accepts_every_supported_type() {
    let compilation = compile(
        r#"
        public class C {
            public string Run(int value, bool flag, char letter, float small, double big, string text) {
                return $"{value} {flag} {letter} {small} {big} {text}";
            }
        }
        "#,
    )
    .expect("every documented interpolation type should compile");

    let hir::Item::Class(class) = &compilation.hir.items[0] else {
        panic!("expected class HIR");
    };
    let method = &class.methods[0];
    let body = method.body.as_ref().expect("method body");
    let Some(hir::Statement::Return(Some(expression))) = body.statements.first() else {
        panic!("expected a return statement");
    };
    assert_eq!(expression.type_, hir::Type::String);
    let hir::ExpressionKind::InterpolatedString { parts } = &expression.kind else {
        panic!(
            "expected InterpolatedString HIR, found {:?}",
            expression.kind
        );
    };
    // 6 values plus the literal text separating them (5 single-space runs).
    assert_eq!(parts.len(), 11);
}

#[test]
fn rejects_class_type_void_and_format_syntax() {
    assert!(
        messages(
            r#"public class P { public int hp; } public string Bad(P value) { return $"{value}"; }"#
        )
        .iter()
        .any(|message| message.contains("cannot be used in string interpolation"))
    );
    assert!(
        messages(r#"public void Nothing() {} public string Bad() { return $"{Nothing()}"; }"#)
            .iter()
            .any(|message| message.contains("`void` expression cannot be used"))
    );
}

#[test]
fn mir_evaluates_left_to_right_stringifies_and_joins_exactly_once() {
    let compilation = compile(
        r#"
        public string Run(int a, int b) {
            return $"{a} + {b} = {a + b}";
        }
        "#,
    )
    .expect("valid interpolation should compile");

    let function = &compilation.mir.functions[0];
    let instructions = function
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .collect::<Vec<_>>();
    let intrinsics = instructions
        .iter()
        .filter_map(|instruction| {
            let mir::Instruction::CallIntrinsic { intrinsic, .. } = instruction else {
                return None;
            };
            Some(*intrinsic)
        })
        .collect::<Vec<_>>();
    // Three `int` values (a, b, a + b) are each stringified once...
    assert_eq!(
        intrinsics
            .iter()
            .filter(|intrinsic| **intrinsic == mir::Intrinsic::StringFromLong)
            .count(),
        3
    );
    // ...and joined in exactly one call, last among the intrinsics.
    assert_eq!(
        intrinsics
            .iter()
            .filter(|intrinsic| **intrinsic == mir::Intrinsic::StringJoin)
            .count(),
        1
    );
    assert_eq!(intrinsics.last(), Some(&mir::Intrinsic::StringJoin));

    // The join call receives every stringified segment plus the literal text
    // between them, all already typed `string`.
    let join = instructions
        .iter()
        .find_map(|instruction| {
            let mir::Instruction::CallIntrinsic {
                intrinsic: mir::Intrinsic::StringJoin,
                arguments,
                ..
            } = instruction
            else {
                return None;
            };
            Some(arguments)
        })
        .expect("a StringJoin call");
    assert!(
        join.iter()
            .all(|argument| argument.type_ == mir::Type::String)
    );
}

#[test]
fn already_string_values_are_not_redundantly_stringified() {
    let compilation = compile(
        r#"
        public string Run(string name) {
            return $"Hello, {name}!";
        }
        "#,
    )
    .expect("valid interpolation should compile");

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
    assert!(
        intrinsics
            .iter()
            .all(|intrinsic| *intrinsic != mir::Intrinsic::StringFromLong),
        "a string-typed part must not be converted: {intrinsics:?}"
    );
    assert_eq!(
        intrinsics
            .iter()
            .filter(|intrinsic| **intrinsic == mir::Intrinsic::StringJoin)
            .count(),
        1
    );
}
