//! T-OBJ-4 (object model B1, pull requests 3 and 4): interfaces through
//! hierarchies and checked downcast. Every positive program runs through the
//! whole pipeline (semantic analysis, HIR, MIR, Cranelift) and observes its
//! result; negative programs are controlled compile-time diagnostics.
//!
//! B1 section 13 cases covered here: P5 (an interface call through a
//! base-typed value after the conversion runs the most-derived override), N8
//! (impossible casts and tests), N9 (a `Base` to `IFoo` implicit conversion
//! when only `Derived` implements it is still an error), and the U3/U4
//! behaviour of `is`/`as` with class and interface targets. V4 and V7 are
//! covered by the backend unit tests next to the descriptor code.

use std::sync::atomic::{AtomicU64, Ordering};

use aster_codegen_cranelift::{ExecutionValue, execute};
use aster_compiler::{compile, compile_project};

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

/// Programs that use `as` need the official `Option`, so they go through the
/// project pipeline with `using aster.core;`.
fn run_project_int(body: &str) -> i32 {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "aster-interfaces-hierarchies-{}-{id}.aster",
        std::process::id()
    ));
    std::fs::write(&path, format!("using aster.core;\n{HIERARCHY}\n{body}"))
        .expect("write temporary project");
    let compiled = compile_project(&path).map_err(|diagnostics| format!("{diagnostics:#?}"));
    std::fs::remove_file(&path).ok();
    let compilation = compiled.expect("project compiles").compilation;
    match execute(&compilation.mir, "Run") {
        Ok(ExecutionValue::Int(value)) => value,
        other => panic!("expected an int result, found {other:?}"),
    }
}

fn project_errors(body: &str) -> Vec<String> {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "aster-interfaces-hierarchies-err-{}-{id}.aster",
        std::process::id()
    ));
    std::fs::write(&path, format!("using aster.core;\n{HIERARCHY}\n{body}"))
        .expect("write temporary project");
    let result = compile_project(&path);
    std::fs::remove_file(&path).ok();
    match result {
        Ok(_) => panic!("expected a compile-time diagnostic for:\n{body}"),
        Err(diagnostics) => diagnostics
            .into_iter()
            .map(|diagnostic| diagnostic.diagnostic.message)
            .collect(),
    }
}

fn project_rejects(body: &str, expected: &str) {
    let messages = project_errors(body);
    assert!(
        messages.iter().any(|message| message.contains(expected)),
        "expected a diagnostic containing `{expected}`, found {messages:#?}"
    );
}

fn rejects(source: &str, expected: &str) {
    let messages: Vec<String> = match compile(source) {
        Ok(_) => panic!("expected a compile-time diagnostic for:\n{source}"),
        Err(diagnostics) => diagnostics
            .into_iter()
            .map(|diagnostic| diagnostic.message)
            .collect(),
    };
    assert!(
        messages.iter().any(|message| message.contains(expected)),
        "expected a diagnostic containing `{expected}`, found {messages:#?}"
    );
}

/// `Shape <- Polygon <- Square` (three levels) and a sibling `Shape <- Circle`.
/// `Shape` implements nothing; `Polygon` introduces `IArea` (through a virtual
/// method that `Square` overrides); `Circle` implements `IName` only. `Sealed`
/// is a sealed leaf of `Shape`.
const HIERARCHY: &str = r"
    public interface IArea { int Area(); }
    public interface IName { int Name(); }
    public interface IUnused { int Unused(); }
    public class Shape {
        public int id;
        public Shape(int id) { this.id = id; }
    }
    public class Polygon : Shape, IArea {
        public int sides;
        public Polygon(int id, int sides) : base(id) { this.sides = sides; }
        public virtual int Area() { return sides * 10; }
    }
    public class Square : Polygon {
        public int edge;
        public Square(int id, int edge) : base(id, 4) { this.edge = edge; }
        public override int Area() { return edge * edge; }
    }
    public class Circle : Shape, IName {
        public int radius;
        public Circle(int id, int radius) : base(id) { this.radius = radius; }
        public int Name() { return 700 + radius; }
    }
    public sealed class Sealed : Shape {
        public Sealed(int id) : base(id) {}
    }
";

