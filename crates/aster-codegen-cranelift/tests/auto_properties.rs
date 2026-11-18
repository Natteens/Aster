//! T-LANG-5 (LANG-005): auto-properties executed through the whole pipeline.

use aster_codegen_cranelift::{ExecutionValue, execute};
use aster_compiler::compile;

fn run_int(source: &str) -> i32 {
    let compilation = compile(source)
        .unwrap_or_else(|diagnostics| panic!("expected a valid program: {diagnostics:#?}"));
    match execute(&compilation.mir, "Run") {
        Ok(ExecutionValue::Int(value)) => value,
        other => panic!("expected an int result, found {other:?}"),
    }
}

#[test]
fn get_and_set_round_trip_with_compound_assignment() {
    assert_eq!(
        run_int(
            r"
            public class Counter
            {
                public int Total { get; set; }
                public Counter() {}
            }
            public int Bump(Counter counter)
            {
                counter.Total = 4;
                counter.Total += 3;
                counter.Total += 1;
                return counter.Total;
            }
            public int Run() { return Bump(new Counter()); }
        "
        ),
        8
    );
}

#[test]
fn a_get_only_property_is_assigned_in_the_constructor() {
    assert_eq!(
        run_int(
            r"
            public class Box
            {
                public int Width { get; }
                public int Height { get; }
                public int Area { get; }
                public Box(int width, int height)
                {
                    Width = width;
                    this.Height = height;
                    Area = Width * this.Height;
                }
            }
            public int Run() { Box box = new Box(3, 4); return box.Area + box.Width; }
        "
        ),
        15
    );
}

#[test]
fn an_init_property_is_assigned_in_the_constructor_and_derived_constructors() {
    assert_eq!(
        run_int(
            r"
            public class Base
            {
                public int Id { get; init; }
                public int Tag { get; init; }
                public Base(int id) { Id = id; }
            }
            public class Derived : Base
            {
                public Derived() : base(7) { Tag = 5; }
            }
            public int Read(Base value) { return value.Id * 10 + value.Tag; }
            public int Run() { return Read(new Derived()); }
        "
        ),
        75
    );
}

#[test]
fn initializers_run_with_field_initializers_in_declaration_order() {
    assert_eq!(
        run_int(
            r"
            public class Order
            {
                public int A = 1;
                public int P { get; set; } = 2;
                public int B = 3;
                public int Q { get; } = 4;
                public int Sum() { return A * 1000 + P * 100 + B * 10 + Q; }
                public Order() {}
            }
            public int Run() { return new Order().Sum(); }
        "
        ),
        1234
    );
}

#[test]
fn an_initializer_may_read_an_earlier_property() {
    assert_eq!(
        run_int(
            r"
            public class Dependent
            {
                public int A { get; set; } = 6;
                public int B { get; set; } = 7;
                public Dependent() { A = A * B; }
            }
            public int Read(Dependent d) { return d.A; }
            public int Run() { return Read(new Dependent()); }
        "
        ),
        42
    );
}

#[test]
fn accessor_visibility_combinations() {
    assert_eq!(
        run_int(
            r"
            public class Gauge
            {
                public int Level { get; private set; }
                public int Peak { get; protected set; }
                public Gauge() {}
                public void Raise(int amount)
                {
                    Level += amount;
                    if (Level > Peak) { Peak = Level; }
                }
            }
            public class Tank : Gauge
            {
                public Tank() : base() {}
                public void Reset() { Peak = 0; }
            }
            public int Drive(Tank tank)
            {
                tank.Raise(5);
                tank.Raise(3);
                int peak = tank.Peak;
                tank.Reset();
                return peak * 100 + tank.Peak * 10 + tank.Level;
            }
            public int Run() { return Drive(new Tank()); }
        "
        ),
        808
    );
}

#[test]
fn a_derived_class_reads_an_inherited_auto_property() {
    assert_eq!(
        run_int(
            r"
            public class Animal
            {
                public int Legs { get; set; }
                public Animal() {}
            }
            public class Dog : Animal
            {
                public Dog() : base() { Legs = 4; }
                public int Double() { return Legs * 2 + this.Legs; }
            }
            public int Run() { return new Dog().Double(); }
        "
        ),
        12
    );
}

#[test]
fn a_virtual_auto_property_dispatches_through_its_accessors() {
    assert_eq!(
        run_int(
            r"
            public class Shape
            {
                public virtual int Sides { get; set; }
                public Shape() {}
            }
            public class Doubled : Shape
            {
                public override int Sides { get { return base.Sides * 2; } set { base.Sides = value; } }
                public Doubled() : base() {}
            }
            public int Probe(Shape shape)
            {
                shape.Sides = 3;
                return shape.Sides;
            }
            public int Run() { return Probe(new Shape()) * 100 + Probe(new Doubled()); }
        "
        ),
        306
    );
}

#[test]
fn an_overriding_auto_property_has_its_own_backing_field() {
    assert_eq!(
        run_int(
            r"
            public class A
            {
                public virtual int V { get; set; }
                public A() {}
            }
            public class B : A
            {
                public override int V { get; set; }
                public B() : base() {}
            }
            public int Probe(A a) { a.V = 9; return a.V; }
            public int Run() { return Probe(new A()) + Probe(new B()); }
        "
        ),
        18
    );
}

#[test]
fn struct_auto_properties_copy_by_value() {
    assert_eq!(
        run_int(
            r"
            public struct Point
            {
                public int X { get; set; }
                public int Y { get; }
            }
            public int Run()
            {
                Point a = Point { X: 1, Y: 2 };
                Point b = a;
                b.X = 10;
                a.X += 4;
                return a.X * 100 + b.X + b.Y;
            }
        "
        ),
        512
    );
}

#[test]
fn struct_auto_properties_work_inside_struct_methods_and_nested_places() {
    assert_eq!(
        run_int(
            r"
            public struct Inner { public int V { get; set; } }
            public struct Outer
            {
                public Inner Part;
                public int Read() { return Part.V; }
            }
            public int Run()
            {
                Outer outer = Outer { Part: Inner { V: 1 } };
                outer.Part.V = 41;
                outer.Part.V += 1;
                return outer.Read();
            }
        "
        ),
        42
    );
}

#[test]
fn a_string_auto_property_with_an_initializer() {
    assert_eq!(
        run_int(
            r#"
            public class Named
            {
                public string Name { get; set; } = "abc";
                public Named() {}
            }
            public int Read(Named n) { return n.Name.Length; }
            public int Run() { return Read(new Named()); }
        "#
        ),
        3
    );
}

#[test]
fn a_generic_class_auto_property() {
    assert_eq!(
        run_int(
            r"
            public class Cell<T>
            {
                public T Value { get; set; }
                public Cell(T value) { Value = value; }
            }
            public int Probe(Cell<int> cell) { cell.Value += 1; return cell.Value; }
            public int Run() { return Probe(new Cell<int>(41)); }
        "
        ),
        42
    );
}
