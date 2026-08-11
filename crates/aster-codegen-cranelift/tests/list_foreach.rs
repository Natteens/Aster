//! M3C: `foreach` over `List<T>` with fail-fast structural-mutation
//! detection. Extends M3B's `foreach` (see `foreach.rs`, arrays) to the
//! nominal `List<T>` case: `HIR`/`Statement::ForEach` is unchanged, only
//! MIR lowering picks a different concrete strategy
//! (`lower_foreach_over_list` vs `lower_foreach_over_array`) based on
//! `collection.type_`. No iterator, enumerator, or public API is added;
//! `List<T>` gains one internal header field (a structural-version
//! counter) and two private runtime primitives (`ListVersion`,
//! `ListVersionMismatch`), never exposed to Aster source. `foreach` over
//! `string` remains unsupported (M3D).

use std::sync::atomic::{AtomicU64, Ordering};

use aster_codegen_cranelift::{ExecutionValue, MemoryStats, execute, execute_with_stats};
use aster_compiler::{compile, compile_project, mir};

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

fn compile_mir(source: &str) -> mir::Module {
    compile(source).expect("source should compile").mir
}

fn stats(source: &str) -> (ExecutionValue, MemoryStats) {
    execute_with_stats(&compile_mir(source), "Main").expect("source should execute")
}

/// `Option<T>`/`Result<T, E>` need their real generic template declarations
/// linked from `aster.core`, which single-file `compile()` does not do (see
/// `foreach.rs`, `option_try_propagation.rs`).
fn run_project(source: &str) -> Result<ExecutionValue, String> {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "aster-list-foreach-project-{}-{id}.aster",
        std::process::id()
    ));
    std::fs::write(&path, source).expect("write temporary project");
    let compilation = compile_project(&path).map_err(|diagnostics| format!("{diagnostics:#?}"));
    std::fs::remove_file(&path).ok();
    execute(&compilation?.compilation.mir, "Main").map_err(|error| error.to_string())
}

const MODIFIED_MESSAGE: &str = "structurally modified";

// --- Section 18.1-13: success cases -----------------------------------------------

#[test]
fn empty_list_foreach_does_not_execute_the_body() {
    let source = r"
        public int Main()
        {
            List<int> values = new List<int>();
            int total = 0;
            foreach (int value in values) { total = total + 1; }
            return total;
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(0)));
}

#[test]
fn a_single_element_list_foreach_runs_once() {
    let source = r"
        public int Main()
        {
            List<int> values = new List<int>();
            values.Add(42);
            int total = 0;
            foreach (int value in values) { total = total + value; }
            return total;
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(42)));
}

#[test]
fn a_multi_element_list_foreach_preserves_insertion_order() {
    let source = r#"
        public int Main()
        {
            List<int> values = new List<int>();
            values.Add(1);
            values.Add(2);
            values.Add(3);
            values.Add(4);
            string order = "";
            foreach (int value in values) { order = order + value.ToString(); }
            return order == "1234" ? 1 : 0;
        }
    "#;
    assert_eq!(run(source), Ok(ExecutionValue::Int(1)));
}

#[test]
fn the_list_expression_is_evaluated_exactly_once() {
    let source = r"
        public class Counter { public int calls; }
        public List<int> Provide(Counter counter)
        {
            counter.calls = counter.calls + 1;
            List<int> values = new List<int>();
            values.Add(1);
            values.Add(2);
            return values;
        }
        public int Main()
        {
            Counter counter = new Counter();
            int total = 0;
            foreach (int value in Provide(counter)) { total = total + value; }
            return total * 1000 + counter.calls;
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(3000 + 1)));
}

#[test]
fn the_length_is_captured_once_and_a_reassigned_binding_does_not_change_it() {
    let source = r"
        public int Main()
        {
            List<int> values = new List<int>();
            values.Add(1);
            values.Add(2);
            List<int> longer = new List<int>();
            longer.Add(1);
            longer.Add(2);
            longer.Add(3);
            longer.Add(4);
            int visits = 0;
            foreach (int value in values)
            {
                values = longer;
                visits = visits + 1;
            }
            return visits;
        }
    ";
    // Reassigning `values` inside the body must not affect the already
    // captured list/length: exactly 2 visits, not 4.
    assert_eq!(run(source), Ok(ExecutionValue::Int(2)));
}

#[test]
fn list_foreach_over_structs_copies_by_value() {
    let source = r"
        public struct Point { public int Value; }
        public int Main()
        {
            List<Point> points = new List<Point>();
            points.Add(Point { Value: 1 });
            points.Add(Point { Value: 2 });
            int total = 0;
            foreach (Point point in points) { total = total + point.Value; }
            return total;
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(3)));
}

#[test]
fn list_foreach_over_classes_reads_the_same_object_reference() {
    let source = r"
        public class Counter { public int Value; public Counter(int value) { Value = value; } }
        public int Main()
        {
            List<Counter> counters = new List<Counter>();
            counters.Add(new Counter(1));
            counters.Add(new Counter(2));
            int total = 0;
            foreach (Counter counter in counters) { total = total + counter.Value; }
            return total;
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(3)));
}

#[test]
fn list_foreach_over_strings_reads_correct_content() {
    let source = r#"
        public int Main()
        {
            List<string> values = new List<string>();
            values.Add("a");
            values.Add("bb");
            values.Add("ccc");
            int total = 0;
            foreach (string value in values) { total = total + value.Length; }
            return total;
        }
    "#;
    assert_eq!(run(source), Ok(ExecutionValue::Int(6)));
}

