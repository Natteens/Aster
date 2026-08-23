use aster_codegen_cranelift::{ExecutionValue, execute, execute_with_console, execute_with_stats};
use aster_compiler::{compile, compile_project};
use aster_runtime::MemoryConsoleBackend;
use std::sync::atomic::{AtomicU64, Ordering};

fn run(source: &str) -> Result<ExecutionValue, String> {
    let compilation = compile(source).map_err(|diagnostics| format!("{diagnostics:#?}"))?;
    execute(&compilation.mir, "Run").map_err(|error| error.to_string())
}

fn run_project(source: &str) -> Result<ExecutionValue, String> {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "aster-language-ergonomics-{}-{id}.aster",
        std::process::id()
    ));
    std::fs::write(&path, source).expect("write temporary project");
    let compilation = compile_project(&path)
        .map_err(|diagnostics| format!("{diagnostics:#?}"))
        .map(|project| project.compilation);
    std::fs::remove_file(&path).ok();
    execute(&compilation?.mir, "Run").map_err(|error| error.to_string())
}

fn run_project_with_console(source: &str) -> (Result<ExecutionValue, String>, Vec<u8>) {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "aster-language-ergonomics-console-{}-{id}.aster",
        std::process::id()
    ));
    std::fs::write(&path, source).expect("write temporary project");
    let compilation = compile_project(&path)
        .map_err(|diagnostics| format!("{diagnostics:#?}"))
        .map(|project| project.compilation);
    std::fs::remove_file(&path).ok();
    let backend = MemoryConsoleBackend::default();
    let output = backend.clone();
    let result = compilation.and_then(|compilation| {
        execute_with_console(&compilation.mir, "Run", Box::new(backend))
            .map_err(|error| error.to_string())
    });
    (result, output.output())
}

fn messages(source: &str) -> Vec<String> {
    compile(source)
        .expect_err("source should be rejected")
        .into_iter()
        .map(|diagnostic| diagnostic.message)
        .collect()
}

fn compiles(source: &str) {
    compile(source).unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"));
}

#[test]
fn expression_bodies_contextual_values_named_arguments_and_defaults_compose() {
    let source = r"
        public enum Choice { Empty, Present }
        public class Box {
            private int value;
            public Box(int value = 4) { this.value = value; }
            public int Read() => value;
        }
        public class Recorder {
            private int order;
            public Recorder() { order = 0; }
            public int Mark(int value) { order = order * 10 + value; return value; }
            public int Order() => order;
        }
        public const int DefaultSecond = 9;
        public int Pair(int first, int second = DefaultSecond) => first * 10 + second;
        public int[] Empty() => [];
        public int[] Select(Choice choice) => choice switch { Empty => [], Present => [1], };
        public Box Make() => new(value: 7);
        public int Run() {
            int[] values = [];
            values = true ? [] : [1];
            int[][] nested = [[], [1]];
            Recorder recorder = new();
            int result = Pair(second: recorder.Mark(2), first: recorder.Mark(1));
            const int DefaultSecond = 1;
            int omitted = Pair(3);
            Box box = new();
            return result * 1000 + recorder.Order() * 10 + omitted + box.Read() + Make().Read()
                + Empty().Length + nested[0].Length;
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(12_260)));
}

#[test]
fn generic_method_and_interface_calls_share_named_and_default_binding() {
    let source = r#"
        public interface ICombine { public int Combine(int left, int right = 2); }
        public class Combiner : ICombine {
            public Combiner() {}
            public int Combine(int a, int b) => a * 10 + b;
            public T Echo<T>(T value) => value;
            public int Extra<T>(T ignored, int amount = 4) => amount;
        }
        public T Identity<T>(T value) => value;
        public int Run() {
            Combiner concrete = new();
            ICombine combined = concrete;
            int a = combined.Combine(left: 3);
            int b = concrete.Echo<int>(value: 5);
            int c = concrete.Extra<string>(ignored: "x");
            return a * 100 + b * 10 + c + Identity<int>(value: 6);
        }
    "#;
    assert_eq!(run(source), Ok(ExecutionValue::Int(3_260)));
}

