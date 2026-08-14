use aster_codegen_cranelift::{ExecutionValue, MemoryStats, execute_with_stats};
use aster_compiler::compile;

fn run(source: &str) -> (ExecutionValue, MemoryStats) {
    let compilation = compile(source).expect("long-lived ownership source compiles");
    execute_with_stats(&compilation.mir, "Run").expect("long-lived ownership source executes")
}

#[test]
fn returned_overwritten_arrays_are_bounded_by_the_live_working_set() {
    let (value, stats) = run(r"
            internal int[] Make() {
                int[] values = new int[500000];
                values[0] = 20;
                values[499999] = 22;
                return values;
            }
            public int Run() {
                int total = 0;
                int[] current = [0];
                for (int i = 0; i < 100; i++) {
                    current = Make();
                    total += current[0] + current[499999];
                }
                return total;
            }
        ");

    assert_eq!(value, ExecutionValue::Int(4_200));
    assert_eq!(stats.array_allocations, 101);
    assert!(stats.requested_bytes >= 200_000_000);
    assert_eq!(stats.used_bytes, 0);
    assert!(stats.peak_used_bytes < 3_000_000);
    assert!(stats.reserved_bytes < 3_000_000);
}

#[test]
fn loop_break_and_continue_keep_owned_cleanup_balanced() {
    let (value, stats) = run(r"
            internal int[] Make(int value) { return [value]; }
            public int Run() {
                int total = 0;
                for (int i = 0; i < 100; i++) {
                    int[] value = Make(i);
                    total += value[0];
                    if (i < 3) { continue; }
                    if (i == 10) { break; }
                }
                return total;
            }
        ");

    assert_eq!(value, ExecutionValue::Int(55));
    assert_eq!(stats.used_bytes, 0);
    assert!(stats.reserved_bytes <= 4 * 1024);
}

#[test]
fn independent_objects_strings_lists_and_dictionaries_reclaim() {
    let (value, stats) = run(r#"
            public class Box {
                public int value;
                public Box(int value) { this.value = value; }
                public int Get() { return value; }
            }
            internal Box MakeBox(int value) { return new Box(value); }
            internal string MakeText(int value) { return $"v{value}"; }
            internal List<int> MakeList(int value) {
                List<int> values = new List<int>();
                values.Add(value);
                return values;
            }
            internal Dictionary<int, int> MakeDictionary(int value) {
                Dictionary<int, int> values = new Dictionary<int, int>();
                values.Add(1, value);
                return values;
            }
            public int Run() {
                int total = 0;
                for (int i = 0; i < 1000; i++) {
                    Box box = MakeBox(i);
                    total += box.Get();
                }
                for (int i = 0; i < 1000; i++) {
                    string text = MakeText(i);
                    total += text.Length;
                }
                for (int i = 0; i < 1000; i++) {
                    List<int> values = MakeList(i);
                    total += values.Length;
                }
                for (int i = 0; i < 1000; i++) {
                    Dictionary<int, int> values = MakeDictionary(i);
                    total += values.Length;
                }
                return total;
            }
        "#);

    assert_eq!(value, ExecutionValue::Int(505_390));
    assert_eq!(stats.used_bytes, 0);
    assert!(stats.peak_used_bytes < 1024);
    assert!(stats.reserved_bytes <= 8 * 1024);
}

#[test]
fn a_live_alias_prevents_the_overlapping_family_from_reclaiming() {
    let (value, stats) = run(r"
            public class Box {
                public int value;
                public Box(int value) { this.value = value; }
            }
            internal Box Make(int value) { return new Box(value); }
            public int Run() {
                int total = 0;
                for (int i = 0; i < 1000; i++) {
                    Box first = Make(i);
                    Box alias = first;
                    Box second = Make(i + 1);
                    total += alias.value + second.value;
                }
                return total;
            }
        ");

    assert_eq!(value, ExecutionValue::Int(1_000_000));
    assert!(stats.used_bytes >= 4_000);
    assert!(stats.used_bytes < 16_000);
}

#[test]
fn returned_reference_graphs_and_interface_aliases_remain_persistent() {
    let (value, stats) = run(r"
            public interface IValue { int Get(); }
            public class Value : IValue {
                public int[] values;
                public Value(int value) { values = [value]; }
                public int Get() { return values[0]; }
            }
            internal IValue Make(int value) { return new Value(value); }
            public int Run() {
                int total = 0;
                for (int i = 0; i < 100; i++) {
                    IValue original = Make(i);
                    IValue alias = original;
                    total += alias.Get();
                }
                return total;
            }
        ");

    assert_eq!(value, ExecutionValue::Int(4_950));
    assert!(stats.used_bytes > 0);
    assert_eq!(stats.object_allocations, 100);
    assert_eq!(stats.array_allocations, 100);
}

#[test]
fn generic_pass_through_cannot_disguise_a_shared_reference() {
    let (value, stats) = run(r"
            public class Tools {
                public T Identity<T>(T value) { return value; }
            }
            public int Run() {
                Tools tools = new Tools();
                int[] shared = [42];
                int total = 0;
                for (int i = 0; i < 100; i++) {
                    int[] alias = tools.Identity<int[]>(shared);
                    total += alias[0];
                }
                return total;
            }
        ");

    assert_eq!(value, ExecutionValue::Int(4_200));
    assert_eq!(stats.array_allocations, 1);
    assert!(stats.peak_used_bytes > 0);
}

#[test]
fn persistent_owned_region_composes_with_temporary_callee_storage() {
    let (value, stats) = run(r"
            internal int[] Make(int value) { return [value]; }
            internal int UseTemporary(int value) {
                int[] temporary = [value, value + 1];
                return temporary[0] + temporary[1];
            }
            public int Run() {
                int total = 0;
                for (int i = 0; i < 1000; i++) {
                    int[] owned = Make(i);
                    total += UseTemporary(i);
                    total += owned[0];
                }
                return total;
            }
        ");

    assert_eq!(value, ExecutionValue::Int(1_499_500));
    assert_eq!(stats.used_bytes, 0);
    assert!(stats.peak_used_bytes < 1024);
}

#[test]
fn worker_local_owned_regions_are_isolated_and_reusable() {
    let (value, _) = run(r"
            internal int[] Make(int value) { return [value]; }
            internal int Work() {
                int total = 0;
                for (int i = 0; i < 100; i++) {
                    int[] value = Make(i);
                    total += value[0];
                }
                return total;
            }
            public int Run() {
                int total = 0;
                for (int i = 0; i < 10; i++) {
                    total += Task.Run(Work).Wait();
                }
                return total;
            }
        ");

    assert_eq!(value, ExecutionValue::Int(49_500));
}

#[test]
fn closed_generic_producers_keep_region_identity_function_local() {
    let compilation = compile(
        r"
            internal T[] Make<T>(T value) { return [value]; }
            internal int UseInts() {
                int total = 0;
                for (int i = 0; i < 100; i++) {
                    int[] ints = Make<int>(i);
                    total += ints[0];
                }
                return total;
            }
            internal long UseLongs() {
                long total = 0L;
                for (long i = 0L; i < 100L; i++) {
                    long[] longs = Make<long>(i);
                    total += longs[0];
                }
                return total;
            }
            public long Run() { return UseInts() + UseLongs(); }
        ",
    )
    .expect("closed generic ownership source compiles");
    let ids = compilation
        .mir
        .functions
        .iter()
        .flat_map(|function| {
            function
                .blocks
                .iter()
                .flat_map(|block| &block.instructions)
                .filter_map(|instruction| match instruction {
                    aster_mir::Instruction::OwnedRegionEnter { id } => Some((function.symbol, *id)),
                    _ => None,
                })
        })
        .collect::<Vec<_>>();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0].0, ids[1].0);
    assert_eq!(ids[0].1, ids[1].1);

    let (value, stats) = execute_with_stats(&compilation.mir, "Run")
        .expect("closed generic ownership source executes");
    assert_eq!(value, ExecutionValue::Long(9_900));
    assert_eq!(stats.used_bytes, 0);
}
