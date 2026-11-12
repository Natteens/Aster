//! T-LANG-4b: semantic rules of `is T x` class type patterns and `when`
//! guards, and the MIR they lower to.

use std::sync::atomic::{AtomicU64, Ordering};

use aster_compiler::{compile, compile_project, mir};

fn messages(source: &str) -> Vec<String> {
    compile(source)
        .expect_err("source should be rejected")
        .into_iter()
        .map(|diagnostic| diagnostic.message)
        .collect()
}

fn rejected_with(source: &str, fragment: &str) {
    let messages = messages(source);
    assert!(
        messages.iter().any(|message| message.contains(fragment)),
        "expected a diagnostic containing `{fragment}`, found {messages:?}"
    );
}

fn accepted(source: &str) {
    if let Err(diagnostics) = compile(source) {
        panic!("expected the program to compile, got {diagnostics:#?}");
    }
}

const CLASSES: &str = r"
    public class Shape { public int id; public Shape(int id) { this.id = id; } }
    public class Circle : Shape { public int radius; public Circle(int id, int radius) : base(id) { this.radius = radius; } }
    public class Square : Shape { public int edge; public Square(int id, int edge) : base(id) { this.edge = edge; } }
    public class Unrelated { public Unrelated() {} }
    public interface IShape { int Area(); }
    public struct Point { public int x; }
";

fn program(body: &str) -> String {
    format!("{CLASSES}\n{body}")
}

// ---------------------------------------------------------------------------
// Acceptance and lowering
// ---------------------------------------------------------------------------

#[test]
fn accepts_patterns_in_every_condition_position() {
    accepted(&program(
        r"
        public int A(Shape s) { if (s is Circle c) { return c.radius; } return 0; }
        public int B(Shape s) { int n = 0; while (s is Circle c) { n = n + c.radius; s = new Shape(0); } return n; }
        public bool C(Shape s) { return s is Circle c && c.radius > 1; }
        public bool D(Shape s) { return s is Circle || s is Square; }
        public bool E(Shape s) { return !(s is Circle); }
        public int F(Shape s) { if (!(s is Circle c)) { return 0; } return c.radius; }
        public bool G(Shape s) { return s is Circle; }
        ",
    ));
}

#[test]
fn lowers_to_a_class_test_and_an_identity_downcast_without_a_copy() {
    let compilation = compile(&program(
        r"public int Radius(Shape s) { if (s is Circle c) { return c.radius; } return 0; }",
    ))
    .expect("compiles");
    let function = compilation
        .mir
        .functions
        .iter()
        .find(|function| function.name == "Radius")
        .expect("function MIR");
    let kinds = function
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            mir::Instruction::Assign { value, .. } => Some(&value.kind),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        kinds
            .iter()
            .any(|kind| matches!(kind, mir::RvalueKind::ClassTest { .. })),
        "a ClassTest rvalue is emitted"
    );
    assert!(
        kinds
            .iter()
            .any(|kind| matches!(kind, mir::RvalueKind::Downcast(_))),
        "the binding is an identity downcast"
    );
    assert!(
        !function
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .any(|instruction| matches!(instruction, mir::Instruction::AllocateObject { .. })),
        "a pattern never allocates or copies an object"
    );
}

#[test]
fn is_and_when_stay_ordinary_identifiers() {
    accepted(
        r"
        public int Run() {
            int is = 1;
            int when = 2;
            switch (when) {
                case 2 when is == 1: return is + when;
                default: return 0;
            }
        }
        ",
    );
}

#[test]
fn guards_lower_to_a_branch_that_falls_to_the_next_case() {
    let compilation = compile(
        r"
        public int Pick(int v, int limit) {
            switch (v) {
                case 1 when limit > 3: return 1;
                case 1: return 2;
                default: return 3;
            }
        }
        ",
    )
    .expect("compiles");
    let function = compilation
        .mir
        .functions
        .iter()
        .find(|function| function.name == "Pick")
        .expect("function MIR");
    let branches = function
        .blocks
        .iter()
        .filter(|block| matches!(block.terminator, mir::Terminator::Branch { .. }))
        .count();
    // Two label tests plus one guard.
    assert!(
        branches >= 3,
        "expected at least 3 branches, found {branches}"
    );
}

// ---------------------------------------------------------------------------
// Definite assignment and scope
// ---------------------------------------------------------------------------

#[test]
fn rejects_use_after_an_if_whose_other_path_continues() {
    rejected_with(
        &program(r"public int Run(Shape s) { if (s is Circle c) { } return c.radius; }"),
        "variable `c` is used before initialization",
    );
}

#[test]
fn rejects_use_in_the_branch_where_the_test_failed() {
    rejected_with(
        &program(
            r"public int Run(Shape s) { if (s is Circle c) { return 1; } else { return c.radius; } }",
        ),
        "variable `c` is used before initialization",
    );
    rejected_with(
        &program(
            r"public int Run(Shape s) { if (!(s is Circle c)) { return c.radius; } return 0; }",
        ),
        "variable `c` is used before initialization",
    );
}

#[test]
fn rejects_use_on_the_right_of_or_and_after_a_stored_test() {
    rejected_with(
        &program(r"public bool Run(Shape s) { return s is Circle c || c.radius > 0; }"),
        "variable `c` is used before initialization",
    );
    rejected_with(
        &program(r"public int Run(Shape s) { bool b = s is Circle c; return c.radius; }"),
        "variable `c` is used before initialization",
    );
}