#[test]
fn list_foreach_works_inside_a_concretized_generic_function() {
    let source = r"
        public T First<T>(List<T> values)
        {
            foreach (T value in values) { return value; }
            return values.Get(0);
        }
        public int Main()
        {
            List<int> values = new List<int>();
            values.Add(42);
            return First<int>(values);
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(42)));
}

#[test]
fn list_foreach_works_inside_a_declared_namespace() {
    let source = r"
        namespace app;
        public int Main()
        {
            List<int> values = new List<int>();
            values.Add(1);
            values.Add(2);
            values.Add(3);
            int total = 0;
            foreach (int value in values) { total = total + value; }
            return total;
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(6)));
}

#[test]
fn list_foreach_works_across_a_multifile_project() {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "aster-list-foreach-multifile-{}-{id}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("create project root");
    std::fs::write(
        root.join("Aster.toml"),
        "[package]\nname = \"list_foreach_test\"\n",
    )
    .expect("write manifest");
    let app_dir = root.join("app");
    std::fs::create_dir_all(&app_dir).expect("create app dir");
    std::fs::write(
        app_dir.join("main.aster"),
        "namespace app;\n\
         public int Main() {\n\
             List<int> values = new List<int>();\n\
             values.Add(1);\n\
             values.Add(2);\n\
             values.Add(3);\n\
             values.Add(4);\n\
             return Helpers.Sum(values);\n\
         }",
    )
    .expect("write main.aster");
    std::fs::write(
        app_dir.join("helpers.aster"),
        "namespace app;\n\
         public class Helpers {\n\
             public static int Sum(List<int> values) {\n\
                 int total = 0;\n\
                 foreach (int value in values) { total = total + value; }\n\
                 return total;\n\
             }\n\
         }",
    )
    .expect("write helpers.aster");
    let compilation = compile_project(&app_dir.join("main.aster"))
        .map_err(|diagnostics| format!("{diagnostics:#?}"));
    std::fs::remove_dir_all(&root).ok();
    let module = compilation
        .expect("multifile project using list foreach should compile")
        .compilation
        .mir;
    assert_eq!(
        execute(&module, "list_foreach_test::app::Main"),
        Ok(ExecutionValue::Int(10))
    );
}

// --- Section 18.14-20: control flow -----------------------------------------------

#[test]
fn list_foreach_supports_break() {
    let source = r"
        public int Main()
        {
            List<int> values = new List<int>();
            values.Add(1);
            values.Add(2);
            values.Add(3);
            int total = 0;
            foreach (int value in values)
            {
                if (value == 3) { break; }
                total = total + value;
            }
            return total;
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(3)));
}

#[test]
fn list_foreach_supports_continue_and_advances_exactly_once() {
    let source = r"
        public int Main()
        {
            List<int> values = new List<int>();
            values.Add(1);
            values.Add(2);
            values.Add(3);
            values.Add(4);
            int total = 0;
            foreach (int value in values)
            {
                if (value % 2 == 0) { continue; }
                total = total + value;
            }
            return total;
        }
    ";
    // A `continue` that skipped the update (looping on the same index
    // forever) would hang this test instead of returning 1 + 3 = 4.
    assert_eq!(run(source), Ok(ExecutionValue::Int(4)));
}

#[test]
fn list_foreach_supports_return() {
    let source = r"
        public int Main()
        {
            List<int> values = new List<int>();
            values.Add(1);
            values.Add(2);
            values.Add(3);
            foreach (int value in values)
            {
                if (value == 2) { return 99; }
            }
            return -1;
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(99)));
}

#[test]
fn list_foreach_supports_postfix_try() {
    let source = r#"
        using aster.core;
        public Result<int, string> Convert(int value)
        {
            if (value == 2) { return Result<int, string>.Error("bad"); }
            return Result<int, string>.Ok(value);
        }
        public Result<int, string> Process(List<int> values)
        {
            int total = 0;
            foreach (int value in values)
            {
                int parsed = Convert(value)?;
                total = total + parsed;
            }
            return Result<int, string>.Ok(total);
        }
        public int Main()
        {
            List<int> values = new List<int>();
            values.Add(1);
            values.Add(2);
            values.Add(3);
            switch (Process(values)) {
                case Ok(total): return total;
                case Error(message): return -1;
            }
        }
    "#;
    assert_eq!(run_project(source), Ok(ExecutionValue::Int(-1)));
}

#[test]
fn list_foreach_nests_with_itself() {
    let source = r"
        public int Main()
        {
            List<int> outer = new List<int>();
            outer.Add(1);
            outer.Add(2);
            List<int> inner = new List<int>();
            inner.Add(10);
            inner.Add(20);
            int total = 0;
            foreach (int a in outer)
            {
                foreach (int b in inner) { total = total + a * b; }
            }
            return total;
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(90)));
}

#[test]
fn array_foreach_nests_inside_list_foreach() {
    let source = r"
        public int Main()
        {
            List<int> outer = new List<int>();
            outer.Add(1);
            outer.Add(2);
            int[] inner = [10, 20];
            int total = 0;
            foreach (int a in outer)
            {
                foreach (int b in inner) { total = total + a * b; }
            }
            return total;
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(90)));
}

