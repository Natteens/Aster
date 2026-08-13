//! Production AARM selector coverage through the ordinary source compiler.

use std::sync::atomic::{AtomicU64, Ordering};

use aster_codegen_cranelift::{ExecutionValue, execute_with_stats};
use aster_compiler::{compile, compile_project};
use aster_mir as mir;

static NEXT_SOURCE: AtomicU64 = AtomicU64::new(0);

fn project_module(source: &str) -> mir::Module {
    let id = NEXT_SOURCE.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "aster-aarm-production-{}-{id}.aster",
        std::process::id()
    ));
    std::fs::write(&path, source).expect("write AARM production source");
    let module = compile_project(&path)
        .expect("AARM production source compiles")
        .compilation
        .mir;
    std::fs::remove_file(path).expect("remove AARM production source");
    module
}

fn marker_count(module: &mir::Module) -> usize {
    module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .filter(|instruction| {
            matches!(
                instruction,
                mir::Instruction::TemporarySubregionEnter { .. }
                    | mir::Instruction::TemporarySubregionExit { .. }
            )
        })
        .count()
}

fn execute(module: &mir::Module, expected: i32) {
    let (value, stats) = execute_with_stats(module, "Run").expect("production AARM executes");
    assert_eq!(value, ExecutionValue::Int(expected));
    assert_eq!(stats.used_bytes, 0);
}

#[test]
fn ordinary_source_enables_only_growing_hidden_backing_loops() {
    let builder = project_module(
        r#"
        using aster.core;
        public int Run() {
            int total = 0;
            for (int i = 0; i < 1000; i++) {
                StringBuilder value = new StringBuilder();
                value.Append("abcdefgh");
                value.Append("ijklmnop");
                total += 1;
            }
            return total;
        }
        "#,
    );
    assert_eq!(marker_count(&builder), 2);
    execute(&builder, 1000);

    let list = compile(
        r"
        public int Run() {
            int total = 0;
            for (int i = 0; i < 1000; i++) {
                List<int> values = new List<int>();
                values.Add(i);
                values.Add(i + 1);
                total += 1;
            }
            return total;
        }
        ",
    )
    .expect("List production source compiles")
    .mir;
    assert_eq!(marker_count(&list), 2);
    execute(&list, 1000);

    let dictionary = compile(
        r"
        public int Run() {
            int total = 0;
            for (int i = 0; i < 1000; i++) {
                Dictionary<int, int> values = new Dictionary<int, int>();
                values.Add(1, i);
                values.Set(2, i + 1);
                total += 1;
            }
            return total;
        }
        ",
    )
    .expect("Dictionary production source compiles")
    .mir;
    assert_eq!(marker_count(&dictionary), 2);
    execute(&dictionary, 1000);

    let dictionary_set = compile(
        r"
        public int Run() {
            int total = 0;
            for (int i = 0; i < 1000; i++) {
                Dictionary<int, int> values = new Dictionary<int, int>();
                values.Set(i, i + 1);
                total += 1;
            }
            return total;
        }
        ",
    )
    .expect("Dictionary.Set production source compiles")
    .mir;
    assert_eq!(marker_count(&dictionary_set), 2);
    execute(&dictionary_set, 1000);
}

#[test]
fn ordinary_source_declines_fixed_and_string_only_loops() {
    let object = compile(
        r"
        public class Box { public int value; }
        public int Run() {
            int total = 0;
            for (int i = 0; i < 1000000; i++) {
                Box value = new Box();
                value.value = i;
                total += 1;
            }
            return total;
        }
        ",
    )
    .expect("tiny object production source compiles")
    .mir;
    assert_eq!(marker_count(&object), 0);
    execute(&object, 1_000_000);

    let string = compile(
        r#"
        public int Run() {
            int total = 0;
            for (int i = 0; i < 1000; i++) {
                string value = $"item {i}";
                total += 1;
            }
            return total;
        }
        "#,
    )
    .expect("string-only production source compiles")
    .mir;
    assert_eq!(marker_count(&string), 0);
    execute(&string, 1000);
}

#[test]
fn ordinary_source_selects_only_the_nested_hidden_backing_leaf() {
    let module = project_module(
        r#"
        using aster.core;
        public int Run() {
            int total = 0;
            for (int outer = 0; outer < 20; outer++) {
                for (int inner = 0; inner < 50; inner++) {
                    StringBuilder value = new StringBuilder();
                    value.Append("nested-growth-payload");
                    total += 1;
                }
            }
            return total;
        }
        "#,
    );
    assert_eq!(marker_count(&module), 2);
    execute(&module, 1000);
}

#[test]
fn production_region_amortizes_safe_array_and_string_work() {
    let module = project_module(
        r#"
        using aster.core;
        public int Run() {
            int total = 0;
            for (int i = 0; i < 1000; i++) {
                new int[4];
                $"item {i}";
                StringBuilder value = new StringBuilder();
                value.Append("mixed");
                total += i;
            }
            return total;
        }
        "#,
    );
    assert_eq!(marker_count(&module), 2);
    execute(&module, 499_500);
}

#[cfg(feature = "aarm-telemetry")]
#[test]
fn automatic_hidden_backing_failures_preserve_controlled_cleanup() {
    use std::{fmt::Write as _, sync::Arc};

    use aster_codegen_cranelift::execute_with_aarm_parallel_governor;
    use aster_runtime::{ExecutionContext, MemoryGovernor};

    fn fail_with_limit(module: &mir::Module, limit: usize) {
        assert_eq!(marker_count(module), 2);
        let governor = Arc::new(MemoryGovernor::new(limit));
        let error = execute_with_aarm_parallel_governor(module, "Run", 1, Arc::clone(&governor))
            .expect_err("the selected production region must hit the configured hard limit");
        assert!(error.message().contains("shared execution memory budget"));
        assert_eq!(governor.telemetry().current_capacity_bytes, 0);
    }

    let payload = "x".repeat(ExecutionContext::AARM_MIN_PAGE_CAPACITY_BYTES * 2);
    let builder = project_module(&format!(
        "using aster.core; public int Run() {{ for (int i = 0; i < 1; i++) {{ \
         StringBuilder value = new StringBuilder(); value.Append(\"{payload}\"); }} return 1; }}"
    ));
    fail_with_limit(&builder, 1);
    fail_with_limit(&builder, ExecutionContext::AARM_MIN_PAGE_CAPACITY_BYTES);
    execute(&builder, 1);

    let mut list_source = String::from(
        "public int Run() { for (int i = 0; i < 1; i++) { List<int> values = new List<int>();",
    );
    for value in 0..2_048 {
        write!(list_source, "values.Add({value});").expect("write List growth source");
    }
    list_source.push_str("} return 1; }");
    let list = compile(&list_source)
        .expect("List failure source compiles")
        .mir;
    fail_with_limit(&list, ExecutionContext::AARM_MIN_PAGE_CAPACITY_BYTES);
    execute(&list, 1);

    let mut dictionary_source = String::from(
        "public int Run() { for (int i = 0; i < 1; i++) { Dictionary<int, int> values = new Dictionary<int, int>();",
    );
    for value in 0..512 {
        write!(dictionary_source, "values.Add({value}, {value});")
            .expect("write Dictionary growth source");
    }
    dictionary_source.push_str("} return 1; }");
    let dictionary = compile(&dictionary_source)
        .expect("Dictionary failure source compiles")
        .mir;
    fail_with_limit(&dictionary, ExecutionContext::AARM_MIN_PAGE_CAPACITY_BYTES);
    execute(&dictionary, 1);
}
