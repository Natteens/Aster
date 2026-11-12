//! T-OBJ-2 (object model B1, pull request 1): single class inheritance.
//!
//! Positive programs run through the whole pipeline (semantic analysis, HIR,
//! MIR, Cranelift) and observe their result; negative programs must be
//! controlled compile-time diagnostics. Layout and descriptor invariants are
//! tested next to the code that owns them (`layouts.rs`, `aster-mir`).
//!
//! B1 section 13 cases covered here: P2, P3, P6, P8, P9 (arguments and
//! bodies; initializer placement is a MIR-order test in the compiler crate),
//! P10, P11 and P12 (observable through the field values; offsets are tested
//! in `layouts.rs`), P13, P14, P15 and N1, N5, N6, N8, N9, N10, N12 to N18,
//! N20 to N22. N2, N3, N4 and N7 belong to virtual dispatch (pull request 2)
//! and are rejected with a "not supported yet" diagnostic, except N4, which
//! is the silent-hiding error. P1, P4, P5 and P7 need `virtual`/`override`.

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

/// The messages of every diagnostic, or a panic when the program compiles.
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

/// A program that must compile.
fn accepts(source: &str) {
    if let Err(diagnostics) = compile(source) {
        panic!("expected the program to compile, got {diagnostics:#?}");
    }
}

const TRACE: &str = r"
    public class Trace {
        public int value;
        public Trace() {}
        public int Mark(int digit) { value = value * 10 + digit; return digit; }
    }
";

// ---------------------------------------------------------------------------
// Positive cases
// ---------------------------------------------------------------------------

#[test]
fn derived_class_inherits_fields_methods_and_runs_the_base_constructor() {
    // P2: the base field is initialized before the body of the derived
    // constructor runs.
    let source = r"
        public class Animal {
            protected int legs;
            public Animal(int legs) { this.legs = legs; }
            public int Legs() { return legs; }
        }
        public class Dog : Animal {
            private int tricks;
            public Dog(int tricks) : base(4) { this.tricks = tricks; }
            public int Score() { return Legs() * 10 + tricks + legs; }
        }
        public int Run() {
            Dog dog = new Dog(3);
            return dog.Score();
        }
    ";
    assert_eq!(run_int(source), 4 * 10 + 3 + 4);
}

#[test]
fn construction_order_follows_arguments_then_base_arguments_then_base_body_then_derived_body() {
    // P9: the arguments of `new` (1), the arguments of `base(...)` (2, 3),
    // the base constructor body (4) and then each derived body (5, 6).
    let source = format!(
        r"{TRACE}
        public class A {{ public A(Trace t, int x) {{ t.Mark(4); }} }}
        public class B : A {{
            public B(Trace t, int x) : base(t, t.Mark(3)) {{ t.Mark(5); }}
        }}
        public class C : B {{
            public C(Trace t, int x) : base(t, t.Mark(2)) {{ t.Mark(6); }}
        }}
        public int Run() {{
            Trace t = new Trace();
            C c = new C(t, t.Mark(1));
            return t.value;
        }}"
    );
    assert_eq!(run_int(&source), 123_456);
}

#[test]
fn field_initializers_of_every_level_run_and_inherited_fields_keep_their_values() {
    let source = r"
        public class A { public int a = 1; public A() {} }
        public class B : A { public int b = a + 0; public B() {} }
        public class C : B { public int c = 3; public C() {} }
        public int Run() {
            C c = new C();
            return c.a * 100 + c.b * 10 + c.c;
        }
    ";
    // `b = a + 0` cannot read `a` in an initializer; it is rejected instead.
    assert!(run(source).is_err());
    let valid = r"
        public class A { public int a = 1; public A() {} }
        public class B : A { public int b = 2; public B() {} }
        public class C : B { public int c = 3; public C() {} }
        public int Run() {
            C c = new C();
            return c.a * 100 + c.b * 10 + c.c;
        }
    ";
    assert_eq!(run_int(valid), 123);
}

