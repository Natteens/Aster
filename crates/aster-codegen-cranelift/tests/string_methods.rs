use std::sync::atomic::{AtomicU64, Ordering};

use aster_codegen_cranelift::{ExecutionValue, execute, execute_with_stats};
use aster_compiler::{compile, compile_project};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

fn run(source: &str, function: &str) -> Result<ExecutionValue, String> {
    let compilation = compile(source).map_err(|diagnostics| format!("{diagnostics:#?}"))?;
    execute(&compilation.mir, function).map_err(|error| error.to_string())
}

fn run_project(source: &str, function: &str) -> Result<ExecutionValue, String> {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "aster-string-methods-{}-{id}.aster",
        std::process::id()
    ));
    std::fs::write(&path, source).expect("write temporary Aster project");
    let compilation = compile_project(&path).map_err(|diagnostics| format!("{diagnostics:#?}"));
    std::fs::remove_file(&path).expect("remove temporary Aster project");
    execute(&compilation?.compilation.mir, function).map_err(|error| error.to_string())
}

fn run_namespace_project() -> Result<ExecutionValue, String> {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "aster-string-namespace-{}-{id}",
        std::process::id()
    ));
    let library = root.join("text");
    std::fs::create_dir_all(&library).expect("create namespace project");
    std::fs::write(
        library.join("slice.aster"),
        "namespace text; public string Name(string value) { return value.Substring(0, 5); }",
    )
    .expect("write namespace helper");
    let main = root.join("main.aster");
    std::fs::write(
        &main,
        "using text; public int Run() { return Name(\"aster language\").Contains(\"aster\") ? 42 : 0; }",
    )
    .expect("write project entry");
    let compilation = compile_project(&main).map_err(|diagnostics| format!("{diagnostics:#?}"));
    std::fs::remove_dir_all(&root).expect("remove namespace project");
    execute(&compilation?.compilation.mir, "Run").map_err(|error| error.to_string())
}

