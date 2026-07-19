use aster_codegen_cranelift::{ExecutionValue, execute, execute_symbol};
use aster_compiler::{compile, compile_project};
use aster_mir as mir;

fn run(source: &str, function: &str) -> Result<ExecutionValue, String> {
    let compilation = compile(source).map_err(|diagnostics| format!("{diagnostics:#?}"))?;
    execute(&compilation.mir, function).map_err(|error| error.to_string())
}

fn run_project(source: &str, function: &str) -> Result<ExecutionValue, String> {
    use std::time::{SystemTime, UNIX_EPOCH};

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("aster-stdlib-jit-{nonce}.aster"));
    std::fs::write(&path, source).expect("write temporary Aster project");
    let compilation = compile_project(&path).map_err(|diagnostics| format!("{diagnostics:#?}"));
    std::fs::remove_file(&path).expect("remove temporary Aster project");
    let compilation = compilation?;
    execute(&compilation.compilation.mir, function).map_err(|error| error.to_string())
}

#[test]
fn executes_enum_payload_switch_and_equality() {
    let source = r"
        public enum Message { Empty, Number(int value), }
        public int Read(Message value) {
            switch (value) {
                case Empty: return 0;
                case Number(number): return number;
            }
        }
        public int Run() {
            Message left = Message.Number(42);
            Message right = Message.Number(42);
            if (left == right) { return Read(left); }
            return 0;
        }
    ";
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn executes_enums_nested_in_struct_arrays_and_classes() {
    let source = r"
        public enum Value { None, Some(int value), }
        public struct Holder { public Value value; }
        public interface IBox { Value Get(); }
        public class Box : IBox {
            private Value value;
            public Box(Value initial) { value = initial; }
            public Value Get() { return value; }
        }
        public int Read(Value value) {
            switch (value) {
                case Some(number): return number;
                case None: return 0;
            }
        }
        public int ReadBox(IBox box) { return Read(box.Get()); }
        public int Run() {
            Value[] values = [Value.Some(42)];
            Holder holder = Holder { value: values[0] };
            Box box = new Box(holder.value);
            return ReadBox(box);
        }
    ";
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn executes_option_and_result_from_aster_core() {
    let source = r"
        using aster.core;
        public T Identity<T>(T value) { return value; }
        public int Read(Option<int> value) {
            switch (value) {
                case Some(number): return number;
                case None: return 0;
            }
        }
        public int Run() {
            Option<int> value = Identity<Option<int>>(Option<int>.Some(42));
            return Read(value);
        }
    ";
    assert_eq!(run_project(source, "Run"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn executes_a_resolved_static_application_entry_symbol() {
    let compilation =
        compile("public class Program { public static int Main() { return 40 + 2; } }")
            .expect("application should compile");
    let symbol = compilation
        .hir
        .items
        .iter()
        .find_map(|item| {
            let aster_compiler::hir::Item::Class(class) = item else {
                return None;
            };
            class
                .methods
                .iter()
                .find(|method| method.name == "Main")
                .map(|method| method.symbol)
        })
        .expect("resolved Main symbol");
    assert_eq!(
        execute_symbol(&compilation.mir, symbol),
        Ok(ExecutionValue::Int(42))
    );
}

#[test]
fn executes_static_methods_and_exact_overloads() {
    let source = "public int Add(int a, int b) { return a + b; } public long Add(long a, long b) { return a + b; } public class Math { public static int Add(int a, int b) { return a + b; } public static long Add(long a, long b) { return a + b; } } public int Run() { return Add(10, 11) + Math.Add(10, 11); }";
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn executes_all_standard_math_overload_families() {
    let source = r"
        using aster.math;
        public bool Run() {
            return Math.Abs(-42) == 42
                && Math.Abs(-42L) == 42L
                && Math.Min(2, 1) == 1
                && Math.Min(2L, 1L) == 1L
                && Math.Min(2.5f, 1.5f) == 1.5f
                && Math.Min(2.5d, 1.5d) == 1.5d
                && Math.Max(1, 2) == 2
                && Math.Max(1L, 2L) == 2L
                && Math.Max(1.5f, 2.5f) == 2.5f
                && Math.Max(1.5d, 2.5d) == 2.5d
                && Math.Clamp(150, 0, 100) == 100
                && Math.Clamp(150L, 0L, 100L) == 100L
                && Math.Clamp(1.5f, 0f, 1f) == 1f
                && Math.Clamp(-1.5d, -1d, 1d) == -1d;
        }
    ";
    assert_eq!(run_project(source, "Run"), Ok(ExecutionValue::Bool(true)));
}

#[test]
fn standard_math_reports_integer_domain_failures_without_panicking() {
    for (source, expected) in [
        (
            "using aster.math; public int Run() { return Math.Abs(-2147483647 - 1); }",
            "minimum int",
        ),
        (
            "using aster.math; public long Run() { return Math.Abs(-9223372036854775807L - 1L); }",
            "minimum long",
        ),
        (
            "using aster.math; public int Run() { int min = 10; int max = 1; return Math.Clamp(5, min, max); }",
            "min to be less than or equal to max",
        ),
        (
            "using aster.math; public int Run() { return Math.Clamp(5, 10, 1); }",
            "min to be less than or equal to max",
        ),
        (
            "using aster.math; public long Run() { return Math.Clamp(5L, 10L, 1L); }",
            "min to be less than or equal to max",
        ),
        (
            "using aster.math; public float Run() { return Math.Clamp(5f, 10f, 1f); }",
            "min to be less than or equal to max",
        ),
        (
            "using aster.math; public double Run() { return Math.Clamp(5d, 10d, 1d); }",
            "min to be less than or equal to max",
        ),
    ] {
        let error = run_project(source, "Run").expect_err("domain error should be controlled");
        assert!(
            error.contains(expected),
            "unexpected runtime error: {error}"
        );
    }
}

#[test]
fn standard_math_documents_ieee_nan_and_infinity_behavior_in_execution() {
    let source = r"
        using aster.math;
        public bool Run() {
            double zero = 0d;
            double nan = zero / zero;
            double infinity = 1d / zero;
            double minimum = Math.Min(nan, 1d);
            double maximum = Math.Max(1d, nan);
            double clampedNan = Math.Clamp(nan, 0d, 1d);
            double clampedInfinity = Math.Clamp(infinity, 0d, 1d);
            return minimum != minimum && maximum != maximum && clampedNan != clampedNan && clampedInfinity == 1d;
        }
    ";
    assert_eq!(run_project(source, "Run"), Ok(ExecutionValue::Bool(true)));
}

#[test]
fn overload_resolution_uses_documented_safe_conversion() {
    let source =
        "public long Widen(long value) { return value; } public long Run() { return Widen(42); }";
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Long(42)));
}

#[test]
fn static_calls_and_overloads_compose_with_generic_functions() {
    let source = "public T Identity<T>(T value) { return value; } public class Math { public static int Pick(int value) { return value; } public static long Pick(long value) { return value; } } public int Run() { return Identity<int>(Math.Pick(42)); }";
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn executes_field_initializers_and_properties() {
    let source = "public class Player { private int health = 100; public Player(int damage) { Health -= damage; } public int Health { get { return health; } private set { health = value; } } } public int Run() { Player p = new Player(58); return p.Health; }";
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn field_initializers_create_structs_and_arrays_before_constructor_body() {
    let source = "public struct P { public int x; } public class Store { private P point = P { x: 10 }; private int[] values = [20, 30]; public Store() { point.x += 2; } public int Total() { return point.x + values[0] + values[1]; } } public int Run() { Store store = new Store(); return store.Total(); }";
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(62)));
}

#[test]
fn field_initializer_constructs_an_object_with_an_explicit_constructor() {
    let source = "public class Dependency { public int Get() { return 42; } } public class Service { private Dependency dependency = new Dependency(); private int value; public Service() { value = dependency.Get(); } public int Read() { return value; } } public int Run() { Service service = new Service(); return service.Read(); }";
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn field_initializer_constructs_an_object_with_a_synthesized_constructor() {
    let source = "public class Dependency { public int Get() { return 42; } } public class Service { private Dependency dependency = new Dependency(); public int Read() { return dependency.Get(); } } public int Run() { Service service = new Service(); return service.Read(); }";
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn field_initializer_constructs_a_distinct_object_per_instance() {
    let source = "public class Dependency { private int value; public Dependency() { value = 0; } public int Get() { return value; } public void Set(int next) { value = next; } } public class Holder { private Dependency dependency = new Dependency(); public Dependency Item() { return dependency; } } public int Run() { Holder a = new Holder(); Holder b = new Holder(); a.Item().Set(41); b.Item().Set(1); return a.Item().Get() + b.Item().Get(); }";
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn field_initializer_constructs_a_nested_object_with_constructor_arguments() {
    let source = "public class Item { public int value; public Item() { value = 42; } } public class Container { private Item item; public Container(Item item) { this.item = item; } public int Get() { return item.value; } } public class Holder { private Container container = new Container(new Item()); public int Read() { return container.Get(); } } public int Run() { Holder holder = new Holder(); return holder.Read(); }";
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn field_initializer_construction_resolves_across_namespace_files() {
    use std::time::{SystemTime, UNIX_EPOCH};

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("aster-field-initializer-{nonce}"));
    std::fs::create_dir_all(root.join("app")).expect("create namespace directory");
    let main = root.join("main.aster");
    std::fs::write(
        &main,
        "using app; public int Run() { Holder holder = new Holder(); return holder.Read(); }",
    )
    .expect("write main source");
    std::fs::write(
        root.join("app/holder.aster"),
        "namespace app; public class Dependency { public int Get() { return 42; } } public class Holder { private Dependency dependency = new Dependency(); public int Read() { return dependency.Get(); } }",
    )
    .expect("write namespaced source");
    let compilation = compile_project(&main).map_err(|diagnostics| format!("{diagnostics:#?}"));
    std::fs::remove_dir_all(&root).expect("remove temporary project");
    let compilation = compilation.expect("multifile project");
    assert_eq!(
        execute(&compilation.compilation.mir, "Run"),
        Ok(ExecutionValue::Int(42))
    );
}

#[test]
fn executes_explicit_equality_rules() {
    let source = "public interface I { int Get(); } public struct P { public int x; public string name; } public class C : I { private int value; public C(int value) { this.value = value; } public int Get() { return value; } } public int Run() { P a = P { x: 1, name: \"A\" }; P b = P { x: 1, name: \"A\" }; P different = P { x: 1, name: \"B\" }; int[] values = [1]; int[] alias = values; C first = new C(1); C second = first; I left = first; I right = second; if (a == b && a != different && values == alias && first == second && left == right) { return 42; } return 0; }";
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn executes_class_constructor_methods_and_reference_identity() {
    let source = "public class Counter { private int value; public Counter(int initial) { value = initial; } public void Add(int amount) { value += amount; } public int Get() { return value; } } public int Run() { Counter first = new Counter(10); Counter second = first; second.Add(32); return first.Get(); }";
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn executes_interface_dispatch_and_preserves_object_identity() {
    let source = "public interface ICounter { void Add(int amount); int Get(); } public class Counter : ICounter { private int value; public Counter(int value) { this.value = value; } public void Add(int amount) { value += amount; } public int Get() { return value; } } internal int Change(ICounter counter) { counter.Add(32); return counter.Get(); } public int Run() { Counter concrete = new Counter(10); ICounter contract = concrete; return Change(contract) + concrete.Get(); }";
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(84)));
}

#[test]
fn interface_dispatch_uses_the_runtime_implementation() {
    let source = "public interface IValue { int Get(); } public class First : IValue { public First() {} public int Get() { return 1; } } public class Second : IValue { public Second() {} public int Get() { return 42; } } public int Run() { IValue value = new First(); value = new Second(); return value.Get(); }";
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn interfaces_can_be_returned_stored_in_fields_and_arrays() {
    let source = "public interface IValue { int Get(); } public class Value : IValue { private int value; public Value(int value) { this.value = value; } public int Get() { return value; } } public class Box { private IValue value; public Box(IValue value) { this.value = value; } public IValue Get() { return value; } } internal IValue Make(int value) { return new Value(value); } public int Run() { IValue first = Make(20); Box box = new Box(new Value(21)); IValue[] values = [first, box.Get()]; return values[0].Get() + values[1].Get() + values.Length; }";
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(43)));
}

#[test]
fn one_class_can_implement_multiple_interfaces() {
    let source = "public interface ILeft { int Left(); } public interface IRight { int Right(); } public class Both : ILeft, IRight { public Both() {} public int Left() { return 20; } public int Right() { return 22; } } internal int ReadLeft(ILeft value) { return value.Left(); } internal int ReadRight(IRight value) { return value.Right(); } public int Run() { Both both = new Both(); return ReadLeft(both) + ReadRight(both); }";
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn executes_interface_dispatch_across_modules() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("examples/multifile_interfaces/main.aster");
    let compilation = compile_project(&root).expect("multifile interface project");
    assert_eq!(
        execute(&compilation.compilation.mir, "Run"),
        Ok(ExecutionValue::Int(42))
    );
}

#[test]
fn executes_generic_scalar_array_struct_class_and_interface_instances() {
    let source = "public T Choose<T>(bool condition, T first, T second) { return condition ? first : second; } public T First<T>(T[] values) { return values[0]; } public struct Pair { public int value; } public interface IValue { int Get(); } public class Value : IValue { private int value; public Value(int value) { this.value = value; } public int Get() { return value; } } public int Run() { Pair a = Pair { value: 20 }; Pair b = Pair { value: 21 }; Pair picked = Choose(false, a, b); Value object = Choose(true, new Value(20), new Value(1)); IValue contract = object; IValue same = Choose<IValue>(true, contract, contract); int first = First([1, 2]); return picked.value + same.Get() + first; }";
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn executes_generic_classes_structs_arrays_and_interfaces() {
    let source = "public interface IValue<T> { T Get(); } public class Box<T> { private T value; public Box(T value) { this.value = value; } public T Get() { return value; } } public class Score : IValue<int> { private int value; public Score(int value) { this.value = value; } public int Get() { return value; } } public struct Pair<T, U> { public T first; public U second; } internal int Read(IValue<int> value) { return value.Get(); } public int Run() { Box<int>[] boxes = [new Box<int>(20), new Box<int>(21)]; Pair<int, string> pair = Pair<int, string> { first: boxes[0].Get(), second: \"Aster\" }; return pair.first + boxes[1].Get() + Read(new Score(1)); }";
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn executes_generic_types_across_namespace_files() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("examples/generic_types_multifile/app/main.aster");
    let compilation = compile_project(&root).expect("generic type project");
    let entry = aster_compiler::select_application_entry(&compilation, &root)
        .expect("manifest selects generic type example");
    assert_eq!(
        execute_symbol(&compilation.compilation.mir, entry.symbol),
        Ok(ExecutionValue::Int(42))
    );
}

#[test]
fn generic_properties_keep_specialization_specific_resolution() {
    let source = "public class Box<T> { private T value; public Box(T value) { this.value = value; } public T Value { get { return value; } private set { this.value = value; } } public void Replace(T next) { Value = next; } } public int Run() { Box<int> number = new Box<int>(20); Box<string> text = new Box<string>(\"Aster\"); number.Replace(42); string copy = text.Value; return number.Value; }";
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn generic_interface_tables_select_the_exact_overload() {
    let source = "public interface IWriter<T> { void Write(T value); } public class Sink : IWriter<int>, IWriter<string> { private int total; public Sink() { total = 0; } public void Write(int value) { total += value; } public void Write(string value) { total += value.Length; } public int Total() { return total; } } public int Run() { Sink sink = new Sink(); IWriter<int> numbers = sink; IWriter<string> text = sink; numbers.Write(40); text.Write(\"Hi\"); return sink.Total(); }";
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn generic_functions_can_call_generic_functions() {
    let source = "public T Identity<T>(T value) { return value; } public T Forward<T>(T value) { return Identity<T>(value); } public int Run() { return Forward(42); }";
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn infers_multiple_generic_parameters_independently() {
    let source = "public U Second<T, U>(T first, U second) { return second; } public int Run() { return Second(true, 42); }";
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn executes_generic_function_imported_from_another_module() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("examples/multifile_generics/main.aster");
    let compilation = compile_project(&root).expect("multifile generic project");
    assert_eq!(
        execute(&compilation.compilation.mir, "Run"),
        Ok(ExecutionValue::Int(42))
    );
}

#[test]
fn explicit_this_accesses_the_receiver() {
    let source = "public class Value { private int number; public Value(int number) { this.number = number; } public void Add(int number) { this.number += number; } public int Get() { return this.number; } } public int Run() { Value value = new Value(20); value.Add(22); return value.Get(); }";
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn class_contains_struct_and_array_fields() {
    let source = "public struct P { public int x; public int y; } public class Player { public P position; private int[] values; public Player(P position, int[] values) { this.position = position; this.values = values; } public int Sum() { return position.x + values[0] + values.Length; } } public int Run() { P p = P { x: 10, y: 20 }; Player player = new Player(p, [30]); return player.Sum(); }";
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(41)));
}

#[test]
fn classes_can_reference_and_return_other_classes_internally() {
    let source = "public class Leaf { private int value; public Leaf(int value) { this.value = value; } public int Get() { return value; } } public class Box { private Leaf leaf; public Box(Leaf leaf) { this.leaf = leaf; } public Leaf GetLeaf() { return leaf; } } internal Box Make(Leaf leaf) { return new Box(leaf); } public int Run() { Leaf leaf = new Leaf(42); Box box = Make(leaf); return box.GetLeaf().Get(); }";
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn rejects_class_layout_with_non_executable_decimal_field() {
    let source = "public class Money { private decimal value; public Money(decimal value) { this.value = value; } } public int Run() { Money money = new Money(1m); return 0; }";
    let error = run(source, "Run").expect_err("decimal object layout is unsupported");
    assert!(error.contains("decimal"));
}

#[test]
fn arrays_share_identity_across_assignments_and_calls() {
    let source = "internal void Add(int[] a) { a[0] += 5; } public int Run() { int[] a = [10, 20, 30]; int[] b = a; Add(b); return a[0] + a[1] + a[2] + a.Length; }";
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(68)));
}

#[test]
fn new_arrays_are_zeroed_and_support_writes() {
    let source = "public int Run() { int[] a = new int[3]; a[1] = 42; return a[0] + a[1] + a[2]; }";
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn string_array_literals_are_fully_initialized() {
    let source =
        "public int Run() { string[] names = [\"Aster\", \"Natte\"]; return names.Length; }";
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(2)));
}

#[test]
fn new_struct_arrays_use_valid_zero_values() {
    let source = "public struct P { public int x; public bool active; } public int Run() { P[] points = new P[2]; points[1].x = 9; return points[0].x + points[1].x; }";
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(9)));
}

#[test]
fn arrays_of_structs_copy_elements_by_value() {
    let source = "public struct P { public int x; public int y; } public int Run() { P[] a = [P { x: 1, y: 2 }, P { x: 3, y: 4 }]; P copy = a[0]; copy.x = 10; a[1] = copy; return a[0].x + a[1].x; }";
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(11)));
}

#[test]
fn arrays_can_be_returned_to_aster_callers() {
    let source = "internal int[] Make() { return [7, 8]; } public int Run() { int[] a = Make(); return a[0] + a[1]; }";
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(15)));
}

#[test]
fn out_of_bounds_is_a_controlled_runtime_error() {
    let source = "public int Run() { int[] a = [1, 2]; int i = 2; return a[i]; }";
    let error = run(source, "Run").expect_err("bounds error");
    assert!(error.contains("Aster runtime error"));
    assert!(error.contains("array index 2"));
}

#[test]
fn each_execution_gets_a_fresh_context() {
    let invalid = "public int Run() { int[] a = [1]; int i = 1; return a[i]; }";
    assert!(run(invalid, "Run").is_err());
    let valid = "public int Run() { int[] a = [9]; return a[0]; }";
    assert_eq!(run(valid, "Run"), Ok(ExecutionValue::Int(9)));
}

#[test]
fn executes_integer_result() {
    assert_eq!(
        run("public int Calculate() { return 42; }", "Calculate"),
        Ok(ExecutionValue::Int(42))
    );
}

#[test]
fn executes_boolean_result() {
    assert_eq!(
        run("public bool IsReady() { return 3 > 2 && true; }", "IsReady"),
        Ok(ExecutionValue::Bool(true))
    );
}

#[test]
fn executes_void_function() {
    assert_eq!(
        run("public void Work() {}", "Work"),
        Ok(ExecutionValue::Void)
    );
}

#[test]
fn executes_a_void_static_main_entry_symbol_without_a_value() {
    let compilation = compile("public class Program { public static void Main() { return; } }")
        .expect("void application should compile");
    let symbol = compilation
        .hir
        .items
        .iter()
        .find_map(|item| {
            let aster_compiler::hir::Item::Class(class) = item else {
                return None;
            };
            class
                .methods
                .iter()
                .find(|method| method.name == "Main")
                .map(|method| method.symbol)
        })
        .expect("resolved Main symbol");
    assert_eq!(
        execute_symbol(&compilation.mir, symbol),
        Ok(ExecutionValue::Void)
    );
}

#[test]
fn implicit_default_constructor_supports_instance_calls_with_implicit_receiver() {
    let source = "public class Counter { private int value = 40; public int Run() { Increment(); Increment(); return value; } private void Increment() { value = value + 1; } } public int Go() { Counter counter = new Counter(); return counter.Run(); }";
    assert_eq!(run(source, "Go"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn distinct_instances_keep_separate_field_storage() {
    let source = "public class Box { private int value = 0; public void Set(int next) { value = next; } public int Get() { return value; } } public int Go() { Box first = new Box(); Box second = new Box(); first.Set(41); second.Set(1); return first.Get() + second.Get(); }";
    assert_eq!(run(source, "Go"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn interpolates_a_literal_integer() {
    let source = r#"public string Run() { return $"Sum: {1234}"; }"#;
    assert_eq!(
        run(source, "Run"),
        Ok(ExecutionValue::String("Sum: 1234".to_owned()))
    );
}

#[test]
fn interpolates_a_variable_and_an_arithmetic_expression() {
    let source = r#"
        public string Run() {
            int quantity = 4;
            int price = 15;
            return $"Total: {quantity * price}";
        }
    "#;
    assert_eq!(
        run(source, "Run"),
        Ok(ExecutionValue::String("Total: 60".to_owned()))
    );
}

#[test]
fn interpolates_bool_char_float_and_double() {
    let source = r#"
        public string Run() {
            bool ok = true;
            char letter = 'x';
            float small = 1.5f;
            double big = 2.5d;
            return $"{ok} {letter} {small} {big}";
        }
    "#;
    assert_eq!(
        run(source, "Run"),
        Ok(ExecutionValue::String("true x 1.5 2.5".to_owned()))
    );
}

#[test]
fn interpolates_every_integer_width_signed_and_unsigned() {
    let source = r#"
        public string Run() {
            sbyte a = -1;
            byte b = 255;
            short c = -2;
            ushort d = 65535;
            int e = -3;
            uint f = 4000000000;
            long g = -9223372036854775808;
            ulong h = 18446744073709551615ul;
            return $"{a} {b} {c} {d} {e} {f} {g} {h}";
        }
    "#;
    assert_eq!(
        run(source, "Run"),
        Ok(ExecutionValue::String(
            "-1 255 -2 65535 -3 4000000000 -9223372036854775808 18446744073709551615".to_owned()
        ))
    );
}

#[test]
fn interpolates_a_call_result_and_multiple_segments() {
    let source = r#"
        public int Calculate(int a, int b) { return a + b; }
        public string Run() {
            return $"{Calculate(1, 2)}-{Calculate(3, 4)}-{Calculate(5, 6)}";
        }
    "#;
    assert_eq!(
        run(source, "Run"),
        Ok(ExecutionValue::String("3-7-11".to_owned()))
    );
}

#[test]
fn receiver_and_getter_are_each_evaluated_exactly_once() {
    // If the receiver were evaluated per interpolated segment instead of once
    // per method call, or if `Next()` reordered, the result would not be
    // "1-2-3": it would be "6" (three fresh counters) or an out-of-order
    // sequence.
    let source = r#"
        public class Tracker {
            private int value = 0;
            public int Next() {
                value = value + 1;
                return value;
            }
            public string Build() {
                return $"{Next()}-{Next()}-{Next()}";
            }
        }
        public string Run() {
            Tracker tracker = new Tracker();
            return tracker.Build();
        }
    "#;
    assert_eq!(
        run(source, "Run"),
        Ok(ExecutionValue::String("1-2-3".to_owned()))
    );
}

#[test]
fn literal_braces_are_preserved() {
    let source = r#"public string Run() { return $"{{value}}"; }"#;
    assert_eq!(
        run(source, "Run"),
        Ok(ExecutionValue::String("{value}".to_owned()))
    );
}

#[test]
fn two_distinct_interpolated_strings_do_not_share_storage() {
    let source = r#"
        public string Run() {
            int a = 1;
            int b = 2;
            string first = $"first: {a}";
            string second = $"second: {b}";
            return first + " / " + second;
        }
    "#;
    assert_eq!(
        run(source, "Run"),
        Ok(ExecutionValue::String("first: 1 / second: 2".to_owned()))
    );
}

#[test]
fn interpolated_string_can_be_stored_and_passed_as_an_argument() {
    let source = r#"
        public class Holder {
            public string message = "";
        }
        public int Length(string value) { return value.Length; }
        public string Run() {
            Holder holder = new Holder();
            holder.message = $"n={41 + 1}";
            return $"len={Length(holder.message)}";
        }
    "#;
    assert_eq!(
        run(source, "Run"),
        Ok(ExecutionValue::String("len=4".to_owned()))
    );
}

#[test]
fn interpolated_string_instances_from_different_objects_do_not_collide() {
    let source = r#"
        public class Box {
            private int value = 0;
            public void Set(int next) { value = next; }
            public string Describe() { return $"value={value}"; }
        }
        public string Run() {
            Box first = new Box();
            Box second = new Box();
            first.Set(1);
            second.Set(2);
            return first.Describe() + " / " + second.Describe();
        }
    "#;
    assert_eq!(
        run(source, "Run"),
        Ok(ExecutionValue::String("value=1 / value=2".to_owned()))
    );
}

#[test]
fn interpolation_is_correct_across_repeated_executions_in_fresh_contexts() {
    let source = r#"public string Run() { int value = 40; return $"{value + 2}"; }"#;
    for _ in 0..3 {
        assert_eq!(
            run(source, "Run"),
            Ok(ExecutionValue::String("42".to_owned()))
        );
    }
}

#[test]
fn executes_direct_calls_with_parameters() {
    let source = "public int Add(int left, int right) { return left + right; } public int Calculate() { return Add(20, 22); }";
    assert_eq!(run(source, "Calculate"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn executes_variables_and_assignments() {
    let source = "public int Calculate() { int value = 10; value += 5; value *= 2; return value; }";
    assert_eq!(run(source, "Calculate"), Ok(ExecutionValue::Int(30)));
}

#[test]
fn executes_condition() {
    let source = "public int Calculate() { int value = 7; if (value >= 7) { return 42; } else { return 0; } }";
    assert_eq!(run(source, "Calculate"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn executes_loop_break_and_continue() {
    let source = "public int Calculate() { int total = 0; for (int index = 0; index < 10; index += 1) { if (index == 2) { continue; } if (index == 5) { break; } total += index; } return total; }";
    assert_eq!(run(source, "Calculate"), Ok(ExecutionValue::Int(8)));
}

#[test]
fn executes_while_loop() {
    let source =
        "public int Calculate() { int value = 0; while (value < 5) { value += 1; } return value; }";
    assert_eq!(run(source, "Calculate"), Ok(ExecutionValue::Int(5)));
}

#[test]
fn rejects_missing_entry_function() {
    let error = run("public int Calculate() { return 1; }", "Missing").unwrap_err();
    assert!(error.contains("was not found"));
}

#[test]
fn rejects_entry_function_with_parameters() {
    let error = run("public int Add(int value) { return value; }", "Add").unwrap_err();
    assert!(error.contains("must have no parameters"));
}

#[test]
fn rejects_non_public_entry_function() {
    let error = run("int Hidden() { return 1; }", "Hidden").unwrap_err();
    assert!(error.contains("is not public"));
}

#[test]
fn semantic_errors_prevent_jit() {
    let diagnostics = compile("public int Broken() { return false; }")
        .expect_err("semantic error must prevent MIR and JIT");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("expected `int`, found `bool`"))
    );
}

#[test]
fn rejects_unsupported_backend_construction_without_panicking() {
    let error = run(
        "int counter = 0; public int Get() { return counter; }",
        "Get",
    )
    .unwrap_err();
    assert!(error.contains("does not yet support"));
}

#[test]
fn executes_prefix_and_postfix_values() {
    let source =
        "public int Calculate() { int i = 1; i++; ++i; int old = i--; return i * 100 + old; }";
    assert_eq!(run(source, "Calculate"), Ok(ExecutionValue::Int(203)));
}

#[test]
fn executes_increment_in_for_update() {
    let source = "public int Calculate() { int total = 0; for (int i = 0; i < 4; i++) { total += i; } return total; }";
    assert_eq!(run(source, "Calculate"), Ok(ExecutionValue::Int(6)));
}

#[test]
fn executes_conditional_lazily() {
    let source =
        "public int Calculate() { int i = 5; int r = i > 3 ? i++ : --i; return r * 10 + i; }";
    assert_eq!(run(source, "Calculate"), Ok(ExecutionValue::Int(56)));
}

#[test]
fn executes_nested_conditionals_right_associatively() {
    let source = "public int Pick(bool a, bool b) { return a ? 1 : b ? 2 : 3; } public int Calculate() { return Pick(false, true); }";
    assert_eq!(run(source, "Calculate"), Ok(ExecutionValue::Int(2)));
}

#[test]
fn short_circuits_logical_operators() {
    // If `&&`/`||` were eager, the right operand would divide by zero and trap.
    let source = "public bool Calculate() { int zero = 0; bool and_result = false && 1 / zero == 1; bool or_result = true || 1 / zero == 1; return !and_result && or_result; }";
    assert_eq!(run(source, "Calculate"), Ok(ExecutionValue::Bool(true)));
}

#[test]
fn conditional_does_not_evaluate_unselected_branch() {
    // If `?:` evaluated both branches, the false branch would divide by zero and trap.
    let source = "public int Calculate() { int zero = 0; return zero == 0 ? 7 : 1 / zero; }";
    assert_eq!(run(source, "Calculate"), Ok(ExecutionValue::Int(7)));
}

#[test]
fn executes_string_locals_parameters_and_returns() {
    let source = r#"
        public string Wrap(string value) { return value; }
        public string Name() { string name = "Aster"; return Wrap(name); }
    "#;
    assert_eq!(
        run(source, "Name"),
        Ok(ExecutionValue::String("Aster".to_owned()))
    );
}

#[test]
fn executes_utf8_string_returns() {
    let source = r#"public string Text() { return "café ✓"; }"#;
    assert_eq!(
        run(source, "Text"),
        Ok(ExecutionValue::String("café ✓".to_owned()))
    );
}

#[test]
fn compares_strings_by_content() {
    let source = r#"
        public string Left() { return "same"; }
        public bool Calculate() {
            string a = "same";
            bool equal = a == Left();
            bool different = a != "other";
            return equal && different;
        }
    "#;
    assert_eq!(run(source, "Calculate"), Ok(ExecutionValue::Bool(true)));
}

#[test]
fn executes_logging_calls() {
    let source = r#"
        public void Run() {
            string name = "Natte";
            Log("Aster iniciou");
            Log(name);
            Log.Warning("Aviso de teste");
            Log.Error("Erro de teste");
        }
    "#;
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Void));
}

#[test]
fn log_argument_is_evaluated_exactly_once() {
    let source = r#"
        public int Calculate() {
            int count = 0;
            for (int i = 0; i < 1; i++) { count++; }
            Log(count == 1 ? "once" : "wrong");
            return count;
        }
    "#;
    assert_eq!(run(source, "Calculate"), Ok(ExecutionValue::Int(1)));
}

#[test]
fn concatenates_strings_and_counts_unicode_scalars() {
    let source = r#"
        public int Run() {
            string name = "Natte";
            string message = "Olá, " + name;
            message += "!";
            return message.Length;
        }
    "#;
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(11)));
}

#[test]
fn string_concatenation_is_left_associative_and_preserves_call_order() {
    let source = r#"
        internal string Piece(int[] order, int digit, string value) {
            order[0] = order[0] * 10 + digit;
            return value;
        }
        public int Run() {
            int[] order = [0];
            string value = Piece(order, 1, "A") + Piece(order, 2, "B") + Piece(order, 3, "C");
            if (value == "ABC" && order[0] == 123) {
                return value.Length;
            }
            return 0;
        }
    "#;
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(3)));
}

#[test]
fn standard_string_is_empty_uses_the_normal_stdlib_pipeline() {
    let source = r#"
        using aster.text;
        public bool Run() {
            string dynamic = "" + "A";
            return String.IsEmpty("") && !String.IsEmpty(dynamic);
        }
    "#;
    assert_eq!(run_project(source, "Run"), Ok(ExecutionValue::Bool(true)));
}

#[test]
fn dynamic_strings_are_owned_by_each_execution_context() {
    let compilation =
        compile(r#"public string Join() { string value = "As"; return value + "ter"; }"#)
            .expect("dynamic string source");
    assert_eq!(
        execute(&compilation.mir, "Join"),
        Ok(ExecutionValue::String("Aster".to_owned()))
    );
    assert_eq!(
        execute(&compilation.mir, "Join"),
        Ok(ExecutionValue::String("Aster".to_owned()))
    );
}

#[test]
fn executes_long_arithmetic_and_returns() {
    let source = "public long Big() { long base = 4000000000L; return base + 1; }";
    assert_eq!(run(source, "Big"), Ok(ExecutionValue::Long(4_000_000_001)));
}

#[test]
fn types_unsuffixed_wide_integer_literals_as_long() {
    let source = "public long Wide() { return 4000000000; }";
    assert_eq!(run(source, "Wide"), Ok(ExecutionValue::Long(4_000_000_000)));
}

#[test]
fn executes_float_and_double_arithmetic() {
    let source = "public float Half() { return 2.5f * 3f; } public double Quarter() { double total = 10; return total / 4; }";
    assert_eq!(run(source, "Half"), Ok(ExecutionValue::float(7.5)));
    assert_eq!(run(source, "Quarter"), Ok(ExecutionValue::double(2.5)));
}

#[test]
fn widens_only_when_the_conversion_is_exact() {
    let source = "public bool Mix() { short a = 5; float b = a; int c = 16777217; double d = c; double e = b; return d == 16777217d && e == 5.0d && 2 < 2.5f; }";
    assert_eq!(run(source, "Mix"), Ok(ExecutionValue::Bool(true)));
}

#[test]
fn executes_explicit_casts() {
    let source = r"
        public int Truncate() { return (int)9.7d; }
        public char Letter() { return (char)65; }
        public long Narrowed() { return (long)(int)4000000000L; }
    ";
    assert_eq!(run(source, "Truncate"), Ok(ExecutionValue::Int(9)));
    assert_eq!(run(source, "Letter"), Ok(ExecutionValue::Char('A')));
    // 4000000000 wraps to -294967296 when truncated to 32 bits.
    assert_eq!(
        run(source, "Narrowed"),
        Ok(ExecutionValue::Long(-294_967_296))
    );
}

#[test]
fn compares_and_returns_char_values() {
    let source = "public bool IsComma() { char c = ','; return c == ','; } public char Accent() { return 'é'; }";
    assert_eq!(run(source, "IsComma"), Ok(ExecutionValue::Bool(true)));
    assert_eq!(run(source, "Accent"), Ok(ExecutionValue::Char('é')));
}

#[test]
fn rejects_invalid_unicode_scalar_returned_as_char() {
    let int_local = mir::LocalId(0);
    let char_local = mir::LocalId(1);
    let module = mir::Module {
        enums: Vec::new(),
        structs: Vec::new(),
        classes: Vec::new(),
        interfaces: Vec::new(),
        interface_implementations: Vec::new(),
        functions: vec![mir::Function {
            constructor: false,
            symbol: mir::SymbolId(0),
            owner: None,
            name: String::from("Invalid"),
            visibility: mir::Visibility::Public,
            parameters: Vec::new(),
            locals: vec![
                mir::Local {
                    id: int_local,
                    symbol: None,
                    name: String::from("negative"),
                    type_: mir::Type::Int,
                    mutable: false,
                    temporary: true,
                },
                mir::Local {
                    id: char_local,
                    symbol: None,
                    name: String::from("invalid_char"),
                    type_: mir::Type::Char,
                    mutable: false,
                    temporary: true,
                },
            ],
            return_type: mir::Type::Char,
            entry: mir::BasicBlockId(0),
            blocks: vec![mir::BasicBlock {
                id: mir::BasicBlockId(0),
                instructions: vec![
                    mir::Instruction::Assign {
                        target: mir::Place::Local(int_local),
                        value: mir::Rvalue {
                            type_: mir::Type::Int,
                            kind: mir::RvalueKind::Unary {
                                operator: mir::UnaryOperator::Negate,
                                operand: mir::Operand {
                                    type_: mir::Type::Int,
                                    kind: mir::OperandKind::Constant(mir::Constant::Integer(
                                        String::from("1"),
                                    )),
                                },
                            },
                        },
                    },
                    mir::Instruction::Assign {
                        target: mir::Place::Local(char_local),
                        value: mir::Rvalue {
                            type_: mir::Type::Char,
                            kind: mir::RvalueKind::Cast(mir::Operand {
                                type_: mir::Type::Int,
                                kind: mir::OperandKind::Copy(mir::Place::Local(int_local)),
                            }),
                        },
                    },
                ],
                terminator: mir::Terminator::Return(Some(mir::Operand {
                    type_: mir::Type::Char,
                    kind: mir::OperandKind::Copy(mir::Place::Local(char_local)),
                })),
            }],
        }],
    };
    let error = execute(&module, "Invalid").unwrap_err().to_string();
    assert!(error.contains("invalid Unicode scalar value U+FFFFFFFF"));
}

#[test]
fn float_division_by_zero_follows_ieee() {
    let source = "public bool Checks() { float zero = 0f; float inf = 1f / zero; float nan = zero / zero; return inf > 1000000f && nan != nan; }";
    assert_eq!(run(source, "Checks"), Ok(ExecutionValue::Bool(true)));
}

#[test]
fn rejects_float_remainder_with_clear_diagnostic() {
    let error = run(
        "public float Bad() { float a = 5f; float b = 2f; return a % b; }",
        "Bad",
    )
    .unwrap_err();
    assert!(error.contains("floating-point remainder"));
}

#[test]
fn executes_direct_recursion() {
    let source = "public int Factorial(int n) { return n <= 1 ? 1 : n * Factorial(n - 1); } public int Calculate() { return Factorial(6); }";
    assert_eq!(run(source, "Calculate"), Ok(ExecutionValue::Int(720)));
}

#[test]
fn executes_mutual_recursion() {
    let source = r"
        public bool IsEven(int n) { return n == 0 ? true : IsOdd(n - 1); }
        public bool IsOdd(int n) { return n == 0 ? false : IsEven(n - 1); }
        public bool Calculate() { return IsEven(10) && IsOdd(7); }
    ";
    assert_eq!(run(source, "Calculate"), Ok(ExecutionValue::Bool(true)));
}

#[test]
fn folds_local_and_module_constants_into_executable_code() {
    let source = r"
        const int Base = 40;
        public int Calculate() { const int Offset = Base / 20 + 0; return Base + Offset * 2 / 2 + 1; }
    ";
    assert_eq!(run(source, "Calculate"), Ok(ExecutionValue::Int(43)));
}

#[test]
fn folds_constant_string_and_conditional_initializers() {
    let source = r#"
        const string Greeting = "Aster" + " " + "0.1";
        public string Name() { return Greeting; }
        const int Threshold = true ? 5 : 10;
        public int Limit() { return Threshold; }
    "#;
    assert_eq!(
        run(source, "Name"),
        Ok(ExecutionValue::String("Aster 0.1".to_owned()))
    );
    assert_eq!(run(source, "Limit"), Ok(ExecutionValue::Int(5)));
}

#[test]
fn executes_small_integer_types_and_explicit_wrapping_casts() {
    let source = "public byte Wrap() { byte b = 250; b = (byte)(b + 10); return b; }";
    assert_eq!(run(source, "Wrap"), Ok(ExecutionValue::Byte(4)));
    let source = "public short Negative() { short s = -300; return s; }";
    assert_eq!(run(source, "Negative"), Ok(ExecutionValue::Short(-300)));
    let source = "public sbyte Signed() { sbyte v = -128; return v; }";
    assert_eq!(run(source, "Signed"), Ok(ExecutionValue::SByte(-128)));
    let source = "public ushort Wide() { ushort v = 65535; return v; }";
    assert_eq!(run(source, "Wide"), Ok(ExecutionValue::UShort(65535)));
}

#[test]
fn executes_unsigned_division_and_comparison() {
    let source = "public uint Half() { uint u = 4294967295u; return u / 2u; }";
    assert_eq!(run(source, "Half"), Ok(ExecutionValue::UInt(2_147_483_647)));
    let source = "public bool Compare() { uint a = 4000000000u; return a > 2000000000u; }";
    assert_eq!(run(source, "Compare"), Ok(ExecutionValue::Bool(true)));
    let source =
        "public ulong Remainder() { ulong x = 18446744073709551615ul; return x % 1000000ul; }";
    assert_eq!(run(source, "Remainder"), Ok(ExecutionValue::ULong(551_615)));
}

#[test]
fn mixed_uint_and_int_promote_to_long() {
    let source = "public long Mixed() { uint u = 10u; int i = 5; return u + i; }";
    assert_eq!(run(source, "Mixed"), Ok(ExecutionValue::Long(15)));
}

#[test]
fn executes_casts_between_integer_widths_and_signs() {
    let source = r"
        public int Truncated() { ulong big = 300ul; byte small = (byte)big; return small; }
        public sbyte Reinterpreted() { return (sbyte)200; }
        public double Converted() { ulong x = 12ul; return (double)x / 8; }
        public uint FromFloat() { return (uint)3.9f; }
    ";
    assert_eq!(run(source, "Truncated"), Ok(ExecutionValue::Int(44)));
    assert_eq!(run(source, "Reinterpreted"), Ok(ExecutionValue::SByte(-56)));
    assert_eq!(run(source, "Converted"), Ok(ExecutionValue::double(1.5)));
    assert_eq!(run(source, "FromFloat"), Ok(ExecutionValue::UInt(3)));
}

#[test]
fn executes_struct_fields_and_independent_value_copies() {
    let source = r"
        public struct Position { public int x; public int y; }
        public int Run() {
            Position a = Position { x: 1, y: 2 };
            Position b = a;
            b.x = 30;
            return a.x * 100 + b.x;
        }
    ";
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(130)));
}

#[test]
fn passes_and_returns_structs_by_value() {
    let source = r"
        public struct Position { public int x; public int y; }
        internal Position Move(Position value, int amount) {
            value.x += amount;
            return value;
        }
        public int Run() {
            Position original = Position { x: 10, y: 20 };
            Position moved = Move(original, 5);
            return original.x * 100 + moved.x;
        }
    ";
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(1015)));
}

#[test]
fn executes_nested_structs_with_natural_layout() {
    let source = r"
        public struct Position { public byte tag; public long x; public short y; }
        public struct Transform { public Position position; public bool visible; }
        public int Run() {
            Transform value = Transform {
                position: Position { tag: 7, x: 4000000000L, y: -3 },
                visible: true
            };
            return value.visible ? (int)(value.position.x / 1000000000L) + value.position.tag + value.position.y : 0;
        }
    ";
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(8)));
}

