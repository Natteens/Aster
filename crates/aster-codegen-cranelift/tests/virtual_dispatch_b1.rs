//! T-OBJ-3 (object model B1, pull request 2): virtual dispatch.
//!
//! Positive programs run through the whole pipeline (semantic analysis, HIR,
//! MIR, Cranelift) and observe their result; negative programs must be
//! controlled compile-time diagnostics. Vtable and descriptor invariants are
//! tested next to the code that owns them (`aster-mir`, `layouts.rs`,
//! `execution.rs`).
//!
//! B1 section 13 cases covered here: P1, P4, P5, P7 and N2, N3, N7, N11 (plus
//! the virtual-member parts of N12 and N17). V8 and the abstract stub belong
//! to the unit tests of the validator and the backend.

use aster_codegen_cranelift::{ExecutionValue, execute};
use aster_compiler::compile;

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

fn errors(source: &str) -> Vec<String> {
    match compile(source) {
        Ok(_) => panic!("expected a compile-time diagnostic for:\n{source}"),
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

fn accepts(source: &str) {
    if let Err(diagnostics) = compile(source) {
        panic!("expected the program to compile, got {diagnostics:#?}");
    }
}

// ---------------------------------------------------------------------------
// Positive cases
// ---------------------------------------------------------------------------

#[test]
fn p1_a_base_typed_variable_runs_the_derived_override() {
    let source = r"
        public class Entity {
            public Entity() {}
            public virtual int Update(int dt) { return dt; }
        }
        public class Enemy : Entity {
            public Enemy() {}
            public override int Update(int dt) { return dt * 10; }
        }
        public int Run() {
            Entity e = new Enemy();
            Entity plain = new Entity();
            return e.Update(2) * 100 + plain.Update(2);
        }
    ";
    assert_eq!(run_int(source), 2002);
}

#[test]
fn p4_an_abstract_class_with_a_concrete_derived_class_compiles_and_dispatches() {
    let source = r"
        public abstract class Shape {
            protected int id;
            public Shape(int id) { this.id = id; }
            public abstract int Area();
            public int Describe() { return id * 1000 + Area(); }
        }
        public class Square : Shape {
            private int side;
            public Square(int id, int side) : base(id) { this.side = side; }
            public override int Area() { return side * side; }
        }
        public int Run() {
            Shape shape = new Square(7, 5);
            return shape.Describe();
        }
    ";
    assert_eq!(run_int(source), 7025);
}

#[test]
fn dispatch_through_base_typed_variables_over_three_levels() {
    let source = r"
        public class A { public A() {} public virtual int Value() { return 1; } }
        public class B : A { public B() {} public override int Value() { return 20; } }
        public class C : B { public C() {} public override int Value() { return 300; } }
        public class D : C { public D() {} }
        public int Run() {
            A a = new A();
            A b = new B();
            A c = new C();
            A d = new D();
            B bc = new C();
            return a.Value() + b.Value() + c.Value() + d.Value() + bc.Value();
        }
    ";
    // A=1, B=20, C=300, D inherits C=300, B-typed C=300.
    assert_eq!(run_int(source), 1 + 20 + 300 + 300 + 300);
}

#[test]
fn a_derived_class_that_does_not_override_inherits_the_nearest_implementation() {
    let source = r"
        public class A { public A() {} public virtual int V() { return 1; } public virtual int W() { return 2; } }
        public class B : A { public B() {} public override int W() { return 20; } }
        public class C : B { public C() {} public override int V() { return 100; } }
        public int Run() {
            A x = new C();
            return x.V() * 1000 + x.W();
        }
    ";
    assert_eq!(run_int(source), 100_020);
}

#[test]
fn overrides_reuse_the_slot_and_new_virtual_members_append_slots() {
    let source = r"
        public class A { public A() {} public virtual int F() { return 1; } }
        public class B : A {
            public B() {}
            public virtual int G() { return 10; }
            public override int F() { return 2; }
        }
        public class C : B {
            public C() {}
            public override int G() { return 30; }
            public virtual int H() { return 5; }
        }
        public int Run() {
            B b = new C();
            C c = new C();
            return b.F() * 10000 + b.G() * 100 + c.H();
        }
    ";
    assert_eq!(run_int(source), 20_000 + 3_000 + 5);
}

#[test]
fn p7_base_method_runs_the_base_implementation_without_redispatch() {
    let source = r"
        public class Entity {
            public Entity() {}
            public virtual int Update(int dt) { return dt; }
        }
        public class Enemy : Entity {
            public Enemy() {}
            public override int Update(int dt) { return base.Update(dt) + 100; }
        }
        public class Boss : Enemy {
            public Boss() {}
            public override int Update(int dt) { return base.Update(dt) * 2; }
        }
        public int Run() {
            Entity e = new Boss();
            return e.Update(1);
        }
    ";
    // Boss: (Enemy.Update(1) = Entity.Update(1) + 100 = 101) * 2.
    assert_eq!(run_int(source), 202);
}

#[test]
fn base_call_does_not_redispatch_even_when_the_most_derived_class_overrides() {
    let source = r"
        public class A {
            public int hits;
            public A() {}
            public virtual int Who() { hits = hits + 1; return 1; }
            public int Twice() { return Who() + Who(); }
        }
        public class B : A {
            public B() {}
            public override int Who() { return base.Who() * 10; }
        }
        public int Run() {
            B b = new B();
            int total = b.Twice();
            return total * 100 + b.hits;
        }
    ";
    // Who() dispatches to B.Who = 10 twice, each running A.Who once.
    assert_eq!(run_int(source), 2002);
}

#[test]
fn virtual_property_accessors_dispatch_and_one_accessor_can_be_overridden() {
    let source = r"
        public class Gauge {
            protected int stored;
            public Gauge() { stored = 5; }
            public virtual int Level {
                get { return stored; }
                set { stored = value; }
            }
        }
        public class Doubled : Gauge {
            public Doubled() {}
            public override int Level {
                get { return base.Level * 2; }
            }
        }
        public class Clamped : Gauge {
            public Clamped() {}
            public override int Level {
                set { base.Level = value > 9 ? 9 : value; }
            }
        }
        public int Run() {
            Gauge a = new Doubled();
            Gauge b = new Clamped();
            a.Level = 7;
            b.Level = 50;
            return a.Level * 100 + b.Level;
        }
    ";
    // Doubled keeps the inherited setter (stored = 7) and doubles on read.
    // Clamped keeps the inherited getter and clamps on write.
    assert_eq!(run_int(source), 14 * 100 + 9);
}

#[test]
fn abstract_property_is_implemented_by_the_derived_class() {
    let source = r"
        public abstract class Source {
            public Source() {}
            public abstract int Reading { get; }
            public int Twice() { return Reading * 2; }
        }
        public class Fixed : Source {
            public Fixed() {}
            public override int Reading { get { return 21; } }
        }
        public int Run() {
            Source s = new Fixed();
            return s.Twice() + s.Reading;
        }
    ";
    assert_eq!(run_int(source), 63);
}

#[test]
fn compound_property_assignment_dispatches_getter_and_setter_virtually() {
    let source = r"
        public class Counter {
            protected int raw;
            public Counter() {}
            public virtual int Count { get { return raw; } set { raw = value; } }
        }
        public class Loud : Counter {
            public Loud() {}
            public override int Count {
                get { return raw + 1000; }
                set { raw = value * 2; }
            }
        }
        public int Run() {
            Counter c = new Loud();
            c.Count = 1;
            c.Count += 4;
            return c.Count;
        }
    ";
    // set 1 -> raw 2; += 4: get = 1002, set 1006 -> raw 2012; read = 3012.
    assert_eq!(run_int(source), 3012);
}

#[test]
fn virtual_method_with_arguments_and_a_string_result() {
    let source = r#"
        public class Greeter {
            public Greeter() {}
            public virtual string Greet(string name, int times) { return name; }
        }
        public class Loud : Greeter {
            public Loud() {}
            public override string Greet(string name, int times) {
                string s = name;
                for (int i = 0; i < times; i++) { s += "!"; }
                return s;
            }
        }
        public int Run() {
            Greeter g = new Loud();
            return g.Greet("ab", 3).Length;
        }
    "#;
    assert_eq!(run_int(source), 5);
}

#[test]
fn virtual_methods_may_return_and_take_class_values() {
    let source = r"
        public class Node {
            public int tag;
            public Node(int tag) { this.tag = tag; }
            public virtual Node Next(Node other) { return other; }
        }
        public class Twin : Node {
            public Twin(int tag) : base(tag) {}
            public override Node Next(Node other) { return this; }
        }
        public int Run() {
            Node a = new Node(1);
            Node b = new Twin(2);
            return b.Next(a).tag * 10 + a.Next(b).tag;
        }
    ";
    assert_eq!(run_int(source), 22);
}

#[test]
fn recursion_through_a_virtual_method_is_depth_guarded() {
    let source = r"
        public class Walker {
            public Walker() {}
            public virtual int Down(int n) { return n; }
        }
        public class Rec : Walker {
            public Rec() {}
            public override int Down(int n) {
                if (n <= 0) { return 0; }
                Walker w = this;
                return 1 + w.Down(n - 1);
            }
        }
        public int Run() {
            Walker w = new Rec();
            return w.Down(50);
        }
    ";
    assert_eq!(run_int(source), 50);
}

#[test]
fn a_virtual_runtime_failure_in_an_override_is_a_controlled_error() {
    let source = r"
        public class A { public A() {} public virtual int Div(int x) { return x; } }
        public class B : A { public B() {} public override int Div(int x) { return 10 / x; } }
        public int Run() {
            A a = new B();
            return a.Div(0);
        }
    ";
    let error = run(source).expect_err("division by zero fails");
    assert!(error.contains("division"), "{error}");
}

// ---------------------------------------------------------------------------
// Interfaces implemented through a virtual method
// ---------------------------------------------------------------------------

#[test]
fn p5_an_interface_value_made_from_a_base_reference_runs_the_derived_override() {
    let source = r"
        public interface IScore { int Value(); }
        public class Base : IScore {
            public Base() {}
            public virtual int Value() { return 1; }
        }
        public class Derived : Base {
            public Derived() {}
            public override int Value() { return 42; }
        }
        public int Total(IScore s) { return s.Value(); }
        public int Run() {
            Base baseRef = new Derived();
            IScore s = baseRef;
            Base plain = new Base();
            return Total(s) * 100 + Total(plain);
        }
    ";
    assert_eq!(run_int(source), 4201);
}

#[test]
fn an_interface_call_runs_the_override_when_a_derived_class_overrides_the_implementing_method() {
    let source = r"
        public interface IName { int Id(); }
        public class A : IName { public A() {} public virtual int Id() { return 1; } }
        public class B : A { public B() {} public override int Id() { return 2; } }
        public class C : B { public C() {} }
        public int Run() {
            A a = new A();
            B b = new B();
            C c = new C();
            IName x = a;
            IName y = b;
            IName z = c;
            B viaB = c;
            IName w = viaB;
            return x.Id() * 1000 + y.Id() * 100 + z.Id() * 10 + w.Id();
        }
    ";
    assert_eq!(run_int(source), 1000 + 200 + 20 + 2);
}

#[test]
fn an_abstract_class_may_implement_an_interface_with_abstract_members() {
    let source = r"
        public interface IShape { int Area(); }
        public abstract class Shape : IShape {
            public Shape() {}
            public abstract int Area();
        }
        public class Rect : Shape {
            private int w;
            private int h;
            public Rect(int w, int h) { this.w = w; this.h = h; }
            public override int Area() { return w * h; }
        }
        public int Run() {
            Shape s = new Rect(3, 4);
            IShape i = s;
            return i.Area();
        }
    ";
    assert_eq!(run_int(source), 12);
}

#[test]
fn a_sealed_class_converts_to_an_interface_with_its_own_implementation() {
    let source = r"
        public interface IScore { int Value(); }
        public class Base : IScore {
            public Base() {}
            public virtual int Value() { return 1; }
        }
        public sealed class Leaf : Base {
            public Leaf() {}
            public override int Value() { return 7; }
        }
        public int Run() {
            Leaf leaf = new Leaf();
            IScore s = leaf;
            return s.Value();
        }
    ";
    assert_eq!(run_int(source), 7);
}

// ---------------------------------------------------------------------------
// Construction and this-confinement (N11, Q-OBJ-2)
// ---------------------------------------------------------------------------

#[test]
fn base_member_calls_are_allowed_in_a_constructor_when_the_base_member_is_confined() {
    let ok = r"
        public class A {
            protected int x;
            public A() {}
            public virtual int Bump() { x = x + 1; return x; }
            public int Read() { return x; }
        }
        public class B : A {
            public B() { base.Bump(); base.Bump(); }
            public override int Bump() { return 100; }
        }
        public int Run() {
            B b = new B();
            return b.Bump() * 10 + b.Read();
        }
    ";
    assert_eq!(run_int(ok), 1002);
}

#[test]
fn n11_a_virtual_call_on_this_in_a_constructor_is_rejected() {
    rejects(
        r"public class A {
            public A() { Init(); }
            public virtual void Init() {}
          }",
        "calls the virtual member",
    );
    rejects(
        r"public class A {
            public A() { this.Init(); }
            public virtual void Init() {}
          }",
        "calls the virtual member",
    );
    // An abstract member is just as dynamic.
    rejects(
        r"public abstract class A {
            public A() { Init(); }
            public abstract void Init();
          }",
        "calls the virtual member",
    );
    // So is an override reached on `this` in the derived constructor.
    rejects(
        r"public class A { public A() {} public virtual int V() { return 1; } }
          public class B : A {
            public int seen;
            public B() { seen = V(); }
            public override int V() { return 2; }
          }",
        "calls the virtual member",
    );
}