#[test]
fn foreach_var_and_list_indexing_execute_with_single_index_evaluation() {
    let source = r"
        public class Counter {
            private int calls;
            public Counter() { calls = 0; }
            public int Index() { calls++; return 0; }
            public int Calls() => calls;
        }
        public int Run() {
            List<int> values = new();
            Counter counter = new();
            values.Add(10);
            int first = values[counter.Index()];
            values[counter.Index()] += 5;
            int old = values[counter.Index()]++;
            int current = ++values[counter.Index()];
            int sum = 0;
            foreach (var value in values) { sum += value; }
            return first * 10000 + old * 100 + current + sum + counter.Calls() * 1000000;
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(4_101_534)));
}

#[test]
fn contextual_and_call_binding_ambiguities_are_rejected() {
    for (source, expected) in [
        (
            "public int Run() { var value = []; return 0; }",
            "cannot infer",
        ),
        (
            "public int Run() { var value = new(); return 0; }",
            "target-typed",
        ),
        (
            "public int Use(int[] value) => 1; public int Use(string[] value) => 2; public int Run() { return Use([]); }",
            "ambiguous",
        ),
        (
            "public int F(int first = 1, int required) => 0; public int Run() => 0;",
            "cannot follow an optional",
        ),
        (
            "public T Bad<T>(T value = 0) => value; public int Run() => 0;",
            "cannot be proven valid for every specialization",
        ),
        (
            "public int F(int first, int second) => 0; public int Run() { return F(first: 1, 2); }",
            "positional arguments cannot follow named",
        ),
    ] {
        let diagnostics = messages(source);
        assert!(
            diagnostics.iter().any(|message| message.contains(expected)),
            "missing `{expected}` in {diagnostics:#?}"
        );
    }
}

#[test]
fn expression_bodies_cover_struct_static_generic_recursive_void_and_test_callables() {
    let source = r"
        public struct Point {
            public int x;
            public int y;
            public int Sum() => x + y;
        }
        public static class Utility {
            public static int Twice(int value) => value * 2;
            public static T Echo<T>(T value) => value;
        }
        public class Box<T> {
            private T value;
            public Box(T value) { this.value = value; }
            public T Read() => value;
        }
        public int Factorial(int value) => value <= 1 ? 1 : value * Factorial(value - 1);
        public bool Even(int value) => value == 0 ? true : Odd(value - 1);
        public bool Odd(int value) => value == 0 ? false : Even(value - 1);
        public void AddOne(List<int> values) => values.Add(1);
        public int One() => 1;
        test void ConciseTest() => One();
        public int Run() {
            Point point = Point { x: 2, y: 3 };
            Box<int> box = new Box<int>(4);
            List<int> values = new();
            AddOne(values);
            return point.Sum() + Utility.Twice(3) + Utility.Echo<int>(box.Read())
                + Factorial(4) + (Even(8) ? 1 : 0) + values[0];
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(41)));
}

#[test]
fn expression_bodies_use_the_existing_recursion_guard() {
    let source = r"
        public int Recursive(int value) => value == 0 ? 0 : Recursive(value - 1) + 1;
        public int Run() => Recursive(1025);
    ";
    let error = run(source).expect_err("expression-bodied recursion must remain bounded");
    assert!(
        error.contains("call depth exceeds the supported limit of 1024"),
        "unexpected recursion error: {error}"
    );
}

#[test]
fn contextual_arrays_and_target_new_flow_through_all_exact_targets() {
    let source = r#"
        public enum Mode { Empty, Filled }
        public struct Groups { public string[][] values; }
        public class Holder {
            public string[] names;
            public List<int> numbers;
            public Holder() { this.names = []; this.numbers = new(); }
        }
        public int Count(string[] values) => values.Length;
        public int CountLists(List<int> values) => values.Length;
        public Holder CreateHolder() => new();
        public int Run() {
            string[] assigned = ["x"];
            assigned = [];
            Groups groups = Groups { values: [[], ["a", "b"], []] };
            Holder holder = new();
            holder.names = [];
            holder.numbers = new();
            List<int> fromCall = true ? new() : new List<int>();
            List<int> fromSwitch = Mode.Empty switch {
                Empty => new(),
                Filled => new List<int>(),
            };
            Dictionary<string, int> counts = new();
            counts.Add("a", 1);
            return Count([]) + CountLists(new()) + assigned.Length + groups.values.Length
                + groups.values[1].Length + holder.names.Length + holder.numbers.Length
                + fromCall.Length + fromSwitch.Length + counts.Length + CreateHolder().names.Length;
        }
    "#;
    assert_eq!(run(source), Ok(ExecutionValue::Int(6)));
}

