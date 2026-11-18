//! T-LANG-8 (LANG-008): object initializers `new T(args) { A = 1 }`.
//! Semantic rules; execution is covered in the Cranelift crate.

fn messages(source: &str) -> Vec<String> {
    match aster_compiler::compile(source) {
        Ok(_) => panic!("expected a diagnostic for:\n{source}"),
        Err(diagnostics) => diagnostics
            .into_iter()
            .map(|diagnostic| diagnostic.message)
            .collect(),
    }
}

fn rejects(source: &str, expected: &str) {
    let messages = messages(source);
    assert!(
        messages.iter().any(|message| message.contains(expected)),
        "expected a diagnostic containing `{expected}`, found {messages:#?}"
    );
}

fn accepts(source: &str) {
    aster_compiler::compile(source)
        .unwrap_or_else(|diagnostics| panic!("expected a valid program: {diagnostics:#?}"));
}

const POINT: &str = r"
    public class Point
    {
        public int X;
        public int Y { get; set; }
        public int Id { get; init; }
        public int Fixed { get; }
        private int secret;
        public Point() {}
        public Point(int x) { X = x; }
    }
";

#[test]
fn fields_properties_and_init_properties_are_assignable() {
    accepts(&format!(
        "{POINT} public int Run() {{ Point p = new Point(1) {{ X = 2, Y = 3, Id = 4 }}; \
         Point q = new Point {{ Y = 1 }}; return p.X + p.Y + p.Id + q.Y; }}"
    ));
}

#[test]
fn a_target_typed_initializer_takes_its_type_from_the_context() {
    accepts(&format!(
        "{POINT} public int Run() {{ Point p = new() {{ X = 2, Id = 1 }}; return p.X + p.Id; }}"
    ));
}

#[test]
fn an_init_property_is_still_rejected_outside_initializers_and_constructors() {
    rejects(
        &format!("{POINT} public int Run() {{ Point p = new Point(); p.Id = 3; return p.Id; }}"),
        "can be assigned only in a constructor or an object initializer",
    );
}

#[test]
fn a_get_only_property_cannot_be_initialized() {
    rejects(
        &format!("{POINT} public int Run() {{ Point p = new Point {{ Fixed = 1 }}; return 0; }}"),
        "get-only auto-property `Point.Fixed`",
    );
}

#[test]
fn a_member_cannot_be_initialized_twice() {
    rejects(
        &format!(
            "{POINT} public int Run() {{ Point p = new Point {{ X = 1, Y = 2, X = 3 }}; return 0; }}"
        ),
        "member `X` is initialized more than once",
    );
}

#[test]
fn an_unknown_member_is_rejected() {
    let found = messages(&format!(
        "{POINT} public int Run() {{ Point p = new Point {{ Missing = 1 }}; return 0; }}"
    ));
    assert!(
        found
            .iter()
            .any(|message| message.contains("has no field `Missing`")),
        "{found:#?}"
    );
}

#[test]
fn an_inaccessible_member_is_rejected() {
    let found = messages(&format!(
        "{POINT} public int Run() {{ Point p = new Point {{ secret = 1 }}; return 0; }}"
    ));
    assert!(
        found
            .iter()
            .any(|message| message.contains("`Point.secret` is private")),
        "{found:#?}"
    );
}

#[test]
fn a_value_must_convert_to_the_member_type() {
    rejects(
        &format!(
            "{POINT} public int Run() {{ Point p = new Point {{ X = \"text\" }}; return 0; }}"
        ),
        "expected `int`, found `string`",
    );
}

#[test]
fn initializers_require_a_class() {
    rejects(
        "public struct S { public int X; } \
         public int Run() { var l = new List<int>() { X = 1 }; return 0; }",
        "object initializers require a class",
    );
}

#[test]
fn a_missing_parameterless_constructor_is_rejected() {
    rejects(
        "public class C { public int X; public C(int x) { X = x; } } \
         public int Run() { C c = new C { X = 1 }; return 0; }",
        "missing required argument `x`",
    );
}

#[test]
fn the_hidden_receiver_is_not_nameable() {
    rejects(
        &format!("{POINT} public int Run() {{ Point p = new Point {{ X = 1 }}; return $init.X; }}"),
        "unexpected character `$`",
    );
}
