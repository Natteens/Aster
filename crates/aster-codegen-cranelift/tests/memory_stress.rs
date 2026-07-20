use aster_codegen_cranelift::{ExecutionValue, MemoryStats, execute_with_stats};
use aster_compiler::compile;

const TEMPORARY_STRESS: &str = r#"
    public class StressBox {
        public int value;
    }

    internal int BuildTemporary() {
        StressBox box = new StressBox();
        box.value = 39;

        int[] values = [1];

        string prefix = "A";
        string text = prefix + "B";

        return box.value + values[0] + text.Length;
    }

    public int Run() {
        int total = 0;
        for (int index = 0; index < 10000; index++) {
            total += BuildTemporary();
        }
        return total;
    }
"#;

const PERSISTENT_STRESS: &str = r#"
    public class StressBox {
        public int value;
    }

    internal StressBox MakeBox() {
        StressBox box = new StressBox();
        box.value = 39;
        return box;
    }

    internal int[] MakeArray() {
        return [1];
    }

    internal string MakeString() {
        string prefix = "A";
        return prefix + "B";
    }

    public int Run() {
        int total = 0;
        for (int index = 0; index < 10000; index++) {
            StressBox box = MakeBox();
            int[] values = MakeArray();
            string text = MakeString();
            total += box.value + values[0] + text.Length;
        }
        return total;
    }
"#;

fn execute(source: &str) -> (ExecutionValue, MemoryStats) {
    let compilation = compile(source).expect("memory stress source should compile");
    execute_with_stats(&compilation.mir, "Run").expect("memory stress source should execute")
}

#[test]
fn temporary_object_array_and_string_stress_stays_bounded() {
    let (value, stats) = execute(TEMPORARY_STRESS);

    assert_eq!(value, ExecutionValue::Int(420_000));
    assert_eq!(stats.total_allocations, 30_000);
    assert_eq!(stats.object_allocations, 10_000);
    assert_eq!(stats.array_allocations, 10_000);
    assert_eq!(stats.string_allocations, 10_000);
    assert_eq!(stats.used_bytes, 0);
    assert_eq!(stats.reserved_bytes, 64 * 1024);
    assert!(stats.peak_used_bytes > 0);
    assert!(stats.peak_used_bytes < 1024);
}

#[test]
fn returned_object_array_and_string_stress_remains_persistent() {
    let (value, stats) = execute(PERSISTENT_STRESS);

    assert_eq!(value, ExecutionValue::Int(420_000));
    assert_eq!(stats.total_allocations, 30_000);
    assert_eq!(stats.object_allocations, 10_000);
    assert_eq!(stats.array_allocations, 10_000);
    assert_eq!(stats.string_allocations, 10_000);
    assert!(stats.used_bytes > 64 * 1024);
    assert!(stats.reserved_bytes >= stats.used_bytes);
    assert_eq!(stats.peak_used_bytes, stats.used_bytes);
}

#[test]
fn values_stored_in_fields_remain_valid_after_temporary_rewinds() {
    let source = r#"
    public class Holder {
        public int[] values;
        public string text;

        public Holder(int[] values, string text) {
            this.values = values;
            this.text = text;
        }
    }

    internal int TemporaryWork() {
        int[] scratch = [20, 22];
        string prefix = "A";
        string text = prefix + "B";
        return scratch[0] + scratch[1] + text.Length;
    }

    public int Run() {
        int[] values = [40];
        string prefix = "A";
        string text = prefix + "B";
        Holder holder = new Holder(values, text);

        int ignored = TemporaryWork();
        return holder.values[0] + holder.text.Length;
    }
"#;

    let (value, stats) = execute(source);

    assert_eq!(value, ExecutionValue::Int(42));
    assert_eq!(stats.object_allocations, 1);
    assert_eq!(stats.array_allocations, 2);
    assert_eq!(stats.string_allocations, 2);
    assert!(stats.used_bytes > 0);
    assert!(stats.peak_used_bytes > stats.used_bytes);
}

#[test]
fn repeated_stress_executions_have_identical_results_and_metrics() {
    let compilation = compile(TEMPORARY_STRESS).expect("memory stress source should compile");
    let first =
        execute_with_stats(&compilation.mir, "Run").expect("first stress execution should work");
    let second =
        execute_with_stats(&compilation.mir, "Run").expect("second stress execution should work");

    assert_eq!(first, second);
}
