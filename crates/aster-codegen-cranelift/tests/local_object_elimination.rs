use aster_codegen_cranelift::{ExecutionValue, execute_with_stats};
use aster_compiler::{compile, mir};

fn instruction_count(module: &mir::Module, predicate: impl Fn(&mir::Instruction) -> bool) -> usize {
    module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .filter(|instruction| predicate(instruction))
        .count()
}

fn run(source: &str, expected: i32) -> aster_codegen_cranelift::MemoryStats {
    let compilation = compile(source).expect("source compiles");
    let (value, stats) = execute_with_stats(&compilation.mir, "Run").expect("source executes");
    assert_eq!(value, ExecutionValue::Int(expected));
    stats
}

#[test]
fn scalarized_object_executes_without_runtime_allocation() {
    let stats = run(
        "public class Pair { public int left; public int right; } \
         public int Run() { Pair pair = new Pair(); pair.left = 20; pair.right = 22; \
         return pair.left + pair.right; }",
        42,
    );
    assert_eq!(stats.object_allocations, 0);
    assert_eq!(stats.requested_bytes, 0);
    assert_eq!(stats.peak_used_bytes, 0);
}

#[test]
fn parameterized_constructor_executes_without_runtime_allocation() {
    let stats = run(
        "public class Point { public int x; public int y; public Point(int first, int second) { \
         this.y = second; this.x = first; } } public int Run() { \
         Point point = new Point(20, 22); return point.x + point.y; }",
        42,
    );
    assert_eq!(stats.object_allocations, 0);
    assert_eq!(stats.requested_bytes, 0);
    assert_eq!(stats.peak_used_bytes, 0);
}

#[test]
fn constructor_values_are_reinitialized_for_every_dynamic_creation() {
    let stats = run(
        "public class Point { public int x; public int y; public Point(int value) { \
         this.x = value; } } public int Run() { int total = 0; \
         for (int i = 0; i < 1000; i++) { Point point = new Point(i); \
         total += point.x + point.y; } return total; }",
        499_500,
    );
    assert_eq!(stats.object_allocations, 0);
}

#[test]
fn scalar_fields_are_zeroed_for_every_dynamic_allocation() {
    let stats = run(
        "public class Pair { public int value; public bool set; } \
         public int Run() { int total = 0; for (int i = 0; i < 1000; i++) { \
         Pair pair = new Pair(); if (pair.set) { total += 100; } total += pair.value; \
         pair.value = 1; pair.set = true; total += pair.value; } return total; }",
        1000,
    );
    assert_eq!(stats.object_allocations, 0);
}

#[test]
fn aliases_and_calls_preserve_the_existing_object_path() {
    let alias = run(
        "public class Box { public int value; } public int Run() { Box box = new Box(); \
         Box alias = box; alias.value = 42; return box.value; }",
        42,
    );
    assert_eq!(alias.object_allocations, 1);

    let call = run(
        "public class Box { public int value; } internal int Read(Box box) { return box.value; } \
         public int Run() { Box box = new Box(); box.value = 42; return Read(box); }",
        42,
    );
    assert_eq!(call.object_allocations, 1);
}

#[test]
fn scalar_replacement_and_production_aarm_compose() {
    let source = "public class Pair { public int left; public int right; } \
                  public int Run() { int total = 0; for (int i = 0; i < 1000; i++) { \
                  Pair pair = new Pair(); pair.left = i; pair.right = 1; \
                  List<int> values = new List<int>(); values.Add(i); \
                  total += pair.left + pair.right; } return total; }";
    let compilation = compile(source).expect("source compiles");
    assert_eq!(
        instruction_count(&compilation.mir, |instruction| matches!(
            instruction,
            mir::Instruction::AllocateObject { .. }
        )),
        0
    );
    assert_eq!(
        instruction_count(&compilation.mir, |instruction| matches!(
            instruction,
            mir::Instruction::TemporarySubregionEnter { .. }
                | mir::Instruction::TemporarySubregionExit { .. }
        )),
        2
    );
    let (value, stats) = execute_with_stats(&compilation.mir, "Run").expect("source executes");
    assert_eq!(value, ExecutionValue::Int(500_500));
    assert_eq!(stats.object_allocations, 2000);
    assert_eq!(stats.used_bytes, 0);
}