#[test]
fn is_with_an_interface_target_searches_the_object_descriptor() {
    let source = format!(
        "{HIERARCHY}
        public int Probe(Shape shape) {{
            int bits = 0;
            if (shape is IArea) {{ bits = bits + 1; }}
            if (shape is IName) {{ bits = bits + 2; }}
            if (shape is IUnused) {{ bits = bits + 4; }}
            return bits;
        }}
        public int Run() {{
            return Probe(new Square(1, 3)) * 1000 + Probe(new Polygon(1, 5)) * 100
                + Probe(new Circle(1, 2)) * 10 + Probe(new Shape(1));
        }}"
    );
    assert_eq!(run_int(&source), 1000 + 100 + 20);
}

#[test]
fn an_interface_binding_calls_the_most_derived_override_through_three_levels() {
    // The static type is `Shape`, the dynamic type `Square` (three levels
    // below the root, overriding `Polygon.Area`): P5 for a dynamic lookup.
    let source = format!(
        "{HIERARCHY}
        public int Run() {{
            Shape shape = new Square(1, 3);
            if (shape is IArea area) {{ return area.Area(); }}
            return -1;
        }}"
    );
    assert_eq!(run_int(&source), 9);
}

#[test]
fn an_interface_binding_of_a_base_typed_polygon_runs_the_base_implementation() {
    let source = format!(
        "{HIERARCHY}
        public int Run() {{
            Shape shape = new Polygon(1, 5);
            Shape other = new Circle(1, 4);
            int total = 0;
            if (shape is IArea area) {{ total = total + area.Area(); }}
            if (other is IName name) {{ total = total + name.Name(); }}
            if (other is IArea impossible) {{ total = total + 100000; }}
            return total;
        }}"
    );
    assert_eq!(run_int(&source), 50 + 704);
}

#[test]
fn is_with_a_class_subject_and_a_statically_implemented_interface_is_always_true() {
    let source = format!(
        "{HIERARCHY}
        public int Run() {{
            Square square = new Square(1, 4);
            if (square is IArea area) {{ return area.Area(); }}
            return -1;
        }}"
    );
    assert_eq!(run_int(&source), 16);
}

#[test]
fn an_interface_subject_tests_and_narrows_back_to_a_class() {
    let source = format!(
        "{HIERARCHY}
        public int Probe(IArea area) {{
            int bits = 0;
            if (area is Square) {{ bits = bits + 1; }}
            if (area is Polygon) {{ bits = bits + 2; }}
            if (area is Shape) {{ bits = bits + 4; }}
            if (area is Circle) {{ bits = bits + 8; }}
            return bits;
        }}
        public int Edge(IArea area) {{
            if (area is Square square) {{ return square.edge * 100 + square.id; }}
            return -1;
        }}
        public int Run() {{
            Polygon polygon = new Square(7, 3);
            Polygon plain = new Polygon(1, 5);
            return Probe(polygon) * 10000 + Probe(plain) * 1000 + Edge(polygon) + Edge(plain);
        }}"
    );
    assert_eq!(run_int(&source), 7 * 10000 + 6 * 1000 + 307 - 1);
}

#[test]
fn an_interface_subject_tests_another_interface() {
    let source = format!(
        "{HIERARCHY}
        public class Both : Polygon, IName {{
            public Both() : base(1, 2) {{}}
            public int Name() {{ return 99; }}
        }}
        public int Run() {{
            IArea one = new Both();
            IArea two = new Square(1, 2);
            int total = 0;
            if (one is IName name) {{ total = total + name.Name(); }}
            if (two is IName) {{ total = total + 1000; }}
            return total;
        }}"
    );
    assert_eq!(run_int(&source), 99);
}

#[test]
fn is_without_a_binding_on_an_interface_subject_is_a_bool() {
    let source = format!(
        "{HIERARCHY}
        public bool Check(IArea area) {{ return area is IName; }}
        public int Run() {{
            if (Check(new Square(1, 1))) {{ return 1; }}
            return 0;
        }}"
    );
    assert_eq!(run_int(&source), 0);
}

#[test]
fn as_with_a_class_target_yields_some_for_a_matching_object_and_none_otherwise() {
    let body = r"
        public int Run() {
            Shape square = new Square(1, 3);
            Shape circle = new Circle(1, 2);
            int total = 0;
            Option<Polygon> polygon = square as Polygon;
            switch (polygon) {
                case Some(value): total = total + value.sides;
                case None: total = total + 1000;
            }
            Option<Polygon> none = circle as Polygon;
            switch (none) {
                case Some(value): total = total + 100;
                case None: total = total + 20;
            }
            return total;
        }";
    assert_eq!(run_project_int(body), 4 + 20);
}

