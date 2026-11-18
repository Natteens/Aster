//! T-LANG-4b: `is T x` class type patterns and `when` guards, run through the
//! whole pipeline (semantic analysis, HIR, MIR, Cranelift).

use aster_codegen_cranelift::{ExecutionValue, execute};
use aster_compiler::compile;

fn run(source: &str) -> Result<ExecutionValue, String> {
    let compilation = compile(source).map_err(|diagnostics| format!("{diagnostics:#?}"))?;
    execute(&compilation.mir, "Run").map_err(|error| error.to_string())
}

fn run_int(source: &str) -> i32 {
    match run(source) {
        Ok(ExecutionValue::Int(value)) => value,
        other => panic!("expected an int result, found {other:?}"),
    }
}

/// A three-level hierarchy `Shape <- Polygon <- Square` plus an unrelated
/// sibling branch `Shape <- Circle`.
const HIERARCHY: &str = r"
    public class Shape {
        public int id;
        public Shape(int id) { this.id = id; }
    }
    public class Polygon : Shape {
        public int sides;
        public Polygon(int id, int sides) : base(id) { this.sides = sides; }
    }
    public class Square : Polygon {
        public int edge;
        public Square(int id, int edge) : base(id, 4) { this.edge = edge; }
    }
    public class Circle : Shape {
        public int radius;
        public Circle(int id, int radius) : base(id) { this.radius = radius; }
    }
";

fn with_hierarchy(body: &str) -> String {
    format!("{HIERARCHY}\n{body}")
}

#[test]
fn tests_across_three_levels_are_true_only_for_the_object_class_and_its_bases() {
    let source = with_hierarchy(
        r"
        public int Classify(Shape shape) {
            int bits = 0;
            if (shape is Square) { bits = bits + 1; }
            if (shape is Polygon) { bits = bits + 2; }
            if (shape is Shape) { bits = bits + 4; }
            if (shape is Circle) { bits = bits + 8; }
            return bits;
        }
        public int Run() {
            // Square: Square + Polygon + Shape; Polygon: Polygon + Shape;
            // Shape: Shape only; Circle: Circle + Shape.
            return Classify(new Square(1, 2)) * 1000
                + Classify(new Polygon(1, 5)) * 100
                + Classify(new Shape(1)) * 10
                + Classify(new Circle(1, 3));
        }",
    );
    assert_eq!(run_int(&source), 7 * 1000 + 6 * 100 + 4 * 10 + 12);
}

#[test]
fn pattern_binding_reads_the_derived_members_of_the_same_object() {
    let source = with_hierarchy(
        r"
        public int Describe(Shape shape) {
            if (shape is Square square) { return square.edge * 100 + square.sides * 10 + square.id; }
            if (shape is Circle circle) { return circle.radius * 1000 + circle.id; }
            return -1;
        }
        public int Run() {
            return Describe(new Square(7, 3)) + Describe(new Circle(2, 5)) + Describe(new Shape(1));
        }",
    );
    assert_eq!(
        run_int(&source),
        (3 * 100 + 4 * 10 + 7) + (5 * 1000 + 2) - 1
    );
}

#[test]
fn same_type_pattern_is_always_true_and_binds_the_same_object() {
    let source = with_hierarchy(
        r"
        public int Run() {
            Circle circle = new Circle(1, 9);
            if (circle is Circle same) {
                same.radius = same.radius + 1;
                return circle.radius;
            }
            return -1;
        }",
    );
    assert_eq!(run_int(&source), 10);
}

#[test]
fn upcast_pattern_to_a_base_class_is_always_true() {
    let source = with_hierarchy(
        r"
        public int Run() {
            Square square = new Square(4, 2);
            if (square is Shape shape) { return shape.id; }
            return -1;
        }",
    );
    assert_eq!(run_int(&source), 4);
}

