//! T-OBJ-4 (object model B1, pull request 4): semantic rules of the checked
//! downcast `value as T` (an `Option<T>`) and of `is` with interface targets,
//! and the MIR they lower to.

use std::sync::atomic::{AtomicU64, Ordering};

use aster_compiler::{compile, compile_project, mir};

const CLASSES: &str = r"
    public interface IShape { int Area(); }
    public interface IOther { int Other(); }
    public class Shape { public int id; public Shape(int id) { this.id = id; } }
    public class Circle : Shape { public int radius; public Circle(int id, int radius) : base(id) { this.radius = radius; } }
    public class Square : Shape, IShape {
        public Square(int id) : base(id) {}
        public int Area() { return 4; }
    }
    public class Unrelated { public Unrelated() {} }
    public sealed class Leaf : Shape { public Leaf() : base(0) {} }
    public struct Point { public int x; }
";

fn write_project(body: &str) -> std::path::PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("aster-downcast-{}-{id}.aster", std::process::id()));
    std::fs::write(&path, format!("using aster.core;\n{CLASSES}\n{body}"))
        .expect("write temporary project");
    path
}

/// Compile a project that imports `aster.core`.
fn project(body: &str) -> Result<mir::Module, Vec<String>> {
    let path = write_project(body);
    let result = compile_project(&path);
    std::fs::remove_file(&path).ok();
    result
        .map(|compiled| compiled.compilation.mir)
        .map_err(|diagnostics| {
            diagnostics
                .into_iter()
                .map(|diagnostic| diagnostic.diagnostic.message)
                .collect()
        })
}

fn accepted(body: &str) -> mir::Module {
    project(body)
        .unwrap_or_else(|messages| panic!("expected the program to compile: {messages:#?}"))
}

fn rejected_with(body: &str, fragment: &str) {
    let messages = project(body).expect_err("source should be rejected");
    assert!(
        messages.iter().any(|message| message.contains(fragment)),
        "expected a diagnostic containing `{fragment}`, found {messages:?}"
    );
}

fn function<'a>(module: &'a mir::Module, name: &str) -> &'a mir::Function {
    module
        .functions
        .iter()
        .find(|function| function.name == name)
        .unwrap_or_else(|| panic!("function `{name}` exists"))
}

fn rvalues(function: &mir::Function) -> impl Iterator<Item = &mir::Rvalue> {
    function
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            mir::Instruction::Assign { value, .. } => Some(value),
            _ => None,
        })
}

// ---------------------------------------------------------------------------
// Acceptance and lowering
// ---------------------------------------------------------------------------

#[test]
fn as_accepts_class_and_interface_targets_and_a_trivial_upcast() {
    accepted(
        r"
        public Option<Circle> A(Shape s) { return s as Circle; }
        public Option<IShape> B(Shape s) { return s as IShape; }
        public Option<Shape> C(Circle c) { return c as Shape; }
        public Option<Circle> D(Circle c) { return c as Circle; }
        public Option<Square> E(IShape s) { return s as Square; }
        public Option<IOther> F(IShape s) { return s as IOther; }
        public Option<IShape> G(Square s) { return s as IShape; }
        ",
    );
}

#[test]
fn as_lowers_to_a_class_test_that_selects_some_or_none() {
    let module = accepted(r"public Option<Circle> A(Shape s) { return s as Circle; }");
    let function = function(&module, "A");
    let kinds = rvalues(function)
        .map(|value| &value.kind)
        .collect::<Vec<_>>();
    assert!(
        kinds
            .iter()
            .any(|kind| matches!(kind, mir::RvalueKind::ClassTest { .. })),
        "a downcast tests the class"
    );
    assert!(
        kinds
            .iter()
            .any(|kind| matches!(kind, mir::RvalueKind::Downcast(_))),
        "the payload is the identity downcast"
    );
    let constructs = kinds
        .iter()
        .filter(|kind| matches!(kind, mir::RvalueKind::EnumConstruct { .. }))
        .count();
    assert_eq!(constructs, 2, "one `Some` and one `None`");
}

