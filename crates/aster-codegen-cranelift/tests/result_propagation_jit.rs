use aster_codegen_cranelift::{ExecutionValue, execute};
use aster_compiler::compile_project;

fn run(source: &str, function: &str) -> Result<ExecutionValue, String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("aster-try-jit-{}-{id}.aster", std::process::id()));
    std::fs::write(&path, source).expect("write temporary project");
    let compilation = compile_project(&path).map_err(|diagnostics| format!("{diagnostics:#?}"));
    std::fs::remove_file(&path).ok();
    execute(&compilation?.compilation.mir, function).map_err(|error| error.to_string())
}

fn int(source: &str) -> ExecutionValue {
    run(source, "Run").expect("program runs")
}

const PARSE: &str = "public Result<int, string> Parse(string text) {\n\
    if (text == \"42\") { return Result<int, string>.Ok(42); }\n\
    return Result<int, string>.Error(\"bad\"); }\n";

#[test]
fn propagates_ok_value() {
    let source = format!(
        "using aster.core;\n{PARSE}\n\
         public Result<int, string> Calc() {{ int v = Parse(\"42\")?; return Result<int, string>.Ok(v); }}\n\
         public int Run() {{ switch (Calc()) {{ case Ok(v): return v; case Error(e): return -1; }} }}"
    );
    assert_eq!(int(&source), ExecutionValue::Int(42));
}

#[test]
fn propagates_error_early() {
    let source = format!(
        "using aster.core;\n{PARSE}\n\
         public Result<int, string> Calc() {{ int v = Parse(\"no\")?; return Result<int, string>.Ok(v); }}\n\
         public int Run() {{ switch (Calc()) {{ case Ok(v): return v; case Error(e): return -1; }} }}"
    );
    assert_eq!(int(&source), ExecutionValue::Int(-1));
}

#[test]
fn chains_two_propagations() {
    let source = format!(
        "using aster.core;\n{PARSE}\n\
         public Result<int, string> Ensure(int value) {{\n\
             if (value == 42) {{ return Result<int, string>.Ok(value); }}\n\
             return Result<int, string>.Error(\"nope\"); }}\n\
         public Result<int, string> Calc() {{ int a = Parse(\"42\")?; int b = Ensure(a)?; return Result<int, string>.Ok(b); }}\n\
         public int Run() {{ switch (Calc()) {{ case Ok(v): return v; case Error(e): return -1; }} }}"
    );
    assert_eq!(int(&source), ExecutionValue::Int(42));
}

#[test]
fn chains_three_propagations() {
    let source = format!(
        "using aster.core;\n{PARSE}\n\
         public Result<int, string> Ensure(int value) {{ return Result<int, string>.Ok(value); }}\n\
         public Result<int, string> Calc() {{\n\
             int a = Parse(\"42\")?; int b = Ensure(a)?; int c = Ensure(b)?;\n\
             return Result<int, string>.Ok(c); }}\n\
         public int Run() {{ switch (Calc()) {{ case Ok(v): return v; case Error(e): return -1; }} }}"
    );
    assert_eq!(int(&source), ExecutionValue::Int(42));
}

#[test]
fn propagation_inside_call_argument() {
    let source = format!(
        "using aster.core;\n{PARSE}\n\
         public int Identity(int value) {{ return value; }}\n\
         public Result<int, string> Calc() {{ return Result<int, string>.Ok(Identity(Parse(\"42\")?)); }}\n\
         public int Run() {{ switch (Calc()) {{ case Ok(v): return v; case Error(e): return -1; }} }}"
    );
    assert_eq!(int(&source), ExecutionValue::Int(42));
}

#[test]
fn propagation_in_arithmetic() {
    let source = format!(
        "using aster.core;\n{PARSE}\n\
         public Result<int, string> Calc() {{ return Result<int, string>.Ok(Parse(\"42\")? + 8); }}\n\
         public int Run() {{ switch (Calc()) {{ case Ok(v): return v; case Error(e): return -1; }} }}"
    );
    assert_eq!(int(&source), ExecutionValue::Int(50));
}

#[test]
fn propagation_in_class_method() {
    let source = "using aster.core;\n\
        public class Reader {\n\
            public Reader() { }\n\
            public Result<int, string> Parse(string text) {\n\
                if (text == \"42\") { return Result<int, string>.Ok(42); }\n\
                return Result<int, string>.Error(\"bad\"); }\n\
            public Result<int, string> Calc(string text) {\n\
                int v = this.Parse(text)?;\n\
                return Result<int, string>.Ok(v); } }\n\
        public int Run() {\n\
            Reader reader = new Reader();\n\
            switch (reader.Calc(\"42\")) { case Ok(v): return v; case Error(e): return -1; } }";
    assert_eq!(int(source), ExecutionValue::Int(42));
}

