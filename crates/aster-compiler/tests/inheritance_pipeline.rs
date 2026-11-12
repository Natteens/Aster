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

// ---------------------------------------------------------------------------
// T-OBJ-3: virtual dispatch reaches HIR and MIR.
// ---------------------------------------------------------------------------

const VIRTUAL_SOURCE: &str = "public class A { public A() {} \
    public virtual int F() { return 1; } public virtual int G() { return 2; } } \
    public class B : A { public B() {} \
    public override int F() { return 10; } public virtual int H() { return 3; } } \
    public abstract class S { public S() {} public abstract int Z(); } \
    public class T : S { public T() {} public override int Z() { return 4; } } \
    public int Run() { A a = new B(); return a.F() + a.G(); }";

fn class<'a>(module: &'a mir::Module, name: &str) -> &'a mir::ClassDefinition {
    module
        .classes
        .iter()
        .find(|class| class.name == name)
        .expect("class exists")
}

fn function_named<'a>(module: &'a mir::Module, name: &str) -> &'a mir::Function {
    module
        .functions
        .iter()
        .find(|function| function.name == name)
        .expect("function exists")
}

fn symbol_of(module: &mir::Module, owner: &str, name: &str) -> mir::SymbolId {
    let owner = class(module, owner).symbol;
    module
        .functions
        .iter()
        .find(|function| function.owner == Some(owner) && function.name == name)
        .expect("method exists")
        .symbol
}

#[test]
fn vtables_reuse_slots_for_overrides_and_append_slots_for_new_virtual_members() {
    let compilation = aster_compiler::compile(VIRTUAL_SOURCE).expect("compiles");
    let module = &compilation.mir;
    let (a, b) = (class(module, "A"), class(module, "B"));
    let (a_f, a_g) = (symbol_of(module, "A", "F"), symbol_of(module, "A", "G"));
    let (b_f, b_h) = (symbol_of(module, "B", "F"), symbol_of(module, "B", "H"));
    // A introduces F (slot 0) and G (slot 1).
    assert_eq!(
        a.vtable
            .iter()
            .map(|slot| (slot.method, slot.implementation))
            .collect::<Vec<_>>(),
        vec![(a_f, Some(a_f)), (a_g, Some(a_g))]
    );
    // B reuses slot 0 for its override (the slot keeps A.F as its identity),
    // inherits slot 1 and appends H.
    assert_eq!(
        b.vtable
            .iter()
            .map(|slot| (slot.method, slot.implementation))
            .collect::<Vec<_>>(),
        vec![(a_f, Some(b_f)), (a_g, Some(a_g)), (b_h, Some(b_h))]
    );
    // An abstract slot has no implementation until the concrete class has one.
    let (s, t) = (class(module, "S"), class(module, "T"));
    assert!(s.is_abstract && s.vtable.len() == 1 && s.vtable[0].implementation.is_none());
    assert_eq!(
        t.vtable[0].implementation,
        Some(symbol_of(module, "T", "Z"))
    );
    // An abstract method has no body, so no function either.
    assert!(
        !module
            .functions
            .iter()
            .any(|function| function.owner == Some(s.symbol) && function.name == "Z")
    );
}

#[test]
fn a_call_through_a_base_typed_receiver_is_a_virtual_call_and_base_calls_stay_direct() {
    let compilation = aster_compiler::compile(VIRTUAL_SOURCE).expect("compiles");
    let module = &compilation.mir;
    let run = function_named(module, "Run");
    let slots = run
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            mir::Instruction::CallVirtual { slot, method, .. } => Some((*slot, *method)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        slots,
        vec![
            (0, symbol_of(module, "A", "F")),
            (1, symbol_of(module, "A", "G"))
        ]
    );

    let source = "public class A { public A() {} public virtual int F() { return 1; } } \
        public class B : A { public B() {} public override int F() { return base.F() + 1; } }";
    let compilation = aster_compiler::compile(source).expect("compiles");
    let module = &compilation.mir;
    let override_body = symbol_of(module, "B", "F");
    let body = module
        .functions
        .iter()
        .find(|function| function.symbol == override_body)
        .expect("B.F");
    let mut instructions = body.blocks.iter().flat_map(|block| &block.instructions);
    let a_f = symbol_of(module, "A", "F");
    assert!(instructions.clone().any(
        |instruction| matches!(instruction, mir::Instruction::Call { function, .. } if *function == a_f)
    ));
    assert!(
        !instructions
            .any(|instruction| matches!(instruction, mir::Instruction::CallVirtual { .. }))
    );
}

#[test]
fn a_virtual_call_is_an_escape_of_its_receiver_until_it_is_devirtualized() {
    // The same shape with a non-virtual method may keep the object local; the
    // virtual call forces the allocation to stay persistent.
    let virtual_call = "public class A { public int x; public A() {} \
        public virtual int Get() { return x; } } \
        public int Run() { A a = new A(); return a.Get(); }";
    let direct_call = "public class A { public int x; public A() {} \
        public int Get() { return x; } } \
        public int Run() { A a = new A(); return a.Get(); }";
    let regions = |source: &str| {
        let compilation = aster_compiler::compile(source).expect("compiles");
        function_named(&compilation.mir, "Run")
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .filter_map(|instruction| match instruction {
                mir::Instruction::AllocateObject { region, .. } => Some(*region),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        regions(virtual_call),
        vec![mir::AllocationRegion::Persistent]
    );
    // With a statically bound call the analysis is free to place it locally.
    assert!(regions(direct_call).len() <= 1);
}

#[test]
fn local_object_elimination_never_removes_an_object_that_receives_a_virtual_call() {
    let source = "public class A { public int x = 4; public A() {} \
        public virtual int Get() { return x; } } \
        public class B : A { public B() {} public override int Get() { return x + 1; } } \
        public int Run() { A a = new B(); return a.Get(); }";
    let compilation = aster_compiler::compile(source).expect("compiles");
    let run = function_named(&compilation.mir, "Run");
    assert!(
        run.blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .any(|instruction| matches!(instruction, mir::Instruction::AllocateObject { .. })),
        "the object stays allocated so its descriptor decides the call"
    );
}

#[test]
fn an_interface_entry_for_a_virtual_implementation_dispatches_through_a_thunk() {
    let source = "public interface IScore { int Value(); } \
        public class Base : IScore { public Base() {} public virtual int Value() { return 1; } } \
        public sealed class Leaf : Base { public Leaf() {} public override int Value() { return 2; } } \
        public int Run() { Base b = new Leaf(); IScore s = b; return s.Value(); }";
    let compilation = aster_compiler::compile(source).expect("compiles");
    let module = &compilation.mir;
    let entry = |class_name: &str| {
        let class = class(module, class_name).symbol;
        module
            .interface_implementations
            .iter()
            .find(|implementation| implementation.class == class)
            .expect("implementation")
            .methods[0]
    };
    // Base is not sealed: its entry is a thunk that performs the virtual call.
    let thunk = module
        .functions
        .iter()
        .find(|function| function.symbol == entry("Base"))
        .expect("thunk");
    assert!(
        thunk.name.ends_with("$virtual")
            && matches!(
                thunk.blocks[0].instructions[0],
                mir::Instruction::CallVirtual { slot: 0, .. }
            )
    );
    // Leaf is sealed: its own override is exact.
    assert_eq!(entry("Leaf"), symbol_of(module, "Leaf", "Value"));
}
