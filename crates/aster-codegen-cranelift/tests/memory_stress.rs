use aster_codegen_cranelift::{ExecutionValue, MemoryStats, execute_with_stats};
use aster_compiler::compile;

const TEMPORARY_STRESS: &str = r#"
    internal int KeepStressValue(int value) { return value; }

    public class StressBox {
        public int value;
        public StressBox(int value) { this.value = KeepStressValue(value); }
    }

    internal int BuildTemporary() {
        StressBox box = new StressBox(39);

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
        public StressBox(int value) { this.value = value; }
    }

    internal StressBox MakeBox() {
        StressBox box = new StressBox(39);
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
    assert_eq!(stats.reserved_bytes, 4 * 1024);
    assert!(stats.peak_used_bytes > 0);
    assert!(stats.peak_used_bytes < 1024);
}

#[test]
fn overlapping_returned_families_fall_back_while_the_final_family_reclaims() {
    let (value, stats) = execute(PERSISTENT_STRESS);

    assert_eq!(value, ExecutionValue::Int(420_000));
    assert_eq!(stats.total_allocations, 30_000);
    assert_eq!(stats.object_allocations, 10_000);
    assert_eq!(stats.array_allocations, 10_000);
    assert_eq!(stats.string_allocations, 10_000);
    assert!(stats.used_bytes > 64 * 1024);
    assert!(stats.reserved_bytes >= stats.used_bytes);
    assert!(stats.peak_used_bytes > stats.used_bytes);
    assert!(stats.peak_used_bytes - stats.used_bytes < 1024);
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

// --- List D: long-lived `List<T>` workloads with real memory metrics ------

const TEMPORARY_LIST_STRESS: &str = r"
    internal int BuildTemporaryList() {
        List<int> values = new List<int>();
        for (int i = 0; i < 8; i++) {
            values.Add(i);
        }
        int sum = 0;
        for (int i = 0; i < values.Length; i++) {
            sum += values.Get(i);
        }
        values.RemoveAt(0);
        return sum;
    }

    public int Run() {
        int total = 0;
        for (int index = 0; index < 10000; index++) {
            total += BuildTemporaryList();
        }
        return total;
    }
";

const PERSISTENT_LIST_STRESS: &str = r"
    internal List<int> MakeList() {
        List<int> values = new List<int>();
        for (int i = 0; i < 8; i++) {
            values.Add(i);
        }
        return values;
    }

    public int Run() {
        int total = 0;
        for (int index = 0; index < 10000; index++) {
            List<int> values = MakeList();
            total += values.Length;
        }
        return total;
    }
";

#[test]
fn temporary_list_stress_stays_bounded() {
    // A non-escaping list, grown and shrunk 10,000 times in a row: every
    // header and every growth generation must land in the temporary arena
    // and be reclaimed on each call's implicit rewind, so `used_bytes`
    // reports the same small number `object_allocations` regardless
    // (expected by the model, not a bug) as the plain-object/array/string
    // stress above.
    let (value, stats) = execute(TEMPORARY_LIST_STRESS);

    assert_eq!(value, ExecutionValue::Int((0..8).sum::<i32>() * 10_000));
    assert_eq!(stats.used_bytes, 0);
    assert_eq!(stats.reserved_bytes, 4 * 1024);
    assert!(stats.peak_used_bytes > 0);
    assert!(
        stats.peak_used_bytes < 1024,
        "a single list's header + growth buffers should stay tiny, got {}",
        stats.peak_used_bytes
    );
}

#[test]
fn returned_overwritten_list_stress_stays_bounded() {
    let (value, stats) = execute(PERSISTENT_LIST_STRESS);

    assert_eq!(value, ExecutionValue::Int(8 * 10_000));
    assert_eq!(stats.used_bytes, 0);
    assert_eq!(stats.reserved_bytes, 4 * 1024);
    assert!(stats.peak_used_bytes > 0);
    assert!(stats.peak_used_bytes < 1024);
}

#[test]
fn a_long_lived_list_workload_peaks_then_shrinks_without_unbounded_growth() {
    // One list per call grows to 50 elements (crossing every capacity
    // doubling 0->4->8->16->32->64), then shrinks back down to 10 via
    // `RemoveAt`, repeated 2,000 times. Because the list never escapes its
    // call, every header and every superseded growth buffer is reclaimed
    // when the call's temporary scope rewinds; `used_bytes` reflects only
    // whatever is live in the *last* completed call, not a running total
    // across all 2,000 -- confirming there is no unexplained linear growth
    // (Section 8's classification: expected by the model).
    let source = r"
        internal int Workload() {
            List<int> values = new List<int>();
            for (int i = 0; i < 50; i++) {
                values.Add(i);
            }
            for (int i = 0; i < 40; i++) {
                values.RemoveAt(0);
            }
            return values.Length;
        }

        public int Run() {
            int total = 0;
            for (int index = 0; index < 2000; index++) {
                total += Workload();
            }
            return total;
        }
    ";
    let (value, stats) = execute(source);

    assert_eq!(value, ExecutionValue::Int(10 * 2000));
    assert_eq!(stats.used_bytes, 0, "the final call's scope was reclaimed");
    assert_eq!(
        stats.reserved_bytes,
        4 * 1024,
        "one temporary page is reused across every call, never growing per iteration"
    );
    // Every generation of the buffer this list ever had (0->4->8->16->32->64)
    // plus its own header is a distinct allocation; that per-call constant
    // is what should scale with the iteration count, not `used_bytes`.
    assert_eq!(stats.object_allocations, 2000 * 6);
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