#[test]
fn constructor_replacement_and_production_aarm_compose() {
    let source = "public class Pair { public int left; public int right; \
                  public Pair(int left, int right) { this.left = left; this.right = right; } } \
                  public int Run() { int total = 0; for (int i = 0; i < 1000; i++) { \
                  Pair pair = new Pair(i, 1); List<int> values = new List<int>(); values.Add(i); \
                  total += pair.left + pair.right; } return total; }";
    let compilation = compile(source).expect("source compiles");
    assert_eq!(
        instruction_count(&compilation.mir, |instruction| matches!(
            instruction,
            mir::Instruction::AllocateObject { .. }
        )),
        0
    );
    assert_eq!(
        instruction_count(&compilation.mir, |instruction| matches!(
            instruction,
            mir::Instruction::TemporarySubregionEnter { .. }
                | mir::Instruction::TemporarySubregionExit { .. }
        )),
        2
    );
    let (value, stats) = execute_with_stats(&compilation.mir, "Run").expect("source executes");
    assert_eq!(value, ExecutionValue::Int(500_500));
    assert_eq!(stats.object_allocations, 2000);
    assert_eq!(stats.used_bytes, 0);
}

#[test]
fn unsupported_constructor_effects_and_escaping_objects_stay_materialized() {
    let effectful = run(
        "internal int Normalize(int value) { return value; } public class Box { public int value; \
         public Box(int value) { this.value = Normalize(value); } } public int Run() { \
         Box box = new Box(42); return box.value; }",
        42,
    );
    assert_eq!(effectful.object_allocations, 1);

    let escaping = run(
        "public class Box { public int value; public Box(int value) { this.value = value; } } \
         public Box Build() { return new Box(42); } public int Run() { return Build().value; }",
        42,
    );
    assert_eq!(escaping.object_allocations, 1);
}

#[test]
fn retained_alias_keeps_object_and_conservatively_withholds_fine_region() {
    let source = "public class Pair { public int left; public int right; } \
                  public int Run() { int total = 0; for (int i = 0; i < 1000; i++) { \
                  Pair pair = new Pair(); Pair alias = pair; alias.left = i; pair.right = 1; \
                  List<int> values = new List<int>(); values.Add(i); \
                  total += pair.left + pair.right; } return total; }";
    let compilation = compile(source).expect("source compiles");
    assert_eq!(
        instruction_count(&compilation.mir, |instruction| matches!(
            instruction,
            mir::Instruction::AllocateObject { .. }
        )),
        1
    );
    assert_eq!(
        instruction_count(&compilation.mir, |instruction| matches!(
            instruction,
            mir::Instruction::TemporarySubregionEnter { .. }
                | mir::Instruction::TemporarySubregionExit { .. }
        )),
        0
    );
    let (value, stats) = execute_with_stats(&compilation.mir, "Run").expect("source executes");
    assert_eq!(value, ExecutionValue::Int(500_500));
    assert_eq!(stats.object_allocations, 3000);
    assert_eq!(stats.used_bytes, 0);
}

#[cfg(feature = "aarm-telemetry")]
#[test]
fn eliminated_objects_do_not_consume_governor_budget_or_runtime_statistics() {
    use std::sync::Arc;

    use aster_codegen_cranelift::execute_with_aarm_parallel_governor;
    use aster_runtime::{ExecutionContext, MemoryGovernor};

    let eliminated = compile(
        "public class Box { public int value; } public int Run() { int total = 0; \
         for (int i = 0; i < 10000; i++) { Box box = new Box(); box.value = 1; \
         total += box.value; } return total; }",
    )
    .expect("eliminable source compiles")
    .mir;
    assert_eq!(
        instruction_count(&eliminated, |instruction| matches!(
            instruction,
            mir::Instruction::AllocateObject { .. }
        )),
        0
    );
    let governor = Arc::new(MemoryGovernor::new(1));
    let (value, _, _, _) =
        execute_with_aarm_parallel_governor(&eliminated, "Run", 1, Arc::clone(&governor))
            .expect("eliminated allocation never reaches the one-byte governor");
    assert_eq!(value, ExecutionValue::Int(10000));
    assert_eq!(governor.telemetry().current_capacity_bytes, 0);
    let (_, stats) = execute_with_stats(&eliminated, "Run").expect("stats execution succeeds");
    assert_eq!(stats.total_allocations, 0);
    assert_eq!(stats.object_allocations, 0);
    assert_eq!(stats.requested_bytes, 0);

    let materialized = compile(
        "internal int Normalize(int value) { return value; } public class Box { public int value; \
         public Box(int value) { this.value = Normalize(value); } } \
         public int Run() { int total = 0; for (int i = 0; i < 10000; i++) { \
         Box box = new Box(1); total += box.value; } return total; }",
    )
    .expect("materialized source compiles")
    .mir;
    assert_eq!(
        instruction_count(&materialized, |instruction| matches!(
            instruction,
            mir::Instruction::AllocateObject { .. }
        )),
        1
    );
    let governor = Arc::new(MemoryGovernor::new(
        ExecutionContext::AARM_MIN_PAGE_CAPACITY_BYTES,
    ));
    let error = execute_with_aarm_parallel_governor(&materialized, "Run", 1, Arc::clone(&governor))
        .expect_err("materialized allocations must still reach the governor");
    assert!(error.message().contains("shared execution memory budget"));
    assert_eq!(governor.telemetry().current_capacity_bytes, 0);
}