#[test]
fn implicit_constructors_chain_through_parameterless_base_constructors() {
    let source = r"
        public class A { public int a = 5; }
        public class B : A { public int b = 6; }
        public class C : B { }
        public int Run() {
            C c = new C();
            return c.a * 10 + c.b;
        }
    ";
    assert_eq!(run_int(source), 56);
}

#[test]
fn base_constructor_with_default_parameters_is_called_implicitly() {
    let source = r"
        public class A { public int a; public A(int a = 9) { this.a = a; } }
        public class B : A { public B() {} }
        public int Run() { B b = new B(); return b.a; }
    ";
    assert_eq!(run_int(source), 9);
}

#[test]
fn upcast_preserves_identity_and_field_writes_are_visible_through_both_references() {
    // P3: `Entity a = enemy; a == enemy` is true; same object, no copy.
    let source = r"
        public class Entity { public int health; public Entity() {} }
        public class Enemy : Entity { public int armor; public Enemy() {} }
        public int Run() {
            Enemy enemy = new Enemy();
            Entity entity = enemy;
            if (entity != enemy) { return -1; }
            if (!(entity == enemy)) { return -2; }
            entity.health = 40;
            enemy.armor = 2;
            enemy.health += 2;
            Entity other = new Enemy();
            if (other == enemy) { return -3; }
            return entity.health * 10 + enemy.armor + enemy.health - 42;
        }
    ";
    assert_eq!(run_int(source), 422);
}

#[test]
fn derived_values_convert_to_base_parameters_returns_arrays_and_conditionals() {
    let source = r"
        public class Shape { public int sides; public Shape(int sides) { this.sides = sides; } }
        public class Square : Shape { public Square() : base(4) {} }
        public class Triangle : Shape { public Triangle() : base(3) {} }
        public int Sides(Shape shape) { return shape.sides; }
        public Shape Pick(bool square) {
            if (square) { return new Square(); }
            return new Triangle();
        }
        public int Run() {
            Shape first = new Square();
            Shape second = Pick(false);
            Shape[] shapes = [first, second];
            Shape chosen = true ? new Square() : new Triangle();
            Shape mixed = false ? new Square() : new Triangle();
            return Sides(shapes[0]) * 1000 + Sides(shapes[1]) * 100
                + chosen.sides * 10 + mixed.sides;
        }
    ";
    assert_eq!(run_int(source), 4000 + 300 + 40 + 3);
}

#[test]
fn three_levels_keep_inherited_field_offsets_and_do_not_reuse_base_tail_padding() {
    // P11: `Base { long x; byte b }`, `Derived : Base { byte d }`, a leaf with
    // no fields. Each reference reads and writes the same storage; a derived
    // field that reused the base's tail padding would alias a base field.
    let source = r"
        public class Base { public long x; public byte b; public Base() {} }
        public class Derived : Base { public byte d; public Derived() {} }
        public class Leaf : Derived { public Leaf() {} }
        public int Run() {
            Leaf leaf = new Leaf();
            leaf.x = 1234567890123L;
            leaf.b = 11;
            leaf.d = 22;
            Base asBase = leaf;
            Derived asDerived = leaf;
            asBase.b = 33;
            if (asDerived.d != 22) { return -1; }
            if (leaf.b != 33) { return -2; }
            if (asBase.x != 1234567890123L) { return -3; }
            asDerived.d = 44;
            if (leaf.d != 44 || asBase.b != 33) { return -4; }
            return (int)(leaf.x % 1000) + leaf.b + leaf.d;
        }
    ";
    assert_eq!(run_int(source), 123 + 33 + 44);
}