#[test]
fn an_interface_target_lowers_to_a_test_and_a_conditional_lookup() {
    let module = accepted(
        r"public int A(Shape s) { if (s is IShape area) { return area.Area(); } return 0; }",
    );
    let function = function(&module, "A");
    let kinds = rvalues(function)
        .map(|value| &value.kind)
        .collect::<Vec<_>>();
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| matches!(kind, mir::RvalueKind::InterfaceTest { .. }))
            .count(),
        1
    );
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| matches!(kind, mir::RvalueKind::InterfaceLookup { .. }))
            .count(),
        1
    );
    // The lookup may fail at run time, so it is never in the block that runs
    // unconditionally with the test.
    let (test_block, lookup_block) = (
        function
            .blocks
            .iter()
            .position(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction,
                        mir::Instruction::Assign {
                            value: mir::Rvalue {
                                kind: mir::RvalueKind::InterfaceTest { .. },
                                ..
                            },
                            ..
                        }
                    )
                })
            })
            .expect("test block"),
        function
            .blocks
            .iter()
            .position(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction,
                        mir::Instruction::Assign {
                            value: mir::Rvalue {
                                kind: mir::RvalueKind::InterfaceLookup { .. },
                                ..
                            },
                            ..
                        }
                    )
                })
            })
            .expect("lookup block"),
    );
    assert_ne!(test_block, lookup_block);
}

#[test]
fn an_interface_subject_pattern_to_a_class_narrows_the_object_half() {
    let module =
        accepted(r"public int A(IShape s) { if (s is Square q) { return q.id; } return 0; }");
    let function = function(&module, "A");
    assert!(rvalues(function).any(|value| matches!(
        &value.kind,
        mir::RvalueKind::Downcast(operand) if matches!(operand.type_, mir::Type::Interface(_))
    )));
}

// ---------------------------------------------------------------------------
// Rejections
// ---------------------------------------------------------------------------

#[test]
fn an_impossible_as_between_unrelated_classes_is_an_error() {
    rejected_with(
        r"public Option<Unrelated> A(Shape s) { return s as Unrelated; }",
        "`as` can never succeed",
    );
    rejected_with(
        r"public Option<Circle> A(Square s) { return s as Circle; }",
        "`as` can never succeed",
    );
}

#[test]
fn as_with_a_sealed_class_that_cannot_implement_the_interface_is_an_error() {
    rejected_with(
        r"public Option<IShape> A(Leaf s) { return s as IShape; }",
        "sealed class `Leaf` does not implement interface `IShape`",
    );
    rejected_with(
        r"public Option<Leaf> A(IShape s) { return s as Leaf; }",
        "sealed class `Leaf` does not implement interface `IShape`",
    );
}

#[test]
fn as_requires_a_class_or_interface_on_both_sides() {
    rejected_with(
        r"public Option<Point> A(Shape s) { return s as Point; }",
        "target must be a class",
    );
    rejected_with(
        r"public Option<Circle> A(int n) { return n as Circle; }",
        "requires a class-typed value",
    );
    rejected_with(
        r"public Option<Circle> A(Point p) { return p as Circle; }",
        "requires a class-typed value",
    );
    rejected_with(
        r"public Option<int> A(Shape s) { return s as Missing; }",
        "unknown type `Missing`",
    );
}

#[test]
fn as_does_not_unwrap_an_option() {
    rejected_with(
        r"public Option<Circle> A(Option<Shape> s) { return s as Circle; }",
        "`Option<T>` with `as` is not supported yet",
    );
}

#[test]
fn the_result_of_as_is_an_option_not_the_target_type() {
    rejected_with(
        r"public Circle A(Shape s) { Circle c = s as Circle; return c; }",
        "Circle",
    );
}

#[test]
fn as_without_the_core_library_is_a_clear_error() {
    let messages = compile(&format!(
        "{CLASSES}\npublic int A(Shape s) {{ var c = s as Circle; return 0; }}"
    ))
    .expect_err("`as` needs the official Option")
    .into_iter()
    .map(|diagnostic| diagnostic.message)
    .collect::<Vec<_>>();
    assert!(
        messages.iter().any(|message| message.contains("Option")),
        "{messages:?}"
    );
}

#[test]
fn a_pattern_binding_from_an_interface_test_is_scoped_like_a_class_binding() {
    rejected_with(
        r"public int A(Shape s) { if (s is IShape area) { } return area.Area(); }",
        "area",
    );
    accepted(
        r"public int A(Shape s) { if (!(s is IShape area)) { return 0; } return area.Area(); }",
    );
}