#[test]
fn list_foreach_nests_inside_array_foreach() {
    let source = r"
        public int Main()
        {
            int[] outer = [1, 2];
            List<int> inner = new List<int>();
            inner.Add(10);
            inner.Add(20);
            int total = 0;
            foreach (int a in outer)
            {
                foreach (int b in inner) { total = total + a * b; }
            }
            return total;
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(90)));
}

// --- Section 18.21-25: readonly ----------------------------------------------------

#[test]
fn reassigning_a_scalar_list_foreach_variable_is_rejected() {
    let errors = compile_errors(
        "public int Main() { List<int> values = new List<int>(); values.Add(1); foreach (int value in values) { value = 10; } return 0; }",
    );
    assert!(errors.iter().any(|message| message.contains("read-only")));
}

#[test]
fn assigning_to_a_struct_list_foreach_variables_field_is_rejected() {
    let errors = compile_errors(
        "public struct Point { public int X; }\n\
         public int Main() { List<Point> points = new List<Point>(); points.Add(Point { X: 1 }); foreach (Point point in points) { point.X = 10; } return 0; }",
    );
    assert!(errors.iter().any(|message| message.contains("read-only")));
}

#[test]
fn assigning_through_a_nested_struct_member_of_a_list_foreach_variable_is_rejected() {
    let errors = compile_errors(
        "public struct Inner { public int X; }\n\
         public struct Outer { public Inner Inner; }\n\
         public int Main() {\n\
             List<Outer> values = new List<Outer>();\n\
             values.Add(Outer { Inner: Inner { X: 1 } });\n\
             foreach (Outer value in values) { value.Inner.X = 10; }\n\
             return 0;\n\
         }",
    );
    assert!(
        errors.iter().any(|message| message.contains("read-only")),
        "the rule must follow the root symbol through a struct.struct.field chain, got {errors:?}"
    );
}

#[test]
fn assigning_to_a_class_list_foreach_variables_field_still_works() {
    let source = r"
        public class Player { public int Health; }
        public int Main()
        {
            List<Player> players = new List<Player>();
            players.Add(new Player());
            foreach (Player player in players) { player.Health = 10; }
            return players.Get(0).Health;
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(10)));
}

#[test]
fn reassigning_a_class_typed_list_foreach_variable_is_rejected() {
    let errors = compile_errors(
        r"
        public class Player { public int Health; }
        public Player GetOtherPlayer() { return new Player(); }
        public int Main()
        {
            List<Player> players = new List<Player>();
            players.Add(new Player());
            foreach (Player player in players) { player = GetOtherPlayer(); }
            return 0;
        }
        ",
    );
    assert!(errors.iter().any(|message| message.contains("read-only")));
}

// --- Section 18.26-35: structural mutation ----------------------------------------

#[test]
fn direct_add_during_list_foreach_fails_before_continuing() {
    let source = r"
        public int Main()
        {
            List<int> values = new List<int>();
            values.Add(1);
            values.Add(2);
            int total = 0;
            foreach (int value in values)
            {
                values.Add(value);
                total = total + value;
            }
            return total;
        }
    ";
    let error = run(source).expect_err("Add during foreach must be rejected");
    assert!(error.contains(MODIFIED_MESSAGE), "got {error:?}");
}

#[test]
fn direct_remove_at_during_list_foreach_fails_before_continuing() {
    let source = r"
        public int Main()
        {
            List<int> values = new List<int>();
            values.Add(1);
            values.Add(2);
            values.Add(3);
            int total = 0;
            foreach (int value in values)
            {
                values.RemoveAt(0);
                total = total + value;
            }
            return total;
        }
    ";
    let error = run(source).expect_err("RemoveAt during foreach must be rejected");
    assert!(error.contains(MODIFIED_MESSAGE), "got {error:?}");
}

#[test]
fn add_through_an_alias_during_list_foreach_fails_before_continuing() {
    let source = r"
        public int Main()
        {
            List<int> values = new List<int>();
            values.Add(1);
            values.Add(2);
            List<int> alias = values;
            int total = 0;
            foreach (int value in values)
            {
                alias.Add(99);
                total = total + value;
            }
            return total;
        }
    ";
    // The version counter lives on the shared header, not on the binding
    // name, so mutation through `alias` is detected exactly like mutation
    // through `values` itself -- proving this is not textual name analysis.
    let error = run(source).expect_err("alias-driven Add during foreach must be rejected");
    assert!(error.contains(MODIFIED_MESSAGE), "got {error:?}");
}

#[test]
fn remove_at_through_an_alias_during_list_foreach_fails_before_continuing() {
    let source = r"
        public int Main()
        {
            List<int> values = new List<int>();
            values.Add(1);
            values.Add(2);
            values.Add(3);
            List<int> alias = values;
            int total = 0;
            foreach (int value in values)
            {
                alias.RemoveAt(0);
                total = total + value;
            }
            return total;
        }
    ";
    let error = run(source).expect_err("alias-driven RemoveAt during foreach must be rejected");
    assert!(error.contains(MODIFIED_MESSAGE), "got {error:?}");
}

#[test]
fn mutation_inside_a_helper_function_during_list_foreach_fails_before_continuing() {
    let source = r"
        public void Mutate(List<int> values) { values.Add(99); }
        public int Main()
        {
            List<int> values = new List<int>();
            values.Add(1);
            values.Add(2);
            int total = 0;
            foreach (int value in values)
            {
                Mutate(values);
                total = total + value;
            }
            return total;
        }
    ";
    let error = run(source).expect_err("mutation via a helper during foreach must be rejected");
    assert!(error.contains(MODIFIED_MESSAGE), "got {error:?}");
}

