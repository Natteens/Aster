use aster_codegen_cranelift::{ExecutionValue, execute};

fn run(source: &str) -> ExecutionValue {
    let compilation = aster_compiler::compile(source).expect("valid language completeness program");
    execute(&compilation.mir, "Run").expect("program executes")
}

fn assert_reference_bearing_receiver_regions(mir: &aster_mir::Module) {
    let caller = mir
        .functions
        .iter()
        .find(|function| function.name == "Run")
        .expect("Run MIR");
    let contained_regions = caller
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            aster_mir::Instruction::AllocateArray { region, .. }
            | aster_mir::Instruction::AllocateObject { region, .. } => Some(*region),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(contained_regions.len() >= 3, "{contained_regions:?}");
    assert!(
        contained_regions
            .iter()
            .all(|region| *region == aster_mir::AllocationRegion::Persistent),
        "reference fields contained by the struct must remain persistent: {contained_regions:?}"
    );

    let make_text = mir
        .functions
        .iter()
        .find(|function| function.name == "MakeText")
        .expect("MakeText MIR");
    assert!(
        make_text
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .any(|instruction| match instruction {
                aster_mir::Instruction::CallIntrinsic { intrinsic, .. } => intrinsic
                    .allocation_region()
                    .is_some_and(|region| region == aster_mir::AllocationRegion::Persistent),
                aster_mir::Instruction::StringBuilderToString { region, .. } => {
                    *region == aster_mir::AllocationRegion::Persistent
                }
                _ => false,
            })
    );
}

#[test]
fn struct_methods_execute_with_by_value_receivers_and_nested_fields() {
    let source = r"
        public struct Point {
            public int x;
            public int y;
            public int Sum() { return x + this.y; }
            public void Move(int amount) { this.x += amount; }
        }
        public struct Transform {
            public Point point;
            public int Read() { return point.Sum(); }
        }
        public Point Pass(Point value) { return value; }
        public int ReadPoint(Point value) { return value.Sum(); }
        public int Run() {
            Point original = Point { x: 20, y: 22 };
            Point copy = original;
            copy.Move(100);
            Transform transform = Transform { point: original };
            if (ReadPoint(Pass(original)) != 42) { return 0; }
            return transform.Read();
        }
    ";
    assert_eq!(run(source), ExecutionValue::Int(42));
}

#[test]
fn reference_bearing_struct_receivers_preserve_references_and_escape_lifetimes() {
    let source = r#"
        public interface IRead { int Read(); }
        public class Reader : IRead {
            private int value;
            public Reader(int value) { this.value = value; }
            public int Read() { return value; }
        }
        public class Cell {
            private int value;
            public Cell(int value) { this.value = value; }
            public int Get() { return value; }
            public void Set(int value) { this.value = value; }
        }
        public struct Holder {
            public string text;
            public int[] values;
            public Cell cell;
            public IRead reader;
            public int Length() { return text.Length; }
            public string GetText() { return text; }
            public int[] GetValues() { return values; }
            public Cell GetCell() { return cell; }
            public IRead GetReader() { return reader; }
            public void MutateCell(int value) { cell.Set(value); }
            public void ReplaceCell(Cell replacement) { cell = replacement; }
        }
        public struct Nested {
            public Holder holder;
            public string GetText() { return holder.GetText(); }
        }
        public string MakeText(int suffix) { return $"Aster{suffix}"; }
        public Holder Pass(Holder value) { return value; }
        public string ReadText(Holder value) { return value.GetText(); }
        public int Run() {
            string text = MakeText(42);
            int[] values = [3, 4];
            Cell shared = new Cell(1);
            IRead reader = new Reader(2);
            Holder holder = Holder { text: text, values: values, cell: shared, reader: reader };
            Holder copy = Pass(holder);
            copy.MutateCell(40);
            copy.ReplaceCell(new Cell(100));
            if (shared.Get() != 40 || holder.GetCell().Get() != 40) { return 0; }
            if (copy.GetCell().Get() != 40 || copy.GetReader().Read() != 2) { return 0; }
            int[] returnedValues = copy.GetValues();
            returnedValues[0] = 5;
            if (values[0] != 5) { return 0; }
            string escaped = ReadText(copy);
            string fromReturn = Pass(copy).GetText();
            Nested nested = Nested { holder: copy };
            for (int i = 0; i < 1000; i++) { string pressure = $"temporary{i}"; }
            if (escaped != text || fromReturn != text || nested.GetText() != text) { return 0; }
            return shared.Get() + (holder.Length() - 5);
        }
    "#;
    let compilation =
        aster_compiler::compile(source).expect("valid reference-bearing struct program");
    assert_reference_bearing_receiver_regions(&compilation.mir);
    assert_eq!(
        execute(&compilation.mir, "Run").expect("program executes"),
        ExecutionValue::Int(42)
    );
}

