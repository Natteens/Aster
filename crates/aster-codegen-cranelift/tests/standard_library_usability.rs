//! End-to-end coverage for the compact standard-library usability surface.

use std::sync::atomic::{AtomicU64, Ordering};

use aster_codegen_cranelift::{ExecutionValue, execute};
use aster_compiler::{compile, compile_project};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

fn run(source: &str) -> Result<ExecutionValue, String> {
    let compilation = compile_project_source(source)?;
    execute(&compilation.mir, "Main").map_err(|error| error.to_string())
}

fn compile_project_source(source: &str) -> Result<aster_compiler::Compilation, String> {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "aster-stdlib-usability-{}-{id}.aster",
        std::process::id()
    ));
    std::fs::write(&path, source).expect("write temporary standard-library project");
    let compilation = compile_project(&path).map_err(|diagnostics| format!("{diagnostics:#?}"));
    std::fs::remove_file(&path).expect("remove temporary standard-library project");
    Ok(compilation?.compilation)
}

#[test]
fn text_helpers_are_ordinal_and_preserve_empty_split_segments() {
    let source = r#"
        using aster.text;
        public int Main() {
            string value = String.Trim("  alpha,,beta  ");
            string[] parts = String.Split(value, ",");
            if (parts.Length != 3 || parts[1] != "") { return 1; }
            if (!String.Contains(value, "alpha") || !String.StartsWith(value, "alpha")) { return 2; }
            if (!String.EndsWith(value, "beta")) { return 3; }
            if (String.Replace(value, "alpha", "ASTER") != "ASTER,,beta") { return 4; }
            if (String.Substring(value, 0, 5) != "alpha") { return 5; }
            return 42;
        }
    "#;
    assert_eq!(run(source), Ok(ExecutionValue::Int(42)));
}

#[test]
fn text_helpers_share_unicode_scalar_and_empty_pattern_semantics() {
    let source = r#"
        using aster.text;
        public int Main() {
            string mixed = "aáβ🙂z";
            if (String.Substring(mixed, 1, 3) != "áβ🙂") { return 1; }
            if (!String.Contains(mixed, "áβ") || !String.StartsWith(mixed, "aá")) { return 2; }
            if (!String.EndsWith(mixed, "🙂z")) { return 3; }
            if (String.Trim("\t  aáβ🙂z　\r") != mixed) { return 4; }
            if (String.Trim("ZERO_WIDTH") != "ZERO_WIDTH") { return 5; }
            if (String.Replace("aaaa", "aa", "aaa") != "aaaaaa") { return 6; }
            string[] parts = String.Split("a,,β🙂,", ",");
            if (parts.Length != 4 || parts[0] != "a" || parts[1] != "" || parts[2] != "β🙂" || parts[3] != "") { return 7; }
            if (!String.Contains("", "") || !String.StartsWith("abc", "") || !String.EndsWith("", "")) { return 8; }
            return 42;
        }
    "#
    .replace("ZERO_WIDTH", &format!("{}a{}", '\u{200b}', '\u{200b}'));
    assert_eq!(run(&source), Ok(ExecutionValue::Int(42)));
}

#[test]
fn text_failure_contracts_remain_controlled() {
    for expression in [
        "String.Replace(\"value\", \"\", \"x\")",
        "String.Split(\"value\", \"\")",
        "String.Substring(\"value\", -1)",
        "String.Substring(\"value\", 2147483647, 2147483647)",
    ] {
        let source = format!(
            "using aster.text; public void Bad() {{ {expression}; }} public int Main() {{ return 42; }}"
        );
        let compilation = compile_project_source(&source)
            .expect("source compiles before controlled runtime failure");
        assert!(execute(&compilation.mir, "Bad").is_err(), "{expression}");
        assert_eq!(
            execute(&compilation.mir, "Main"),
            Ok(ExecutionValue::Int(42))
        );
    }
}

#[test]
fn math_helpers_use_the_runtime_float_contract() {
    let source = r"
        using aster.math;
        public int Main() {
            if (Math.Sqrt(81d) != 9d || Math.Pow(2d, 3d) != 8d) { return 1; }
            if (Math.Sqrt(81f) != 9f || Math.Pow(-2f, 3f) != -8f) { return 2; }
            if (Math.Floor(-2.1d) != -3d || Math.Ceil(-2.9d) != -2d) { return 3; }
            if (Math.Round(0.5d) != 0d || Math.Round(1.5d) != 2d || Math.Round(2.5d) != 2d) { return 4; }
            if (Math.Round(-1.5f) != -2f || Math.Round(-2.5f) != -2f) { return 5; }
            double invalidRoot = Math.Sqrt(-1d);
            double invalidPower = Math.Pow(-2d, 0.5d);
            if (invalidRoot == invalidRoot || invalidPower == invalidPower) { return 6; }
            if (Math.Sin(0d) != 0d || Math.Cos(0d) != 1d || Math.Tan(0d) != 0d) { return 7; }
            return 42;
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(42)));
}

#[test]
fn collection_mutation_and_snapshots_have_independent_storage() {
    let source = r#"
        using aster.collections;
        public int Main() {
            List<string> values = new List<string>();
            values.Add("first");
            values.Add("second");
            values.Set(1, "changed");
            string[] snapshot = values.ToArray();
            values.Clear();
            if (values.Length != 0 || snapshot.Length != 2 || snapshot[1] != "changed") { return 1; }

            Dictionary<string, int> scores = new Dictionary<string, int>();
            scores.Add("a", 10);
            scores.Add("b", 20);
            string[] keys = scores.Keys();
            int[] numbers = scores.Values();
            scores.Clear();
            if (scores.Length != 0 || keys.Length != 2 || numbers.Length != 2) { return 2; }
            return numbers[0] + numbers[1] == 30 ? 42 : 3;
        }
    "#;
    assert_eq!(run(source), Ok(ExecutionValue::Int(42)));
}

