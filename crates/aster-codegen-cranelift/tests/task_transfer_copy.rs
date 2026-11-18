//! CONC-005: values cross a task boundary by deep copy (strings, arrays,
//! `List<T>`, `Dictionary<K, V>`, structs and enums of those), as `Task.Run`
//! arguments and as `Wait`/`WaitAll` results. Class instances and interfaces
//! stay rejected at compile time.

#![allow(clippy::needless_raw_string_hashes)]

use aster_codegen_cranelift::{ExecutionValue, execute};
use aster_compiler::compile;

fn run(source: &str) -> Result<ExecutionValue, String> {
    let compilation = compile(source).map_err(|diagnostics| format!("{diagnostics:#?}"))?;
    execute(&compilation.mir, "Main").map_err(|error| error.to_string())
}

fn compile_errors(source: &str) -> Vec<String> {
    match compile(source) {
        Ok(_) => Vec::new(),
        Err(diagnostics) => diagnostics
            .into_iter()
            .map(|diagnostic| diagnostic.message)
            .collect(),
    }
}

fn int(source: &str) -> i32 {
    match run(source) {
        Ok(ExecutionValue::Int(value)) => value,
        other => panic!("expected an int result, got {other:?}"),
    }
}

#[test]
fn string_argument_and_result_round_trip() {
    let source = r#"
        public string Decorate(string text) { return "[" + text + "]"; }
        public int Main() {
            Task<string> task = Task.Run(Decorate, "hello é");
            string result = task.Wait();
            if (result != "[hello é]") { return 1; }
            return result.Length;
        }
    "#;
    assert_eq!(int(source), 9);
}

#[test]
fn empty_string_and_empty_containers_round_trip() {
    let source = r#"
        public int Inspect(string text, int[] numbers, List<string> names, Dictionary<string, int> map) {
            return text.Length + numbers.Length + names.Length + map.Length;
        }
        public int Main() {
            List<string> names = new List<string>();
            Dictionary<string, int> map = new Dictionary<string, int>();
            int[] numbers = new int[0];
            return Task.Run(Inspect, "", numbers, names, map).Wait();
        }
    "#;
    assert_eq!(int(source), 0);
}

#[test]
fn array_argument_is_copied_and_independent() {
    let source = r#"
        public int Bump(int[] values) {
            values[0] = 100;
            int total = 0;
            for (int i = 0; i < values.Length; i++) { total += values[i]; }
            return total;
        }
        public int Main() {
            int[] values = new int[3];
            values[0] = 1; values[1] = 2; values[2] = 3;
            Task<int> task = Task.Run(Bump, values);
            int total = task.Wait();
            if (values[0] != 1) { return -1; }
            values[1] = 50;
            return total;
        }
    "#;
    assert_eq!(int(source), 105);
}

#[test]
fn array_result_is_a_fresh_copy() {
    let source = r#"
        public int[] Make(int count) {
            int[] values = new int[count];
            for (int i = 0; i < count; i++) { values[i] = i * i; }
            return values;
        }
        public int Main() {
            int[] values = Task.Run(Make, 5).Wait();
            return values.Length * 100 + values[4];
        }
    "#;
    assert_eq!(int(source), 516);
}

#[test]
fn string_array_argument_and_result_round_trip() {
    let source = r#"
        public string[] Shout(string[] words) {
            string[] result = [words[0] + "!", words[1] + "!"];
            return result;
        }
        public int Main() {
            string[] words = ["ab", "cde"];
            string[] shouted = Task.Run(Shout, words).Wait();
            if (words[0] != "ab") { return -1; }
            if (shouted[0] != "ab!") { return -2; }
            return shouted[1].Length;
        }
    "#;
    assert_eq!(int(source), 4);
}

#[test]
fn list_of_strings_argument_and_result_round_trip() {
    let source = r#"
        public List<string> Extend(List<string> names) {
            names.Add("worker");
            return names;
        }
        public int Main() {
            List<string> names = new List<string>();
            names.Add("a"); names.Add("bb");
            List<string> grown = Task.Run(Extend, names).Wait();
            if (names.Length != 2) { return -1; }
            names.Add("caller-only");
            if (grown.Length != 3) { return -2; }
            if (grown[2] != "worker") { return -3; }
            return grown[1].Length + names.Length * 10;
        }
    "#;
    assert_eq!(int(source), 32);
}