#[test]
fn empty_class_and_derived_class_without_own_fields_are_instantiable() {
    // P12.
    let source = r"
        public class Empty { public Empty() {} }
        public class AlsoEmpty : Empty { public AlsoEmpty() {} }
        public class One { public int v = 7; public One() {} }
        public class Two : One { public Two() {} }
        public int Run() {
            Empty e = new AlsoEmpty();
            Empty f = new Empty();
            Two two = new Two();
            return two.v + (e == f ? 100 : 0);
        }
    ";
    assert_eq!(run_int(source), 7);
}

#[test]
fn abstract_class_is_inherited_and_its_constructor_and_protected_field_are_reached() {
    // P13.
    let source = r"
        public abstract class Shape {
            protected int id;
            public Shape(int id) { this.id = id; }
            public int Id() { return id; }
        }
        public class Circle : Shape {
            public int radius;
            public Circle(int id, int radius) : base(id) { this.radius = radius; }
            public int Describe() { return id * 100 + radius; }
        }
        public int Run() {
            Circle circle = new Circle(7, 5);
            Shape shape = circle;
            return circle.Describe() + shape.Id();
        }
    ";
    assert_eq!(run_int(source), 705 + 7);
}

#[test]
fn protected_members_are_readable_through_this_and_derived_receivers() {
    // P6.
    let source = r"
        public class Base {
            protected int secret = 5;
            protected int Twice() { return secret * 2; }
            protected int Prop { get { return secret + 1; } }
            public Base() {}
        }
        public class Derived : Base {
            public Derived() {}
            public int Read(Derived other) {
                return secret + other.secret + Twice() + other.Twice() + Prop + other.Prop;
            }
        }
        public int Run() {
            Derived a = new Derived();
            Derived b = new Derived();
            return a.Read(b);
        }
    ";
    assert_eq!(run_int(source), 5 + 5 + 10 + 10 + 6 + 6);
}

#[test]
fn inherited_properties_and_methods_work_on_derived_objects_and_in_constructors() {
    // P8 over an inherited property: `Health -= damage` on `this`.
    let source = r"
        public class Actor {
            private int health = 100;
            public int Health { get { return health; } set { health = value; } }
            public Actor() {}
        }
        public class Player : Actor {
            public Player(int damage) { Health -= damage; }
        }
        public int Run() {
            Player player = new Player(30);
            player.Health += 5;
            return player.Health;
        }
    ";
    assert_eq!(run_int(source), 75);
}

#[test]
fn private_members_of_the_base_stay_usable_by_the_base_methods() {
    let source = r"
        public class Counter {
            private int count;
            public Counter() {}
            public void Add(int n) { count += n; }
            public int Count() { return count; }
        }
        public class Named : Counter {
            public int tag = 3;
            public Named() {}
        }
        public int Run() {
            Named named = new Named();
            named.Add(4);
            named.Add(5);
            return named.Count() * 10 + named.tag;
        }
    ";
    assert_eq!(run_int(source), 93);
}

#[test]
fn closed_generic_base_works() {
    let source = r"
        public class Box<T> {
            public T value;
            public Box(T value) { this.value = value; }
            public T Get() { return value; }
        }
        public class IntBox : Box<int> {
            public int extra;
            public IntBox(int value, int extra) : base(value) { this.extra = extra; }
            public int Sum() { return Get() + value + extra; }
        }
        public int Run() {
            IntBox box = new IntBox(10, 5);
            Box<int> plain = box;
            return box.Sum() + plain.Get();
        }
    ";
    assert_eq!(run_int(source), 25 + 10);
}

#[test]
fn sealed_class_compiles_and_converts_to_an_interface() {
    // P10.
    let source = r"
        public interface IScore { int Value(); }
        public class Entity { public int hp = 4; public Entity() {} }
        public sealed class Leaf : Entity, IScore {
            public Leaf() {}
            public int Value() { return hp * 2; }
        }
        public int Read(IScore score) { return score.Value(); }
        public int Run() { return Read(new Leaf()); }
    ";
    assert_eq!(run_int(source), 8);
}