#[test]
fn list_set_is_non_structural_but_clear_invalidates_foreach() {
    let source = r"
        public int SetDuringForeach() {
            List<int> values = new List<int>();
            values.Add(1);
            values.Add(2);
            List<int> alias = values;
            int index = 0;
            int total = 0;
            foreach (int value in values) {
                if (index == 0) { alias.Set(1, 7); }
                total += value;
                index += 1;
            }
            return total;
        }
        public void ClearDuringForeach() {
            List<int> values = new List<int>();
            values.Add(1);
            values.Add(2);
            foreach (int value in values) { values.Clear(); }
        }
        public int Main() { return SetDuringForeach(); }
    ";
    let compilation = compile(source).expect("collection foreach source compiles");
    assert_eq!(
        execute(&compilation.mir, "Main"),
        Ok(ExecutionValue::Int(8))
    );
    assert!(execute(&compilation.mir, "ClearDuringForeach").is_err());
}

#[test]
fn dictionary_snapshots_follow_entry_order_and_reference_copy_semantics() {
    let source = r#"
        using aster.collections;
        public class Box { public int Value; public Box(int value) { Value = value; } }
        public struct Point { public int X; }
        public int Main() {
            Dictionary<string, int> values = new Dictionary<string, int>();
            values.Add("a", 10);
            values.Add("b", 20);
            values.Set("a", 11);
            values.Remove("a");
            values.Add("a", 30);
            string[] keys = values.Keys();
            int[] numbers = values.Values();
            Dictionary<string, int> alias = values;
            values.Clear();
            alias.Add("c", 40);
            if (keys[0] != "b" || keys[1] != "a" || numbers[0] != 20 || numbers[1] != 30) { return 1; }
            if (values.Length != 1 || !values.ContainsKey("c") || values.ContainsKey("a")) { return 2; }

            List<Box> boxes = new List<Box>();
            Box box = new Box(5);
            boxes.Add(box);
            Box replacement = new Box(12);
            boxes.Set(0, replacement);
            Box[] boxSnapshot = boxes.ToArray();
            replacement.Value = 9;
            boxes.Clear();
            int pressureChecksum = 0;
            for (int index = 0; index < 100; index += 1) {
                int[] pressure = new int[1024];
                pressure[0] = index;
                pressureChecksum += pressure[0];
            }
            if (pressureChecksum != 4950) { return 3; }
            if (boxSnapshot[0].Value != 9) { return 3; }

            List<Point> points = new List<Point>();
            points.Add(Point { X: 4 });
            Point[] pointSnapshot = points.ToArray();
            points.Set(0, Point { X: 8 });
            if (pointSnapshot[0].X != 4 || points.Get(0).X != 8) { return 4; }
            return 42;
        }
    "#;
    assert_eq!(run(source), Ok(ExecutionValue::Int(42)));
}

#[test]
fn collection_mutation_failures_are_not_optimized_away() {
    let source = r"
        public void Bad() {
            List<int> values = new List<int>();
            values.Set(0, 1);
        }
        public int Main() { return 42; }
    ";
    let compilation = compile(source).expect("source compiles before bounds failure");
    assert!(execute(&compilation.mir, "Bad").is_err());
    assert_eq!(
        execute(&compilation.mir, "Main"),
        Ok(ExecutionValue::Int(42))
    );
}

#[test]
fn usability_surface_resolves_and_executes_across_project_files() {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let directory = std::env::temp_dir().join(format!(
        "aster-stdlib-usability-project-{}-{id}",
        std::process::id()
    ));
    let namespace = directory.join("helpers");
    std::fs::create_dir_all(&namespace).expect("create multi-file project");
    let root = directory.join("main.aster");
    std::fs::write(
        &root,
        "using helpers; public int Main() { return StandardLibraryResult(); }",
    )
    .expect("write multi-file root");
    std::fs::write(
        namespace.join("api.aster"),
        r#"
            namespace helpers;
            using aster.collections;
            using aster.math;
            using aster.text;
            public int StandardLibraryResult() {
                List<string> values = new List<string>();
                values.Add(String.Replace("a,b", ",", ":"));
                string[] snapshot = values.ToArray();
                Dictionary<string, int> lengths = new Dictionary<string, int>();
                lengths.Add(snapshot[0], snapshot[0].Length);
                string[] keys = lengths.Keys();
                return keys[0] == "a:b" && Math.Sqrt(225d) == 15d ? 15 : 0;
            }
        "#,
    )
    .expect("write multi-file helper");
    let compilation = compile_project(&root)
        .expect("multi-file standard-library project compiles")
        .compilation;
    std::fs::remove_dir_all(&directory).expect("remove multi-file project");
    assert_eq!(
        execute(&compilation.mir, "Main"),
        Ok(ExecutionValue::Int(15))
    );
}

#[test]
fn worker_local_usability_operations_preserve_existing_transfer_rules() {
    let source = r#"
        using aster.math;
        using aster.text;
        public int Worker() {
            List<int> values = new List<int>();
            values.Add(9);
            values.Set(0, 16);
            int[] snapshot = values.ToArray();
            values.Clear();
            string text = String.Replace("worker-local", "local", "safe");
            return snapshot[0] + (text == "worker-safe" && Math.Sqrt(81d) == 9d ? 1 : 0);
        }
        public int Main() {
            Task<int> task = Task.Run(Worker);
            return task.Wait();
        }
    "#;
    assert_eq!(run(source), Ok(ExecutionValue::Int(17)));
}