#[test]
fn field_initializers_receive_the_same_bounded_context_as_locals() {
    let source = r"
        public class Holder {
            public string[] names = [];
            public List<int> numbers = new();
            public Holder() {}
        }
        public int Run() {
            Holder holder = new();
            return holder.names.Length * 10 + holder.numbers.Length;
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(0)));
}

#[test]
fn overload_probing_checks_typed_siblings_inside_contextual_expressions() {
    let source = r#"
        public int Select(int[][] values) => 1;
        public int Select(string[][] values) => 2;
        public int Run() => Select([[], ["ASTER"]]);
    "#;
    assert_eq!(run(source), Ok(ExecutionValue::Int(2)));
}

#[test]
fn contextual_overload_resolution_is_speculative_and_declaration_order_independent() {
    for declarations in [
        "public int Use(int[] values) => 1; public int Use(string value) => 2;",
        "public int Use(string value) => 2; public int Use(int[] values) => 1;",
    ] {
        let source = format!("{declarations} public int Run() => Use([]);");
        assert_eq!(run(&source), Ok(ExecutionValue::Int(1)));
    }

    for declarations in [
        "public int Use(int[] values) => 1; public int Use(string[] values) => 2;",
        "public int Use(string[] values) => 2; public int Use(int[] values) => 1;",
    ] {
        let source = format!("{declarations} public int Run() => Use([]);");
        let diagnostics = messages(&source);
        assert_eq!(
            diagnostics
                .iter()
                .filter(|message| message.contains("ambiguous"))
                .count(),
            1,
            "unexpected diagnostics: {diagnostics:#?}"
        );
        assert!(
            diagnostics
                .iter()
                .all(|message| !message.contains("cannot infer")),
            "candidate-local diagnostics leaked: {diagnostics:#?}"
        );
    }

    for declarations in [
        "public int Use(Player value) => 3; public int Use(int value) => 4;",
        "public int Use(int value) => 4; public int Use(Player value) => 3;",
    ] {
        let source = format!(
            "public class Player {{ public Player() {{}} }} {declarations} public int Run() => Use(new());"
        );
        assert_eq!(run(&source), Ok(ExecutionValue::Int(3)));
    }

    for declarations in [
        "public int Use(Player value) => 1; public int Use(Enemy value) => 2;",
        "public int Use(Enemy value) => 2; public int Use(Player value) => 1;",
    ] {
        let source = format!(
            "public class Player {{ public Player() {{}} }} public class Enemy {{ public Enemy() {{}} }} {declarations} public int Run() => Use(new());"
        );
        let diagnostics = messages(&source);
        assert!(
            diagnostics
                .iter()
                .any(|message| message.contains("ambiguous")),
            "unexpected diagnostics: {diagnostics:#?}"
        );
    }

    let generic = r"
        public class Box<T> { public Box() {} }
        public static class Picks {
            public static int Pick(int[] values, int bonus = 1) => bonus;
            public static int Pick<T>(Box<T> value, int bonus = 2) => bonus;
        }
        public int Run() {
            return Picks.Pick(values: []) * 10 + Picks.Pick<int>(value: new());
        }
    ";
    assert_eq!(run(generic), Ok(ExecutionValue::Int(12)));

    let diagnostics = messages(
        "public interface I {} public int Use(I value) => 1; public int Run() => Use(new());",
    );
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("target-typed")),
        "non-constructible contextual target should be rejected: {diagnostics:#?}"
    );
    assert_eq!(
        diagnostics.len(),
        1,
        "unexpected diagnostic cascade: {diagnostics:#?}"
    );
}

