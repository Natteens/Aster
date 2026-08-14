use aster_compiler::{compile, compile_without_loop_string_concat_rewrite_for_research};
use aster_mir as mir;

fn function<'a>(module: &'a mir::Module, name: &str) -> &'a mir::Function {
    module
        .functions
        .iter()
        .find(|function| function.name == name && function.owner.is_none())
        .unwrap_or_else(|| panic!("missing function `{name}`"))
}

fn count(function: &mir::Function, predicate: impl Fn(&mir::Instruction) -> bool) -> usize {
    function
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter(|instruction| predicate(instruction))
        .count()
}

fn concat_count(function: &mir::Function) -> usize {
    count(function, |instruction| {
        matches!(
            instruction,
            mir::Instruction::CallIntrinsic {
                intrinsic: mir::Intrinsic::StringConcat | mir::Intrinsic::StringConcatTemporary,
                ..
            }
        )
    })
}

fn builder_count(function: &mir::Function) -> (usize, usize, usize) {
    (
        count(function, |instruction| {
            matches!(instruction, mir::Instruction::AllocateStringBuilder { .. })
        }),
        count(function, |instruction| {
            matches!(instruction, mir::Instruction::StringBuilderAppend { .. })
        }),
        count(function, |instruction| {
            matches!(instruction, mir::Instruction::StringBuilderToString { .. })
        }),
    )
}

#[test]
fn canonical_loop_concat_rewrites_without_a_source_builder_import() {
    let source = r#"
        public string Run(int count, string part) {
            string value = "";
            int i = 0;
            while (i < count) {
                value = value + part;
                i = i + 1;
            }
            return value;
        }
    "#;
    let baseline =
        compile_without_loop_string_concat_rewrite_for_research(source).expect("baseline compiles");
    assert_eq!(concat_count(function(&baseline.mir, "Run")), 1);
    assert_eq!(builder_count(function(&baseline.mir, "Run")), (0, 0, 0));

    let optimized = compile(source).expect("optimized source compiles");
    let function = function(&optimized.mir, "Run");
    assert_eq!(concat_count(function), 0);
    assert_eq!(builder_count(function), (1, 1, 1));
    assert!(
        optimized
            .mir
            .classes
            .iter()
            .any(|class| class.name == "aster.core::StringBuilder")
    );
}

#[test]
fn canonical_loop_concat_can_materialize_once_for_final_length() {
    let optimized = compile(
        r#"
        public int Run(int count, string part) {
            string value = "";
            int i = 0;
            while (i < count) {
                value = value + part;
                i = i + 1;
            }
            return value.Length;
        }
        "#,
    )
    .expect("source compiles");
    let function = function(&optimized.mir, "Run");
    assert_eq!(concat_count(function), 0);
    assert_eq!(builder_count(function), (1, 1, 1));
}

#[test]
fn rewrite_uses_an_existing_builder_class_without_duplicate_injection() {
    let optimized = compile(
        r#"
        using aster.core;
        public string Run(int count, string part) {
            string value = "";
            int i = 0;
            while (i < count) {
                value = value + part;
                i = i + 1;
            }
            return value;
        }
        "#,
    )
    .expect("source compiles");
    assert_eq!(
        optimized
            .mir
            .classes
            .iter()
            .filter(|class| class.name == "aster.core::StringBuilder")
            .count(),
        1
    );
    assert_eq!(builder_count(function(&optimized.mir, "Run")), (1, 1, 1));
}

#[test]
#[allow(clippy::too_many_lines)]
fn observable_or_ambiguous_loop_concat_shapes_remain_pairwise() {
    for (name, source) in [
        (
            "intermediate-read",
            r#"
            public string Run(int count, string part) {
                string value = ""; int i = 0;
                while (i < count) { int length = value.Length; value = value + part; i = i + 1; }
                return value;
            }
            "#,
        ),
        (
            "alias",
            r#"
            public string Run(int count, string part) {
                string value = ""; int i = 0;
                while (i < count) { string alias = value; value = value + part; i = i + 1; }
                return value;
            }
            "#,
        ),
        (
            "storage",
            r#"
            public string Run(int count, string part) {
                string[] observed = [""];
                string value = ""; int i = 0;
                while (i < count) { observed[0] = value; value = value + part; i = i + 1; }
                return value;
            }
            "#,
        ),
        (
            "call",
            r#"
            public void Observe(string value) {}
            public string Run(int count, string part) {
                string value = ""; int i = 0;
                while (i < count) { Observe(value); value = value + part; i = i + 1; }
                return value;
            }
            "#,
        ),
        (
            "reverse",
            r#"
            public string Run(int count, string part) {
                string value = ""; int i = 0;
                while (i < count) { value = part + value; i = i + 1; }
                return value;
            }
            "#,
        ),
        (
            "accumulator-append-operand",
            r#"
            public string Run(int count) {
                string value = "x"; int i = 0;
                while (i < count) { value = value + value; i = i + 1; }
                return value;
            }
            "#,
        ),
        (
            "conditional-append",
            r#"
            public string Run(int count, string part) {
                string value = ""; int i = 0;
                while (i < count) { if (i == 2) { value = value + part; } i = i + 1; }
                return value;
            }
            "#,
        ),
        (
            "early-return",
            r#"
            public string Run(int count, string part) {
                string value = ""; int i = 0;
                while (i < count) { if (i == 2) { return value; } value = value + part; i = i + 1; }
                return value;
            }
            "#,
        ),
        (
            "nested-loop",
            r#"
            public string Run(int count, string part) {
                string value = ""; int i = 0;
                while (i < count) { int j = 0; while (j < 1) { value = value + part; j = j + 1; } i = i + 1; }
                return value;
            }
            "#,
        ),
        (
            "break",
            r#"
            public string Run(int count, string part) {
                string value = ""; int i = 0;
                while (i < count) { if (i == 2) { break; } value = value + part; i = i + 1; }
                return value;
            }
            "#,
        ),
        (
            "continue",
            r#"
            public string Run(int count, string part) {
                string value = ""; int i = 0;
                while (i < count) { i = i + 1; if (i == 2) { continue; } value = value + part; }
                return value;
            }
            "#,
        ),
    ] {
        let module = compile(source)
            .unwrap_or_else(|diagnostics| panic!("{name} should compile: {diagnostics:?}"));
        let function = function(&module.mir, "Run");
        assert_eq!(builder_count(function), (0, 0, 0), "{name}");
        assert_eq!(concat_count(function), 1, "{name}");
    }
}

#[test]
fn multiple_accumulator_assignments_remain_pairwise() {
    let module = compile(
        r#"
        public string Run(int count, string part) {
            string value = ""; int i = 0;
            while (i < count) {
                value = value + part;
                value = value + part;
                i = i + 1;
            }
            return value;
        }
        "#,
    )
    .expect("source compiles");
    let function = function(&module.mir, "Run");
    assert_eq!(builder_count(function), (0, 0, 0));
    assert_eq!(concat_count(function), 2);
}