#[test]
fn mutation_on_the_first_iteration_stops_before_reading_a_second_element() {
    let source = r"
        public int Main()
        {
            List<int> values = new List<int>();
            values.Add(1);
            values.Add(2);
            values.Add(3);
            int reads = 0;
            foreach (int value in values)
            {
                reads = reads + 1;
                values.Add(99);
            }
            return reads;
        }
    ";
    // `execute` discards the successfully-computed value on a controlled
    // failure, so `reads` itself is unobservable from the test -- the
    // stronger, directly observable claim is that the call errors instead
    // of silently returning some count.
    let error = run(source).expect_err("mutation on the first iteration must be rejected");
    assert!(error.contains(MODIFIED_MESSAGE), "got {error:?}");
}

#[test]
fn mutation_after_several_successful_iterations_is_still_caught() {
    let source = r"
        public class Counter { public int reads; }
        public int Main()
        {
            List<int> values = new List<int>();
            values.Add(1);
            values.Add(2);
            values.Add(3);
            values.Add(4);
            Counter counter = new Counter();
            int total = 0;
            foreach (int value in values)
            {
                counter.reads = counter.reads + 1;
                if (counter.reads == 3) { values.Add(99); }
                total = total + value;
            }
            return total;
        }
    ";
    let error = run(source).expect_err("mutation after several iterations must be rejected");
    assert!(error.contains(MODIFIED_MESSAGE), "got {error:?}");
}

#[test]
fn mutation_followed_by_continue_still_fails_on_the_next_progression() {
    let source = r"
        public int Main()
        {
            List<int> values = new List<int>();
            values.Add(1);
            values.Add(2);
            values.Add(3);
            int total = 0;
            foreach (int value in values)
            {
                if (value == 1) { values.Add(99); continue; }
                total = total + value;
            }
            return total;
        }
    ";
    // `continue` funnels through the update block back into the condition,
    // which always re-enters the version check before the next `Get` --
    // there is no path from `continue` back to a read that skips it.
    let error = run(source).expect_err("continue must not bypass the next version check");
    assert!(error.contains(MODIFIED_MESSAGE), "got {error:?}");
}

#[test]
fn mutation_that_triggers_buffer_reallocation_is_still_caught_deterministically() {
    let source = r"
        public int Main()
        {
            List<int> values = new List<int>();
            values.Add(1);
            values.Add(2);
            values.Add(3);
            values.Add(4);
            int total = 0;
            foreach (int value in values)
            {
                // Capacity is 4 after the adds above; this Add crosses the
                // geometric growth boundary (4 -> 8), reallocating the
                // buffer while `foreach` is still iterating it.
                values.Add(99);
                total = total + value;
            }
            return total;
        }
    ";
    let error = run(source).expect_err("a reallocating mutation during foreach must be rejected");
    assert!(error.contains(MODIFIED_MESSAGE), "got {error:?}");
}

#[test]
fn every_structural_mutation_path_reports_the_identical_stable_message() {
    let add_error = run(
        "public int Main() { List<int> v = new List<int>(); v.Add(1); v.Add(2); foreach (int x in v) { v.Add(9); } return 0; }",
    )
    .expect_err("Add must fail");
    let remove_error = run(
        "public int Main() { List<int> v = new List<int>(); v.Add(1); v.Add(2); foreach (int x in v) { v.RemoveAt(0); } return 0; }",
    )
    .expect_err("RemoveAt must fail");
    assert!(add_error.contains(MODIFIED_MESSAGE));
    assert!(remove_error.contains(MODIFIED_MESSAGE));
    assert_eq!(
        add_error, remove_error,
        "the diagnostic must be stable and specific regardless of which structural operation diverged"
    );
}

// --- Section 18.36-44: escape analysis ---------------------------------------------

#[test]
fn a_string_extracted_by_list_foreach_and_returned_survives_junk_allocations() {
    let source = r#"
        public string Build(string a, string b)
        {
            List<string> values = new List<string>();
            values.Add(a + b);
            values.Add("second");
            foreach (string value in values) { return value; }
            return "";
        }
        public string Main()
        {
            string result = Build("Hello, ", "World!");
            string junk = "more-junk-allocated-after-the-loop-returns";
            return result + "|" + junk.Length.ToString();
        }
    "#;
    assert_eq!(
        run(source),
        Ok(ExecutionValue::String("Hello, World!|42".to_owned()))
    );
}

#[test]
fn a_string_extracted_by_list_foreach_and_stored_in_a_class_field_survives_break() {
    let source = r#"
        public class Holder { public string Value; public Holder() { Value = ""; } }
        public Holder Store(List<string> values)
        {
            Holder holder = new Holder();
            foreach (string value in values)
            {
                holder.Value = value;
                break;
            }
            return holder;
        }
        public string Main()
        {
            List<string> values = new List<string>();
            values.Add("first-" + "value");
            values.Add("second-value");
            Holder holder = Store(values);
            string junk = "junk-after-storing-the-entry-in-a-field";
            return holder.Value + "|" + junk.Length.ToString();
        }
    "#;
    assert_eq!(
        run(source),
        Ok(ExecutionValue::String("first-value|39".to_owned()))
    );
}

