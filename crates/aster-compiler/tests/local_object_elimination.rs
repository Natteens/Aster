use aster_compiler::{compile, mir};

fn function<'a>(module: &'a mir::Module, name: &str) -> &'a mir::Function {
    module
        .functions
        .iter()
        .find(|function| function.name == name)
        .expect("test function exists")
}

fn object_allocations(function: &mir::Function) -> usize {
    function
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter(|instruction| matches!(instruction, mir::Instruction::AllocateObject { .. }))
        .count()
}

fn constructor_calls(function: &mir::Function, constructor: mir::SymbolId) -> usize {
    function
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter(|instruction| {
            matches!(instruction, mir::Instruction::Call { function, .. } if *function == constructor)
        })
        .count()
}

fn all_constructor_calls(module: &mir::Module, function: &mir::Function) -> usize {
    let constructors = module
        .functions
        .iter()
        .filter(|function| function.constructor)
        .map(|function| function.symbol)
        .collect::<Vec<_>>();
    function
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter(|instruction| {
            matches!(instruction, mir::Instruction::Call { function, .. } if constructors.contains(function))
        })
        .count()
}

fn fine_markers(function: &mir::Function) -> usize {
    function
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter(|instruction| {
            matches!(
                instruction,
                mir::Instruction::TemporarySubregionEnter { .. }
                    | mir::Instruction::TemporarySubregionExit { .. }
            )
        })
        .count()
}

fn object_fields(function: &mir::Function) -> usize {
    fn place_has_object_field(place: &mir::Place) -> bool {
        match place {
            mir::Place::ObjectField { .. } => true,
            mir::Place::Field { base, .. } | mir::Place::EnumField { base, .. } => {
                place_has_object_field(base)
            }
            mir::Place::Index { array, index, .. } => {
                operand_has_object_field(array) || operand_has_object_field(index)
            }
            mir::Place::Local(_) | mir::Place::Symbol(_) => false,
        }
    }

    fn operand_has_object_field(operand: &mir::Operand) -> bool {
        matches!(&operand.kind, mir::OperandKind::Copy(place) if place_has_object_field(place))
    }

    fn rvalue_has_object_field(value: &mir::Rvalue) -> bool {
        match &value.kind {
            mir::RvalueKind::Use(operand)
            | mir::RvalueKind::Discriminant(operand)
            | mir::RvalueKind::ArrayLength(operand)
            | mir::RvalueKind::ListLength(operand)
            | mir::RvalueKind::DictionaryLength(operand)
            | mir::RvalueKind::ListVersion(operand)
            | mir::RvalueKind::StringByteLength(operand)
            | mir::RvalueKind::Cast(operand)
            | mir::RvalueKind::Unary { operand, .. } => operand_has_object_field(operand),
            mir::RvalueKind::Aggregate(fields) | mir::RvalueKind::EnumConstruct { fields, .. } => {
                fields
                    .iter()
                    .any(|field| operand_has_object_field(&field.value))
            }
            mir::RvalueKind::MakeInterface { object, .. } => operand_has_object_field(object),
            mir::RvalueKind::Binary { left, right, .. }
            | mir::RvalueKind::Equality { left, right, .. } => {
                operand_has_object_field(left) || operand_has_object_field(right)
            }
        }
    }

    function
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter(|instruction| match instruction {
            mir::Instruction::Assign { target, value } => {
                place_has_object_field(target) || rvalue_has_object_field(value)
            }
            _ => false,
        })
        .count()
}

#[test]
fn direct_local_scalar_object_becomes_typed_scalar_locals() {
    let source = "public class Pair { public int left; public int right; } \
                  public int Run() { Pair pair = new Pair(); pair.left = 20; pair.right = 22; \
                  return pair.left + pair.right; }";
    let compilation = compile(source).expect("source compiles");
    let run = function(&compilation.mir, "Run");
    let constructor = compilation
        .mir
        .functions
        .iter()
        .find(|function| function.constructor)
        .expect("constructor exists")
        .symbol;

    assert_eq!(object_allocations(run), 0);
    assert_eq!(object_fields(run), 0);
    assert!(!run.blocks.iter().flat_map(|block| &block.instructions).any(
        |instruction| matches!(instruction, mir::Instruction::Call { function, .. } if *function == constructor)
    ));
    assert_eq!(
        run.locals
            .iter()
            .filter(|local| local.name.starts_with("_scalarized_"))
            .count(),
        2
    );
}

#[test]
fn simple_parameterized_constructor_becomes_typed_scalar_locals() {
    let source = "public class Point { public int x; public Point(int value) { x = value; } } \
                  public int Run() { Point point = new Point(42); return point.x; }";
    let compilation = compile(source).expect("source compiles");
    let run = function(&compilation.mir, "Run");
    let constructor = compilation
        .mir
        .functions
        .iter()
        .find(|function| function.constructor)
        .expect("constructor exists")
        .symbol;

    assert_eq!(object_allocations(run), 0);
    assert_eq!(object_fields(run), 0);
    assert_eq!(constructor_calls(run, constructor), 0);
    assert_eq!(
        run.locals
            .iter()
            .filter(|local| local.name.starts_with("_scalarized_"))
            .count(),
        1
    );
}

