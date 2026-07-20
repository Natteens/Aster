use aster_codegen_cranelift::{ExecutionValue, execute, execute_with_stats};
use aster_compiler::compile;
use aster_mir as mir;

fn mark_object_allocations_temporary(module: &mut mir::Module, function_names: &[&str]) {
    for function in &mut module.functions {
        if !function_names.contains(&function.name.as_str()) {
            continue;
        }
        for instruction in function
            .blocks
            .iter_mut()
            .flat_map(|block| &mut block.instructions)
        {
            if let mir::Instruction::AllocateObject { region, .. } = instruction {
                *region = mir::AllocationRegion::Temporary;
            }
        }
    }
}

#[test]
fn temporary_object_executes_and_rewinds_at_function_return() {
    let source = r"
        public class Point { public int value; }
        public int Run() {
            Point point = new Point();
            point.value = 42;
            return point.value;
        }
    ";
    let mut compilation = compile(source).expect("valid temporary object program");
    mark_object_allocations_temporary(&mut compilation.mir, &["Run"]);

    let (value, stats) =
        execute_with_stats(&compilation.mir, "Run").expect("temporary object should execute");

    assert_eq!(value, ExecutionValue::Int(42));
    assert_eq!(stats.total_allocations, 1);
    assert_eq!(stats.object_allocations, 1);
    assert!(stats.requested_bytes > 0);
    assert_eq!(stats.used_bytes, 0);
    assert!(stats.reserved_bytes > 0);
    assert!(stats.peak_used_bytes > 0);
}

#[test]
fn nested_function_scopes_preserve_the_callers_temporary_object() {
    let source = r"
        public class Box { public int value; }
        internal int Build() {
            Box inner = new Box();
            inner.value = 22;
            return inner.value;
        }
        public int Run() {
            Box outer = new Box();
            outer.value = 20;
            return outer.value + Build();
        }
    ";
    let mut compilation = compile(source).expect("valid nested temporary object program");
    mark_object_allocations_temporary(&mut compilation.mir, &["Build", "Run"]);

    let (value, stats) = execute_with_stats(&compilation.mir, "Run")
        .expect("nested temporary scopes should execute");

    assert_eq!(value, ExecutionValue::Int(42));
    assert_eq!(stats.object_allocations, 2);
    assert_eq!(stats.total_allocations, 2);
    assert_eq!(stats.used_bytes, 0);
    assert!(stats.peak_used_bytes >= stats.requested_bytes);
}

#[test]
fn leaving_a_nested_temporary_scope_does_not_rewind_persistent_storage() {
    let source = r"
        public class Box { public int value; }
        internal int Build() {
            Box temporary = new Box();
            temporary.value = 22;
            return temporary.value;
        }
        public int Run() {
            Box persistent = new Box();
            persistent.value = 20;
            return persistent.value + Build();
        }
    ";
    let mut compilation = compile(source).expect("valid mixed-region program");
    mark_object_allocations_temporary(&mut compilation.mir, &["Build"]);

    let (value, stats) =
        execute_with_stats(&compilation.mir, "Run").expect("mixed regions should execute");

    assert_eq!(value, ExecutionValue::Int(42));
    assert_eq!(stats.object_allocations, 2);
    assert!(stats.used_bytes > 0);
    assert!(stats.peak_used_bytes >= stats.used_bytes);
}

#[test]
fn every_early_return_path_leaves_its_temporary_scope() {
    let source = r"
        public class Box { public int value; }
        internal int Choose(bool first) {
            Box box = new Box();
            if (first) {
                box.value = 20;
                return box.value;
            }
            box.value = 22;
            return box.value;
        }
        public int Run() {
            return Choose(true) + Choose(false);
        }
    ";
    let mut compilation = compile(source).expect("valid early-return program");
    mark_object_allocations_temporary(&mut compilation.mir, &["Choose"]);

    let (value, stats) =
        execute_with_stats(&compilation.mir, "Run").expect("every return should leave its scope");

    assert_eq!(value, ExecutionValue::Int(42));
    assert_eq!(stats.object_allocations, 2);
    assert_eq!(stats.used_bytes, 0);
}

#[test]
fn implicit_end_leaves_a_temporary_scope() {
    let source = r"
        public class Box { public int value; }
        internal void Work() {
            Box box = new Box();
            box.value = 42;
        }
        public int Run() {
            Work();
            return 42;
        }
    ";
    let mut compilation = compile(source).expect("valid implicit-end program");
    mark_object_allocations_temporary(&mut compilation.mir, &["Work"]);

    let (value, stats) =
        execute_with_stats(&compilation.mir, "Run").expect("End should leave its scope");

    assert_eq!(value, ExecutionValue::Int(42));
    assert_eq!(stats.object_allocations, 1);
    assert_eq!(stats.used_bytes, 0);
}

#[test]
fn temporary_arrays_remain_rejected_until_the_array_lote() {
    let source = "public int Run() { int[] values = [42]; return values[0]; }";
    let mut compilation = compile(source).expect("valid array program");
    for instruction in compilation
        .mir
        .functions
        .iter_mut()
        .flat_map(|function| &mut function.blocks)
        .flat_map(|block| &mut block.instructions)
    {
        if let mir::Instruction::AllocateArray { region, .. } = instruction {
            *region = mir::AllocationRegion::Temporary;
        }
    }

    let error = execute(&compilation.mir, "Run").expect_err("temporary arrays are not active yet");

    assert!(error.message().contains("temporary array allocations"));
}
