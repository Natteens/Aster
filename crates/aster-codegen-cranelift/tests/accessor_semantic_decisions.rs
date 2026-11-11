//! Property accessor bodies receive every semantic decision recorded for them.
//!
//! Semantic analysis records decisions under the context of the synthesized
//! `get_`/`set_` function, keyed by the accessor's position. HIR lowering must
//! look them up under the same context (`semantic::accessor_context`).

use aster_codegen_cranelift::{ExecutionValue, execute};
use aster_compiler::compile;

fn run(source: &str) -> ExecutionValue {
    let compilation = compile(source).unwrap_or_else(|diagnostics| panic!("{diagnostics:?}"));
    execute(&compilation.mir, "Run").unwrap()
}

#[test]
fn small_integer_promotion_applies_inside_a_getter() {
    let source = "
public class Holder
{
    private byte small;
    public Holder(byte small) { this.small = small; }
    public int Doubled
    {
        get { return small + small; }
    }
}

public int Run()
{
    Holder holder = new Holder(200);
    return holder.Doubled;
}
";
    assert_eq!(run(source), ExecutionValue::Int(400));
}

#[test]
fn list_indexing_and_shifts_resolve_inside_getter_and_setter() {
    let source = "
public class Bag
{
    private List<int> items;
    private int last;
    public Bag(List<int> items) { this.items = items; }
    public int First
    {
        get { return items[0] << 2; }
        set { last = value; items[0] = value + 1; }
    }
    public int Last
    {
        get { return last; }
    }
}

public int Run()
{
    List<int> items = new List<int>();
    items.Add(5);
    Bag bag = new Bag(items);
    int before = bag.First;
    bag.First = 9;
    return before * 1000 + bag.First * 10 + bag.Last;
}
";
    // before = 5 << 2 = 20; after the setter items[0] = 10, so First = 40; Last = 9.
    assert_eq!(run(source), ExecutionValue::Int(20_409));
}
