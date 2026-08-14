use aster_codegen_cranelift::{ExecutionValue, execute_with_stats};
use aster_compiler::{compile, mir};

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

#[cfg(feature = "aarm-telemetry")]
fn instruction_count(module: &mir::Module, predicate: impl Fn(&mir::Instruction) -> bool) -> usize {
    module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .filter(|instruction| predicate(instruction))
        .count()
}

fn run_length(count: i32, part: &str, expected: i32, expected_string_allocations: u64) {
    let source = format!(
        "public int Run() {{ string value = \"\"; int i = 0; while (i < {count}) {{ \
         value = value + \"{part}\"; i = i + 1; }} return value.Length; }}"
    );
    let module = compile(&source).expect("loop concat source compiles").mir;
    let (value, stats) = execute_with_stats(&module, "Run").expect("loop concat executes");
    assert_eq!(value, ExecutionValue::Int(expected));
    assert_eq!(stats.string_allocations, expected_string_allocations);
}

#[test]
fn rewritten_concat_preserves_zero_one_many_empty_and_unicode_results() {
    run_length(0, "x", 0, 0);
    run_length(1, "x", 1, 1);
    run_length(1000, "x", 1000, 1);
    run_length(1000, "", 0, 1);
    run_length(3, "\u{1F642}", 3, 1);
}

#[test]
fn rewritten_concat_returns_the_final_immutable_snapshot() {
    let module = compile(
        r#"
        public string Run() {
            string value = "";
            int i = 0;
            while (i < 3) { value = value + "ab"; i = i + 1; }
            return value;
        }
        "#,
    )
    .expect("source compiles")
    .mir;
    assert_eq!(
        aster_codegen_cranelift::execute(&module, "Run").expect("source executes"),
        ExecutionValue::String("ababab".to_owned())
    );
}

#[test]
fn rewritten_concat_does_not_create_an_aarm_loop_region() {
    let module = compile(
        r#"
        public int Run() {
            string value = "";
            int i = 0;
            while (i < 1000) { value = value + "x"; i = i + 1; }
            return value.Length;
        }
        "#,
    )
    .expect("source compiles")
    .mir;
    assert_eq!(marker_count(&module), 0);
    run_length(1000, "x", 1000, 1);
}

#[test]
fn rewritten_builder_stays_inside_its_task_function() {
    let module = compile(
        r#"
        public int Build() {
            string value = "";
            int i = 0;
            while (i < 1000) { value = value + "x"; i = i + 1; }
            return value.Length;
        }
        public int Main() {
            Task<int> task = Task.Run(Build);
            return task.Wait();
        }
        "#,
    )
    .expect("source compiles")
    .mir;
    let build = module
        .functions
        .iter()
        .find(|function| function.name == "Build")
        .expect("Build is present");
    assert_eq!(
        build
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .filter(|instruction| matches!(
                instruction,
                mir::Instruction::AllocateStringBuilder { .. }
            ))
            .count(),
        1
    );
    assert_eq!(
        aster_codegen_cranelift::execute(&module, "Main").expect("task executes"),
        ExecutionValue::Int(1000)
    );
}

#[cfg(feature = "aarm-telemetry")]
#[test]
fn rewritten_concat_builder_creation_failure_cleans_up_the_context() {
    use std::sync::Arc;

    use aster_codegen_cranelift::execute_with_aarm_parallel_governor;
    use aster_runtime::MemoryGovernor;

    let source = r#"
        public int Run() {
            string value = "";
            int i = 0;
            while (i < 1) { value = value + "x"; i = i + 1; }
            return value.Length;
        }
    "#;
    let module = compile(source).expect("source compiles").mir;
    let governor = Arc::new(MemoryGovernor::new(0));
    let error = execute_with_aarm_parallel_governor(&module, "Run", 1, Arc::clone(&governor))
        .expect_err("builder creation must respect the governor");
    assert!(error.message().contains("shared execution memory budget"));
    assert_eq!(governor.telemetry().current_capacity_bytes, 0);
    let (value, _) = execute_with_stats(&module, "Run").expect("fresh context executes");
    assert_eq!(value, ExecutionValue::Int(1));
}

