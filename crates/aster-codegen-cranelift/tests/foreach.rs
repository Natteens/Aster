//! M3B: `foreach` over arrays. AST/HIR carry a dedicated `ForEach` node;
//! MIR lowering expands it into ordinary indexed CFG (`Place::Index` reads
//! against a captured collection/length, ordinary `Branch`/`Goto`) before
//! codegen ever sees it -- there is no iterator, enumerator, or runtime
//! support of any kind. Only arrays are iterable; `List<T>`/`string` remain
//! explicitly rejected (`array_pipeline.rs`/parser tests already cover the
//! parse/type-check surface).

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

/// Like `run`, but through the full project pipeline instead of the
/// single-file `compile()` convenience API. `Option<T>`/`Result<T, E>` need
/// their real generic template declarations linked from `aster.core`, which
/// single-file `compile()` does not do (see `option_try_propagation.rs`'s
/// and `string_try_parse.rs`'s established convention for the same reason).
fn run_project(source: &str) -> Result<ExecutionValue, String> {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "aster-foreach-project-{}-{id}.aster",
        std::process::id()
    ));
    std::fs::write(&path, source).expect("write temporary project");
    let compilation = compile_project(&path).map_err(|diagnostics| format!("{diagnostics:#?}"));
    std::fs::remove_file(&path).ok();
    execute(&compilation?.compilation.mir, "Main").map_err(|error| error.to_string())
}

#[test]
fn foreach_over_arrays_preserves_order_and_control_flow() {
    let source = r"
        public int Main()
        {
            int[] values = [1, 2, 3, 4];
            int total = 0;
            foreach (int value in values)
            {
                if (value == 2) { continue; }
                if (value == 4) { break; }
                total += value;
            }
            return total;
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(4)));
}

#[test]
fn foreach_evaluates_collection_once_and_captures_it() {
    let source = r"
        public int[] First() { return [1, 2]; }
        public int[] Other() { return [100]; }
        public int Main()
        {
            int[] current = First();
            int total = 0;
            foreach (int value in current)
            {
                current = Other();
                total += value;
            }
            return total;
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(3)));
}

#[test]
fn foreach_copies_structs_but_keeps_class_references_observable() {
    let source = r"
        public struct Point { public int Value; }
        public class Player { public int Health; public Player(int health) { Health = health; } }
        public int Main()
        {
            Point[] points = [Point { Value: 1 }];
            Player[] players = [new Player(1)];
            foreach (Player player in players) { player.Health = 42; }
            foreach (Point point in points) { }
            return points[0].Value + players[0].Health;
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(43)));
}

#[test]
fn empty_array_does_not_execute_the_body() {
    assert_eq!(
        run(
            "public int Main() { int[] values = new int[0]; int total = 0; foreach (int value in values) { total += value; } return total; }"
        ),
        Ok(ExecutionValue::Int(0))
    );
}

#[test]
fn foreach_uses_the_concrete_element_type_after_monomorphization() {
    let source = r"
        public T First<T>(T[] values)
        {
            foreach (T value in values) { return value; }
            return values[0];
        }
        public int Main() { return First<int>([42]); }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(42)));
}

// --- Section 1: escape analysis and lifetime ------------------------------------

#[test]
fn a_string_built_locally_extracted_by_foreach_and_returned_survives_junk_allocations() {
    // The array element is a dynamically concatenated string (a temporary-
    // candidate allocation), never a parameter -- the only way a `Place::
    // Index`-read alias gap could actually corrupt something. Junk
    // allocations after the loop returns would reuse a reclaimed temporary
    // arena if the extracted string's region were ever chosen wrong.
    let source = r#"
        public string Build(string a, string b)
        {
            string[] values = [a + b, "second"];
            foreach (string value in values)
            {
                return value;
            }
            return "";
        }
        public string Main()
        {
            string result = Build("Hello, ", "World!");
            string junk = ("filler-one-" + "filler-two-") + ("filler-three-" + "filler-four-");
            return result + "|" + junk.Length.ToString();
        }
    "#;
    assert_eq!(
        run(source),
        Ok(ExecutionValue::String("Hello, World!|47".to_owned()))
    );
}

#[test]
fn a_string_extracted_by_foreach_and_stored_in_a_class_field_survives_break() {
    let source = r#"
        public class Holder { public string Value; public Holder() { Value = ""; } }
        public Holder Store(string[] values)
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
            string[] values = ["first-" + "value", "second-value"];
            Holder holder = Store(values);
            string junk = "more-junk-allocated-after-the-loop-returns";
            return holder.Value + "|" + junk.Length.ToString();
        }
    "#;
    assert_eq!(
        run(source),
        Ok(ExecutionValue::String("first-value|42".to_owned()))
    );
}