#[test]
fn generic_struct_methods_and_generic_methods_execute() {
    let source = r"
        public struct Box<T> {
            public T value;
            public T Get() { return value; }
            public U Choose<U>(U candidate) { return candidate; }
        }
        public class Tools {
            public Tools() {}
            public T Identity<T>(T value) { return value; }
            public static T StaticIdentity<T>(T value) { return value; }
        }
        public int Run() {
            Box<int> box = Box<int> { value: 20 };
            Tools tools = new Tools();
            return box.Get() + box.Choose<int>(1) + Tools.StaticIdentity(tools.Identity(21));
        }
    ";
    assert_eq!(run(source), ExecutionValue::Int(42));
}

#[test]
fn backend_validation_rejects_an_adulterated_struct_receiver() {
    let source = "public struct Value { public int value; public int Read() { return value; } } \
                  public int Run() { Value value = Value { value: 42 }; return value.Read(); }";
    let mut compilation = aster_compiler::compile(source).expect("valid struct method");
    let call = compilation
        .mir
        .functions
        .iter_mut()
        .find(|function| function.name == "Run")
        .into_iter()
        .flat_map(|function| &mut function.blocks)
        .flat_map(|block| &mut block.instructions)
        .find_map(|instruction| match instruction {
            aster_mir::Instruction::Call { arguments, .. } => Some(arguments),
            _ => None,
        })
        .expect("struct method call");
    call[0].type_ = aster_mir::Type::Int;
    let error = execute(&compilation.mir, "Run")
        .expect_err("invalid receiver MIR must fail closed")
        .to_string();
    assert!(error.contains("invalid argument signature"), "{error}");
}

#[test]
fn backend_validation_rejects_wrong_receiver_arity_and_specialized_arguments() {
    let struct_source = "public struct Value { public int value; public int Read() { return value; } } \
                         public int Run() { Value value = Value { value: 42 }; return value.Read(); }";
    let mut missing_receiver =
        aster_compiler::compile(struct_source).expect("valid struct method program");
    let arguments = missing_receiver
        .mir
        .functions
        .iter_mut()
        .find(|function| function.name == "Run")
        .into_iter()
        .flat_map(|function| &mut function.blocks)
        .flat_map(|block| &mut block.instructions)
        .find_map(|instruction| match instruction {
            aster_mir::Instruction::Call { arguments, .. } => Some(arguments),
            _ => None,
        })
        .expect("struct method call");
    arguments.clear();
    let error = execute(&missing_receiver.mir, "Run")
        .expect_err("missing receiver must fail validation")
        .to_string();
    assert!(error.contains("invalid argument signature"), "{error}");

    let method_source = "public class Tools { public Tools() {} public T Identity<T>(T value) { return value; } } \
                         public int Run() { return new Tools().Identity<int>(42); }";
    let mut wrong_argument =
        aster_compiler::compile(method_source).expect("valid specialized method program");
    let arguments = wrong_argument
        .mir
        .functions
        .iter_mut()
        .find(|function| function.name == "Run")
        .into_iter()
        .flat_map(|function| &mut function.blocks)
        .flat_map(|block| &mut block.instructions)
        .filter_map(|instruction| match instruction {
            aster_mir::Instruction::Call { arguments, .. } => Some(arguments),
            _ => None,
        })
        .find(|arguments| arguments.len() == 2)
        .expect("generic method call");
    arguments[1].type_ = aster_mir::Type::Long;
    let error = execute(&wrong_argument.mir, "Run")
        .expect_err("wrong specialized argument must fail validation")
        .to_string();
    assert!(error.contains("invalid argument signature"), "{error}");
}