#[test]
fn binding_is_an_alias_not_a_copy() {
    let source = with_hierarchy(
        r"
        public int Run() {
            Shape shape = new Circle(1, 5);
            if (shape is Circle c) { c.radius = 40; }
            if (shape is Circle again) { return again.radius; }
            return -1;
        }",
    );
    assert_eq!(run_int(&source), 40);
}

#[test]
fn definite_assignment_in_the_false_branch_of_a_negated_pattern() {
    let source = with_hierarchy(
        r"
        public int Read(Shape shape) {
            if (!(shape is Circle circle)) { return -1; }
            return circle.radius;
        }
        public int Else(Shape shape) {
            if (!(shape is Circle circle)) { return 0; } else { return circle.radius + 1; }
        }
        public int Run() {
            return Read(new Circle(1, 8)) * 100 + Read(new Shape(1)) * 10 + Else(new Circle(1, 2));
        }",
    );
    assert_eq!(run_int(&source), 800 - 10 + 3);
}

#[test]
fn and_right_operand_sees_the_binding_and_short_circuits() {
    let source = with_hierarchy(
        r"
        public int Run() {
            Shape a = new Circle(1, 6);
            Shape b = new Square(2, 3);
            int hits = 0;
            if (a is Circle c && c.radius > 5) { hits = hits + 1; }
            if (a is Circle c2 && c2.radius > 50) { hits = hits + 10; }
            if (b is Circle c3 && c3.radius > 0) { hits = hits + 100; }
            if (b is Square s && s.edge == 3 && s.sides == 4) { hits = hits + 1000; }
            return hits;
        }",
    );
    assert_eq!(run_int(&source), 1001);
}

#[test]
fn or_and_negation_combine_patterns() {
    let source = with_hierarchy(
        r"
        public int Kind(Shape shape) {
            if (shape is Circle || shape is Square) { return 1; }
            if (!(shape is Polygon)) { return 2; }
            return 3;
        }
        public int Run() {
            return Kind(new Circle(1, 1)) * 100 + Kind(new Square(1, 1)) * 10 + Kind(new Shape(1)) + Kind(new Polygon(1, 3)) * 1000;
        }",
    );
    assert_eq!(run_int(&source), 100 + 10 + 2 + 3000);
}

#[test]
fn while_condition_pattern_walks_a_chain() {
    let source = r"
        public class Node {
            public int value;
            public Node(int value) { this.value = value; }
        }
        public class Link : Node {
            public Node next;
            public Link(int value, Node next) : base(value) { this.next = next; }
        }
        public int Run() {
            Node list = new Link(1, new Link(2, new Link(3, new Node(4))));
            int sum = 0;
            Node current = list;
            while (current is Link link) {
                sum = sum + link.value;
                current = link.next;
            }
            return sum * 10 + current.value;
        }
    ";
    assert_eq!(run_int(source), 64);
}

#[test]
fn pattern_variable_leaks_to_the_enclosing_block_after_an_if_that_returns() {
    // Like C#: the guard clause form keeps the variable in scope afterwards.
    let source = with_hierarchy(
        r"
        public int Run() {
            Shape shape = new Circle(1, 11);
            if (!(shape is Circle c)) { return -1; }
            return c.radius;
        }",
    );
    assert_eq!(run_int(&source), 11);
}

#[test]
fn boolean_result_of_a_pattern_can_be_stored_and_returned() {
    let source = with_hierarchy(
        r"
        public bool IsCircle(Shape shape) { return shape is Circle; }
        public int Run() {
            bool yes = IsCircle(new Circle(1, 1));
            bool no = IsCircle(new Shape(1));
            if (yes && !no) { return 1; }
            return 0;
        }",
    );
    assert_eq!(run_int(&source), 1);
}

// ---------------------------------------------------------------------------
// `when` guards
// ---------------------------------------------------------------------------

