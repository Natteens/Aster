//! T-LANG-4a: semantic rules of `switch` on integer, `char` and `string`.

use aster_compiler::{compile, hir, mir};

fn messages(source: &str) -> Vec<String> {
    compile(source)
        .expect_err("source should be rejected")
        .into_iter()
        .map(|diagnostic| diagnostic.message)
        .collect()
}

fn rejected_with(source: &str, fragment: &str) {
    let messages = messages(source);
    assert!(
        messages.iter().any(|message| message.contains(fragment)),
        "expected a diagnostic containing `{fragment}`, found {messages:?}"
    );
}

#[test]
fn accepts_scalar_switches_and_lowers_them_to_branches() {
    let compilation = compile(
        r#"
        public int A(int v) { switch (v) { case 1: return 1; case 2: return 2; default: return 0; } }
        public int B(char v) { switch (v) { case 'a': return 1; default: return 0; } }
        public int C(string v) { switch (v) { case "x": return 1; case "": return 2; default: return 0; } }
        public int D(long v) { return v switch { 1 => 1, default => 0, }; }
        "#,
    )
    .expect("valid scalar switches compile");
    let function = |name: &str| {
        compilation
            .mir
            .functions
            .iter()
            .find(|function| function.name == name)
            .expect("function MIR")
    };
    for name in ["A", "B", "C", "D"] {
        let function = function(name);
        assert!(
            function
                .blocks
                .iter()
                .any(|block| matches!(block.terminator, mir::Terminator::Branch { .. })),
            "{name} lowers to branches"
        );
        assert!(
            !function
                .blocks
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
                )),
            "{name} does not read an enum discriminant"
        );
    }
    let uses_string_equality = |function: &mir::Function| {
        function
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .any(|instruction| {
                matches!(
                    instruction,
                    mir::Instruction::CallIntrinsic {
                        intrinsic: mir::Intrinsic::StringEquals,
                        ..
                    }
                )
            })
    };
    assert!(uses_string_equality(function("C")));
    assert!(!uses_string_equality(function("A")));
    assert!(format!("{:#?}", compilation.hir).contains("label: Some"));
    assert!(
        compilation
            .hir
            .items
            .iter()
            .any(|item| matches!(item, hir::Item::Function(function) if function.name == "A"))
    );
}

#[test]
fn rejects_duplicate_constant_labels() {
    rejected_with(
        "public int F(int v) { switch (v) { case 1: return 1; case 0 + 1: return 2; default: return 0; } }",
        "duplicate switch case `1`",
    );
    rejected_with(
        "public int F(char v) { switch (v) { case 'a': return 1; case 'a': return 2; default: return 0; } }",
        "duplicate switch case `'a'`",
    );
    rejected_with(
        r#"public int F(string v) { switch (v) { case "a": return 1; case "a": return 2; default: return 0; } }"#,
        "duplicate switch case `\"a\"`",
    );
    rejected_with(
        r#"public int F(string v) { return v switch { "a" => 1, "a" => 2, default => 0, }; }"#,
        "duplicate switch case",
    );
    rejected_with(
        "const int Two = 2; public int F(byte v) { switch (v) { case Two: return 1; case 2: return 2; default: return 0; } }",
        "duplicate switch case `2`",
    );
}

#[test]
fn requires_default_for_non_enum_switches() {
    rejected_with(
        "public int F(int v) { switch (v) { case 1: return 1; } return 0; }",
        "requires a `default` case",
    );
    rejected_with(
        "public int F(char v) { switch (v) { case 'a': return 1; } return 0; }",
        "requires a `default` case",
    );
    rejected_with(
        r#"public int F(string v) { switch (v) { case "a": return 1; } return 0; }"#,
        "requires a `default` case",
    );
    rejected_with(
        "public int F(int v) { return v switch { 1 => 1, }; }",
        "requires a `default` case",
    );
}

#[test]
fn rejects_labels_of_the_wrong_type_or_out_of_range() {
    rejected_with(
        r#"public int F(int v) { switch (v) { case "a": return 1; default: return 0; } }"#,
        "expected `int`, found `string`",
    );
    rejected_with(
        "public int F(string v) { switch (v) { case 1: return 1; default: return 0; } }",
        "expected `string`, found `int`",
    );
    rejected_with(
        "public int F(char v) { switch (v) { case 97: return 1; default: return 0; } }",
        "expected `char`",
    );
    rejected_with(
        "public int F(byte v) { switch (v) { case 300: return 1; default: return 0; } }",
        "does not fit `byte`",
    );
    rejected_with(
        "public int F(int v) { switch (v) { case 5000000000L: return 1; default: return 0; } }",
        "does not fit `int`",
    );
    rejected_with(
        "public int F(int v) { switch (v) { case 1.5: return 1; default: return 0; } }",
        "expected `int`, found `float`",
    );
}

#[test]
fn rejects_non_constant_labels() {
    rejected_with(
        "public int F(int v, int w) { switch (v) { case w: return 1; default: return 0; } }",
        "must be a constant expression",
    );
    rejected_with(
        "public int G() { return 1; } public int F(int v) { switch (v) { case G(): return 1; default: return 0; } }",
        "must be a constant label",
    );
    rejected_with(
        r"public int F(string v, string w) { return v switch { w => 1, default => 0, }; }",
        "must be a constant expression",
    );
}

#[test]
fn rejects_enum_patterns_and_unsupported_subjects() {
    rejected_with(
        "public enum E { A, B } public int F(int v) { switch (v) { case A: return 1; default: return 0; } }",
        "A",
    );
    rejected_with(
        "public int F(bool v) { switch (v) { case true: return 1; default: return 0; } }",
        "requires an enum, integer, `char` or `string` value, found `bool`",
    );
    rejected_with(
        "public int F(double v) { return v switch { 1.0 => 1, default => 0, }; }",
        "requires an enum, integer, `char` or `string` value, found `double`",
    );
}

#[test]
fn enum_switches_stay_exhaustive_without_default() {
    compile(
        "public enum E { A, B } public int F(E v) { switch (v) { case A: return 1; case B: return 2; } }",
    )
    .expect("exhaustive enum switch needs no default");
    rejected_with(
        "public enum E { A, B } public int F(E v) { switch (v) { case A: return 1; } return 0; }",
        "non-exhaustive switch",
    );
}

#[test]
fn allows_default_only_scalar_switch_and_unreachable_flow_rules() {
    compile("public int F(int v) { switch (v) { default: return 7; } }")
        .expect("a default-only switch is exhaustive by construction");
}