#[test]
fn accepts_use_where_c_sharp_proves_assignment() {
    accepted(&program(
        r"
        public bool A(Shape s) { return !(s is Circle c) || c.radius > 0; }
        public int B(Shape s) { if (!(s is Circle c)) { return 0; } else { return c.radius; } }
        public int C(Shape s) { if (s is Circle c && c.radius > 0) { return c.radius; } return 0; }
        public int D(Shape s) { if (s is Circle a || s is Circle b) { return 1; } return 0; }
        public int E(Shape s) { if (!(s is Circle c)) { return 0; } return c.radius; }
        ",
    ));
}

#[test]
fn rejects_a_variable_assigned_on_only_one_side_of_or() {
    rejected_with(
        &program(
            r"public int Run(Shape s) { if (s is Circle c || s is Square q) { return c.radius; } return 0; }",
        ),
        "variable `c` is used before initialization",
    );
}

#[test]
fn loop_condition_variable_is_scoped_to_the_loop() {
    rejected_with(
        &program(
            r"public int Run(Shape s) { while (s is Circle c) { s = new Shape(0); } return c.radius; }",
        ),
        "unknown name `c`",
    );
}

#[test]
fn rejects_duplicate_pattern_variables() {
    rejected_with(
        &program(
            r"public int Run(Shape s) { if (s is Circle c) { } if (s is Circle c) { } return 0; }",
        ),
        "duplicate name `c`",
    );
}

// ---------------------------------------------------------------------------
// Impossible and unsupported patterns
// ---------------------------------------------------------------------------

#[test]
fn rejects_patterns_that_can_never_match() {
    rejected_with(
        &program(r"public bool Run(Circle c) { return c is Square; }"),
        "pattern can never match",
    );
    rejected_with(
        &program(r"public bool Run(Shape s) { return s is Unrelated u; }"),
        "pattern can never match",
    );
}

#[test]
fn rejects_interface_targets_with_a_clear_not_supported_yet() {
    rejected_with(
        &program(r"public bool Run(Shape s) { return s is IShape i; }"),
        "not supported yet",
    );
}

#[test]
fn rejects_non_class_subjects_and_targets() {
    rejected_with(
        &program(r"public bool Run(int n) { return n is Circle; }"),
        "requires a class-typed value",
    );
    rejected_with(
        &program(r"public bool Run(Shape s) { return s is Point; }"),
        "target must be a class",
    );
    rejected_with(
        &program(r"public bool Run(Shape s) { return s is int; }"),
        "target must be a class",
    );
    rejected_with(
        &program(r"public bool Run(Shape s) { return s is Missing m; }"),
        "unknown type `Missing`",
    );
}

#[test]
fn option_unwrap_patterns_are_not_supported_yet() {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "aster-patterns-option-{}-{id}.aster",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "using aster.core;\n\
         public int Run(Option<int> o) { if (o is int x) { return x; } return 0; }",
    )
    .expect("write temporary project");
    let result = compile_project(&path);
    std::fs::remove_file(&path).ok();
    let messages = match result {
        Ok(_) => Vec::new(),
        Err(diagnostics) => diagnostics
            .into_iter()
            .map(|d| d.diagnostic.message)
            .collect(),
    };
    assert!(
        messages
            .iter()
            .any(|message| message.contains("`Option<T>` with `is` is not supported yet")),
        "unexpected diagnostics: {messages:?}"
    );
}

// ---------------------------------------------------------------------------
// Guards
// ---------------------------------------------------------------------------

#[test]
fn guard_must_be_bool() {
    rejected_with(
        r"
        public int Run(int v) {
            switch (v) { case 1 when 5: return 1; default: return 0; }
        }
        ",
        "`when` condition must be `bool`",
    );
}

#[test]
fn guarded_case_does_not_make_a_later_duplicate_an_error_but_subsumption_is() {
    accepted(
        r"
        public int Run(int v, bool g) {
            switch (v) { case 1 when g: return 1; case 1: return 2; default: return 0; }
        }
        ",
    );
    rejected_with(
        r"
        public int Run(int v, bool g) {
            switch (v) { case 1: return 1; case 1 when g: return 2; default: return 0; }
        }
        ",
        "duplicate",
    );
}

#[test]
fn guarded_enum_case_does_not_count_for_exhaustiveness() {
    rejected_with(
        r"
        public enum E { A, B }
        public int Run(E e, bool g) {
            switch (e) { case A when g: return 1; case B: return 2; }
        }
        ",
        "missing case(s): A",
    );
    accepted(
        r"
        public enum E { A, B }
        public int Run(E e, bool g) {
            switch (e) { case A when g: return 1; case A: return 3; case B: return 2; }
        }
        ",
    );
}

#[test]
fn guard_sees_enum_payload_bindings_but_not_other_cases_bindings() {
    accepted(
        r"
        public enum E { A(int x), B }
        public int Run(E e) {
            return e switch { A(x) when x > 1 => x, A(x) => 0, B => 9 };
        }
        ",
    );
    rejected_with(
        r"
        public enum E { A(int x), B }
        public int Run(E e) {
            return e switch { A(x) => x, B when x > 1 => 9 };
        }
        ",
        "unknown name `x`",
    );
}

#[test]
fn guard_pattern_variable_is_assigned_in_the_arm_only() {
    accepted(&program(
        r"
        public int Run(int tag, Shape s) {
            return tag switch {
                1 when s is Circle c => c.radius,
                default => 0,
            };
        }
        ",
    ));
    rejected_with(
        &program(
            r"
            public int Run(int tag, Shape s) {
                return tag switch {
                    1 when s is Circle c => 1,
                    default => c.radius,
                };
            }
            ",
        ),
        "unknown name `c`",
    );
}
