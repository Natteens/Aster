//! T-OBJ-2: inheritance reaches HIR and MIR with the C# construction order.

use aster_mir as mir;

const SOURCE: &str = "public class A { public int a = 1; public A() {} } \
    public class B : A { public int b = 2; public B(int x) : base() { b += x; } } \
    public int Run() { A up = new B(1); return up.a; }";

#[test]
fn class_definitions_carry_base_and_flags() {
    let compilation = aster_compiler::compile(
        "public abstract class A { public A() {} } public sealed class B : A { public B() {} }",
    )
    .expect("compiles");
    let classes = &compilation.mir.classes;
    assert_eq!(classes[1].base, Some(classes[0].symbol));
    assert!(classes[0].is_abstract && classes[1].is_sealed);
}

#[test]
fn constructor_runs_own_initializers_then_base_constructor_then_body() {
    let compilation = aster_compiler::compile(SOURCE).expect("compiles");
    let derived = compilation
        .mir
        .classes
        .iter()
        .find(|class| class.name == "B")
        .expect("B");
    let ctor = compilation
        .mir
        .functions
        .iter()
        .find(|function| function.constructor && function.owner == Some(derived.symbol))
        .expect("B constructor");
    let instructions = &ctor.blocks[0].instructions;
    let init = instructions
        .iter()
        .position(|i| matches!(i, mir::Instruction::Assign { target: mir::Place::ObjectField { field, .. }, .. } if *field == derived.fields[0].symbol))
        .expect("initializer store");
    let base_call = instructions
        .iter()
        .position(|i| matches!(i, mir::Instruction::Call { function, .. } if compilation.mir.functions.iter().any(|f| f.symbol == *function && f.constructor && f.owner != Some(derived.symbol))))
        .expect("base constructor call");
    let upcast = instructions
        .iter()
        .position(|i| {
            matches!(
                i,
                mir::Instruction::Assign {
                    value: mir::Rvalue {
                        kind: mir::RvalueKind::Upcast(_),
                        ..
                    },
                    ..
                }
            )
        })
        .expect("receiver upcast");
    assert!(init < upcast && upcast < base_call);
    // The declared body (`b += x`) runs after the base constructor.
    let body = instructions
        .iter()
        .rposition(|i| {
            matches!(
                i,
                mir::Instruction::Assign {
                    target: mir::Place::ObjectField { .. },
                    ..
                }
            )
        })
        .expect("body store");
    assert!(body > base_call);
}

#[test]
fn escape_analysis_follows_an_upcast_alias() {
    // The upcast alias is returned, so the allocation must stay persistent.
    let compilation = aster_compiler::compile(
        "public class A { public A() {} } public class B : A { public B() {} } \
         public A Make() { B b = new B(); A a = b; return a; }",
    )
    .expect("compiles");
    assert!(compilation.mir.functions.iter().flat_map(|f| &f.blocks).flat_map(|b| &b.instructions).all(|i| !matches!(
        i,
        mir::Instruction::AllocateObject { class, region: mir::AllocationRegion::Temporary, .. }
            if compilation.mir.classes.iter().any(|c| c.symbol == *class && c.name == "B")
    )));
}