#[test]
fn contextual_arrays_propagate_through_deep_arrays_conditionals_and_switches() {
    let source = r#"
        public enum Mode { First, Second }
        public string[][] Select(bool condition) => condition ? [] : [];
        public string[][] Choose(Mode mode) => mode switch { First => [], Second => [], };
        public int Run() {
            string[][] a = [[], []];
            string[][][] b = [[[], ["x"]], []];
            return a.Length * 100 + b.Length * 10 + b[0].Length
                + Select(true).Length + Choose(Mode.First).Length;
        }
    "#;
    assert_eq!(run(source), Ok(ExecutionValue::Int(222)));
}

#[test]
fn named_arguments_preserve_receiver_and_source_expression_order_exactly_once() {
    let source = r"
        public class Recorder {
            private int order;
            public Recorder() { order = 0; }
            public Recorder Receiver() { order = order * 10 + 9; return this; }
            public int Mark(int value) { order = order * 10 + value; return value; }
            public int Combine(int first, int second) => first * 10 + second;
            public int Order() => order;
        }
        public int Pair(int first, int second) => first * 10 + second;
        public int Run() {
            Recorder recorder = new();
            int combined = recorder.Receiver().Combine(
                second: recorder.Mark(2), first: recorder.Mark(1));
            int value = 0;
            int incremented = Pair(second: value++, first: ++value);
            return recorder.Order() * 10000 + combined * 100 + incremented * 10 + value;
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(9_211_402)));
}

#[test]
fn enum_named_payloads_preserve_source_order_and_declared_layout() {
    let source = r"
        public enum Message { Move(int x, int y) }
        public class Recorder {
            private int order;
            public Recorder() { order = 0; }
            public int Mark(int value) { order = order * 10 + value; return value; }
            public int Order() => order;
        }
        public int Run() {
            Recorder recorder = new();
            Message message = Message.Move(y: recorder.Mark(2), x: recorder.Mark(1));
            int payload = message switch { Move(x, y) => x * 10 + y, };
            return recorder.Order() * 100 + payload;
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(2_112)));

    for (source, expected) in [
        (
            "public enum Message { Move(int x, int y) } public int Run() { Message value = Message.Move(z: 1, y: 2); return 0; }",
            "unknown enum payload argument `z`",
        ),
        (
            "public enum Message { Move(int x, int y) } public int Run() { Message value = Message.Move(x: 1, x: 2); return 0; }",
            "enum payload `x` is supplied more than once",
        ),
    ] {
        let diagnostics = messages(source);
        assert!(
            diagnostics.iter().any(|message| message.contains(expected)),
            "missing `{expected}` in {diagnostics:#?}"
        );
    }
}

#[test]
fn defaults_skip_parameters_without_affecting_specialization_identity() {
    let source = r"
        public int F(int a, int b = 2, int c = 3) => a * 100 + b * 10 + c;
        public int Generic<T>(T ignored, int amount = 4) => amount;
        public int Run() {
            return F(1, c: 10) * 100
                + Generic<int>(1) * 10
                + Generic<int>(1, 4);
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(13_044)));
}

#[test]
fn expression_body_result_propagation_and_generic_foreach_reuse_existing_lowering() {
    let source = r#"
        using aster.core;
        public Result<int, string> Forward(Result<int, string> input)
            => Result<int, string>.Ok(input? + 1);
        public int Count<T>(List<T> values) {
            int count = 0;
            foreach (var ignored in values) { count++; }
            return count;
        }
        public int Read(Result<int, string> value) {
            return Forward(value) switch { Ok(number) => number, Error(error) => -1, };
        }
        public int Run() {
            List<string> values = new();
            values.Add("a");
            values.Add("b");
            return Read(Result<int, string>.Ok(6)) * 10 + Count<string>(values);
        }
    "#;
    assert_eq!(run_project(source), Ok(ExecutionValue::Int(72)));
}