#[test]
fn executes_the_public_string_inspection_example() {
    let source = r#"
        public int Run() {
            string text = "aster language";
            if (!text.StartsWith("aster")) { return 1; }
            if (!text.Contains("language")) { return 2; }
            if (text.IndexOf("language") != 6) { return 3; }
            string name = text.Substring(0, 5);
            if (name != "aster") { return 4; }
            return 42;
        }
    "#;
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn search_is_ordinal_and_scalar_indexed_for_unicode() {
    let source = r#"
        public int Run() {
            string text = "aéβ🙂z";
            if (text.Length != 5) { return 1; }
            if (text.IndexOf("é") != 1) { return 2; }
            if (text.IndexOf("β") != 2) { return 3; }
            if (text.IndexOf("🙂") != 3) { return 4; }
            if (text.IndexOf("missing") != -1) { return 5; }
            if (text.IndexOf("") != 0) { return 6; }
            if (!text.Contains("éβ")) { return 7; }
            if (text.Contains("ASTER")) { return 8; }
            if (!text.StartsWith("")) { return 9; }
            if (text.StartsWith("β")) { return 10; }
            if (!text.EndsWith("z")) { return 11; }
            if (!text.EndsWith("")) { return 12; }
            if (text.EndsWith("aster")) { return 13; }
            if (!text.Contains("")) { return 14; }
            if (!"".Contains("")) { return 15; }
            if ("".Contains("x")) { return 16; }
            if ("a".Contains("longer")) { return 17; }
            if ("aaaa".IndexOf("aa") != 0) { return 18; }
            return 42;
        }
    "#;
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn substring_copies_only_complete_utf8_scalars() {
    let source = r#"
        public int Run() {
            string text = "aéβ🙂z";
            if (text.Substring(1, 3) != "éβ🙂") { return 1; }
            if (text.Substring(4) != "z") { return 2; }
            if (text.Substring(5) != "") { return 3; }
            if (text.Substring(2, 0) != "") { return 4; }
            if (text.Substring(0, text.Length) != text) { return 5; }
            if ("你好世界".Substring(1, 2) != "好世") { return 6; }
            return 42;
        }
    "#;
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn receiver_and_arguments_are_evaluated_once_left_to_right() {
    let source = r#"
        public class Probe {
            public int order;
            public Probe() { order = 0; }
            public string Text() { order = order * 10 + 1; return "aster"; }
            public string Needle() { order = order * 10 + 2; return "ster"; }
            public int Start() { order = order * 10 + 3; return 0; }
            public int Count() { order = order * 10 + 4; return 5; }
        }
        public int Run() {
            Probe probe = new Probe();
            if (!probe.Text().Contains(probe.Needle())) { return -1; }
            if (probe.Text().Substring(probe.Start(), probe.Count()) != "aster") { return -2; }
            return probe.order;
        }
    "#;
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(12_134)));
}

#[test]
fn substring_bounds_are_controlled_and_do_not_contaminate_the_next_execution() {
    for (expression, expected) in [
        ("text.Substring(-1)", "start -1"),
        ("text.Substring(0, -1)", "start 0, length -1"),
        ("text.Substring(4)", "start 4"),
        ("text.Substring(2, 2)", "start 2, length 2"),
        (
            "text.Substring(2147483647, 1)",
            "start 2147483647, length 1",
        ),
    ] {
        let source = format!(
            "public string Bad() {{ string text = \"abc\"; return {expression}; }} public int Good() {{ return 42; }}"
        );
        let compilation = compile(&source).expect("bounds source compiles");
        let error = execute(&compilation.mir, "Bad").expect_err("invalid range fails");
        assert!(error.message().contains("String.Substring"));
        assert!(error.message().contains(expected));
        assert_eq!(
            execute(&compilation.mir, "Good"),
            Ok(ExecutionValue::Int(42))
        );
    }
}

#[test]
fn string_operations_compose_with_existing_types_and_core_enums() {
    let source = r#"
        using aster.core;
        public struct Label { public string text; }
        public class Holder {
            private string text;
            public Holder(string text) { this.text = text; }
            public string Text { get { return text; } }
            public bool Matches(string value) { return text.Contains(value); }
        }
        public T Identity<T>(T value) { return value; }
        public int Run() {
            string[] values = ["prefix-value-suffix"];
            List<string> list = new List<string>();
            list.Add(values[0].Substring(7, 5));
            Label label = Label { text: Identity<string>(list.Get(0)) };
            Holder holder = new Holder(label.text);
            if (!holder.Matches("alu")) { return 5; }
            Option<string> option = Option<string>.Some(holder.Text);
            Result<string, string> result = Result<string, string>.Ok(holder.Text);
            switch (option) {
                case Some(value):
                    if (!value.StartsWith("value")) { return 1; }
                case None: return 2;
            }
            switch (result) {
                case Ok(value): return value.EndsWith("value") ? 42 : 3;
                case Error(message): return 4;
            }
        }
    "#;
    assert_eq!(run_project(source, "Run"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn string_operations_link_across_files_and_namespaces() {
    assert_eq!(run_namespace_project(), Ok(ExecutionValue::Int(42)));
}

#[test]
fn searches_do_not_allocate_and_local_substrings_are_reclaimed() {
    let searches = r#"
        public int Run() {
            string text = "aster language";
            int total = 0;
            for (int index = 0; index < 10000; index++) {
                if (text.Contains("language")) { total += 1; }
                if (text.StartsWith("aster")) { total += 1; }
                if (text.EndsWith("language")) { total += 1; }
                total += text.IndexOf("language");
            }
            return total;
        }
    "#;
    let compilation = compile(searches).expect("search stress compiles");
    let (value, stats) = execute_with_stats(&compilation.mir, "Run").expect("search stress runs");
    assert_eq!(value, ExecutionValue::Int(90_000));
    assert_eq!(stats.total_allocations, 0);
    assert_eq!(stats.string_allocations, 0);
    assert_eq!(stats.used_bytes, 0);
    assert_eq!(stats.reserved_bytes, 0);

    let substrings = r#"
        internal int LocalSlice(string text) {
            string slice = text.Substring(1, 3);
            return slice.Length;
        }
        public int Run() {
            int total = 0;
            for (int index = 0; index < 10000; index++) {
                total += LocalSlice("aéβ🙂z");
            }
            return total;
        }
    "#;
    let compilation = compile(substrings).expect("substring stress compiles");
    let (value, stats) =
        execute_with_stats(&compilation.mir, "Run").expect("substring stress runs");
    assert_eq!(value, ExecutionValue::Int(30_000));
    assert_eq!(stats.total_allocations, 10_000);
    assert_eq!(stats.string_allocations, 10_000);
    assert_eq!(stats.requested_bytes, 160_000);
    assert_eq!(stats.used_bytes, 0);
    assert_eq!(stats.reserved_bytes, 64 * 1024);
    assert_eq!(stats.peak_used_bytes, 16);

    let persistent = compile(
        "internal string Slice(string text) { string alias = text.Substring(0, 5); return alias; } public string Run() { return Slice(\"aster language\"); }",
    )
    .expect("persistent substring compiles");
    let (value, stats) =
        execute_with_stats(&persistent.mir, "Run").expect("persistent substring runs");
    assert_eq!(value, ExecutionValue::String("aster".to_owned()));
    assert_eq!(stats.total_allocations, 1);
    assert_eq!(stats.string_allocations, 1);
    assert_eq!(stats.requested_bytes, 13);
    assert_eq!(stats.used_bytes, 13);
    assert_eq!(stats.reserved_bytes, 64 * 1024);
}