#[test]
fn n11_a_virtual_property_on_this_in_a_constructor_is_rejected() {
    rejects(
        r"public class A {
            public A() { Level = 3; }
            public virtual int Level { get { return 1; } set { } }
          }",
        "calls the virtual member",
    );
    rejects(
        r"public class A {
            public int seen;
            public A() { seen = Level; }
            public virtual int Level { get { return 1; } }
          }",
        "calls the virtual member",
    );
    rejects(
        r"public class A {
            public int seen;
            public A() { seen = this.Level; }
            public virtual int Level { get { return 1; } }
          }",
        "calls the virtual member",
    );
}

#[test]
fn n11_non_virtual_properties_stay_allowed_in_constructors() {
    // P8 stays valid for non-virtual properties only.
    accepts(
        r"public class A {
            public int health = 10;
            public int Health { get { return health; } set { health = value; } }
            public A(int damage) { Health -= damage; }
          }",
    );
}

#[test]
fn n12_a_constructor_cannot_call_a_helper_that_calls_a_virtual_member() {
    rejects(
        r"public class A {
            public A() { Helper(); }
            public int Helper() { return V(); }
            public virtual int V() { return 1; }
          }",
        "not this-confined",
    );
}

// ---------------------------------------------------------------------------
// Negative cases: N2, N3, N7 and modifier misuse
// ---------------------------------------------------------------------------