#[test]
fn partially_initialized_scalar_constructor_keeps_unassigned_fields_zeroed() {
    let source = "public class Point { public int x; public int y; public Point(int value) { \
                  this.x = value; } } public int Run() { Point first = new Point(10); \
                  Point second = new Point(20); return first.x + first.y + second.x + second.y; }";
    let compilation = compile(source).expect("source compiles");
    let run = function(&compilation.mir, "Run");
    let constructor = compilation
        .mir
        .functions
        .iter()
        .find(|function| function.constructor)
        .expect("constructor exists")
        .symbol;

    assert_eq!(object_allocations(run), 0);
    assert_eq!(object_fields(run), 0);
    assert_eq!(constructor_calls(run, constructor), 0);
    assert_eq!(
        run.locals
            .iter()
            .filter(|local| local.name.starts_with("_scalarized_"))
            .count(),
        4,
    );
}

#[test]
fn constructor_assignment_order_constants_and_repeated_parameters_are_scalarized() {
    for source in [
        "public class Point { public int x; public int y; public Point(int first, int second) { \
         y = second; x = first; } } public int Run() { Point point = new Point(20, 22); \
         return point.x + point.y; }",
        "public class Pair { public int left; public int right; public Pair(int value) { \
         left = value; right = value; } } public int Run() { Pair pair = new Pair(21); \
         return pair.left + pair.right; }",
        "public class Fixed { public int value; public Fixed(int ignored) { value = 42; } } \
         public int Run() { Fixed fixed = new Fixed(0); return fixed.value; }",
    ] {
        let compilation = compile(source).expect("source compiles");
        let run = function(&compilation.mir, "Run");
        assert_eq!(object_allocations(run), 0);
        assert_eq!(object_fields(run), 0);
    }
}

#[test]
fn scalarized_fields_work_across_ordinary_control_flow() {
    let source = "public class Pair { public int left; public int right; } \
                  public int Run(bool choose) { Pair pair = new Pair(); pair.left = 20; \
                  if (choose) { pair.right = 22; } else { pair.right = 1; } \
                  return pair.left + pair.right; }";
    let compilation = compile(source).expect("source compiles");
    assert_eq!(object_allocations(function(&compilation.mir, "Run")), 0);
}

#[test]
fn every_supported_scalar_field_kind_is_representation_safe() {
    let source = "public class Scalars { public bool flag; public sbyte i8; public byte u8; \
                  public short i16; public ushort u16; public int i32; public uint u32; \
                  public long i64; public ulong u64; public float f32; public double f64; \
                  public char character; } public int Run() { Scalars values = new Scalars(); \
                  return 1; }";
    let compilation = compile(source).expect("source compiles");
    let run = function(&compilation.mir, "Run");
    assert_eq!(object_allocations(run), 0);
    assert_eq!(
        run.locals
            .iter()
            .filter(|local| local.name.starts_with("_scalarized_"))
            .count(),
        12
    );
}

#[test]
fn source_alias_and_identity_observation_keep_object_allocation() {
    for source in [
        "public class Box { public int value; } public int Run() { \
         Box box = new Box(); Box alias = box; alias.value = 7; return box.value; }",
        "public class Box { public int value; } public bool Run() { \
         Box box = new Box(); Box alias = box; return box == alias; }",
        "public class Box { public int value; public Box(int value) { this.value = value; } } \
         public int Run() { Box box = new Box(7); Box alias = box; return alias.value; }",
        "public class Box { public int value; public Box(int value) { this.value = value; } } \
         public bool Run() { Box box = new Box(7); Box alias = box; return box == alias; }",
    ] {
        let compilation = compile(source).expect("source compiles");
        assert_eq!(object_allocations(function(&compilation.mir, "Run")), 1);
    }
}

#[test]
fn calls_and_unsupported_constructors_keep_object_allocation() {
    for source in [
        "public class Box { public int value; } internal int Read(Box box) { return box.value; } \
         public int Run() { Box box = new Box(); box.value = 7; return Read(box); }",
        "internal int Normalize(int value) { return value; } public class Box { public int value; \
         public Box(int value) { this.value = Normalize(value); } } public int Run() { \
         Box box = new Box(7); return box.value; }",
        "public class Box { public int value; public Box(int value) { if (value > 0) { \
         this.value = value; } else { this.value = 0; } } } public int Run() { \
         Box box = new Box(7); return box.value; }",
        "public class Box { public int value; public Box(int value) { this.value = value; \
         this.value = 42; } } public int Run() { Box box = new Box(7); return box.value; }",
        "public class Box { public int value; public Box(string label) { this.value = 42; } } \
         public int Run() { Box box = new Box(\"label\"); return box.value; }",
    ] {
        let compilation = compile(source).expect("source compiles");
        let run = function(&compilation.mir, "Run");
        assert_eq!(object_allocations(run), 1);
        assert_eq!(all_constructor_calls(&compilation.mir, run), 1);
    }
}