#[test]
fn interface_implemented_by_the_base_is_usable_from_base_and_derived_references() {
    // The table of the static class is correct for every dynamic type because
    // no member is virtual yet: a derived class inherits the base's method.
    let source = r"
        public interface IScore { int Value(); }
        public class Base : IScore {
            public int score;
            public Base(int score) { this.score = score; }
            public int Value() { return score; }
        }
        public class Derived : Base { public Derived() : base(9) {} }
        public int Read(IScore s) { return s.Value(); }
        public int Run() {
            Derived derived = new Derived();
            Base asBase = derived;
            IScore fromBase = asBase;
            IScore fromDerived = derived;
            return Read(fromBase) * 100 + Read(fromDerived) * 10 + Read(derived);
        }
    ";
    assert_eq!(run_int(source), 900 + 90 + 9);
}

#[test]
fn constructor_may_call_a_confined_helper_after_assigning_the_reference_fields_it_reads() {
    // P14.
    let source = r"
        public class Node { public int v; public Node(int v) { this.v = v; } }
        public class Holder {
            private Node node;
            private int total;
            public Holder(int v) {
                node = new Node(v);
                Init();
            }
            private void Init() { total = node.v * 2; }
            public int Total() { return total; }
        }
        public int Run() { return new Holder(21).Total(); }
    ";
    assert_eq!(run_int(source), 42);
}

#[test]
fn constructor_may_call_a_helper_that_reads_only_primitive_fields_before_they_are_assigned() {
    // P15: the zero default is read.
    let source = r"
        public class Counter {
            private int value;
            private int seen;
            public Counter() {
                Snapshot();
                value = 5;
            }
            private void Snapshot() { seen = value + 1; }
            public int Seen() { return seen; }
        }
        public int Run() { return new Counter().Seen(); }
    ";
    assert_eq!(run_int(source), 1);
}

#[test]
fn this_identity_comparison_is_allowed_in_a_constructor() {
    let source = r"
        public class Probe {
            public bool same;
            public bool different;
            public Probe(int unused) {
                same = this == this;
                different = this != this;
            }
        }
        public int Run() {
            Probe probe = new Probe(0);
            return (probe.same ? 10 : 0) + (probe.different ? 1 : 0);
        }
    ";
    assert_eq!(run_int(source), 10);
}

#[test]
fn deep_hierarchies_resolve_members_through_every_level() {
    let source = r"
        public class L0 { public int a = 1; public L0() {} public int A() { return a; } }
        public class L1 : L0 { public int b = 2; public L1() {} public int B() { return b + A(); } }
        public class L2 : L1 { public int c = 3; public L2() {} public int C() { return c + B(); } }
        public class L3 : L2 { public int d = 4; public L3() {} public int D() { return d + C(); } }
        public int Run() { return new L3().D(); }
    ";
    assert_eq!(run_int(source), 1 + 2 + 3 + 4);
}

// ---------------------------------------------------------------------------
// Negative cases
// ---------------------------------------------------------------------------

#[test]
fn n1_inheritance_cycles_are_rejected() {
    rejects(
        "public class A : B { public A() {} } public class B : A { public B() {} }",
        "cyclic inheritance",
    );
    rejects("public class A : A { public A() {} }", "cyclic inheritance");
    rejects(
        r"public class A : B { public A() {} }
          public class B : C { public B() {} }
          public class C : A { public C() {} }",
        "cyclic inheritance",
    );
}

#[test]
fn n2_n3_virtual_dispatch_modifiers_are_not_supported_yet() {
    for modifier in ["virtual", "override", "abstract"] {
        rejects(
            &format!(
                "public abstract class A {{ public A() {{}} public {modifier} int M() {{ return 1; }} }}"
            ),
            &format!("`{modifier}` members are not supported yet"),
        );
    }
    rejects(
        "public class A { public A() {} public virtual int P { get { return 1; } } }",
        "`virtual` members are not supported yet",
    );
}