#[test]
fn n2_override_of_a_member_that_is_not_virtual_or_does_not_exist() {
    rejects(
        r"public class A { public A() {} public int M() { return 1; } }
          public class B : A { public B() {} public override int M() { return 2; } }",
        "not virtual, abstract or an override",
    );
    rejects(
        r"public class A { public A() {} }
          public class B : A { public B() {} public override int M() { return 2; } }",
        "no virtual or abstract member `M` to override",
    );
    rejects(
        r"public class A { public A() {} public override int M() { return 2; } }",
        "no virtual or abstract member `M` to override",
    );
    rejects(
        r"public class A { public A() {} public int P { get { return 1; } } }
          public class B : A { public B() {} public override int P { get { return 2; } } }",
        "not virtual, abstract or an override",
    );
}

#[test]
fn n3_override_with_different_parameters_return_type_or_visibility() {
    rejects(
        r"public class A { public A() {} public virtual int M(int x) { return x; } }
          public class B : A { public B() {} public override int M(long x) { return 1; } }",
        "no inherited virtual method has these parameters",
    );
    rejects(
        r"public class A { public A() {} public virtual int M(int x) { return x; } }
          public class B : A { public B() {} public override long M(int x) { return 1; } }",
        "different return type",
    );
    rejects(
        r"public class A { public A() {} public virtual int M() { return 1; } }
          public class B : A { public B() {} protected override int M() { return 1; } }",
        "different visibility",
    );
    rejects(
        r"public class A { public A() {} public virtual int P { get { return 1; } } }
          public class B : A { public B() {} public override long P { get { return 1; } } }",
        "different type",
    );
}

