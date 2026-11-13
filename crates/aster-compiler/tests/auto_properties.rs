//! T-LANG-5 (LANG-005): auto-properties. A private, unnameable backing field
//! plus trivial accessors; statically bound ones are lowered straight to the
//! field, `virtual` ones keep dispatching through their accessors.

use aster_mir as mir;

fn compile_valid(source: &str) -> aster_compiler::Compilation {
    aster_compiler::compile(source)
        .unwrap_or_else(|diagnostics| panic!("expected a valid program: {diagnostics:#?}"))
}

/// Unoptimized MIR: the optimizer may scalar-replace a non-escaping object,
/// which would hide the field accesses these tests assert.
fn compile_unoptimized(source: &str) -> aster_compiler::Compilation {
    aster_compiler::compile_without_mir_optimizer_for_research(source)
        .unwrap_or_else(|diagnostics| panic!("expected a valid program: {diagnostics:#?}"))
}

fn errors(source: &str) -> Vec<String> {
    match aster_compiler::compile(source) {
        Ok(_) => panic!("expected a diagnostic for:\n{source}"),
        Err(diagnostics) => diagnostics
            .into_iter()
            .map(|diagnostic| diagnostic.message)
            .collect(),
    }
}

fn rejects(source: &str, expected: &str) {
    let messages = errors(source);
    assert!(
        messages.iter().any(|message| message.contains(expected)),
        "expected a diagnostic containing `{expected}`, found {messages:#?}"
    );
}

fn function<'a>(compilation: &'a aster_compiler::Compilation, name: &str) -> &'a mir::Function {
    compilation
        .mir
        .functions
        .iter()
        .find(|function| function.name == name && function.owner.is_none())
        .unwrap_or_else(|| panic!("function `{name}`"))
}

fn instructions(function: &mir::Function) -> impl Iterator<Item = &mir::Instruction> {
    function
        .blocks
        .iter()
        .flat_map(|block| block.instructions.iter())
}

const COUNTER: &str = r"
    public class Counter
    {
        public int Total { get; set; }
        public Counter() {}
    }
    // The object arrives as a parameter, so local object elimination cannot
    // turn the accesses into scalar locals: they stay field accesses.
    public int Bump(Counter counter)
    {
        counter.Total = 4;
        counter.Total += 3;
        return counter.Total;
    }
    public int Run() { return Bump(new Counter()); }
";

#[test]
fn a_non_virtual_auto_property_is_a_field_access_in_mir() {
    let compilation = compile_unoptimized(COUNTER);
    let counter = compilation
        .mir
        .classes
        .iter()
        .find(|class| class.name == "Counter")
        .expect("Counter");
    // The backing field is a real private field with an unnameable name.
    let backing = counter
        .fields
        .iter()
        .find(|field| field.name == "<Total>k__BackingField")
        .expect("backing field");
    let run = function(&compilation, "Bump");
    // No accessor is called...
    let accessor_calls = instructions(run)
        .filter(|instruction| match instruction {
            mir::Instruction::Call { function, .. } => compilation
                .mir
                .functions
                .iter()
                .any(|callee| callee.symbol == *function && callee.name == "Total"),
            _ => false,
        })
        .count();
    assert_eq!(accessor_calls, 0, "accessor calls remain in Run");
    // ...the field is stored and loaded directly.
    let stores = instructions(run)
        .filter(|instruction| {
            matches!(
                instruction,
                mir::Instruction::Assign {
                    target: mir::Place::ObjectField { field, .. },
                    ..
                } if *field == backing.symbol
            )
        })
        .count();
    assert!(stores >= 2, "expected direct stores, found {stores}");
    // Reads appear as operands (instruction values or the terminator), so
    // search the debug rendering for a copy of the backing field.
    let rendered = format!("{:?}", run.blocks);
    let load = format!("field: {:?} }})", backing.symbol);
    let loads = rendered
        .matches("Copy(ObjectField")
        .count()
        .min(rendered.matches(&load).count());
    assert!(loads > 0, "expected a direct field load in {rendered}");
}