#[test]
fn rejects_decimal_execution_with_a_useful_message() {
    let error = run(
        "public decimal Money() { decimal price = 10.50m; return price; }",
        "Money",
    )
    .unwrap_err();
    assert!(error.contains("`decimal` is checked by the compiler but cannot execute yet"));
}

#[test]
fn executes_functions_and_classes_from_local_modules() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("examples/multifile/main.aster");
    let project = compile_project(&root).expect("multifile example should compile");
    assert_eq!(
        execute(&project.compilation.mir, "Run"),
        Ok(ExecutionValue::Int(42))
    );
}

#[test]
fn executes_standard_library_together_with_a_local_import() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("examples/standard_library/app/main.aster");
    let project = compile_project(&root).expect("standard-library project should compile");
    let entry = aster_compiler::select_application_entry(&project, &root)
        .expect("manifest should select Main");
    assert_eq!(
        execute_symbol(&project.compilation.mir, entry.symbol),
        Ok(ExecutionValue::Int(100))
    );
}

#[test]
fn executes_directory_inferred_namespaces_with_main() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("examples/namespaces/app/main.aster");
    let project = compile_project(&root).expect("namespace example should compile");
    let entry = aster_compiler::select_application_entry(&project, &root)
        .expect("manifest should select app.Program.Main");
    assert_eq!(
        execute_symbol(&project.compilation.mir, entry.symbol),
        Ok(ExecutionValue::Int(100))
    );
}