#[test]
fn a_property_override_may_not_add_an_accessor() {
    rejects(
        r"public class A { public A() {} public virtual int P { get { return 1; } } }
          public class B : A {
            public B() {}
            public override int P { get { return 1; } set { } }
          }",
        "adds a setter",
    );
}

#[test]
fn n7_a_concrete_class_must_override_every_inherited_abstract_member() {
    rejects(
        r"public abstract class A { public A() {} public abstract int M(); }
          public class B : A { public B() {} }",
        "does not override the inherited abstract member `A.M`",
    );
    // An abstract member inherited through two levels.
    rejects(
        r"public abstract class A { public A() {} public abstract int M(); }
          public abstract class B : A { public B() {} }
          public class C : B { public C() {} }",
        "does not override the inherited abstract member `A.M`",
    );
    rejects(
        r"public abstract class A { public A() {} public abstract int P { get; } }
          public class B : A { public B() {} }",
        "does not override the inherited abstract member `A.P`",
    );
    // An abstract derived class may leave it open.
    accepts(
        r"public abstract class A { public A() {} public abstract int M(); }
          public abstract class B : A { public B() {} }",
    );
}

#[test]
fn abstract_members_exist_only_in_abstract_classes_and_abstract_classes_are_not_instantiable() {
    rejects(
        "public class A { public A() {} public abstract int M(); }",
        "abstract method in class `A`, which is not abstract",
    );
    rejects(
        "public class A { public A() {} public abstract int P { get; } }",
        "abstract property in class `A`, which is not abstract",
    );
    rejects(
        r"public abstract class A { public A() {} public abstract int M(); }
          public int Run() { A a = new A(); return a.M(); }",
        "abstract",
    );
}