#[test]
fn sixty_four_named_optional_parameters_compile_and_execute_deterministically() {
    let parameters = (1..=64)
        .map(|index| format!("int p{index:02} = {index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sum = (1..=64)
        .map(|index| format!("p{index:02}"))
        .collect::<Vec<_>>()
        .join(" + ");
    let supplied = (33..=64)
        .rev()
        .map(|index| format!("p{index:02}: {index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let source =
        format!("public int Sum({parameters}) => {sum}; public int Run() => Sum({supplied});");
    assert_eq!(run(&source), Ok(ExecutionValue::Int(2_080)));
}

#[test]
fn foreach_var_infers_array_list_string_and_dictionary_snapshot_elements() {
    let source = r#"
        public int Run() {
            int total = 0;
            int[] array = [1, 2, 3];
            foreach (var value in array) { total += value; }
            List<int> list = new();
            list.Add(4);
            list.Add(5);
            foreach (var value in list) { total += value; }
            string text = "ab";
            foreach (var scalar in text) { if (scalar == 'b') { total += 10; } }
            Dictionary<string, int> values = new();
            values.Add("a", 1);
            values.Add("b", 2);
            foreach (var key in values.Keys()) { total += key.Length; }
            foreach (var value in values.Values()) { total += value; }
            return total;
        }
    "#;
    assert_eq!(run(source), Ok(ExecutionValue::Int(30)));
}

#[test]
fn list_indexing_preserves_reference_and_value_semantics_and_failure_paths() {
    let source = r"
        public struct Pair { public int value; }
        public class Box { public int value; public Box(int value) { this.value = value; } }
        public int Run() {
            List<Pair> pairs = new();
            pairs.Add(Pair { value: 3 });
            Pair copy = pairs[0];
            copy.value = 9;
            pairs[0] = Pair { value: 4 };
            List<Box> boxes = new();
            Box box = new(5);
            boxes.Add(box);
            boxes[0].value = 7;
            return copy.value * 100 + pairs[0].value * 10 + box.value;
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(947)));

    for source in [
        "public int Run() { List<int> values = new(); return values[0]; }",
        "public int Run() { List<int> values = new(); values.Add(1); values[1] = 2; return 0; }",
    ] {
        let error = run(source).expect_err("out-of-range List indexing is a controlled failure");
        assert!(error.contains("index"), "unexpected runtime error: {error}");
    }
}

#[test]
fn list_index_places_evaluate_receiver_index_get_rhs_and_set_once() {
    let source = r"
        public class Probe {
            private List<int> values;
            private int receiverCalls;
            private int indexCalls;
            private int valueCalls;
            public Probe() {
                values = new();
                values.Add(10);
                receiverCalls = 0;
                indexCalls = 0;
                valueCalls = 0;
            }
            public List<int> Values() { receiverCalls++; return values; }
            public int Index() { indexCalls++; return 0; }
            public int Value() { valueCalls++; return 5; }
            public int Result() {
                return values[0] * 1000 + receiverCalls * 100 + indexCalls * 10 + valueCalls;
            }
        }
        public int Run() {
            Probe probe = new();
            probe.Values()[probe.Index()] += probe.Value();
            return probe.Result();
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(15_111)));
}

#[test]
fn failed_compound_list_get_stops_before_rhs_set_and_later_source_work() {
    let source = r#"
        using aster.io;
        public int SideEffect() { WriteLine("RHS"); return 1; }
        public int Run() {
            List<int> values = new();
            values[0] += SideEffect();
            WriteLine("LATER");
            return 0;
        }
    "#;
    let (result, output) = run_project_with_console(source);
    let error = result.expect_err("out-of-range compound indexing must fail");
    assert!(error.contains("index"), "unexpected runtime error: {error}");
    assert_eq!(output, b"");
}

#[test]
fn list_indexing_preserves_alias_identity() {
    let source = r"
        public int Run() {
            List<int> first = new();
            first.Add(1);
            List<int> second = first;
            second[0] = 7;
            return first[0] * 10 + second[0];
        }
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(77)));
}