#[test]
fn dictionary_of_arrays_nested_containers_round_trip() {
    let source = r#"
        public Dictionary<string, int[]> Scale(Dictionary<string, int[]> input) {
            Dictionary<string, int[]> output = new Dictionary<string, int[]>();
            int[] odd = new int[2];
            odd[0] = 7; odd[1] = 9;
            output.Add("odd", odd);
            int[] first = input.GetOr("first", odd);
            output.Add("echo", first);
            return output;
        }
        public int Main() {
            Dictionary<string, int[]> input = new Dictionary<string, int[]>();
            int[] first = new int[3];
            first[0] = 4; first[1] = 5; first[2] = 6;
            input.Add("first", first);
            Dictionary<string, int[]> result = Task.Run(Scale, input).Wait();
            first[0] = 99;
            int[] echoed = result.GetOr("echo", first);
            int[] odd = result.GetOr("odd", first);
            return result.Length * 1000 + echoed[0] * 100 + echoed.Length * 10 + odd[1];
        }
    "#;
    assert_eq!(int(source), 2000 + 400 + 30 + 9);
}

#[test]
fn list_of_string_arrays_nests_two_levels() {
    let source = r#"
        public int Count(List<string[]> rows) {
            int total = 0;
            for (int i = 0; i < rows.Length; i++) {
                string[] row = rows[i];
                for (int j = 0; j < row.Length; j++) { total += row[j].Length; }
            }
            return total;
        }
        public int Main() {
            List<string[]> rows = new List<string[]>();
            string[] a = ["x", "yy"];
            string[] b = ["zzz"];
            rows.Add(a); rows.Add(b);
            return Task.Run(Count, rows).Wait();
        }
    "#;
    assert_eq!(int(source), 6);
}

#[test]
fn struct_with_string_argument_and_result_round_trip() {
    let source = r#"
        public struct Person { public string name; public int age; public int[] scores; }
        public Person Older(Person person) {
            person.age = person.age + 1;
            person.name = person.name + "*";
            person.scores[0] = 77;
            return person;
        }
        public int Main() {
            int[] scores = new int[1];
            scores[0] = 5;
            Person person = Person { name: "Ada", age: 36, scores: scores };
            Person older = Task.Run(Older, person).Wait();
            if (person.name != "Ada") { return -1; }
            if (scores[0] != 5) { return -2; }
            if (older.name != "Ada*") { return -3; }
            return older.age * 100 + older.scores[0];
        }
    "#;
    assert_eq!(int(source), 3777);
}

#[test]
fn enum_with_string_payload_round_trips() {
    let source = r#"
        public enum Message { Empty, Text(string value), Pair(string left, int right) }
        public Message Wrap(Message message) {
            return message switch {
                Empty => Message.Empty,
                Text(value) => Message.Text(value + "?"),
                Pair(left, right) => Message.Pair(left, right + 1),
            };
        }
        public int Main() {
            Message sent = Message.Pair("p", 41);
            Message back = Task.Run(Wrap, sent).Wait();
            return back switch {
                Empty => -1,
                Text(value) => -2,
                Pair(left, right) => right + left.Length,
            };
        }
    "#;
    assert_eq!(int(source), 43);
}

#[test]
fn wait_all_materializes_every_string_result() {
    let source = r#"
        public string Name(int id) { return "task-" + id.ToString(); }
        public int Main() {
            Task<string>[] tasks = new Task<string>[3];
            tasks[0] = Task.Run(Name, 1);
            tasks[1] = Task.Run(Name, 22);
            tasks[2] = Task.Run(Name, 333);
            string[] names = Task.WaitAll(tasks);
            if (names[1] != "task-22") { return -1; }
            return names[0].Length + names[1].Length + names[2].Length;
        }
    "#;
    assert_eq!(int(source), 6 + 7 + 8);
}

#[test]
fn waiting_twice_yields_independent_copies() {
    let source = r#"
        public int[] Make() { int[] values = new int[2]; values[0] = 1; values[1] = 2; return values; }
        public int Main() {
            Task<int[]> task = Task.Run(Make);
            int[] first = task.Wait();
            first[0] = 50;
            int[] second = task.Wait();
            return first[0] * 10 + second[0];
        }
    "#;
    assert_eq!(int(source), 501);
}

#[test]
fn large_array_round_trips() {
    let source = r#"
        public int[] Double(int[] values) {
            int[] result = new int[values.Length];
            for (int i = 0; i < values.Length; i++) { result[i] = values[i] * 2; }
            return result;
        }
        public int Main() {
            int[] values = new int[50000];
            for (int i = 0; i < values.Length; i++) { values[i] = i; }
            int[] doubled = Task.Run(Double, values).Wait();
            if (doubled.Length != 50000) { return -1; }
            return doubled[49999] - 99990;
        }
    "#;
    assert_eq!(int(source), 8);
}