#[test]
fn sealed_override_is_rejected() {
    rejects(
        r"public class A { public A() {} public virtual int M() { return 1; } }
          public class B : A { public B() {} public sealed override int M() { return 2; } }",
        "`sealed override` is not supported",
    );
}

#[test]
fn a_same_signature_method_without_override_is_still_silent_hiding() {
    rejects(
        r"public class A { public A() {} public virtual int M() { return 1; } }
          public class B : A { public B() {} public int M() { return 2; } }",
        "has the signature of the inherited method",
    );
    rejects(
        r"public class A { public A() {} public virtual int P { get { return 1; } } }
          public class B : A { public B() {} public int P { get { return 2; } } }",
        "conflicts with the inherited property",
    );
}

#[test]
fn virtual_members_must_be_instance_non_private_class_members() {
    rejects(
        "public class A { public A() {} public static virtual int M() { return 1; } }",
        "static method cannot be `virtual`",
    );
    rejects(
        "public class A { public A() {} private virtual int M() { return 1; } }",
        "private method cannot be `virtual`",
    );
    rejects(
        "public struct S { public virtual int M() { return 1; } }",
        "only a class method can be `virtual`",
    );
    rejects(
        "public class A { virtual A() {} }",
        "constructors cannot be `virtual`",
    );
}

#[test]
fn base_member_misuse_is_rejected() {
    rejects(
        r"public class A { public A() {} public int F() { return base.F(); } }",
        "requires the class to have a base class",
    );
    rejects(
        r"public abstract class A { public A() {} public abstract int M(); }
          public class B : A {
            public B() {}
            public override int M() { return base.M(); }
          }",
        "`base.M` is abstract and has no implementation to call",
    );
    rejects(
        r"public class A { public A() {} public virtual int M() { return 1; } }
          public class B : A {
            public B() {}
            public static int S() { return base.M(); }
          }",
        "`base` is valid only inside an instance member",
    );
}

#[test]
fn a_protected_virtual_member_is_overridden_and_called_by_derived_classes() {
    let source = r"
        public class A {
            public A() {}
            protected virtual int Step() { return 1; }
            public int Run3() { return Step() + Step() + Step(); }
        }
        public class B : A {
            public B() {}
            protected override int Step() { return base.Step() + 1; }
        }
        public int Run() {
            A a = new B();
            return a.Run3();
        }
    ";
    assert_eq!(run_int(source), 6);
}

#[test]
fn many_slots_dispatch_to_the_right_function() {
    let source = r"
        public class A {
            public A() {}
            public virtual int S0() { return 0; }
            public virtual int S1() { return 1; }
            public virtual int S2() { return 2; }
            public virtual int S3() { return 3; }
            public virtual int S4() { return 4; }
        }
        public class B : A {
            public B() {}
            public override int S1() { return 11; }
            public override int S3() { return 33; }
            public virtual int S5() { return 5; }
        }
        public int Run() {
            A a = new B();
            B b = new B();
            return a.S0() + a.S1() * 10 + a.S2() * 100 + a.S3() * 1000 + a.S4() * 10000
                + b.S5() * 100000;
        }
    ";
    assert_eq!(run_int(source), 110 + 200 + 33_000 + 40_000 + 500_000);
}