#[test]
fn n4_a_same_signature_method_in_the_derived_class_is_an_error() {
    rejects(
        r"public class A { public A() {} public int M() { return 1; } }
          public class B : A { public B() {} public int M() { return 2; } }",
        "has the signature of the inherited method",
    );
    // A different parameter list is an ordinary overload.
    accepts(
        r"public class A { public A() {} public int M() { return 1; } }
          public class B : A { public B() {} public int M(int x) { return x; } }",
    );
    rejects(
        r"public class A { public int x; public A() {} }
          public class B : A { public int x; public B() {} }",
        "conflicts with the inherited field",
    );
}

#[test]
fn n5_invalid_bases_are_rejected() {
    rejects(
        "public sealed class A { public A() {} } public class B : A { public B() {} }",
        "is sealed and cannot be derived from",
    );
    rejects(
        "public struct S { public int x; } public class B : S { public B() {} }",
        "is a struct and cannot be a base class",
    );
    rejects(
        "public enum E { One, Two } public class B : E { public B() {} }",
        "is a enum and cannot be a base class",
    );
    rejects(
        "public static class U { public static int F() { return 1; } } public class B : U { public B() {} }",
        "static class `U` cannot be a base class",
    );
}

#[test]
fn n6_an_abstract_class_cannot_be_instantiated() {
    rejects(
        "public abstract class A { public A() {} } public int Run() { A a = new A(); return 0; }",
        "abstract class `A` cannot be instantiated",
    );
    // Target-typed construction too.
    rejects(
        "public abstract class A { public A() {} } public int Run() { A a = new(); return 0; }",
        "abstract class `A` cannot be instantiated",
    );
}

#[test]
fn n8_unrelated_classes_and_implicit_downcasts_are_rejected() {
    rejects(
        r"public class A { public A() {} } public class B { public B() {} }
          public int Run() { A a = new B(); return 0; }",
        "expected `A`, found `B`",
    );
    rejects(
        r"public class Base { public Base() {} } public class Derived : Base { public Derived() {} }
          public int Run() { Base b = new Derived(); Derived d = b; return 0; }",
        "expected `Derived`, found `Base`",
    );
    // Arrays of a derived class are not arrays of the base class.
    rejects(
        r"public class Base { public Base() {} } public class Derived : Base { public Derived() {} }
          public int Run() { Base[] items = new Derived[1]; return 0; }",
        "expected `Base[]`, found `Derived[]`",
    );
}

#[test]
fn n9_a_base_value_does_not_convert_to_an_interface_only_the_derived_class_implements() {
    rejects(
        r"public interface I { int Value(); }
          public class Base { public Base() {} }
          public class Derived : Base, I { public Derived() {} public int Value() { return 1; } }
          public int Use(I i) { return i.Value(); }
          public int Run() { Base b = new Derived(); return Use(b); }",
        "expected `I`, found `Base`",
    );
    accepts(
        r"public interface I { int Value(); }
          public class Base { public Base() {} }
          public class Derived : Base, I { public Derived() {} public int Value() { return 1; } }
          public int Use(I i) { return i.Value(); }
          public int Run() { return Use(new Derived()); }",
    );
}

#[test]
fn n10_this_cannot_escape_a_constructor() {
    let escapes = [
        // passed as an argument
        "public class P { public P() { Take(this); } public static void Take(P p) {} }",
        // stored in a field
        "public class P { public P next; public P() { next = this; } }",
        "public class P { public P next; public P() { this.next = this; } }",
        // stored in a local
        "public class P { public P() { P self = this; } }",
        // converted to an interface
        "public interface I { } public class P : I { public P() { I i = this; } }",
        // converted to a base class
        "public class B { public B() {} } public class P : B { public P() { B b = this; } }",
        // used as a call receiver of a constructor argument
        "public class P { public int v; public P() { Use(new Q(this)); } public static void Use(Q q) {} } public class Q { public Q(P p) {} }",
    ];
    for source in escapes {
        rejects(
            source,
            "`this` cannot be passed, stored, returned or converted while the object is under construction",
        );
    }
}

