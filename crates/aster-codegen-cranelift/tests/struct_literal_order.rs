//! A struct literal may name its fields in any order. Field expressions run in
//! source order; the value is laid out in declaration order.

use aster_codegen_cranelift::{ExecutionValue, execute};
use aster_compiler::compile;

fn run(source: &str) -> ExecutionValue {
    let compilation = compile(source).unwrap_or_else(|diagnostics| panic!("{diagnostics:?}"));
    execute(&compilation.mir, "Run").unwrap()
}

#[test]
fn fields_named_out_of_declaration_order_execute() {
    let source = "
public struct Position { public int x; public int y; }

public int Run()
{
    Position p = Position { y: 2, x: 1 };
    return p.x * 10 + p.y;
}
";
    assert_eq!(run(source), ExecutionValue::Int(12));
}

#[test]
fn field_expressions_are_evaluated_in_source_order() {
    let source = "
public class Counter
{
    private int value;
    public int Next() { value = value + 1; return value; }
}

public struct Position { public int x; public int y; }

public int Run()
{
    Counter counter = new Counter();
    Position p = Position { y: counter.Next(), x: counter.Next() };
    return p.x * 10 + p.y;
}
";
    // `y` is evaluated first (1), then `x` (2).
    assert_eq!(run(source), ExecutionValue::Int(21));
}