#[test]
fn many_concurrent_transfers_stay_correct() {
    let source = r#"
        public string Label(string prefix, int[] values) {
            int total = 0;
            for (int i = 0; i < values.Length; i++) { total += values[i]; }
            return prefix + total.ToString();
        }
        public int Main() {
            int[] values = new int[4];
            values[0] = 1; values[1] = 2; values[2] = 3; values[3] = 4;
            Task<string>[] tasks = new Task<string>[16];
            for (int i = 0; i < 16; i++) { tasks[i] = Task.Run(Label, "sum=", values); }
            string[] labels = Task.WaitAll(tasks);
            int ok = 0;
            for (int i = 0; i < 16; i++) { if (labels[i] == "sum=10") { ok += 1; } }
            return ok;
        }
    "#;
    assert_eq!(int(source), 16);
}

#[test]
fn class_instance_arguments_and_results_stay_rejected() {
    let argument = compile_errors(
        r"
        public class Box { public int value; }
        public int Read(Box box) { return box.value; }
        public int Main() { Box box = new Box(); return Task.Run(Read, box).Wait(); }
        ",
    );
    assert!(
        argument
            .iter()
            .any(|message| message.contains("cannot cross a worker boundary")),
        "expected rejection of a class argument, got {argument:?}"
    );
    let result = compile_errors(
        r"
        public class Box { public int value; }
        public Box Make() { return new Box(); }
        public int Main() { Task<Box> task = Task.Run(Make); return 0; }
        ",
    );
    assert!(
        result
            .iter()
            .any(|message| message.contains("cannot cross a worker boundary")),
        "expected rejection of a class result, got {result:?}"
    );
    let nested = compile_errors(
        r"
        public class Box { public int value; }
        public int Read(List<Box> boxes) { return boxes.Length; }
        public int Main() { List<Box> boxes = new List<Box>(); return Task.Run(Read, boxes).Wait(); }
        ",
    );
    assert!(
        nested
            .iter()
            .any(|message| message.contains("cannot cross a worker boundary")),
        "expected rejection of a list of class instances, got {nested:?}"
    );
}

#[test]
fn awaiting_a_copying_task_is_a_clear_compile_error() {
    let errors = compile_errors(
        r#"
        public string Name() { return "x"; }
        public async Task<int> Go() { string s = await Task.Run(Name); return s.Length; }
        public int Main() { return 0; }
        "#,
    );
    assert!(
        errors
            .iter()
            .any(|message| message.contains("not supported in this version")),
        "expected an await restriction diagnostic, got {errors:?}"
    );
}

/// Counting configuration (`--features mem-exp-c6`): repeated materialization
/// of strings, string arrays, lists and dictionaries in the waiting context
/// must never expose a freed block (debug builds poison released blocks) and
/// the main context's accounting must stay bounded by the live data.
#[cfg(feature = "mem-exp-rc-core")]
#[test]
fn counting_configuration_has_no_use_after_free_and_sane_accounting() {
    let source = r#"
        public string[] Words(string seed, int[] marks) {
            string[] words = [seed + "a", seed + "bb", seed + "ccc"];
            return words;
        }
        public List<string> Names(List<string> input) { input.Add("tail"); return input; }
        public int Main() {
            int total = 0;
            int[] marks = new int[2];
            marks[0] = 1; marks[1] = 2;
            for (int i = 0; i < 300; i++) {
                string[] words = Task.Run(Words, "w", marks).Wait();
                List<string> names = new List<string>();
                names.Add("x");
                List<string> grown = Task.Run(Names, names).Wait();
                if (words[2] != "wccc") { return -1; }
                if (grown[1] != "tail") { return -2; }
                total += words[1].Length + grown.Length;
            }
            return total;
        }
    "#;
    let compilation = compile(source).expect("compiles");
    let (value, stats) =
        aster_codegen_cranelift::execute_with_stats(&compilation.mir, "Main").expect("runs");
    assert_eq!(value, ExecutionValue::Int(300 * 5));
    assert!(
        stats.peak_reserved_bytes < 256 * 1024 * 1024,
        "materialized transfers must not grow without bound: {stats:?}"
    );
    assert!(stats.string_allocations >= 300, "{stats:?}");
}