#[test]
fn int_switch_guard_falls_through_to_a_later_case_and_to_default() {
    let source = r"
        public int Pick(int value, int limit) {
            switch (value) {
                case 1 when limit > 10: return 100;
                case 1 when limit > 5: return 50;
                case 1: return 1;
                case 2 when limit > 100: return 200;
                default: return -1;
            }
        }
        public int Run() {
            return Pick(1, 20) + Pick(1, 7) + Pick(1, 0) + Pick(2, 0) + Pick(2, 1000) + Pick(3, 0);
        }
    ";
    assert_eq!(run_int(source), 100 + 50 + 1 - 1 + 200 - 1);
}

#[test]
fn string_and_char_switch_guards() {
    let source = r#"
        public int ByString(string name, bool flag) {
            switch (name) {
                case "a" when flag: return 1;
                case "a": return 2;
                case "b" when !flag: return 3;
                default: return 9;
            }
        }
        public int ByChar(char value, int n) {
            switch (value) {
                case 'x' when n > 0: return 1;
                case 'x': return 2;
                default: return 3;
            }
        }
        public int Run() {
            return ByString("a", true) * 10000 + ByString("a", false) * 1000
                + ByString("b", false) * 100 + ByString("b", true) * 10 + ByChar('x', 1) * 100000
                + ByChar('x', 0) * 1000000 + ByChar('y', 0) * 10000000;
        }
    "#;
    assert_eq!(
        run_int(source),
        10_000 + 2000 + 300 + 90 + 100_000 + 2_000_000 + 30_000_000
    );
}

#[test]
fn guard_in_a_switch_expression_falls_through() {
    let source = r"
        public int Score(int value, int bonus) {
            return value switch {
                0 when bonus > 0 => 100,
                0 => 1,
                1 when bonus > 0 => 200,
                default => 7,
            };
        }
        public int Run() {
            return Score(0, 1) + Score(0, 0) + Score(1, 1) + Score(1, 0) + Score(5, 0);
        }
    ";
    assert_eq!(run_int(source), 100 + 1 + 200 + 7 + 7);
}

#[test]
fn guard_is_evaluated_after_the_match_in_order_and_the_subject_once() {
    let source = r"
        public class Trace {
            public int value;
            public Trace() {}
            public int Mark(int digit) { value = value * 10 + digit; return digit; }
        }
        public int Run() {
            Trace t = new Trace();
            int result = 0;
            switch (t.Mark(1)) {
                case 2 when t.Mark(9) > 0: result = 1;
                case 1 when t.Mark(2) > 5: result = 2;
                case 1 when t.Mark(3) > 2: result = 3;
                case 1 when t.Mark(4) > 0: result = 4;
                default: result = 5;
            }
            return t.value * 10 + result;
        }
    ";
    // Subject (1) once; guard of `case 2` never runs (no match); guards of
    // `case 1` run in order 2 (false), 3 (true); the fourth is never reached.
    assert_eq!(run_int(source), 1230 + 3);
}

#[test]
fn switch_without_matching_guard_runs_default() {
    let source = r"
        public int Run() {
            int result = 0;
            switch (4) {
                case 4 when result > 0: result = 1;
                default: result = 2;
            }
            return result;
        }
    ";
    assert_eq!(run_int(source), 2);
}

#[test]
fn enum_payload_binding_is_visible_in_the_guard() {
    let source = r"
        public enum Shape { Circle(int radius), Square(int edge), Empty }
        public int Weigh(Shape shape) {
            switch (shape) {
                case Circle(r) when r > 10: return 1000;
                case Circle(r): return r;
                case Square(e) when e % 2 == 0: return e * 2;
                case Square(e): return e;
                case Empty: return 0;
            }
        }
        public int Run() {
            return Weigh(Shape.Circle(20)) + Weigh(Shape.Circle(3))
                + Weigh(Shape.Square(4)) + Weigh(Shape.Square(5)) + Weigh(Shape.Empty);
        }
    ";
    assert_eq!(run_int(source), 1000 + 3 + 8 + 5);
}