#[test]
fn as_with_an_interface_target_yields_an_interface_value() {
    let body = r"
        public int Run() {
            Shape square = new Square(1, 3);
            Shape circle = new Circle(1, 2);
            int total = 0;
            switch (square as IArea) {
                case Some(area): total = total + area.Area();
                case None: total = total + 1000;
            }
            switch (circle as IArea) {
                case Some(area): total = total + 5000;
                case None: total = total + 20;
            }
            switch (circle as IName) {
                case Some(name): total = total + name.Name();
                case None: total = total + 100000;
            }
            return total;
        }";
    assert_eq!(run_project_int(body), 9 + 20 + 702);
}

#[test]
fn as_from_an_interface_value_to_a_class() {
    let body = r"
        public int Run() {
            IArea area = new Square(5, 6);
            int total = 0;
            switch (area as Square) {
                case Some(square): total = total + square.edge;
                case None: total = total + 1000;
            }
            switch (area as Polygon) {
                case Some(polygon): total = total + polygon.sides * 10;
                case None: total = total + 1000;
            }
            return total;
        }";
    assert_eq!(run_project_int(body), 6 + 40);
}

#[test]
fn a_trivial_upcast_as_is_allowed_and_always_some() {
    let body = r"
        public int Run() {
            Square square = new Square(5, 6);
            switch (square as Shape) {
                case Some(shape): return shape.id;
                case None: return -1;
            }
        }";
    assert_eq!(run_project_int(body), 5);
}

#[test]
fn as_with_an_interface_value_called_through_the_option_runs_the_override() {
    let body = r"
        public int Area(Shape shape) {
            switch (shape as IArea) {
                case Some(area): return area.Area();
                case None: return 0;
            }
        }
        public int Run() {
            return Area(new Square(1, 5)) + Area(new Polygon(1, 3)) + Area(new Circle(1, 3));
        }";
    assert_eq!(run_project_int(body), 25 + 30);
}

// ---------------------------------------------------------------------------
// Negative cases
// ---------------------------------------------------------------------------

#[test]
fn n8_an_impossible_as_between_unrelated_classes_is_an_error() {
    project_rejects(
        "public int Run() { Circle c = new Circle(1, 1); Option<Square> s = c as Square; return 0; }",
        "`as` can never succeed",
    );
}

#[test]
fn an_impossible_is_between_unrelated_classes_is_still_an_error() {
    project_rejects(
        "public bool Run() { Circle c = new Circle(1, 1); return c is Square; }",
        "pattern can never match",
    );
}

#[test]
fn a_sealed_class_that_does_not_implement_the_interface_can_never_match() {
    project_rejects(
        "public bool Run() { Sealed s = new Sealed(1); return s is IArea; }",
        "sealed class `Sealed` does not implement interface `IArea`",
    );
    project_rejects(
        "public int Run() { Sealed s = new Sealed(1); Option<IArea> a = s as IArea; return 0; }",
        "`as` can never succeed",
    );
}

#[test]
fn a_sealed_target_that_does_not_implement_the_interface_can_never_match() {
    project_rejects(
        "public bool Run(IArea area) { return area is Sealed; }",
        "sealed class `Sealed` does not implement interface `IArea`",
    );
}

#[test]
fn n9_an_implicit_base_to_interface_conversion_is_still_rejected() {
    rejects(
        &format!(
            "{HIERARCHY}
            public int Run() {{ Shape shape = new Square(1, 1); IArea area = shape; return 0; }}"
        ),
        "IArea",
    );
}

#[test]
fn an_implicit_downcast_is_still_rejected() {
    rejects(
        &format!(
            "{HIERARCHY}
            public int Run() {{ Shape shape = new Square(1, 1); Square s = shape; return 0; }}"
        ),
        "Square",
    );
}

#[test]
fn as_requires_the_official_option() {
    rejects(
        &format!(
            "{HIERARCHY}
            public int Run() {{ Shape shape = new Square(1, 1); var x = shape as Polygon; return 0; }}"
        ),
        "Option",
    );
}

// ---------------------------------------------------------------------------
// Counting configuration (MEM-EXP-1 C5/C6): bindings and `Option` results that
// hold references must stay balanced, so loop-carried objects are released.
// ---------------------------------------------------------------------------

#[cfg(feature = "mem-exp-rc-core")]
mod counting {
    use super::*;
    use aster_codegen_cranelift::{MemoryStats, execute_with_stats};