const ENTRY_STRUCT: &str = "public struct Entry { public string Text; }\n";

#[test]
fn a_struct_containing_a_string_returned_directly_from_foreach_stays_correct() {
    let source = format!(
        "{ENTRY_STRUCT}
        public Entry Build(string a, string b) {{
            Entry[] entries = [Entry {{ Text: a + b }}];
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
fn the_string_field_extracted_from_a_foreach_struct_element_stays_correct() {
    let source = format!(
        "{ENTRY_STRUCT}
        public string Build(string a, string b) {{
            Entry[] entries = [Entry {{ Text: a + b }}];
            foreach (Entry entry in entries) {{ return entry.Text; }}
            return \"\";
        }}
        public string Main() {{
            string text = Build(\"field-\", \"extracted\");
            string junk = \"more-junk-text-after-field-extraction\";
            return text + \"|\" + junk.Length.ToString();
        }}"
    );
    assert_eq!(
        run(&source),
        Ok(ExecutionValue::String("field-extracted|37".to_owned()))
    );
}

#[test]
fn a_foreach_struct_element_stored_in_a_class_field_stays_correct() {
    let source = format!(
        "{ENTRY_STRUCT}
        public class Holder {{ public Entry Stored; public Holder() {{ Stored = Entry {{ Text: \"\" }}; }} }}
        public Holder Build(string a, string b) {{
            Entry[] entries = [Entry {{ Text: a + b }}];
            Holder holder = new Holder();
            foreach (Entry entry in entries) {{ holder.Stored = entry; break; }}
            return holder;
        }}
        public string Main() {{
            Holder holder = Build(\"stored-\", \"entry\");
            string junk = \"junk-after-storing-the-entry-in-a-field\";
            return holder.Stored.Text + \"|\" + junk.Length.ToString();
        }}"
    );
    assert_eq!(
        run(&source),
        Ok(ExecutionValue::String("stored-entry|39".to_owned()))
    );
}

#[test]
fn a_foreach_struct_elements_string_field_stored_in_a_class_field_stays_correct() {
    let source = format!(
        "{ENTRY_STRUCT}
        public class Holder {{ public string Value; public Holder() {{ Value = \"\"; }} }}
        public Holder Build(string a, string b) {{
            Entry[] entries = [Entry {{ Text: a + b }}];
            Holder holder = new Holder();
            foreach (Entry entry in entries) {{ holder.Value = entry.Text; break; }}
            return holder;
        }}
        public string Main() {{
            Holder holder = Build(\"field-stored-\", \"text\");
            string junk = \"junk-after-storing-the-field-text\";
            return holder.Value + \"|\" + junk.Length.ToString();
        }}"
    );
    assert_eq!(
        run(&source),
        Ok(ExecutionValue::String("field-stored-text|33".to_owned()))
    );
}

#[test]
fn a_foreach_struct_element_passed_to_a_helper_stays_correct() {
    let source = format!(
        "{ENTRY_STRUCT}
        public string Describe(Entry entry) {{ return entry.Text; }}
        public string Build(string a, string b) {{
            Entry[] entries = [Entry {{ Text: a + b }}];
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
fn a_foreach_struct_element_placed_in_an_option_stays_correct() {
    let source = format!(
        "using aster.core;\n{ENTRY_STRUCT}
        public Option<Entry> Build(string a, string b) {{
            Entry[] entries = [Entry {{ Text: a + b }}];
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
fn a_foreach_struct_element_placed_in_a_result_stays_correct() {
    let source = format!(
        "using aster.core;\n{ENTRY_STRUCT}
        public Result<Entry, string> Build(string a, string b) {{
            Entry[] entries = [Entry {{ Text: a + b }}];
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
fn a_class_reference_extracted_by_foreach_and_returned_stays_valid_and_mutation_is_observable() {
    let source = r"
        public class Counter { public int Value; public Counter(int value) { Value = value; } }
        public Counter Build()
        {
            Counter[] counters = [new Counter(1), new Counter(2)];
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
fn reassigning_a_class_typed_foreach_variable_is_rejected() {
    let errors = compile_errors(
        r"
        public class Player { public int Health; }
        public Player GetOther() { return new Player(); }
        public int Main()
        {
            Player[] players = [new Player()];
            foreach (Player player in players)
            {
                player = GetOther();
            }
            return 0;
        }
        ",
    );
    assert!(
        errors.iter().any(|message| message.contains("read-only")),
        "got {errors:?}"
    );
}

// --- Section 2: postfix `?` ------------------------------------------------------

fn convert_source(threshold: i32) -> String {
    format!(
        "using aster.core;
        public Result<int, string> Convert(int value) {{
            if (value >= {threshold}) {{ return Result<int, string>.Error(\"bad\"); }}
            return Result<int, string>.Ok(value);
        }}
        public class Counter {{ public int calls; }}
        public Result<int, string> Process(int[] values, Counter counter) {{
            int total = 0;
            foreach (int value in values)
            {{
                counter.calls = counter.calls + 1;
                int parsed = Convert(value)?;
                total = total + parsed;
            }}
            return Result<int, string>.Ok(total);
        }}
        public int Main() {{
            Counter counter = new Counter();
            switch (Process([1, 2, 3], counter)) {{
                case Ok(total): return total * 1000 + counter.calls;
                case Error(message): return -1000 - counter.calls;
            }}
        }}"
    )
}

#[test]
fn postfix_try_inside_foreach_succeeds_for_every_iteration() {
    assert_eq!(
        run_project(&convert_source(1000)),
        Ok(ExecutionValue::Int(6 * 1000 + 3))
    );
}

#[test]
fn postfix_try_inside_foreach_fails_on_the_first_iteration() {
    assert_eq!(
        run_project(&convert_source(1)),
        Ok(ExecutionValue::Int(-1000 - 1))
    );
}

#[test]
fn postfix_try_inside_foreach_fails_after_some_iterations() {
    assert_eq!(
        run_project(&convert_source(3)),
        Ok(ExecutionValue::Int(-1000 - 3))
    );
}

#[test]
fn postfix_try_operand_inside_foreach_is_evaluated_exactly_once() {
    let source = r#"
        using aster.core;
        public class Counter { public int calls; }
        public Result<int, string> Fail(Counter counter) {
            counter.calls = counter.calls + 1;
            return Result<int, string>.Error("stop");
        }
        public Result<int, string> Run(int[] values, Counter counter) {
            foreach (int value in values)
            {
                int parsed = Fail(counter)?;
            }
            return Result<int, string>.Ok(0);
        }
        public int Main() {
            Counter counter = new Counter();
            Run([1, 2, 3], counter);
            return counter.calls;
        }
    "#;
    assert_eq!(run_project(source), Ok(ExecutionValue::Int(1)));
}

#[test]
fn postfix_try_inside_foreach_never_reaches_the_statement_after_it_on_error() {
    let source = r#"
        using aster.core;
        public Result<int, string> Fail() { return Result<int, string>.Error("stop"); }
        public Result<int, string> Run(int[] values) {
            int reached = 0;
            foreach (int value in values)
            {
                int parsed = Fail()?;
                reached = 999;
            }
            return Result<int, string>.Ok(reached);
        }
        public int Main() {
            switch (Run([1, 2, 3])) {
                case Ok(reached): return reached;
                case Error(message): return -1;
            }
        }
    "#;
    assert_eq!(run_project(source), Ok(ExecutionValue::Int(-1)));
}

// --- Section 3: control flow -----------------------------------------------------

#[test]
fn continue_advances_the_index_exactly_once() {
    let source = r"
        public int Main()
        {
            int[] values = [1, 2, 3, 4, 5];
            int visits = 0;
            foreach (int value in values)
            {
                if (value % 2 == 0) { continue; }
                visits = visits + 1;
            }
            return visits;
        }
    ";
    // Odd values are 1, 3, 5 -- exactly 3 visits; a `continue` that skipped
    // the index update (looping on the same element forever) would hang
    // this test instead of returning.
    assert_eq!(run(source), Ok(ExecutionValue::Int(3)));
}

#[test]
fn multiple_continues_in_the_same_iteration_space_all_advance_correctly() {
    let source = r"
        public int Main()
        {
            int[] values = [1, 2, 3, 4, 5, 6, 7, 8];
            int total = 0;
            foreach (int value in values)
            {
                if (value % 2 == 0) { continue; }
                if (value % 3 == 0) { continue; }
                total = total + value;
            }
            return total;
        }
    ";
    // Odd and not multiples of 3: 1, 5, 7 -> 13.
    assert_eq!(run(source), Ok(ExecutionValue::Int(13)));
}

#[test]
fn break_does_not_run_the_update_block_afterward() {
    let source = r"
        public int Main()
        {
            int[] values = [10, 20, 30];
            int lastSeen = -1;
            foreach (int value in values)
            {
                lastSeen = value;
                if (value == 20) { break; }
            }
            return lastSeen;
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(20)));
}

#[test]
fn return_inside_foreach_ends_the_function_immediately() {
    let source = r"
        public int Main()
        {
            int[] values = [1, 2, 3];
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
fn nested_foreach_loops_iterate_independently() {
    let source = r"
        public int Main()
        {
            int[] outer = [1, 2];
            int[] inner = [10, 20];
            int total = 0;
            foreach (int a in outer)
            {
                foreach (int b in inner)
                {
                    total = total + a * b;
                }
            }
            return total;
        }
    ";
    // (1*10 + 1*20) + (2*10 + 2*20) = 30 + 60 = 90.
    assert_eq!(run(source), Ok(ExecutionValue::Int(90)));
}

#[test]
fn foreach_nested_inside_while() {
    let source = r"
        public int Main()
        {
            int[] values = [1, 2, 3];
            int rounds = 0;
            int total = 0;
            while (rounds < 2)
            {
                foreach (int value in values) { total = total + value; }
                rounds = rounds + 1;
            }
            return total;
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(12)));
}

#[test]
fn while_nested_inside_foreach() {
    let source = r"
        public int Main()
        {
            int[] values = [3, 2];
            int total = 0;
            foreach (int value in values)
            {
                int countdown = value;
                while (countdown > 0)
                {
                    total = total + 1;
                    countdown = countdown - 1;
                }
            }
            return total;
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(5)));
}

#[test]
fn for_nested_inside_foreach() {
    let source = r"
        public int Main()
        {
            int[] values = [2, 3];
            int total = 0;
            foreach (int value in values)
            {
                for (int i = 0; i < value; i++) { total = total + 1; }
            }
            return total;
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(5)));
}

#[test]
fn foreach_nested_inside_for() {
    let source = r"
        public int Main()
        {
            int[] values = [1, 2];
            int total = 0;
            for (int i = 0; i < 2; i++)
            {
                foreach (int value in values) { total = total + value; }
            }
            return total;
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(6)));
}

#[test]
fn break_targets_only_the_innermost_loop() {
    let source = r"
        public int Main()
        {
            int[] outer = [1, 2];
            int[] inner = [10, 20, 30];
            int total = 0;
            foreach (int a in outer)
            {
                foreach (int b in inner)
                {
                    if (b == 20) { break; }
                    total = total + b;
                }
                total = total + a;
            }
            return total;
        }
    ";
    // Inner breaks after adding 10 each outer pass: (10) + 1 + (10) + 2 = 23.
    assert_eq!(run(source), Ok(ExecutionValue::Int(23)));
}

#[test]
fn continue_targets_only_the_innermost_loop() {
    let source = r"
        public int Main()
        {
            int[] outer = [1, 2];
            int[] inner = [1, 2, 3];
            int total = 0;
            foreach (int a in outer)
            {
                foreach (int b in inner)
                {
                    if (b == 2) { continue; }
                    total = total + b;
                }
                total = total + 100;
            }
            return total;
        }
    ";
    // Inner sums 1+3=4 per outer pass, outer itself always adds 100: 4+100+4+100=208.
    assert_eq!(run(source), Ok(ExecutionValue::Int(208)));
}

// --- Section 4: evaluation and capture -------------------------------------------

#[test]
fn the_collection_expression_is_evaluated_exactly_once() {
    let source = r"
        public class Counter { public int calls; }
        public int[] Provide(Counter counter) { counter.calls = counter.calls + 1; return [1, 2, 3]; }
        public int Main()
        {
            Counter counter = new Counter();
            int total = 0;
            foreach (int value in Provide(counter)) { total = total + value; }
            return total * 1000 + counter.calls;
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(6000 + 1)));
}

#[test]
fn length_is_captured_once_even_if_the_array_binding_is_reassigned() {
    let source = r"
        public int Main()
        {
            int[] values = [1, 2, 3];
            int[] longer = [1, 2, 3, 4, 5];
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
    // captured collection/length: exactly 3 visits, not 5.
    assert_eq!(run(source), Ok(ExecutionValue::Int(3)));
}

#[test]
fn mutating_array_elements_during_the_loop_is_observed_only_on_future_iterations() {
    let source = r"
        public int Main()
        {
            int[] values = [1, 2, 3];
            int total = 0;
            int index = 0;
            foreach (int value in values)
            {
                if (index == 0) { values[1] = 999; }
                total = total + value;
                index = index + 1;
            }
            return total;
        }
    ";
    // Iteration 0 reads 1 (before the mutation), iteration 1 reads the
    // freshly mutated 999, iteration 2 reads 3: 1 + 999 + 3 = 1003.
    assert_eq!(run(source), Ok(ExecutionValue::Int(1003)));
}

#[test]
fn the_current_element_is_a_value_already_loaded_not_a_live_view() {
    let source = r"
        public int Main()
        {
            int[] values = [1, 2, 3];
            int total = 0;
            foreach (int value in values)
            {
                values[0] = 555;
                total = total + value;
            }
            return total;
        }
    ";
    // The first iteration's `value` was already loaded as 1 before the
    // in-body mutation of `values[0]`; it must not retroactively read 555.
    assert_eq!(run(source), Ok(ExecutionValue::Int(1 + 2 + 3)));
}

// --- Section 5: readonly ----------------------------------------------------------

#[test]
fn assigning_to_a_scalar_foreach_variable_is_rejected() {
    let errors = compile_errors(
        "public int Main() { int[] values = [1]; foreach (int value in values) { value = 10; } return 0; }",
    );
    assert!(errors.iter().any(|message| message.contains("read-only")));
}

#[test]
fn assigning_to_a_struct_foreach_variables_field_is_rejected() {
    let errors = compile_errors(
        "public struct Point { public int X; }\n\
         public int Main() { Point[] points = [Point { X: 1 }]; foreach (Point point in points) { point.X = 10; } return 0; }",
    );
    assert!(errors.iter().any(|message| message.contains("read-only")));
}

#[test]
fn assigning_through_a_nested_struct_member_of_a_foreach_variable_is_rejected() {
    let errors = compile_errors(
        "public struct Inner { public int X; }\n\
         public struct Outer { public Inner Inner; }\n\
         public int Main() {\n\
             Outer[] values = [Outer { Inner: Inner { X: 1 } }];\n\
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
fn assigning_to_a_class_foreach_variables_field_still_works() {
    let source = r"
        public class Player { public int Health; }
        public int Main()
        {
            Player[] players = [new Player()];
            foreach (Player player in players) { player.Health = 10; }
            return players[0].Health;
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(10)));
}

#[test]
fn reassigning_a_scalar_foreach_variable_binding_is_rejected() {
    let errors = compile_errors(
        "public int Main() { int[] values = [1, 2]; foreach (int value in values) { value = value + 1; } return 0; }",
    );
    assert!(errors.iter().any(|message| message.contains("read-only")));
}

// --- Section 6: scope and integration ---------------------------------------------

#[test]
fn the_element_variable_does_not_exist_after_the_loop() {
    let errors = compile_errors(
        "public int Main() { int[] values = [1]; foreach (int value in values) { } return value; }",
    );
    assert!(!errors.is_empty(), "expected an unknown-name diagnostic");
}

#[test]
fn the_collection_expression_cannot_reference_the_element_variable() {
    let errors = compile_errors(
        "public int Main() { int[] values = [1]; foreach (int value in value) { } return 0; }",
    );
    assert!(
        !errors.is_empty(),
        "the element must not be visible in its own collection expression"
    );
}

#[test]
fn shadowing_an_outer_variable_with_the_same_name_follows_existing_rules() {
    let source = r"
        public int Main()
        {
            int value = 100;
            int[] values = [1, 2, 3];
            int total = 0;
            foreach (int value in values) { total = total + value; }
            return total + value;
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(6 + 100)));
}

#[test]
fn declaring_a_colliding_name_in_the_same_body_scope_is_rejected() {
    let errors = compile_errors(
        "public int Main() {\n\
             int[] values = [1];\n\
             foreach (int value in values) {\n\
                 int value2 = value;\n\
                 int value2 = value;\n\
             }\n\
             return 0;\n\
         }",
    );
    assert!(
        !errors.is_empty(),
        "expected a duplicate-declaration diagnostic"
    );
}

#[test]
fn foreach_body_as_a_single_statement_works() {
    assert_eq!(
        run(
            "public int Main() { int[] values = [1, 2, 3]; int total = 0; foreach (int value in values) total = total + value; return total; }"
        ),
        Ok(ExecutionValue::Int(6))
    );
}

#[test]
fn foreach_works_inside_a_declared_namespace() {
    let source = r"
        namespace app;
        public int Main()
        {
            int[] values = [1, 2, 3];
            int total = 0;
            foreach (int value in values) { total = total + value; }
            return total;
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(6)));
}

#[test]
fn foreach_works_across_a_multifile_project() {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "aster-foreach-multifile-{}-{id}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("create project root");
    std::fs::write(
        root.join("Aster.toml"),
        "[package]\nname = \"foreach_test\"\n",
    )
    .expect("write manifest");
    let app_dir = root.join("app");
    std::fs::create_dir_all(&app_dir).expect("create app dir");
    std::fs::write(
        app_dir.join("main.aster"),
        "namespace app;\n\
         public int Main() { return Helpers.Sum([1, 2, 3, 4]); }",
    )
    .expect("write main.aster");
    std::fs::write(
        app_dir.join("helpers.aster"),
        "namespace app;\n\
         public class Helpers {\n\
             public static int Sum(int[] values) {\n\
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
        .expect("multifile project using foreach should compile")
        .compilation
        .mir;
    assert_eq!(
        execute(&module, "foreach_test::app::Main"),
        Ok(ExecutionValue::Int(10))
    );
}

#[test]
fn foreach_over_an_array_returned_by_a_property_or_function() {
    let source = r"
        public class Holder
        {
            private int[] backing;
            public Holder() { backing = [4, 5, 6]; }
            public int[] Values { get { return backing; } }
        }
        public int Main()
        {
            Holder holder = new Holder();
            int total = 0;
            foreach (int value in holder.Values) { total = total + value; }
            return total;
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(15)));
}

#[test]
fn foreach_over_an_array_stored_in_a_field() {
    let source = r"
        public class Holder { public int[] Values; public Holder() { Values = [7, 8, 9]; } }
        public int Main()
        {
            Holder holder = new Holder();
            int total = 0;
            foreach (int value in holder.Values) { total = total + value; }
            return total;
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(24)));
}

// --- Section 7: workers ------------------------------------------------------------

#[test]
fn an_array_result_type_used_across_a_worker_boundary_is_still_rejected() {
    let errors = compile_errors(
        r"
        public int[] Make() { return [1, 2, 3]; }
        public int Main() {
            Task<int[]> task = Task.Run(Make);
            return 0;
        }
        ",
    );
    assert!(
        errors
            .iter()
            .any(|message| message.contains("cross a worker boundary")),
        "foreach must not have changed array worker-transferability, got {errors:?}"
    );
}

#[test]
fn console_io_inside_a_foreach_body_reachable_from_a_worker_is_still_rejected() {
    let source = r"
        using aster.io;
        public int Body() {
            int[] values = [1, 2, 3];
            foreach (int value in values) { WriteLine(value.ToString()); }
            return 0;
        }
        public int Main() { Task<int> task = Task.Run(Body); return task.Wait(); }
        ";
    let error = run_project(source).expect_err("expected Task.Run with console I/O to be rejected");
    assert!(error.contains("Task.Run"), "got {error:?}");
}

#[test]
fn an_ordinary_foreach_body_still_compiles_and_runs_as_a_worker_body() {
    let source = r"
        public void Body(int i) {
            int[] values = [1, 2, 3];
            int total = 0;
            foreach (int value in values) { total = total + value; }
        }
        public int Main() { Parallel.For(0, 4, Body); return 0; }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(0)));
}

#[test]
fn foreach_does_not_change_ordinary_parallel_for_each_semantics() {
    let source = r"
        public void Body(int value) { }
        public int Main()
        {
            int[] values = [1, 2, 3, 4];
            Parallel.ForEach(values, Body);
            int total = 0;
            foreach (int value in values) { total = total + value; }
            return total;
        }
    ";
    // A normal `foreach` elsewhere in the same function must not perturb
    // `Parallel.ForEach`'s own resolution or validation.
    assert_eq!(run(source), Ok(ExecutionValue::Int(10)));
}

// --- Section 8: MIR and validation -------------------------------------------------

const FOREACH_PROGRAM: &str = r"
    public int Main()
    {
        int[] values = [1, 2, 3];
        int total = 0;
        foreach (int value in values) { total = total + value; }
        return total;
    }
";

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

fn find_index_place(module: &mut mir::Module) -> &mut mir::Place {
    let instruction = find_first_matching(module, |instruction| {
        matches!(
            instruction,
            mir::Instruction::Assign {
                value: mir::Rvalue {
                    kind: mir::RvalueKind::Use(mir::Operand {
                        kind: mir::OperandKind::Copy(mir::Place::Index { .. }),
                        ..
                    }),
                    ..
                },
                ..
            }
        )
    });
    let mir::Instruction::Assign {
        value:
            mir::Rvalue {
                kind: mir::RvalueKind::Use(operand),
                ..
            },
        ..
    } = instruction
    else {
        unreachable!();
    };
    let mir::Operand {
        kind: mir::OperandKind::Copy(place),
        ..
    } = operand
    else {
        unreachable!();
    };
    place
}

fn execute_error(module: &mir::Module) -> String {
    execute(module, "Main")
        .expect_err("adulterated MIR must be rejected before/without executing normally")
        .to_string()
}

#[test]
fn adulterated_mir_rejects_an_index_place_retargeted_to_an_unknown_local() {
    let mut module = compile_mir(FOREACH_PROGRAM);
    let mir::Place::Index { array, .. } = find_index_place(&mut module) else {
        unreachable!();
    };
    array.kind = mir::OperandKind::Copy(mir::Place::Local(mir::LocalId(u32::MAX)));
    let error = execute_error(&module);
    assert!(!error.is_empty());
}

#[test]
fn adulterated_mir_rejects_a_length_local_retyped_to_a_non_int() {
    let mut module = compile_mir(FOREACH_PROGRAM);
    let instruction = find_first_matching(&mut module, |instruction| {
        matches!(
            instruction,
            mir::Instruction::Assign {
                value: mir::Rvalue {
                    kind: mir::RvalueKind::ArrayLength(_),
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
fn adulterated_mir_rejects_an_index_operand_retyped_to_a_non_int() {
    let mut module = compile_mir(FOREACH_PROGRAM);
    let mir::Place::Index { index, .. } = find_index_place(&mut module) else {
        unreachable!();
    };
    index.type_ = mir::Type::Bool;
    let error = execute_error(&module);
    assert!(!error.is_empty());
}

#[test]
fn adulterated_mir_rejects_an_element_destination_with_an_incompatible_type() {
    let mut module = compile_mir(FOREACH_PROGRAM);
    let instruction = find_first_matching(&mut module, |instruction| {
        matches!(
            instruction,
            mir::Instruction::Assign {
                value: mir::Rvalue {
                    kind: mir::RvalueKind::Use(mir::Operand {
                        kind: mir::OperandKind::Copy(mir::Place::Index { .. }),
                        ..
                    }),
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
fn adulterated_mir_rejects_an_index_place_with_a_mismatched_element_type() {
    let mut module = compile_mir(FOREACH_PROGRAM);
    let mir::Place::Index { element_type, .. } = find_index_place(&mut module) else {
        unreachable!();
    };
    *element_type = mir::Type::Bool;
    let error = execute_error(&module);
    assert!(!error.is_empty());
}

#[test]
fn adulterated_mir_rejects_a_non_bool_branch_condition() {
    let mut module = compile_mir(FOREACH_PROGRAM);
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
    unreachable!("a foreach program always lowers to at least one Branch");
}

#[test]
fn adulterated_mir_rejects_a_branch_targeting_an_unknown_block() {
    let mut module = compile_mir(FOREACH_PROGRAM);
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
    unreachable!("a foreach program always lowers to at least one Branch");
}

#[test]
fn adulterated_mir_rejects_a_goto_targeting_an_unknown_update_block() {
    let mut module = compile_mir(FOREACH_PROGRAM);
    let mut retargeted = false;
    for function in &mut module.functions {
        for block in &mut function.blocks {
            if let mir::Terminator::Goto(target) = &mut block.terminator
                && !retargeted
            {
                // The first `Goto` reached in block order is the body's own
                // jump into the update block; redirect it to a nonexistent
                // block id instead of leaving it a legitimate self-loop.
                *target = mir::BasicBlockId(u32::MAX);
                retargeted = true;
            }
        }
    }
    assert!(
        retargeted,
        "a foreach program always lowers to at least one Goto"
    );
    let error = execute_error(&module);
    assert!(!error.is_empty());
}

#[test]
fn adulterated_mir_rejects_a_duplicated_block_id() {
    let mut module = compile_mir(FOREACH_PROGRAM);
    for function in &mut module.functions {
        if function.blocks.len() >= 2 {
            let duplicate_id = function.blocks[0].id;
            function.blocks[1].id = duplicate_id;
            let error = execute_error(&module);
            assert!(!error.is_empty());
            return;
        }
    }
    unreachable!("a foreach program always lowers to at least two blocks");
}

#[test]
fn adulterated_mir_rejects_a_missing_entry_block() {
    let mut module = compile_mir(FOREACH_PROGRAM);
    for function in &mut module.functions {
        if function.name == "Main" {
            function.entry = mir::BasicBlockId(u32::MAX);
        }
    }
    let error = execute_error(&module);
    assert!(!error.is_empty());
}

#[test]
fn foreach_lowering_never_reuses_a_local_id_or_symbol_within_one_function() {
    // `Local.symbol`/`Local.id` uniqueness within one function is a lowering
    // invariant (`mir_lowering/symbols.rs`'s collision-avoidance scan, plus
    // codegen keying stack slots by `LocalId`), not something the general
    // MIR validator polices today -- so this is a positive regression on
    // legitimate `lower_foreach` output rather than an adversarial-rejection
    // test: two foreach loops over the same source array, in the same
    // function, must still get their own, non-colliding element locals.
    let module = compile_mir(
        r"
        public int Main()
        {
            int[] values = [1, 2, 3];
            int total = 0;
            foreach (int value in values) { total = total + value; }
            foreach (int value in values) { total = total + value; }
            return total;
        }
        ",
    );
    for function in &module.functions {
        let mut ids = std::collections::HashSet::new();
        let mut symbols = std::collections::HashSet::new();
        for local in function.parameters.iter().chain(&function.locals) {
            assert!(
                ids.insert(local.id),
                "duplicate LocalId in `{}`",
                function.name
            );
            if let Some(symbol) = local.symbol {
                assert!(
                    symbols.insert(symbol),
                    "duplicate local symbol in `{}`",
                    function.name
                );
            }
        }
    }
}

// --- Section 9: memory and stress ---------------------------------------------------

#[test]
fn an_empty_array_foreach_repeated_many_times_allocates_nothing_new() {
    let source = r"
        public int Main()
        {
            int total = 0;
            for (int i = 0; i < 5000; i++)
            {
                int[] values = new int[0];
                foreach (int value in values) { total = total + value; }
            }
            return total;
        }
    ";
    let (value, memory) = stats(source);
    assert_eq!(value, ExecutionValue::Int(0));
    assert_eq!(memory.object_allocations, 0);
    assert_eq!(memory.string_allocations, 0);
}

#[test]
fn a_small_array_foreach_across_many_calls_allocates_only_the_arrays_not_the_loop() {
    let source = r"
        public int Sum(int[] values)
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
                total = total + Sum([1, 2, 3]);
            }
            return total;
        }
    ";
    let (value, memory) = stats(source);
    assert_eq!(value, ExecutionValue::Int(6 * 2000));
    // Every allocation this program performs is the array literal itself
    // (one `array_allocations` per call); the loop body contributes no
    // object/string allocations of its own.
    assert_eq!(memory.array_allocations, 2000);
    assert_eq!(memory.object_allocations, 0);
    assert_eq!(memory.string_allocations, 0);
}

#[test]
fn millions_of_iterations_over_one_existing_array_allocate_nothing_new() {
    let source = r"
        public int Main()
        {
            int[] values = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
            long total = 0;
            for (int round = 0; round < 300000; round++)
            {
                foreach (int value in values) { total = total + value; }
            }
            return total > 0 ? 1 : 0;
        }
    ";
    // The array is allocated exactly once, up front; 3,000,000 iterations
    // over it (300,000 rounds * 10 elements) must attribute zero further
    // ASTER allocations to the loop itself.
    let (value, memory) = stats(source);
    assert_eq!(value, ExecutionValue::Int(1));
    assert_eq!(memory.array_allocations, 1);
    assert_eq!(memory.object_allocations, 0);
    assert_eq!(memory.string_allocations, 0);
}

#[test]
fn foreach_over_an_array_of_structs_allocates_only_the_array() {
    let source = r"
        public struct Point { public int X; public int Y; }
        public int Main()
        {
            Point[] points = [Point { X: 1, Y: 2 }, Point { X: 3, Y: 4 }];
            int total = 0;
            foreach (Point point in points) { total = total + point.X + point.Y; }
            return total;
        }
    ";
    let (value, memory) = stats(source);
    assert_eq!(value, ExecutionValue::Int(10));
    assert_eq!(memory.array_allocations, 1);
    assert_eq!(memory.object_allocations, 0);
}

#[test]
fn foreach_over_an_array_of_classes_allocates_only_the_array_and_the_objects() {
    let source = r"
        public class Counter { public int Value; public Counter(int value) { Value = value; } }
        public int Main()
        {
            Counter[] counters = [new Counter(1), new Counter(2), new Counter(3)];
            int total = 0;
            foreach (Counter counter in counters) { total = total + counter.Value; }
            return total;
        }
    ";
    let (value, memory) = stats(source);
    assert_eq!(value, ExecutionValue::Int(6));
    assert_eq!(memory.array_allocations, 1);
    // Exactly the 3 `Counter` objects constructed to populate the array;
    // the loop itself creates none of its own.
    assert_eq!(memory.object_allocations, 3);
}

#[test]
fn foreach_over_an_array_of_existing_strings_allocates_only_the_array() {
    let source = r#"
        public int Main()
        {
            string[] values = ["a", "b", "c"];
            int total = 0;
            foreach (string value in values) { total = total + value.Length; }
            return total;
        }
    "#;
    let (value, memory) = stats(source);
    assert_eq!(value, ExecutionValue::Int(3));
    assert_eq!(memory.array_allocations, 1);
    // String literals are not dynamic allocations; the loop must not
    // allocate a new ASTER string merely to read each element.
    assert_eq!(memory.string_allocations, 0);
}