#[test]
fn executes_static_properties_and_overloads_across_modules() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("examples/multifile_ergonomics/main.aster");
    let project = compile_project(&root).expect("ergonomics project should compile");
    assert_eq!(
        execute(&project.compilation.mir, "Run"),
        Ok(ExecutionValue::Int(42))
    );
}

#[test]
fn executes_imported_structs_used_by_imported_classes() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("examples/multifile_types/main.aster");
    let project = compile_project(&root).expect("multifile type example should compile");
    assert_eq!(
        execute(&project.compilation.mir, "Run"),
        Ok(ExecutionValue::Int(9))
    );
}

#[test]
fn selects_free_function_over_instance_method_with_same_name() {
    let source = r"
        public class Service {
            public int Run() { return 1; }
        }
        public int Run() {
            Service s = new Service();
            return s.Run() + 41;
        }
    ";
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn selects_free_function_over_static_method_with_same_name() {
    let source = r"
        public class Service {
            public static int Run() { return 41; }
        }
        public int Run() {
            return Service.Run() + 1;
        }
    ";
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn program_main_calls_method_with_same_name() {
    let source = "public class Service { public int Main() { return 42; } } public class Program { public static int Main() { Service s = new Service(); return s.Main(); } }";
    let compilation = compile(source).expect("should compile");
    let symbol = compilation
        .hir
        .items
        .iter()
        .find_map(|item| {
            let aster_compiler::hir::Item::Class(class) = item else {
                return None;
            };
            if class.name != "Program" {
                return None;
            }
            class
                .methods
                .iter()
                .find(|m| m.name == "Main")
                .map(|m| m.symbol)
        })
        .expect("Program.Main symbol");
    assert_eq!(
        execute_symbol(&compilation.mir, symbol),
        Ok(ExecutionValue::Int(42))
    );
}