#[test]
fn n12_a_constructor_cannot_call_a_helper_that_lets_this_escape() {
    rejects(
        r"public class P {
              public P() { Register(); }
              private void Register() { Take(this); }
              public static void Take(P p) {}
          }",
        "which is not this-confined",
    );
    // Transitively: the helper calls another helper that lets `this` escape.
    rejects(
        r"public class P {
              public P() { Outer(); }
              private void Outer() { Inner(); }
              private void Inner() { Take(this); }
              public static void Take(P p) {}
          }",
        "which is not this-confined",
    );
    // Returning `this` also escapes.
    rejects(
        r"public class P {
              public P() { Me(); }
              private P Me() { return this; }
          }",
        "which is not this-confined",
    );
    // A confined helper that only touches fields is fine.
    accepts(
        r"public class P {
              private int v;
              public P() { Set(); }
              private void Set() { v = 3; }
          }",
    );
}

#[test]
fn n13_reading_a_field_before_it_is_assigned_or_leaving_it_unassigned_is_an_error() {
    rejects(
        r"public class P { private string name; public P() { string copy = name; name = copy; } }",
        "used before initialization",
    );
    rejects(
        r"public class P { private string name; public P() { } }",
        "does not initialize field `name`",
    );
}

#[test]
fn n14_private_base_members_and_protected_access_through_a_base_receiver_are_rejected() {
    // A private base member is not accessible from the derived class.
    rejects(
        r"public class A { private int hidden; public A() {} }
          public class B : A { public B() {} public int Read() { return hidden; } }",
        "is private to its base class",
    );
    rejects(
        r"public class A { private int hidden; public A() {} }
          public class B : A { public B() {} public int Read() { return this.hidden; } }",
        "is private to its base class",
    );
    rejects(
        r"public class A { private int M() { return 1; } public A() {} }
          public class B : A { public B() {} public int Read() { return this.M(); } }",
        "is private to its base class",
    );
    // `protected` through a `Base`-typed receiver does not grant access.
    rejects(
        r"public class A { protected int shared; public A() {} }
          public class B : A { public B() {} public int Read(A other) { return other.shared; } }",
        "protected",
    );
    // `protected` from outside the hierarchy.
    rejects(
        r"public class A { protected int shared; public A() {} }
          public int Run() { A a = new A(); return a.shared; }",
        "is protected",
    );
    rejects(
        r"public class A { protected int M() { return 1; } public A() {} }
          public int Run() { A a = new A(); return a.M(); }",
        "is protected",
    );
    // A protected constructor is reached through `base(...)` only.
    rejects(
        r"public class A { protected A() {} }
          public int Run() { A a = new A(); return 0; }",
        "is protected",
    );
    accepts(
        r"public class A { protected A() {} }
          public class B : A { public B() : base() {} }",
    );
}

#[test]
fn n15_base_calls_are_only_valid_in_a_constructor_of_a_derived_class() {
    rejects(
        "public class A { public A() : base() {} }",
        "`base(...)` requires `A` to have a base class",
    );
    rejects(
        "public class A { public A() {} public int M() : base() { return 1; } }",
        "expected `{`",
    );
    rejects(
        r"public class A { public int x; public A(int x) { this.x = x; } }
          public class B : A { public B() {} }",
        "must call `base(...)`",
    );
    rejects(
        r"public class A { public int x; public A(int x) { this.x = x; } }
          public class B : A { public B() : base() {} }",
        "missing required argument",
    );
    rejects(
        r"public class A { public int x; public A(int x) { this.x = x; } }
          public class B : A { public B() : base(1, 2) {} }",
        "too many arguments",
    );
    rejects(
        r"public class A { private A() {} }
          public class B : A { public B() : base() {} }",
        "constructor of base class `A` is private",
    );
    // `this(...)` chaining is a separate record.
    rejects(
        "public class A { public A() {} public A(int x) : this() {} }",
        "constructor chaining is not supported yet",
    );
    // `this` in the base arguments: the object is not usable yet.
    rejects(
        r"public class A { public A(int x) {} }
          public class B : A { public int v; public B() : base(v) {} }",
        "cannot be used in the arguments of `base(...)`",
    );
}