#[test]
fn unbounded_recursion_through_a_virtual_method_hits_the_call_depth_guard() {
    let source = r"
        public class Walker {
            public Walker() {}
            public virtual int Down(int n) { return n; }
        }
        public class Forever : Walker {
            public Forever() {}
            public override int Down(int n) {
                Walker w = this;
                return w.Down(n + 1);
            }
        }
        public int Run() {
            Walker w = new Forever();
            return w.Down(0);
        }
    ";
    let error = run(source).expect_err("the guard stops the recursion");
    assert!(error.contains("call depth"), "{error}");
}

#[test]
fn a_closed_generic_base_with_virtual_members_dispatches() {
    let source = r"
        public class Box<T> {
            public T value;
            public Box(T value) { this.value = value; }
            public virtual T Get() { return value; }
        }
        public class DoubleBox : Box<int> {
            public DoubleBox(int value) : base(value) {}
            public override int Get() { return value * 2; }
        }
        public int Run() {
            Box<int> b = new DoubleBox(21);
            return b.Get();
        }
    ";
    assert_eq!(run_int(source), 42);
}

#[test]
fn a_derived_class_may_implement_an_interface_through_an_inherited_virtual_override() {
    let source = r"
        public interface IScore { int Value(); }
        public class A { public A() {} public virtual int Value() { return 1; } }
        public class B : A, IScore { public B() {} public override int Value() { return 5; } }
        public class C : B { public C() {} public override int Value() { return 9; } }
        public int Run() {
            B b = new C();
            IScore s = b;
            return s.Value();
        }
    ";
    assert_eq!(run_int(source), 9);
}

#[test]
fn virtual_methods_work_with_struct_and_array_parameters_and_results() {
    let source = r"
        public struct Pair { public int a; public int b; }
        public class Maker {
            public Maker() {}
            public virtual Pair Make(int x) { Pair p = Pair { a: x, b: 0 }; return p; }
            public virtual int Sum(int[] values, Pair p) {
                int total = p.a + p.b;
                for (int i = 0; i < values.Length; i++) { total += values[i]; }
                return total;
            }
        }
        public class Doubler : Maker {
            public Doubler() {}
            public override Pair Make(int x) { Pair p = Pair { a: x, b: x }; return p; }
            public override int Sum(int[] values, Pair p) { return base.Sum(values, p) * 2; }
        }
        public int Run() {
            Maker m = new Doubler();
            Pair p = m.Make(4);
            int[] values = [1, 2, 3];
            return m.Sum(values, p);
        }
    ";
    assert_eq!(run_int(source), (4 + 4 + 1 + 2 + 3) * 2);
}

#[test]
fn virtual_dispatch_works_on_values_stored_in_fields_arrays_and_lists() {
    let source = r"
        public class Shape { public Shape() {} public virtual int Sides() { return 0; } }
        public class Tri : Shape { public Tri() {} public override int Sides() { return 3; } }
        public class Quad : Shape { public Quad() {} public override int Sides() { return 4; } }
        public class Holder { public Shape inner; public Holder(Shape s) { inner = s; } }
        public int Run() {
            Shape first = new Tri();
            Shape second = new Quad();
            Shape third = new Shape();
            Shape[] shapes = [first, second, third];
            Holder h = new Holder(new Quad());
            int total = h.inner.Sides();
            for (int i = 0; i < shapes.Length; i++) { total += shapes[i].Sides(); }
            return total;
        }
    ";
    assert_eq!(run_int(source), 4 + 3 + 4);
}

#[test]
fn the_documented_virtual_dispatch_example_runs() {
    // The example of `docs/reference/classes.md`.
    let source = r"
        public abstract class Shape
        {
            protected int id;
            public Shape(int id) { this.id = id; }
            public abstract int Area();
            public virtual int Perimeter() { return 0; }
            public virtual int Label { get { return id; } set { id = value; } }
            public int Describe() { return Area() + Perimeter(); }
        }

        public class Square : Shape
        {
            private int side;
            public Square(int id, int side) : base(id) { this.side = side; }
            public override int Area() { return side * side; }
            public override int Perimeter() { return base.Perimeter() + 4 * side; }
            public override int Label { get { return base.Label + 1000; } }
        }

        public int Run() {
            Shape shape = new Square(7, 5);
            shape.Label = 8;
            return shape.Describe() * 10000 + shape.Label;
        }
    ";
    assert_eq!(run_int(source), 45 * 10_000 + 1008);
}

#[test]
fn a_virtual_property_cannot_have_a_private_accessor() {
    rejects(
        "public class A { public A() {} public virtual int P { get { return 1; } private set { } } }",
        "cannot be private",
    );
}