#[test]
fn a_virtual_auto_property_keeps_its_accessor_calls() {
    let compilation = compile_unoptimized(
        r"
        public class Shape
        {
            public virtual int Sides { get; set; }
            public Shape() {}
        }
        public int Touch(Shape shape)
        {
            shape.Sides = 3;
            return shape.Sides;
        }
        public int Run() { return Touch(new Shape()); }
    ",
    );
    let run = function(&compilation, "Touch");
    let accessor_calls = instructions(run)
        .filter(|instruction| match instruction {
            mir::Instruction::CallVirtual { method, .. } => compilation
                .mir
                .functions
                .iter()
                .any(|callee| callee.symbol == *method && callee.name == "Sides"),
            _ => false,
        })
        .count();
    assert_eq!(accessor_calls, 2, "virtual accessors must stay calls");
}

#[test]
fn accepted_forms() {
    for source in [
        "public class C { public int X { get; set; } public C() {} }",
        "public class C { public int X { get; } public C() { X = 1; } }",
        "public class C { public int X { get; } public C() { this.X = 1; } }",
        "public class C { public int X { get; init; } public C() { X = 1; } }",
        "public class C { public int X { get; private set; } public C() {} \
            public void Bump() { X = 2; } }",
        "public class C { public int X { get; protected set; } public C() {} }",
        "public class C { public int X { get; set; } = 5; public C() {} }",
        "public class C { public int X { private get; set; } public C() {} }",
        "public class C { public string Name { get; } = \"n\"; public C() {} }",
        "public struct S { public int X { get; set; } }",
        "public abstract class C { public abstract int X { get; set; } public C() {} }",
    ] {
        compile_valid(&format!("{source} public int Run() {{ return 0; }}"));
    }
}

#[test]
fn initializers_run_with_field_initializers_in_declaration_order() {
    let compilation = compile_valid(
        r"
        public class C
        {
            public int A = 1;
            public int P { get; set; } = 2;
            public int B = 3;
            public C() {}
        }
        public int Run() { C c = new C(); return c.P; }
    ",
    );
    let class = compilation
        .mir
        .classes
        .iter()
        .find(|class| class.name == "C")
        .expect("C");
    let names = class
        .fields
        .iter()
        .map(|field| field.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["A", "<P>k__BackingField", "B"]);
    let constructor = compilation
        .mir
        .functions
        .iter()
        .find(|function| function.constructor && function.owner == Some(class.symbol))
        .expect("constructor");
    let stored = instructions(constructor)
        .filter_map(|instruction| match instruction {
            mir::Instruction::Assign {
                target: mir::Place::ObjectField { field, .. },
                ..
            } => Some(*field),
            _ => None,
        })
        .take(3)
        .collect::<Vec<_>>();
    let expected = class
        .fields
        .iter()
        .map(|field| field.symbol)
        .collect::<Vec<_>>();
    assert_eq!(stored, expected);
}

#[test]
fn a_get_only_property_cannot_be_assigned_outside_a_constructor() {
    rejects(
        "public class C { public int X { get; } public C() { X = 1; } \
            public void M() { X = 2; } } \
         public int Run() { return 0; }",
        "get-only auto-property `C.X` cannot be assigned here",
    );
    rejects(
        "public class C { public int X { get; } public C() { X = 1; } } \
         public int Run() { C c = new C(); c.X = 2; return 0; }",
        "get-only auto-property `C.X` cannot be assigned here",
    );
}

#[test]
fn an_init_property_cannot_be_assigned_outside_a_constructor() {
    rejects(
        "public class C { public int X { get; init; } public C() { X = 1; } \
            public void M() { X = 2; } } \
         public int Run() { return 0; }",
        "init-only property `C.X` can be assigned only in a constructor",
    );
    rejects(
        "public class C { public int X { get; init; } public C() {} } \
         public int Run() { C c = new C(); c.X = 2; return 0; }",
        "init-only property `C.X` can be assigned only in a constructor",
    );
}