#[test]
fn n16_malformed_heritage_lists_are_rejected() {
    rejects(
        r"public class A { public A() {} } public class B { public B() {} }
          public class C : A, B { public C() {} }",
        "cannot have a second base class",
    );
    rejects(
        r"public interface I { } public class A { public A() {} }
          public class C : I, A { public C() {} }",
        "must be listed before the interfaces",
    );
    rejects(
        r"public interface I { int V(); }
          public class A : I { public A() {} public int V() { return 1; } }
          public class B : A, I { public B() {} }",
        "is already implemented by base class `A`",
    );
}

#[test]
fn n17_modifier_combinations_are_rejected() {
    rejects(
        "public abstract sealed class A { public A() {} }",
        "cannot be both `abstract` and `sealed`",
    );
    rejects(
        "public abstract struct S { public int x; }",
        "valid only on a class",
    );
    rejects(
        "public static abstract class U { public static int F() { return 1; } }",
        "cannot be `abstract` or `sealed`",
    );
    rejects(
        "public class A { public A() {} public abstract int M(); }",
        "`abstract` members are not supported yet",
    );
}

#[test]
fn n18_an_open_base_is_rejected() {
    rejects(
        r"public class Container<T> { public Container() {} }
          public class Box<T> : Container<T> { public Box() {} }
          public int Run() { Box<int> b = new Box<int>(); return 0; }",
        "open",
    );
}

#[test]
fn base_is_a_reserved_word() {
    // U5.
    rejects(
        "public int Run() { int base = 1; return base; }",
        "`base` is a reserved word",
    );
    rejects(
        "public class A { public int base; public A() {} }",
        "`base` is a reserved word",
    );
}

#[test]
fn n20_a_helper_reading_an_unassigned_reference_field_is_rejected() {
    rejects(
        r"public class Node { public int v; public Node() {} }
          public class Holder {
              private Node node;
              private int total;
              public Holder() {
                  Init();
                  node = new Node();
              }
              private void Init() { total = node.v; }
          }",
        "reads `Holder.node` before the constructor of `Holder` has assigned it",
    );
}

#[test]
fn n21_helper_writes_do_not_count_as_assignments() {
    rejects(
        r"public class Node { public int v; public Node() {} }
          public class Holder {
              private Node node;
              private int total;
              public Holder() {
                  Create();
                  Init();
              }
              private void Create() { node = new Node(); }
              private void Init() { total = node.v; }
          }",
        "reads `Holder.node` before the constructor of `Holder` has assigned it",
    );
    // Assigning in the constructor body itself is what counts.
    accepts(
        r"public class Node { public int v; public Node() {} }
          public class Holder {
              private Node node;
              private int total;
              public Holder() {
                  node = new Node();
                  Init();
              }
              private void Init() { total = node.v; }
          }",
    );
}

#[test]
fn n22_a_helper_whose_confinement_cannot_be_proven_is_rejected() {
    // An instance method that is `async` cannot exist in this version, so the
    // constructor call is rejected with the method's own diagnostic.
    rejects(
        r"public class P {
              public P() { Later(); }
              public async Task<int> Later() { return await Task.Run(Work); }
          }
          public int Work() { return 1; }",
        "`async` instance methods are not supported",
    );
    // A helper reached through an interface value is not confined.
    rejects(
        r"public interface IMark { void Mark(); }
          public class P : IMark {
              public P() { IMark me = this; me.Mark(); }
              public void Mark() {}
          }",
        "`this` cannot be passed, stored, returned or converted",
    );
}
