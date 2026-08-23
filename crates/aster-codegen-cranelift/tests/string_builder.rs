use std::sync::atomic::{AtomicU64, Ordering};

use aster_codegen_cranelift::{ExecutionValue, execute, execute_with_stats};
use aster_compiler::{compile_project, mir};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

fn compile(source: &str) -> Result<mir::Module, String> {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "aster-string-builder-{}-{id}.aster",
        std::process::id()
    ));
    std::fs::write(&path, source).expect("write temporary StringBuilder source");
    let result = compile_project(&path)
        .map(|project| project.compilation.mir)
        .map_err(|diagnostics| format!("{diagnostics:#?}"));
    std::fs::remove_file(path).expect("remove temporary StringBuilder source");
    result
}

fn run(source: &str) -> Result<ExecutionValue, String> {
    execute(&compile(source)?, "Main").map_err(|error| error.to_string())
}

#[test]
#[allow(clippy::unicode_not_nfc)] // deliberately verifies a combining scalar sequence
fn empty_append_alias_snapshot_and_unicode_semantics() {
    let source = r#"
        using aster.core;
        public int Main() {
            StringBuilder a = new StringBuilder();
            StringBuilder b = a;
            string empty = a.ToString();
            if (empty != "") { return 5; }
            b.Append("");
            b.Append("A");
            b.Append("á");
            string first = a.ToString();
            a.Append("😀");
            a.Append("é");
            string second = b.ToString();
            if (first != "Aá") { return 1; }
            if (second != "Aá😀é") { return 2; }
            if (second.Length != 5) { return 3; }
            int count = 0;
            foreach (char value in second) { count += 1; }
            return count == 5 ? 42 : 4;
        }
    "#;
    assert_eq!(run(source), Ok(ExecutionValue::Int(42)));
}

#[test]
fn large_single_append_and_persistent_builder_storage_are_valid() {
    let large = "x".repeat(64 * 1024);
    let source = format!(
        r#"
            using aster.core;
            public class Holder {{
                public StringBuilder Builder;
                public Holder(StringBuilder builder) {{ Builder = builder; }}
            }}
            internal StringBuilder Make() {{ return new StringBuilder(); }}
            public int Main() {{
                Holder holder = new Holder(Make());
                holder.Builder.Append("{large}");
                return holder.Builder.ToString().Length;
            }}
        "#
    );
    let module = compile(&source).expect("large append source compiles");
    let (value, stats) = execute_with_stats(&module, "Main").expect("large append source runs");
    assert_eq!(value, ExecutionValue::Int(64 * 1024));
    assert_eq!(stats.string_allocations, 1);
    assert!(stats.used_bytes > 0, "escaped builder stays persistent");
    assert!(stats.requested_bytes < 200_000);
}

#[test]
fn public_surface_rejects_wrong_constructor_and_method_shapes() {
    for (body, expected) in [
        (
            "StringBuilder builder = new StringBuilder(1); return 0;",
            "too many arguments for this callable",
        ),
        (
            "StringBuilder builder = new StringBuilder(); builder.Append(1); return 0;",
            "expected `string`, found `int`",
        ),
        (
            "StringBuilder builder = new StringBuilder(); builder.ToString(\"x\"); return 0;",
            "too many arguments for this callable",
        ),
    ] {
        let source = format!("using aster.core; public int Main() {{ {body} }}");
        let diagnostics = compile(&source).expect_err("invalid builder call must be rejected");
        assert!(diagnostics.contains(expected), "{diagnostics}");
    }
}

#[test]
fn unrelated_class_methods_named_to_string_remain_ordinary_calls() {
    let source = r#"
        public class Value {
            public Value() {}
            public string ToString() { return "ordinary"; }
        }
        public int Main() { return new Value().ToString() == "ordinary" ? 42 : 0; }
    "#;
    assert_eq!(run(source), Ok(ExecutionValue::Int(42)));
}

#[test]
fn builder_crosses_helpers_and_returned_strings_own_their_storage() {
    let source = r#"
        using aster.core;
        internal void AppendSuffix(StringBuilder builder) { builder.Append("ter"); }
        internal string Build() {
            StringBuilder builder = new StringBuilder();
            builder.Append("as");
            AppendSuffix(builder);
            return builder.ToString();
        }
        public int Main() { return Build() == "aster" ? 42 : 0; }
    "#;
    let module = compile(source).expect("helper source compiles");
    let (value, stats) = execute_with_stats(&module, "Main").expect("helper source runs");
    assert_eq!(value, ExecutionValue::Int(42));
    assert_eq!(stats.string_allocations, 1);
    assert!(stats.object_allocations >= 2);
    assert!(stats.used_bytes > 0, "returned snapshot is persistent");
}

#[test]
fn many_small_appends_use_geometric_storage() {
    let source = r#"
        using aster.core;
        public int Main() {
            StringBuilder builder = new StringBuilder();
            for (int i = 0; i < 20000; i++) { builder.Append("x"); }
            return builder.ToString().Length;
        }
    "#;
    let module = compile(source).expect("stress source compiles");
    let (value, stats) = execute_with_stats(&module, "Main").expect("stress source runs");
    assert_eq!(value, ExecutionValue::Int(20_000));
    assert_eq!(stats.string_allocations, 1);
    assert!(stats.total_allocations < 32);
    assert!(stats.requested_bytes < 100_000);
}

#[test]
fn string_builder_is_not_worker_transferable() {
    let source = r"
        using aster.core;
        public StringBuilder Build() { return new StringBuilder(); }
        public int Main() { Task<StringBuilder> task = Task.Run(Build); return 0; }
    ";
    let diagnostics = compile(source).expect_err("mutable builder cannot cross worker boundary");
    assert!(diagnostics.contains("scalar results"), "{diagnostics}");
}

#[test]
fn lowering_uses_typed_builder_operations_and_regions() {
    let module = compile(
        r#"
            using aster.core;
            internal int Local() {
                StringBuilder builder = new StringBuilder();
                builder.Append("x");
                return builder.ToString().Length;
            }
            public string Main() {
                StringBuilder builder = new StringBuilder();
                builder.Append("result");
                return builder.ToString();
            }
        "#,
    )
    .expect("lowering source compiles");

    let mut local_regions = Vec::new();
    let mut main_regions = Vec::new();
    for function in &module.functions {
        let regions = function
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .filter_map(|instruction| match instruction {
                mir::Instruction::AllocateStringBuilder { region, .. }
                | mir::Instruction::StringBuilderToString { region, .. } => Some(*region),
                _ => None,
            })
            .collect::<Vec<_>>();
        match function.name.as_str() {
            "Local" => local_regions = regions,
            "Main" => main_regions = regions,
            _ => {}
        }
    }
    assert_eq!(
        local_regions,
        [
            mir::AllocationRegion::Temporary,
            mir::AllocationRegion::Temporary
        ]
    );
    assert_eq!(
        main_regions,
        [
            mir::AllocationRegion::Temporary,
            mir::AllocationRegion::Persistent
        ]
    );
}