#[test]
fn a_derived_constructor_may_assign_an_inherited_init_property() {
    compile_valid(
        "public class A { public int X { get; init; } public A() {} } \
         public class B : A { public B() : base() { X = 3; } } \
         public int Run() { return 0; }",
    );
    rejects(
        "public class A { public int X { get; } public A() { X = 1; } } \
         public class B : A { public B() : base() { X = 3; } } \
         public int Run() { return 0; }",
        "get-only auto-property `B.X` cannot be assigned here",
    );
}

#[test]
fn the_backing_field_cannot_be_named() {
    // `<` cannot start an identifier, so the lexer rejects any spelling.
    assert!(
        aster_compiler::compile(
            "public class C { public int X { get; set; } public C() { this.<X>k__BackingField = 1; } } \
             public int Run() { return 0; }"
        )
        .is_err()
    );
    rejects(
        "public class C { public int X { get; set; } public C() {} } \
         public int Run() { C c = new C(); return c.X_BackingField; }",
        "has no field",
    );
}

#[test]
fn static_auto_properties_are_not_supported_yet() {
    rejects(
        "public class C { public static int X { get; set; } } public int Run() { return 0; }",
        "static auto-properties are not supported yet",
    );
}

#[test]
fn an_auto_property_in_an_interface_follows_the_interface_property_rules() {
    rejects(
        "public interface I { int X { get; set; } } public int Run() { return 0; }",
        "properties are currently supported only in classes",
    );
}

#[test]
fn an_abstract_auto_property_cannot_have_an_initializer() {
    rejects(
        "public abstract class C { public abstract int X { get; set; } = 5; public C() {} } \
         public int Run() { return 0; }",
        "only an auto-property can have an initializer",
    );
}

#[test]
fn an_initializer_needs_an_auto_property() {
    rejects(
        "public class C { public int X { get { return 1; } } = 5; } public int Run() { return 0; }",
        "only an auto-property can have an initializer",
    );
}

#[test]
fn auto_accessors_cannot_mix_with_bodies() {
    rejects(
        "public class C { public int X { get; set { } } } public int Run() { return 0; }",
        "cannot mix accessors with and without bodies",
    );
}

#[test]
fn an_accessor_cannot_be_more_visible_than_its_auto_property() {
    rejects(
        "public class C { private int X { get; public set; } } public int Run() { return 0; }",
        "an accessor cannot be more visible than its property",
    );
}

#[test]
fn a_private_setter_is_not_callable_from_outside() {
    rejects(
        "public class C { public int X { get; private set; } public C() {} } \
         public int Run() { C c = new C(); c.X = 1; return 0; }",
        "setter",
    );
}

#[test]
fn a_virtual_auto_property_on_this_in_a_constructor_stays_rejected() {
    rejects(
        "public class C { public virtual int X { get; set; } \
            public C() { X = 1; } } public int Run() { return 0; }",
        "virtual member",
    );
}

#[test]
fn an_unassigned_reference_auto_property_is_a_constructor_error() {
    rejects(
        "public class C { public string Name { get; } public C() {} } \
         public int Run() { return 0; }",
        "auto-property `Name`",
    );
}

#[test]
fn struct_literals_name_auto_properties_not_the_backing_field() {
    compile_valid(
        "public struct P { public int X { get; set; } public int Y { get; } } \
         public int Run() { P p = P { X: 1, Y: 2 }; p.X = 5; return p.X + p.Y; }",
    );
    rejects(
        "public struct P { public int X { get; set; } public int Y { get; } } \
         public int Run() { P p = P { X: 1 }; return 0; }",
        "missing auto-property `Y`",
    );
    rejects(
        "public struct P { public int X { get; } } \
         public int Run() { P p = P { X: 1 }; p.X = 2; return 0; }",
        "get-only auto-property `P.X` cannot be assigned here",
    );
}

#[test]
fn explicit_struct_properties_stay_rejected() {
    rejects(
        "public struct P { public int X { get { return 1; } } } public int Run() { return 0; }",
        "properties are currently supported only in classes",
    );
}