#[test]
fn named_arguments_and_defaults_cover_callable_families_and_constant_kinds() {
    let source = r#"
        public const int Base = 2;
        public interface IScale { public int Apply(int value, int amount = 3); }
        public struct Scale {
            public int Apply(int value, int amount = Base + 2) => value * amount;
        }
        public class Calculator : IScale {
            private int seed;
            public Calculator(int seed = Base > 0 ? 5 : 1) { this.seed = seed; }
            public int Apply(int input, int factor) => input * factor + seed;
            public static int Static(int left, int right = (int)4L) => left * 10 + right;
            public T Echo<T>(T value, int ignored = 0) => value;
        }
        public int Choose<T>(T value, int amount = 6) => amount;
        public int GenericConst<T>(int value = Base + 3) => value;
        public int Named(int value, string text) => 1;
        public int Named(string text, int count) => 2;
        public string Label(string value = "ok") => value;
        public bool Enabled(bool value = true) => value;
        public char Marker(char value = 'z') => value;
        public int Run() {
            Calculator calculator = new(seed: 7);
            IScale contract = calculator;
            Scale scale = Scale {};
            int inferred = Choose(value: "x");
            return calculator.Apply(factor: 2, input: 3) * 100000
                + contract.Apply(value: 2) * 10000
                + Calculator.Static(left: 3) * 100
                + scale.Apply(value: 2)
                + inferred + Named(count: 1, text: "x")
                + calculator.Echo<int>(ignored: 9, value: 1)
                + GenericConst<string>()
                + (Label().Length == 2 && Enabled() && Marker() == 'z' ? 1 : 0);
        }
    "#;
    assert_eq!(run(source), Ok(ExecutionValue::Int(1_433_423)));
}

#[test]
fn many_named_and_default_arguments_have_no_artificial_low_limit() {
    let source = r"
        public int Sum(
            int p01 = 1, int p02 = 2, int p03 = 3, int p04 = 4,
            int p05 = 5, int p06 = 6, int p07 = 7, int p08 = 8,
            int p09 = 9, int p10 = 10, int p11 = 11, int p12 = 12,
            int p13 = 13, int p14 = 14, int p15 = 15, int p16 = 16,
            int p17 = 17, int p18 = 18, int p19 = 19, int p20 = 20,
            int p21 = 21, int p22 = 22, int p23 = 23, int p24 = 24,
            int p25 = 25, int p26 = 26, int p27 = 27, int p28 = 28,
            int p29 = 29, int p30 = 30, int p31 = 31, int p32 = 32)
            => p01 + p02 + p03 + p04 + p05 + p06 + p07 + p08
             + p09 + p10 + p11 + p12 + p13 + p14 + p15 + p16
             + p17 + p18 + p19 + p20 + p21 + p22 + p23 + p24
             + p25 + p26 + p27 + p28 + p29 + p30 + p31 + p32;
        public int Run() => Sum(
            p32: 32, p31: 31, p30: 30, p29: 29, p28: 28, p27: 27,
            p26: 26, p25: 25, p24: 24, p23: 23, p22: 22, p21: 21,
            p20: 20, p19: 19, p18: 18, p17: 17);
    ";
    assert_eq!(run(source), Ok(ExecutionValue::Int(528)));
}

#[test]
fn ergonomic_and_explicit_forms_have_identical_allocation_regions_and_counts() {
    let source = r"
        public List<int> ExplicitList() { return new List<int>(); }
        public List<int> ErgonomicList() => new();
        public int Explicit() {
            List<int> values = ExplicitList();
            Dictionary<int, int> counts = new Dictionary<int, int>();
            int[] empty = new int[0];
            values.Add(4);
            values.Set(0, values.Get(0) + 1);
            counts.Add(1, 1);
            return values.Get(0) + counts.Length + empty.Length;
        }
        public int Ergonomic() {
            List<int> values = ErgonomicList();
            Dictionary<int, int> counts = new();
            int[] empty = [];
            values.Add(4);
            values[0] += 1;
            counts.Add(1, 1);
            return values[0] + counts.Length + empty.Length;
        }
    ";
    let module = compile(source).expect("equivalent explicit and ergonomic forms compile");
    let explicit = execute_with_stats(&module.mir, "Explicit").expect("explicit form runs");
    let ergonomic = execute_with_stats(&module.mir, "Ergonomic").expect("ergonomic form runs");
    assert_eq!(explicit.0, ExecutionValue::Int(6));
    assert_eq!(ergonomic.0, explicit.0);
    assert_eq!(ergonomic.1, explicit.1);
}