#[test]
fn effectful_argument_evaluation_remains_on_the_existing_object_path() {
    let source = "internal int Next() { return 42; } public class Box { public int value; \
                  public Box(int value) { this.value = value; } } public int Run() { \
                  Box box = new Box(Next()); return box.value; }";
    let compilation = compile(source).expect("source compiles");
    let run = function(&compilation.mir, "Run");
    assert_eq!(object_allocations(run), 1);
    assert_eq!(all_constructor_calls(&compilation.mir, run), 1);
}

#[test]
fn escape_storage_and_reference_fields_keep_object_allocation() {
    let returned =
        compile("public class Box { public int value; } public Box Build() { return new Box(); }")
            .expect("source compiles");
    assert_eq!(object_allocations(function(&returned.mir, "Build")), 1);

    let reference_field = compile(
        "public class Item { public int value; } \
         public class Holder { public Item item; public Holder(Item item) { this.item = item; } } \
         public int Run() { Item item = new Item(); Holder holder = new Holder(item); \
         return holder.item.value; }",
    )
    .expect("source compiles");
    assert!(object_allocations(function(&reference_field.mir, "Run")) >= 1);

    for source in [
        "public struct Holder { public Box item; } public class Box { public int value; \
         public Box(int value) { this.value = value; } } public int Run() { Box box = new Box(7); \
         Holder holder = Holder { item: box }; return holder.item.value; }",
        "public interface IBox { } public class Box : IBox { public int value; \
         public Box(int value) { this.value = value; } } public int Run() { \
         Box box = new Box(7); IBox contract = box; return 7; }",
        "public class Box { public int value; public Box(int value) { this.value = value; } \
         public int Read() { return value; } } public int Run() { Box box = new Box(7); \
         return box.Read(); }",
    ] {
        let compilation = compile(source).expect("source compiles");
        assert_eq!(object_allocations(function(&compilation.mir, "Run")), 1);
    }
}

#[test]
fn field_initializer_keeps_constructor_and_allocation_semantics() {
    let source = "public class Box { public int value = 41; } \
                  public int Run() { Box box = new Box(); return box.value + 1; }";
    let compilation = compile(source).expect("source compiles");
    assert_eq!(object_allocations(function(&compilation.mir, "Run")), 1);
}

#[test]
fn eliminated_object_does_not_create_an_object_only_aarm_region() {
    let compilation = compile(
        "public class Box { public int value; } public int Run() { int total = 0; \
         for (int i = 0; i < 100; i++) { Box box = new Box(); box.value = i; \
         total += box.value; } return total; }",
    )
    .expect("source compiles");
    let run = function(&compilation.mir, "Run");
    assert_eq!(object_allocations(run), 0);
    assert_eq!(object_fields(run), 0);
    assert_eq!(fine_markers(run), 0);
}

#[test]
fn rejected_object_only_loop_keeps_allocation_without_aarm_markers() {
    let compilation = compile(
        "public class Box { public int value; } public int Run() { int total = 0; \
         for (int i = 0; i < 100; i++) { Box box = new Box(); Box alias = box; \
         alias.value = i; total += box.value; } return total; }",
    )
    .expect("source compiles");
    let run = function(&compilation.mir, "Run");
    assert_eq!(object_allocations(run), 1);
    assert_eq!(fine_markers(run), 0);
}

#[test]
fn elimination_composes_with_selected_hidden_backing_loop() {
    let eliminated = compile(
        "public class Pair { public int left; public int right; } public int Run() { \
         int total = 0; for (int i = 0; i < 100; i++) { Pair pair = new Pair(); \
         pair.left = i; pair.right = 1; List<int> values = new List<int>(); \
         values.Add(i); total += pair.left + pair.right; } return total; }",
    )
    .expect("source compiles");
    let run = function(&eliminated.mir, "Run");
    assert_eq!(object_allocations(run), 0);
    assert_eq!(object_fields(run), 0);
    assert_eq!(fine_markers(run), 2, "{run:#?}");

    let retained = compile(
        "public class Pair { public int value; public string reference; public Pair() { reference = \"\"; } } public int Run() { \
         int total = 0; for (int i = 0; i < 100; i++) { Pair pair = new Pair(); \
         pair.value = i; List<int> values = new List<int>(); values.Add(i); \
         total += pair.value; } return total; }",
    )
    .expect("source compiles");
    let run = function(&retained.mir, "Run");
    assert_eq!(object_allocations(run), 1);
    assert_eq!(fine_markers(run), 0);
}

#[test]
fn constructor_elimination_composes_with_selected_hidden_backing_loop() {
    let compilation = compile(
        "public class Pair { public int left; public int right; public Pair(int left, int right) { \
         this.left = left; this.right = right; } } public int Run() { int total = 0; \
         for (int i = 0; i < 100; i++) { Pair pair = new Pair(i, 1); \
         List<int> values = new List<int>(); values.Add(i); total += pair.left + pair.right; } \
         return total; }",
    )
    .expect("source compiles");
    let run = function(&compilation.mir, "Run");
    assert_eq!(object_allocations(run), 0);
    assert_eq!(object_fields(run), 0);
    assert_eq!(fine_markers(run), 2);
}