    fn run_counted(body: &str, expected: i32) -> MemoryStats {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "aster-interfaces-hierarchies-rc-{}-{id}.aster",
            std::process::id()
        ));
        std::fs::write(&path, format!("using aster.core;\n{HIERARCHY}\n{body}"))
            .expect("write temporary project");
        let compiled = compile_project(&path);
        std::fs::remove_file(&path).ok();
        let compilation = compiled.expect("project compiles").compilation;
        let (value, stats) = execute_with_stats(&compilation.mir, "Run").expect("source executes");
        assert_eq!(value, ExecutionValue::Int(expected));
        stats
    }

    #[test]
    fn interface_bindings_in_a_loop_release_every_object() {
        let stats = run_counted(
            r"
            public int Run() {
                int total = 0;
                for (int i = 0; i < 100; i += 1) {
                    Shape shape = new Square(i, 2);
                    if (shape is IArea area) { total = total + area.Area(); }
                    if (shape is IName name) { total = total + 1000; }
                }
                return total;
            }",
            400,
        );
        assert!(
            stats.object_heap.released_blocks >= 99,
            "objects leaked: {:?}",
            stats.object_heap
        );
        assert!(
            stats.object_heap.fresh_blocks < 20,
            "{:?}",
            stats.object_heap
        );
    }

    /// The counts of one run that must not differ between `value as T` and
    /// the equivalent explicit `Option<T>.Some(value)` construction.
    fn balance(stats: &MemoryStats) -> (i128, u64) {
        (
            i128::from(stats.object_heap.retains) - i128::from(stats.object_heap.releases),
            stats.object_heap.fresh_blocks,
        )
    }

    #[test]
    fn option_results_holding_references_are_counted_like_explicit_options() {
        // An `Option` whose payload is a class reference is the existing
        // enum-payload ownership path; `as` must add no retain or release of
        // its own to it, for a class payload and for an interface payload.
        let via_as = run_counted(
            r"
            public int Run() {
                int total = 0;
                for (int i = 0; i < 100; i += 1) {
                    Shape shape = new Square(i, 2);
                    Option<Polygon> polygon = shape as Polygon;
                    Option<IArea> area = shape as IArea;
                    switch (polygon) {
                        case Some(value): total = total + value.sides;
                        case None: total = total + 1000;
                    }
                    switch (area) {
                        case Some(value): total = total + value.Area();
                        case None: total = total + 1000;
                    }
                }
                return total;
            }",
            800,
        );
        let explicit = run_counted(
            r"
            public int Run() {
                int total = 0;
                for (int i = 0; i < 100; i += 1) {
                    Square square = new Square(i, 2);
                    Shape shape = square;
                    Option<Polygon> polygon = Option<Polygon>.Some(square);
                    IArea target = square;
                    Option<IArea> area = Option<IArea>.Some(target);
                    switch (polygon) {
                        case Some(value): total = total + value.sides;
                        case None: total = total + 1000;
                    }
                    switch (area) {
                        case Some(value): total = total + value.Area();
                        case None: total = total + 1000;
                    }
                }
                return total;
            }",
            800,
        );
        assert_eq!(
            balance(&via_as),
            balance(&explicit),
            "as: {:?}, explicit: {:?}",
            via_as.object_heap,
            explicit.object_heap
        );
    }

    #[test]
    fn a_failed_downcast_releases_every_object() {
        let stats = run_counted(
            r"
            public int Run() {
                int total = 0;
                for (int i = 0; i < 100; i += 1) {
                    Shape shape = new Circle(i, 2);
                    Option<Polygon> polygon = shape as Polygon;
                    Option<IArea> area = shape as IArea;
                    switch (polygon) {
                        case Some(value): total = total + 1000;
                        case None: total = total + 1;
                    }
                    switch (area) {
                        case Some(value): total = total + 1000;
                        case None: total = total + 1;
                    }
                }
                return total;
            }",
            200,
        );
        assert!(
            stats.object_heap.released_blocks >= 99,
            "objects leaked: {:?}",
            stats.object_heap
        );
        assert!(
            stats.object_heap.fresh_blocks < 20,
            "{:?}",
            stats.object_heap
        );
    }

    #[test]
    fn an_interface_subject_narrowed_back_to_a_class_stays_balanced() {
        let stats = run_counted(
            r"
            public int Edge(IArea area) {
                if (area is Square square) { return square.edge; }
                return 0;
            }
            public int Run() {
                int total = 0;
                for (int i = 0; i < 100; i += 1) {
                    Square square = new Square(i, 3);
                    total = total + Edge(square);
                }
                return total;
            }",
            300,
        );
        assert!(
            stats.object_heap.released_blocks >= 99,
            "objects leaked: {:?}",
            stats.object_heap
        );
    }
}
