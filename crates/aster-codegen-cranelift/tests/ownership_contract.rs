use aster_codegen_cranelift::{ExecutionValue, execute_with_stats};
use aster_compiler::compile_project;
use std::sync::atomic::{AtomicU64, Ordering};

fn execute(source: &str) -> (ExecutionValue, aster_codegen_cranelift::MemoryStats) {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "aster-ownership-contract-{}-{id}.aster",
        std::process::id()
    ));
    std::fs::write(&path, source).expect("write ownership-contract source");
    let compilation = compile_project(&path).expect("ownership-contract source should compile");
    std::fs::remove_file(&path).ok();
    execute_with_stats(&compilation.compilation.mir, "Run")
        .expect("ownership-contract source should execute")
}

#[test]
fn returned_list_of_interfaces_grows_after_scope_rewinds() {
    let source = r"
        public interface IValue { int Get(); }

        public class Value : IValue {
            private int value;
            public Value(int value) { this.value = value; }
            public int Get() { return value; }
        }

        internal List<IValue> MakeValues() {
            List<IValue> values = new List<IValue>();
            values.Add(new Value(20));
            return values;
        }

        internal int BurnTemporaryMemory() {
            int total = 0;
            for (int index = 0; index < 1000; index++) {
                int[] scratch = [index];
                total = total + scratch[0];
            }
            return total;
        }

        public int Run() {
            List<IValue> values = MakeValues();
            IValue first = values.Get(0);
            values.Add(new Value(20));
            int ignored = BurnTemporaryMemory();
            return first.Get() + values.Get(1).Get() + values.Length;
        }
    ";

    let (value, stats) = execute(source);

    assert_eq!(value, ExecutionValue::Int(42));
    assert!(stats.used_bytes > 0, "the returned list stays persistent");
}

#[test]
fn cyclic_references_remain_safe_and_persistent_for_the_execution() {
    let source = r#"
        public class Node {
            public List<Node> links;
            public int value;

            public Node(List<Node> links) {
                this.value = 42;
                this.links = links;
            }
        }

        internal Node MakeCycle() {
            List<Node> links = new List<Node>();
            Node node = new Node(links);
            links.Add(node);
            return node;
        }

        internal int TemporaryWork() {
            string left = "As";
            string value = left + "ter";
            return value.Length;
        }

        public int Run() {
            Node root = MakeCycle();
            int ignored = TemporaryWork();
            return root.links.Get(0).value;
        }
    "#;

    let (value, stats) = execute(source);

    assert_eq!(value, ExecutionValue::Int(42));
    assert!(
        stats.used_bytes > 0,
        "cycles are retained until context teardown"
    );
}

#[test]
fn copied_struct_and_enum_references_survive_a_callee_scope_rewind() {
    let source = r#"
        public interface IValue { int Get(); }

        public class Value : IValue {
            private int value;
            public Value(int value) { this.value = value; }
            public int Get() { return value; }
        }

        public struct Snapshot {
            public string text;
            public int[] items;
            public IValue value;
        }

        public enum Reply { Ready(Snapshot snapshot), Empty }

        internal Reply MakeReply() {
            string prefix = "As";
            Snapshot snapshot = Snapshot {
                text: prefix + "ter",
                items: [20],
                value: new Value(17)
            };
            return Reply.Ready(snapshot);
        }

        internal int TemporaryWork() {
            int[] scratch = [1, 2];
            return scratch[0] + scratch[1];
        }

        public int Run() {
            Reply reply = MakeReply();
            int ignored = TemporaryWork();
            switch (reply) {
                case Ready(snapshot):
                    return snapshot.items[0] + snapshot.text.Length + snapshot.value.Get();
                case Empty:
                    return 0;
            }
        }
    "#;

    assert_eq!(execute(source).0, ExecutionValue::Int(42));
}

#[test]
fn dictionary_option_and_entries_preserve_reference_values_after_allocation_pressure() {
    let source = r#"
        using aster.core;
        using aster.collections;

        public interface IValue { int Get(); }

        public class Value : IValue {
            private int value;
            public Value(int value) { this.value = value; }
            public int Get() { return value; }
        }

        public struct Record {
            public string name;
            public IValue value;
        }

        internal Dictionary<int, Record> MakeRecords() {
            Dictionary<int, Record> records = new Dictionary<int, Record>();
            Record record = Record { name: "As" + "ter", value: new Value(18) };
            records.Add(7, record);
            return records;
        }

        internal int BurnTemporaryMemory() {
            int total = 0;
            for (int index = 0; index < 1000; index = index + 1) {
                int[] scratch = [index, index + 1];
                total = total + scratch[0];
            }
            return total;
        }

        public int Run() {
            Dictionary<int, Record> records = MakeRecords();
            int ignored = BurnTemporaryMemory();
            switch (records.TryGet(7)) {
                case Some(record):
                    DictionaryEntry<int, Record>[] entries = records.Entries();
                    return record.value.Get() + record.name.Length
                        + entries[0].Value.value.Get() + entries.Length;
                case None:
                    return 0;
            }
        }
    "#;

    assert_eq!(execute(source).0, ExecutionValue::Int(42));
}

#[test]
fn worker_and_caller_contexts_keep_reference_allocations_isolated() {
    let source = r"
        public int Compute() {
            int[] values = [20, 20];
            return values[0] + values[1];
        }

        public int Run() {
            Task<int> task = Task.Run(Compute);
            int[] caller = [2];
            return task.Wait() + caller[0];
        }
    ";

    assert_eq!(execute(source).0, ExecutionValue::Int(42));
}