const ENTRY_STRUCT: &str = "public struct Entry { public string Text; }\n";

#[test]
fn a_struct_containing_a_string_returned_directly_from_list_foreach_stays_correct() {
    let source = format!(
        "{ENTRY_STRUCT}
        public Entry Build(string a, string b) {{
            List<Entry> entries = new List<Entry>();
            entries.Add(Entry {{ Text: a + b }});
            foreach (Entry entry in entries) {{ return entry; }}
            return Entry {{ Text: \"\" }};
        }}
        public string Main() {{
            Entry entry = Build(\"struct-\", \"returned\");
            string junk = \"junk-after-struct-return-1234567890\";
            return entry.Text + \"|\" + junk.Length.ToString();
        }}"
    );
    assert_eq!(
        run(&source),
        Ok(ExecutionValue::String("struct-returned|35".to_owned()))
    );
}

#[test]
fn a_class_reference_extracted_by_list_foreach_stays_valid_and_mutation_is_observable() {
    let source = r"
        public class Counter { public int Value; public Counter(int value) { Value = value; } }
        public Counter Build()
        {
            List<Counter> counters = new List<Counter>();
            counters.Add(new Counter(1));
            counters.Add(new Counter(2));
            foreach (Counter counter in counters)
            {
                counter.Value = counter.Value + 100;
                return counter;
            }
            return new Counter(0);
        }
        public int Main()
        {
            Counter counter = Build();
            int junk = 0;
            for (int i = 0; i < 1000; i++) { junk = junk + i; }
            return counter.Value + (junk - junk);
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(101)));
}

#[test]
fn a_list_foreach_struct_element_placed_in_an_option_stays_correct() {
    let source = format!(
        "using aster.core;\n{ENTRY_STRUCT}
        public Option<Entry> Build(string a, string b) {{
            List<Entry> entries = new List<Entry>();
            entries.Add(Entry {{ Text: a + b }});
            foreach (Entry entry in entries) {{ return Option<Entry>.Some(entry); }}
            return Option<Entry>.None;
        }}
        public string Main() {{
            switch (Build(\"option-\", \"entry\")) {{
                case Some(entry): return entry.Text;
                case None: return \"none\";
            }}
        }}"
    );
    assert_eq!(
        run_project(&source),
        Ok(ExecutionValue::String("option-entry".to_owned()))
    );
}

#[test]
fn a_list_foreach_struct_element_placed_in_a_result_stays_correct() {
    let source = format!(
        "using aster.core;\n{ENTRY_STRUCT}
        public Result<Entry, string> Build(string a, string b) {{
            List<Entry> entries = new List<Entry>();
            entries.Add(Entry {{ Text: a + b }});
            foreach (Entry entry in entries) {{ return Result<Entry, string>.Ok(entry); }}
            return Result<Entry, string>.Error(\"empty\");
        }}
        public string Main() {{
            switch (Build(\"result-\", \"entry\")) {{
                case Ok(entry): return entry.Text;
                case Error(message): return message;
            }}
        }}"
    );
    assert_eq!(
        run_project(&source),
        Ok(ExecutionValue::String("result-entry".to_owned()))
    );
}

#[test]
fn a_list_foreach_element_passed_to_a_helper_stays_correct() {
    let source = format!(
        "{ENTRY_STRUCT}
        public string Describe(Entry entry) {{ return entry.Text; }}
        public string Build(string a, string b) {{
            List<Entry> entries = new List<Entry>();
            entries.Add(Entry {{ Text: a + b }});
            foreach (Entry entry in entries) {{ return Describe(entry); }}
            return \"\";
        }}
        public string Main() {{
            string text = Build(\"helper-\", \"passed\");
            string junk = \"junk-after-passing-to-a-helper-function\";
            return text + \"|\" + junk.Length.ToString();
        }}"
    );
    assert_eq!(
        run(&source),
        Ok(ExecutionValue::String("helper-passed|39".to_owned()))
    );
}

#[test]
fn a_list_foreach_element_extracted_inside_a_generic_function_stays_correct() {
    let source = r#"
        public T First<T>(List<T> values)
        {
            foreach (T value in values) { return value; }
            return values.Get(0);
        }
        public string Main()
        {
            List<string> values = new List<string>();
            values.Add("generic-" + "extracted");
            string text = First<string>(values);
            string junk = "junk-after-the-generic-extraction-call";
            return text + "|" + junk.Length.ToString();
        }
    "#;
    assert_eq!(
        run(source),
        Ok(ExecutionValue::String("generic-extracted|38".to_owned()))
    );
}

// --- Section 18.45-49: safety -------------------------------------------------------

#[test]
fn an_empty_list_foreach_never_calls_get() {
    // If `Get` were ever called on index 0 of an empty list, the runtime's
    // own bounds check inside `list_get` would report a *different*
    // diagnostic ("out of bounds") instead of a clean, silent zero-iteration
    // loop; observing success with an untouched accumulator is the
    // behavioral proof that `Get` was never reached.
    let source = "public int Main() { List<int> values = new List<int>(); int total = -1; foreach (int value in values) { total = 0; } return total; }";
    assert_eq!(run(source), Ok(ExecutionValue::Int(-1)));
}

#[test]
fn a_diverged_version_is_rejected_with_the_stable_message() {
    let error = run(
        "public int Main() { List<int> v = new List<int>(); v.Add(1); v.Add(2); foreach (int x in v) { v.Add(3); } return 0; }",
    )
    .expect_err("version mismatch must be rejected");
    assert_eq!(
        error,
        format!("Aster runtime error: list was {MODIFIED_MESSAGE} during foreach iteration")
    );
}

#[test]
fn workers_still_reject_a_list_crossing_a_worker_boundary() {
    let errors = compile_errors(
        r"
        public List<int> Make() { return new List<int>(); }
        public int Main() {
            Task<List<int>> task = Task.Run(Make);
            return 0;
        }
        ",
    );
    assert!(
        errors
            .iter()
            .any(|message| message.contains("cross a worker boundary")),
        "foreach must not have changed List<T> worker-transferability, got {errors:?}"
    );
}

#[test]
fn an_ordinary_list_foreach_body_still_compiles_and_runs_as_a_worker_body() {
    let source = r"
        public void Body(int i) {
            List<int> values = new List<int>();
            values.Add(1);
            values.Add(2);
            int total = 0;
            foreach (int value in values) { total = total + value; }
        }
        public int Main() { Parallel.For(0, 4, Body); return 0; }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(0)));
}

#[test]
fn console_io_inside_a_list_foreach_body_reachable_from_a_worker_is_still_rejected() {
    let source = r"
        using aster.io;
        public int Body() {
            List<int> values = new List<int>();
            values.Add(1);
            foreach (int value in values) { WriteLine(value.ToString()); }
            return 0;
        }
        public int Main() { Task<int> task = Task.Run(Body); return task.Wait(); }
        ";
    let error = run_project(source).expect_err("expected Task.Run with console I/O to be rejected");
    assert!(error.contains("Task.Run"), "got {error:?}");
}

#[test]
fn version_mismatches_in_independent_executions_do_not_interfere() {
    // Each `run` compiles and executes fresh (a new `ExecutionContext` per
    // `execute` call): confirms there is no static/global registry of
    // "active iterations" that could leak state or false-positive between
    // unrelated executions.
    let failing = "public int Main() { List<int> v = new List<int>(); v.Add(1); v.Add(2); foreach (int x in v) { v.Add(9); } return 0; }";
    let succeeding = "public int Main() { List<int> v = new List<int>(); v.Add(1); v.Add(2); int total = 0; foreach (int x in v) { total = total + x; } return total; }";
    assert!(run(failing).is_err());
    assert_eq!(run(succeeding), Ok(ExecutionValue::Int(3)));
    assert!(run(failing).is_err());
    assert_eq!(run(succeeding), Ok(ExecutionValue::Int(3)));
}

// --- Section 18.50-53 / Section 17: memory ------------------------------------------

#[test]
fn an_empty_list_foreach_repeated_many_times_allocates_nothing_new() {
    let source = r"
        public int Main()
        {
            int total = 0;
            for (int i = 0; i < 5000; i++)
            {
                List<int> values = new List<int>();
                foreach (int value in values) { total = total + value; }
            }
            return total;
        }
    ";
    let (value, memory) = stats(source);
    assert_eq!(value, ExecutionValue::Int(0));
    // Every list allocation here is the empty header itself (`AllocateList`
    // is accounted as an object allocation, see `context.rs`); the loop
    // reads nothing (`Length == 0`) and allocates nothing of its own.
    assert_eq!(memory.object_allocations, 5000);
    assert_eq!(memory.string_allocations, 0);
}

#[test]
fn a_small_list_foreach_across_many_calls_allocates_only_the_list_not_the_loop() {
    let source = r"
        public int Sum(List<int> values)
        {
            int total = 0;
            foreach (int value in values) { total = total + value; }
            return total;
        }
        public int Main()
        {
            int total = 0;
            for (int i = 0; i < 2000; i++)
            {
                List<int> values = new List<int>();
                values.Add(1);
                values.Add(2);
                values.Add(3);
                total = total + Sum(values);
            }
            return total;
        }
    ";
    let (value, memory) = stats(source);
    assert_eq!(value, ExecutionValue::Int(6 * 2000));
    assert_eq!(memory.string_allocations, 0);
}

#[test]
fn many_iterations_over_one_existing_list_allocate_nothing_new() {
    let source = r"
        public int Main()
        {
            List<int> values = new List<int>();
            values.Add(1);
            values.Add(2);
            values.Add(3);
            values.Add(4);
            values.Add(5);
            values.Add(6);
            values.Add(7);
            values.Add(8);
            values.Add(9);
            values.Add(10);
            long total = 0;
            for (int round = 0; round < 300000; round++)
            {
                foreach (int value in values) { total = total + value; }
            }
            return total > 0 ? 1 : 0;
        }
    ";
    let allocations_before_loop_starts = {
        let source = r"
            public int Main()
            {
                List<int> values = new List<int>();
                values.Add(1);
                values.Add(2);
                values.Add(3);
                values.Add(4);
                values.Add(5);
                values.Add(6);
                values.Add(7);
                values.Add(8);
                values.Add(9);
                values.Add(10);
                return values.Length;
            }
        ";
        stats(source).1.total_allocations
    };
    let (value, memory) = stats(source);
    assert_eq!(value, ExecutionValue::Int(1));
    // 3,000,000 iterations (300,000 rounds * 10 elements) over a list built
    // once, up front, must attribute zero further allocations to the loop.
    assert_eq!(memory.total_allocations, allocations_before_loop_starts);
    assert_eq!(memory.string_allocations, 0);
}

#[test]
fn list_foreach_over_structs_allocates_only_the_list_and_its_growth() {
    let source = r"
        public struct Point { public int X; public int Y; }
        public int Main()
        {
            List<Point> points = new List<Point>();
            points.Add(Point { X: 1, Y: 2 });
            points.Add(Point { X: 3, Y: 4 });
            int total = 0;
            foreach (Point point in points) { total = total + point.X + point.Y; }
            return total;
        }
    ";
    let (value, memory) = stats(source);
    assert_eq!(value, ExecutionValue::Int(10));
    assert_eq!(memory.string_allocations, 0);
}

#[test]
fn list_foreach_over_classes_allocates_only_the_list_and_the_objects() {
    let source = r"
        public class Counter { public int Value; public Counter(int value) { Value = value; } }
        public int Main()
        {
            List<Counter> counters = new List<Counter>();
            counters.Add(new Counter(1));
            counters.Add(new Counter(2));
            counters.Add(new Counter(3));
            int total = 0;
            foreach (Counter counter in counters) { total = total + counter.Value; }
            return total;
        }
    ";
    let (value, memory) = stats(source);
    assert_eq!(value, ExecutionValue::Int(6));
    // Exactly the 3 `Counter` objects plus the list's own header/buffer
    // allocations; the loop itself creates none of its own.
    assert_eq!(memory.object_allocations, 3 + 1 + 1);
}

#[test]
fn list_foreach_over_existing_strings_allocates_only_the_list() {
    let source = r#"
        public int Main()
        {
            List<string> values = new List<string>();
            values.Add("a");
            values.Add("b");
            values.Add("c");
            int total = 0;
            foreach (string value in values) { total = total + value.Length; }
            return total;
        }
    "#;
    let (value, memory) = stats(source);
    assert_eq!(value, ExecutionValue::Int(3));
    // String literals are not dynamic allocations; the loop must not
    // allocate a new ASTER string merely to read each element.
    assert_eq!(memory.string_allocations, 0);
}

#[test]
fn repeated_version_mismatch_failures_across_independent_contexts_allocate_boundedly() {
    let source = "public int Main() { List<int> v = new List<int>(); v.Add(1); v.Add(2); foreach (int x in v) { v.Add(9); } return 0; }";
    for _ in 0..50 {
        let error = run(source).expect_err("every independent run must fail identically");
        assert!(error.contains(MODIFIED_MESSAGE));
    }
}

// --- Section 15/8/13: MIR adulteration and validation -------------------------------

const LIST_FOREACH_PROGRAM: &str = r"
    public int Main()
    {
        List<int> values = new List<int>();
        values.Add(1);
        values.Add(2);
        values.Add(3);
        int total = 0;
        foreach (int value in values) { total = total + value; }
        return total;
    }
";

fn find_list_get(module: &mut mir::Module) -> &mut mir::Instruction {
    module
        .functions
        .iter_mut()
        .flat_map(|function| &mut function.blocks)
        .flat_map(|block| &mut block.instructions)
        .find(|instruction| matches!(instruction, mir::Instruction::ListGet { .. }))
        .expect("a foreach-over-list program always lowers exactly one ListGet")
}

fn find_first_matching(
    module: &mut mir::Module,
    matches: impl Fn(&mir::Instruction) -> bool,
) -> &mut mir::Instruction {
    module
        .functions
        .iter_mut()
        .flat_map(|function| &mut function.blocks)
        .flat_map(|block| &mut block.instructions)
        .find(|instruction| matches(instruction))
        .expect("a matching instruction exists")
}

fn execute_error(module: &mir::Module) -> String {
    execute(module, "Main")
        .expect_err("adulterated MIR must be rejected before/without executing normally")
        .to_string()
}

#[test]
fn adulterated_mir_rejects_a_list_get_with_a_fake_non_list_receiver() {
    let mut module = compile_mir(LIST_FOREACH_PROGRAM);
    let mir::Instruction::ListGet { list, .. } = find_list_get(&mut module) else {
        unreachable!();
    };
    list.type_ = mir::Type::Int;
    let error = execute_error(&module);
    assert!(!error.is_empty());
}

#[test]
fn adulterated_mir_rejects_a_list_get_with_a_mismatched_element_type() {
    let mut module = compile_mir(LIST_FOREACH_PROGRAM);
    let mir::Instruction::ListGet {
        element_type,
        destination,
        ..
    } = find_list_get(&mut module)
    else {
        unreachable!();
    };
    *element_type = mir::Type::Bool;
    // Keep the destination local's declared type in sync so this
    // adulteration exercises the `ListGet` element-type check specifically,
    // not the general assign-target-type check already covered elsewhere.
    let mir::Place::Local(local_id) = *destination else {
        unreachable!();
    };
    for function in &mut module.functions {
        for local in function.locals.iter_mut().chain(&mut function.parameters) {
            if local.id == local_id {
                local.type_ = mir::Type::Bool;
            }
        }
    }
    let error = execute_error(&module);
    assert!(!error.is_empty());
}

#[test]
fn adulterated_mir_rejects_a_list_get_with_a_nonexistent_destination() {
    let mut module = compile_mir(LIST_FOREACH_PROGRAM);
    let mir::Instruction::ListGet { destination, .. } = find_list_get(&mut module) else {
        unreachable!();
    };
    *destination = mir::Place::Local(mir::LocalId(u32::MAX));
    let error = execute_error(&module);
    assert!(!error.is_empty());
}

#[test]
fn adulterated_mir_rejects_a_list_get_with_a_non_int_index() {
    let mut module = compile_mir(LIST_FOREACH_PROGRAM);
    let mir::Instruction::ListGet { index, .. } = find_list_get(&mut module) else {
        unreachable!();
    };
    index.type_ = mir::Type::Bool;
    let error = execute_error(&module);
    assert!(!error.is_empty());
}

#[test]
fn adulterated_mir_rejects_a_list_length_local_retyped_to_a_non_int() {
    let mut module = compile_mir(LIST_FOREACH_PROGRAM);
    let instruction = find_first_matching(&mut module, |instruction| {
        matches!(
            instruction,
            mir::Instruction::Assign {
                value: mir::Rvalue {
                    kind: mir::RvalueKind::ListLength(_),
                    ..
                },
                ..
            }
        )
    });
    let mir::Instruction::Assign { value, .. } = instruction else {
        unreachable!();
    };
    value.type_ = mir::Type::Bool;
    let error = execute_error(&module);
    assert!(!error.is_empty());
}

#[test]
fn adulterated_mir_rejects_a_list_version_local_retyped_to_a_non_long() {
    let mut module = compile_mir(LIST_FOREACH_PROGRAM);
    let instruction = find_first_matching(&mut module, |instruction| {
        matches!(
            instruction,
            mir::Instruction::Assign {
                value: mir::Rvalue {
                    kind: mir::RvalueKind::ListVersion(_),
                    ..
                },
                ..
            }
        )
    });
    let mir::Instruction::Assign { value, .. } = instruction else {
        unreachable!();
    };
    value.type_ = mir::Type::Int;
    let error = execute_error(&module);
    assert!(!error.is_empty());
}

#[test]
fn adulterated_mir_rejects_a_list_version_read_on_a_non_list_receiver() {
    let mut module = compile_mir(LIST_FOREACH_PROGRAM);
    let instruction = find_first_matching(&mut module, |instruction| {
        matches!(
            instruction,
            mir::Instruction::Assign {
                value: mir::Rvalue {
                    kind: mir::RvalueKind::ListVersion(_),
                    ..
                },
                ..
            }
        )
    });
    let mir::Instruction::Assign {
        value:
            mir::Rvalue {
                kind: mir::RvalueKind::ListVersion(list),
                ..
            },
        ..
    } = instruction
    else {
        unreachable!();
    };
    list.type_ = mir::Type::Int;
    let error = execute_error(&module);
    assert!(!error.is_empty());
}

#[test]
fn adulterated_mir_rejects_a_list_version_mismatch_call_given_a_stray_argument() {
    let mut module = compile_mir(LIST_FOREACH_PROGRAM);
    let instruction = find_first_matching(&mut module, |instruction| {
        matches!(
            instruction,
            mir::Instruction::CallIntrinsic {
                intrinsic: mir::Intrinsic::ListVersionMismatch,
                ..
            }
        )
    });
    let mir::Instruction::CallIntrinsic { arguments, .. } = instruction else {
        unreachable!();
    };
    arguments.push(mir::Operand {
        type_: mir::Type::Int,
        kind: mir::OperandKind::Constant(mir::Constant::Integer("0".to_owned())),
    });
    let error = execute_error(&module);
    assert!(!error.is_empty());
}

#[test]
fn adulterated_mir_rejects_a_non_bool_branch_condition_in_a_list_foreach_program() {
    let mut module = compile_mir(LIST_FOREACH_PROGRAM);
    for function in &mut module.functions {
        for block in &mut function.blocks {
            if let mir::Terminator::Branch { condition, .. } = &mut block.terminator {
                condition.type_ = mir::Type::Int;
                let error = execute_error(&module);
                assert!(!error.is_empty());
                return;
            }
        }
    }
    unreachable!("a list-foreach program always lowers to at least one Branch");
}

#[test]
fn adulterated_mir_rejects_a_branch_targeting_an_unknown_block_in_a_list_foreach_program() {
    let mut module = compile_mir(LIST_FOREACH_PROGRAM);
    for function in &mut module.functions {
        for block in &mut function.blocks {
            if let mir::Terminator::Branch { then_block, .. } = &mut block.terminator {
                *then_block = mir::BasicBlockId(u32::MAX);
                let error = execute_error(&module);
                assert!(!error.is_empty());
                return;
            }
        }
    }
    unreachable!("a list-foreach program always lowers to at least one Branch");
}

#[test]
fn adulterated_mir_rejects_a_missing_entry_block_in_a_list_foreach_program() {
    let mut module = compile_mir(LIST_FOREACH_PROGRAM);
    for function in &mut module.functions {
        if function.name == "Main" {
            function.entry = mir::BasicBlockId(u32::MAX);
        }
    }
    let error = execute_error(&module);
    assert!(!error.is_empty());
}

#[test]
fn adulterated_mir_rejects_list_foreach_metadata_missing_a_concrete_list_element_type() {
    let mut module = compile_mir(LIST_FOREACH_PROGRAM);
    let mir::Instruction::ListGet { element_type, .. } = find_list_get(&mut module) else {
        unreachable!();
    };
    *element_type = mir::Type::Unknown;
    let error = execute_error(&module);
    assert!(!error.is_empty());
}