#[cfg(feature = "aarm-telemetry")]
#[test]
fn rewritten_concat_growth_and_snapshot_failures_clean_up_the_context() {
    use std::sync::Arc;

    use aster_codegen_cranelift::execute_with_aarm_parallel_governor;
    use aster_runtime::{ExecutionContext, MemoryGovernor};

    let growth = compile(
        r#"
        public int Run() {
            string value = "";
            int i = 0;
            while (i < 2000) { value = value + "x"; i = i + 1; }
            return value.Length;
        }
        "#,
    )
    .expect("growth source compiles")
    .mir;
    let growth_governor = Arc::new(MemoryGovernor::new(
        ExecutionContext::AARM_MIN_PAGE_CAPACITY_BYTES,
    ));
    let growth_error =
        execute_with_aarm_parallel_governor(&growth, "Run", 1, Arc::clone(&growth_governor))
            .expect_err("builder growth must respect the governor");
    assert!(
        growth_error
            .message()
            .contains("shared execution memory budget")
    );
    assert_eq!(growth_governor.telemetry().current_capacity_bytes, 0);

    let large_part = "x".repeat(3_000);
    let snapshot = compile(&format!(
        r#"
        public int Run() {{
            string value = "";
            int i = 0;
            while (i < 1) {{ value = value + "{large_part}"; i = i + 1; }}
            return value.Length;
        }}
        "#
    ))
    .expect("snapshot source compiles")
    .mir;
    let snapshot_governor = Arc::new(MemoryGovernor::new(
        ExecutionContext::AARM_MIN_PAGE_CAPACITY_BYTES,
    ));
    let snapshot_error =
        execute_with_aarm_parallel_governor(&snapshot, "Run", 1, Arc::clone(&snapshot_governor))
            .expect_err("final snapshot must respect the governor");
    assert!(
        snapshot_error
            .message()
            .contains("shared execution memory budget")
    );
    assert_eq!(snapshot_governor.telemetry().current_capacity_bytes, 0);

    let (value, _) = execute_with_stats(&snapshot, "Run").expect("fresh context executes");
    assert_eq!(value, ExecutionValue::Int(3_000));
}

#[cfg(feature = "aarm-telemetry")]
#[test]
fn removed_pairwise_allocations_have_no_phantom_budget_cost() {
    use std::sync::Arc;

    use aster_codegen_cranelift::execute_with_aarm_parallel_governor;
    use aster_runtime::{ExecutionContext, MemoryGovernor};

    let optimized = compile(
        r#"
        public int Run() {
            string value = "";
            int i = 0;
            while (i < 1000) { value = value + "x"; i = i + 1; }
            return value.Length;
        }
        "#,
    )
    .expect("optimized source compiles")
    .mir;
    assert_eq!(
        instruction_count(&optimized, |instruction| matches!(
            instruction,
            mir::Instruction::CallIntrinsic {
                intrinsic: mir::Intrinsic::StringConcat | mir::Intrinsic::StringConcatTemporary,
                ..
            }
        )),
        0
    );
    assert_eq!(
        instruction_count(&optimized, |instruction| matches!(
            instruction,
            mir::Instruction::AllocateStringBuilder { .. }
        )),
        1
    );
    let optimized_governor = Arc::new(MemoryGovernor::new(
        ExecutionContext::AARM_MIN_PAGE_CAPACITY_BYTES,
    ));
    let (value, ..) =
        execute_with_aarm_parallel_governor(&optimized, "Run", 1, Arc::clone(&optimized_governor))
            .expect("rewritten builder fits one page");
    assert_eq!(value, ExecutionValue::Int(1000));
    assert_eq!(optimized_governor.telemetry().current_capacity_bytes, 0);
    let (_, optimized_stats) =
        execute_with_stats(&optimized, "Run").expect("optimized stats execute");
    assert_eq!(optimized_stats.string_allocations, 1);
    assert!(
        optimized_stats.requested_bytes
            <= u64::try_from(ExecutionContext::AARM_MIN_PAGE_CAPACITY_BYTES)
                .expect("page size fits u64")
    );

    let rejected = compile(
        r#"
        public int Run() {
            string value = "";
            int observed = 0;
            int i = 0;
            while (i < 1000) {
                observed = observed + value.Length;
                value = value + "x";
                i = i + 1;
            }
            return value.Length + observed;
        }
        "#,
    )
    .expect("rejected source compiles")
    .mir;
    assert_eq!(
        instruction_count(&rejected, |instruction| matches!(
            instruction,
            mir::Instruction::CallIntrinsic {
                intrinsic: mir::Intrinsic::StringConcat | mir::Intrinsic::StringConcatTemporary,
                ..
            }
        )),
        1
    );
    let rejected_governor = Arc::new(MemoryGovernor::new(
        ExecutionContext::AARM_MIN_PAGE_CAPACITY_BYTES,
    ));
    let error =
        execute_with_aarm_parallel_governor(&rejected, "Run", 1, Arc::clone(&rejected_governor))
            .expect_err("materialized pairwise allocations exceed one page");
    assert!(error.message().contains("shared execution memory budget"));
    assert_eq!(rejected_governor.telemetry().current_capacity_bytes, 0);
}
