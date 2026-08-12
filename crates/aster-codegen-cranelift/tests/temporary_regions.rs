use aster_codegen_cranelift::{ExecutionValue, execute_with_stats};
use aster_compiler::compile;
use aster_mir as mir;

fn object_regions(module: &mir::Module, function_name: &str) -> Vec<mir::AllocationRegion> {
    module
        .functions
        .iter()
        .find(|function| function.name == function_name && function.owner.is_none())
        .unwrap_or_else(|| panic!("missing MIR function `{function_name}`"))
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| {
            let mir::Instruction::AllocateObject { region, .. } = instruction else {
                return None;
            };
            Some(*region)
        })
        .collect()
}

fn array_regions(module: &mir::Module, function_name: &str) -> Vec<mir::AllocationRegion> {
    module
        .functions
        .iter()
        .find(|function| function.name == function_name && function.owner.is_none())
        .unwrap_or_else(|| panic!("missing MIR function `{function_name}`"))
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| {
            let mir::Instruction::AllocateArray { region, .. } = instruction else {
                return None;
            };
            Some(*region)
        })
        .collect()
}

fn string_regions(module: &mir::Module, function_name: &str) -> Vec<mir::AllocationRegion> {
    module
        .functions
        .iter()
        .find(|function| function.name == function_name && function.owner.is_none())
        .unwrap_or_else(|| panic!("missing MIR function `{function_name}`"))
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| {
            let mir::Instruction::CallIntrinsic { intrinsic, .. } = instruction else {
                return None;
            };
            intrinsic.string_allocation_region()
        })
        .collect()
}

#[test]
fn compiler_marks_local_object_temporary_and_rewinds_at_return() {
    let source = r"
        public class Point { public int value; }
        public int Run() {
            Point point = new Point();
            point.value = 42;
            return point.value;
        }
    ";
    let compilation = compile(source).expect("valid temporary object program");
    assert_eq!(
        object_regions(&compilation.mir, "Run"),
        vec![mir::AllocationRegion::Temporary]
    );

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
fn direct_and_helper_object_loops_match_and_rewind_at_their_existing_boundaries() {
    let source = r"
        public class Box { public int value; }
        internal int Build() {
            Box box = new Box();
            box.value = 1;
            return box.value;
        }
        public int Direct() {
            int total = 0;
            for (int index = 0; index < 1000; index++) {
                Box box = new Box();
                box.value = 1;
                total += box.value;
            }
            return total;
        }
        public int Helper() {
            int total = 0;
            for (int index = 0; index < 1000; index++) {
                total += Build();
            }
            return total;
        }
    ";
    let compilation = compile(source).expect("valid direct/helper temporary-object program");
    let (direct_value, direct) =
        execute_with_stats(&compilation.mir, "Direct").expect("direct loop executes");
    let (helper_value, helper) =
        execute_with_stats(&compilation.mir, "Helper").expect("helper loop executes");

    assert_eq!(direct_value, ExecutionValue::Int(1_000));
    assert_eq!(helper_value, direct_value);
    assert_eq!(direct.object_allocations, 1_000);
    assert_eq!(helper.object_allocations, direct.object_allocations);
    assert_eq!(direct.requested_bytes, helper.requested_bytes);
    assert_eq!(direct.used_bytes, 0);
    assert_eq!(helper.used_bytes, 0);
    assert!(direct.peak_used_bytes > helper.peak_used_bytes);
    assert!(direct.reserved_bytes >= helper.reserved_bytes);
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
    let compilation = compile(source).expect("valid nested temporary object program");
    assert_eq!(
        object_regions(&compilation.mir, "Build"),
        vec![mir::AllocationRegion::Temporary]
    );
    assert_eq!(
        object_regions(&compilation.mir, "Run"),
        vec![mir::AllocationRegion::Temporary]
    );

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
        public interface IBox { int Get(); }
        public class Box : IBox {
            public int value;
            public int Get() { return value; }
        }
        internal int Build() {
            Box temporary = new Box();
            temporary.value = 22;
            return temporary.value;
        }
        public int Run() {
            Box persistent = new Box();
            persistent.value = 20;
            IBox view = persistent;
            return view.Get() + Build();
        }
    ";
    let compilation = compile(source).expect("valid mixed-region program");
    assert_eq!(
        object_regions(&compilation.mir, "Build"),
        vec![mir::AllocationRegion::Temporary]
    );
    assert_eq!(
        object_regions(&compilation.mir, "Run"),
        vec![mir::AllocationRegion::Persistent]
    );

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
    let compilation = compile(source).expect("valid early-return program");
    assert_eq!(
        object_regions(&compilation.mir, "Choose"),
        vec![mir::AllocationRegion::Temporary]
    );

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
    let compilation = compile(source).expect("valid implicit-end program");
    assert_eq!(
        object_regions(&compilation.mir, "Work"),
        vec![mir::AllocationRegion::Temporary]
    );

    let (value, stats) =
        execute_with_stats(&compilation.mir, "Run").expect("End should leave its scope");

    assert_eq!(value, ExecutionValue::Int(42));
    assert_eq!(stats.object_allocations, 1);
    assert_eq!(stats.used_bytes, 0);
}

#[test]
fn compiler_marks_local_array_temporary_and_rewinds_header_and_data() {
    let source = "public int Run() { int[] values = [20, 22]; return values[0] + values[1]; }";
    let compilation = compile(source).expect("valid temporary array program");
    assert_eq!(
        array_regions(&compilation.mir, "Run"),
        vec![mir::AllocationRegion::Temporary]
    );

    let (value, stats) =
        execute_with_stats(&compilation.mir, "Run").expect("temporary array should execute");

    assert_eq!(value, ExecutionValue::Int(42));
    assert_eq!(stats.total_allocations, 1);
    assert_eq!(stats.array_allocations, 1);
    assert_eq!(stats.used_bytes, 0);
    assert!(stats.reserved_bytes > 0);
    assert!(stats.peak_used_bytes > 0);
}

#[test]
fn compiler_marks_local_dynamic_string_temporary_and_rewinds_it() {
    let source = r#"
        public int Run() {
            string left = "As";
            string value = left + "ter";
            return value.Length + 37;
        }
    "#;
    let compilation = compile(source).expect("valid temporary string program");
    assert_eq!(
        string_regions(&compilation.mir, "Run"),
        vec![mir::AllocationRegion::Temporary]
    );

    let (value, stats) =
        execute_with_stats(&compilation.mir, "Run").expect("temporary string should execute");

    assert_eq!(value, ExecutionValue::Int(42));
    assert_eq!(stats.total_allocations, 1);
    assert_eq!(stats.string_allocations, 1);
    assert_eq!(stats.used_bytes, 0);
    assert!(stats.reserved_bytes > 0);
    assert!(stats.peak_used_bytes > 0);
}