#[test]
fn enum_switch_expression_guard_uses_the_payload() {
    let source = r"
        public enum Shape { Circle(int radius), Square(int edge) }
        public int Weigh(Shape shape) {
            return shape switch {
                Circle(r) when r > 10 => 1000,
                Circle(r) => r,
                Square(e) => e,
            };
        }
        public int Run() { return Weigh(Shape.Circle(11)) + Weigh(Shape.Circle(2)) + Weigh(Shape.Square(6)); }
    ";
    assert_eq!(run_int(source), 1000 + 2 + 6);
}

#[test]
fn guard_can_use_a_type_pattern_whose_binding_is_visible_in_the_body() {
    let source = with_hierarchy(
        r"
        public int Pick(int tag, Shape shape) {
            switch (tag) {
                case 1 when shape is Circle c && c.radius > 3: return c.radius;
                case 1: return -1;
                default: return -2;
            }
        }
        public int Run() {
            return Pick(1, new Circle(1, 9)) * 100 + Pick(1, new Circle(1, 2)) * 10 + Pick(1, new Shape(1));
        }",
    );
    assert_eq!(run_int(&source), 900 - 10 - 1);
}

#[test]
fn patterns_work_inside_methods_and_through_fields() {
    let source = r"
        public class Animal { public Animal() {} }
        public class Dog : Animal { public int tricks; public Dog(int tricks) { this.tricks = tricks; } }
        public class Cat : Animal { public Cat() {} }
        public class Zoo {
            public Animal resident;
            public Zoo(Animal resident) { this.resident = resident; }
            public int Tricks() {
                if (resident is Dog dog) { return dog.tricks; }
                return 0;
            }
        }
        public int Run() {
            Zoo a = new Zoo(new Dog(6));
            Zoo b = new Zoo(new Cat());
            return a.Tricks() * 10 + b.Tricks();
        }
    ";
    assert_eq!(run_int(source), 60);
}

#[test]
fn pattern_on_a_generic_class_specialization() {
    let source = r"
        public class Box<T> { public T value; public Box(T value) { this.value = value; } }
        public class IntBox : Box<int> { public IntBox(int v) : base(v) {} }
        public int Run() {
            Box<int> box = new IntBox(12);
            if (box is IntBox ib) { return ib.value; }
            return -1;
        }
    ";
    assert_eq!(run_int(source), 12);
}

/// Under reference counting (`--features mem-exp-c6`) the binding is a
/// borrowed alias of the tested value: it must not retain, release or free
/// the object, so objects tested in a loop are still released exactly once
/// (by their owner) and one still referenced from a field survives the
/// pattern scope.
#[cfg(feature = "mem-exp-rc-core")]
#[test]
fn pattern_bindings_do_not_disturb_reference_counts() {
    let source = with_hierarchy(
        r"
        public class Holder { public Shape shape; public Holder(Shape shape) { this.shape = shape; } }
        public Shape Make(int i) { return new Circle(i, i); }
        public int Run() {
            Holder holder = new Holder(Make(0));
            int sum = 0;
            for (int i = 1; i < 100; i += 1) {
                holder.shape = Make(i);
                if (holder.shape is Circle c && c.radius > 0) { sum = sum + c.radius; }
                if (holder.shape is Square s) { sum = sum - 1000000; }
            }
            // The last object is still alive and readable after every pattern scope.
            if (holder.shape is Circle last) { return sum * 1000 + last.radius; }
            return -1;
        }",
    );
    let compilation = compile(&source).expect("source compiles");
    let (value, stats) =
        aster_codegen_cranelift::execute_with_stats(&compilation.mir, "Run").expect("executes");
    assert_eq!(value, ExecutionValue::Int(4950 * 1000 + 99));
    assert!(
        stats.object_heap.released_blocks >= 98,
        "replaced objects were not released: {:?}",
        stats.object_heap
    );
}