#[test]
fn the_negative_ergonomics_matrix_is_controlled_and_specific() {
    for (source, expected) in [
        (
            "public int F() => true; public int Run() => 0;",
            "expected `int`, found `bool`",
        ),
        (
            "public interface I { public int F() => 1; } public int Run() => 0;",
            "expected `;`",
        ),
        (
            "public unsafe foreign int Native() => 1; public int Run() => 0;",
            "expected `;`",
        ),
        ("public int Run() { [] ; return 0; }", "cannot infer"),
        (
            "public int Run() { int value = []; return value; }",
            "cannot infer",
        ),
        (
            "public int Run() { int value = new(); return value; }",
            "target-typed",
        ),
        (
            "public int A(List<int> value) => 1; public int A(Dictionary<int, int> value) => 2; public int Run() => A(new());",
            "ambiguous",
        ),
        (
            "public int Run() { foreach (var value in 1) { } return 0; }",
            "not iterable",
        ),
        (
            "public int Run() { List<int> values = new(); return values[true]; }",
            "index must have type `int`",
        ),
        (
            "public int Run() { List<int> values = new(); return values[-1]; }",
            "list index cannot be negative",
        ),
        (
            "public int Run() { List<int> values = new(); values.Add(1); values[0] = false; return 0; }",
            "expected `int`, found `bool`",
        ),
        (
            "public int F(int value) => value; public int Run() => F(missing: 1);",
            "unknown named argument",
        ),
        (
            "public int F(int value) => value; public int Run() => F(value: 1, value: 2);",
            "supplied more than once",
        ),
        (
            "public int F(int first, int second) => 0; public int Run() => F(1, first: 2);",
            "supplied more than once",
        ),
        (
            "public int F(int first, int second) => 0; public int Run() => F(first: 1);",
            "missing required argument",
        ),
        (
            "public int F(int value) => value; public int Run() => F(1, 2);",
            "too many arguments",
        ),
        (
            "public int F(int value = Next()) => value; public int Next() => 1; public int Run() => 0;",
            "compile-time constant",
        ),
        (
            "public int F(int value = \"x\") => value; public int Run() => 0;",
            "expected `int`, found `string`",
        ),
        (
            "public int Bad<T>(int value = Next()) => value; public int Next() => 1; public int Run() => 0;",
            "compile-time constant",
        ),
        (
            "public int Bad<T>(int value = \"x\") => value; public int Run() => 0;",
            "expected `int`, found `string`",
        ),
        (
            "public int F(int value) => 1; public int F(int value, int other = 2) => 2; public int Run() => F(1);",
            "ambiguous",
        ),
        (
            "public T Make<T>(int value = 1) => value; public int Run() => Make();",
            "infer",
        ),
    ] {
        let diagnostics = messages(source);
        assert!(
            diagnostics.iter().any(|message| message.contains(expected)),
            "missing `{expected}` in {diagnostics:#?} for {source}"
        );
    }
}

#[test]
fn malformed_ergonomic_syntax_reports_focused_errors_without_panicking() {
    for (source, expected) in [
        (
            "public int F() => ; public int Run() => 0;",
            "expected expression",
        ),
        ("public int F() => 1 public int Run() => 0;", "expected `;`"),
        (
            "public class C { public C() {} } public int Run() { C value = new(; return 0; }",
            "expected expression",
        ),
        (
            "public int F(int value) => value; public int Run() => F(value:);",
            "expected expression",
        ),
        (
            "public int F(int value = ) => value; public int Run() => 0;",
            "expected expression",
        ),
        (
            "public int Run() { int[] values = []; foreach (var in values) {} return 0; }",
            "expected identifier",
        ),
        (
            "public int Run() { List<int> values = new(); return values[0; }",
            "expected `]`",
        ),
    ] {
        let diagnostics = messages(source);
        assert!(
            diagnostics.iter().any(|message| message.contains(expected)),
            "missing `{expected}` in {diagnostics:#?} for {source}"
        );
        assert!(
            diagnostics.len() <= 4,
            "unexpected diagnostic cascade for {source}: {diagnostics:#?}"
        );
    }
}

#[test]
fn declaration_only_callable_defaults_remain_supported_but_expression_bodies_do_not() {
    compiles(
        "public interface I { public int F(int value = 2); } public class C : I { public C() {} public int F(int input) => input; } public int Run() { I value = new C(); return value.F(); }",
    );
}