#[test]
fn propagation_in_generic_function() {
    let source = format!(
        "using aster.core;\n{PARSE}\n\
         public Result<int, string> Tagged<T>(string text, T unused) {{\n\
             int v = Parse(text)?; return Result<int, string>.Ok(v); }}\n\
         public int Run() {{ switch (Tagged<bool>(\"42\", true)) {{ case Ok(v): return v; case Error(e): return -1; }} }}"
    );
    assert_eq!(int(&source), ExecutionValue::Int(42));
}

#[test]
fn error_type_struct_propagates() {
    let source = "using aster.core;\n\
        public struct Failure { public int code; }\n\
        public Result<int, Failure> Parse() { return Result<int, Failure>.Error(Failure { code: 9 }); }\n\
        public Result<int, Failure> Calc() { int v = Parse()?; return Result<int, Failure>.Ok(v); }\n\
        public int Run() { switch (Calc()) { case Ok(v): return v; case Error(e): return e.code; } }";
    assert_eq!(int(source), ExecutionValue::Int(9));
}

#[test]
fn error_type_enum_propagates() {
    let source = "using aster.core;\n\
        public enum Kind { Bad(int code), Worse }\n\
        public Result<int, Kind> Parse() { return Result<int, Kind>.Error(Kind.Bad(7)); }\n\
        public Result<int, Kind> Calc() { int v = Parse()?; return Result<int, Kind>.Ok(v); }\n\
        public int Run() { switch (Calc()) {\n\
            case Ok(v): return v;\n\
            case Error(k): switch (k) { case Bad(code): return code; case Worse: return -1; } } }";
    assert_eq!(int(source), ExecutionValue::Int(7));
}

#[test]
fn success_type_struct_propagates() {
    let source = "using aster.core;\n\
        public struct Point { public int x; public int y; }\n\
        public Result<Point, string> Make() { return Result<Point, string>.Ok(Point { x: 42, y: 0 }); }\n\
        public Result<int, string> Calc() { Point p = Make()?; return Result<int, string>.Ok(p.x); }\n\
        public int Run() { switch (Calc()) { case Ok(v): return v; case Error(e): return -1; } }";
    assert_eq!(int(source), ExecutionValue::Int(42));
}

#[test]
fn success_type_class_propagates() {
    let source = "using aster.core;\n\
        public class Box { public int value; public Box(int v) { value = v; } }\n\
        public Result<Box, string> Make() { return Result<Box, string>.Ok(new Box(42)); }\n\
        public Result<int, string> Calc() { Box b = Make()?; return Result<int, string>.Ok(b.value); }\n\
        public int Run() { switch (Calc()) { case Ok(v): return v; case Error(e): return -1; } }";
    assert_eq!(int(source), ExecutionValue::Int(42));
}

#[test]
fn success_type_array_propagates() {
    let source = "using aster.core;\n\
        public Result<int[], string> Make() { int[] a = [42, 1, 2]; return Result<int[], string>.Ok(a); }\n\
        public Result<int, string> Calc() { int[] a = Make()?; return Result<int, string>.Ok(a[0]); }\n\
        public int Run() { switch (Calc()) { case Ok(v): return v; case Error(e): return -1; } }";
    assert_eq!(int(source), ExecutionValue::Int(42));
}

#[test]
fn operand_is_evaluated_exactly_once() {
    // The counter increments once per `Step` call; if `?` evaluated its operand
    // twice, the observed count would be 2.
    let source = "using aster.core;\n\
        public class Counter {\n\
            private int count;\n\
            public Counter() { count = 0; }\n\
            public Result<int, string> Step() { count = count + 1; return Result<int, string>.Ok(count); }\n\
            public Result<int, string> Once() { int first = this.Step()?; return Result<int, string>.Ok(count); } }\n\
        public int Run() {\n\
            Counter counter = new Counter();\n\
            switch (counter.Once()) { case Ok(n): return n; case Error(e): return -1; } }";
    assert_eq!(int(source), ExecutionValue::Int(1));
}

#[test]
fn invalid_propagation_does_not_panic() {
    // A `?` on a non-Result value must surface as a diagnostic, never a Rust panic.
    let source = "using aster.core;\n\
        public Result<int, string> F() { int v = 42?; return Result<int, string>.Ok(v); }";
    assert!(run(source, "F").is_err());
}
