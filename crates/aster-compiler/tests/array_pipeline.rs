use aster_hir as hir;
use aster_mir as mir;

#[test]
fn arrays_are_typed_in_hir_and_explicit_in_mir() {
    let compilation = aster_compiler::compile(
        "public int Run() { int[] values = [1, 2, 3]; values[1] += 4; return values.Length; }",
    )
    .expect("valid arrays");
    let hir::Item::Function(function) = &compilation.hir.items[0] else {
        panic!("function")
    };
    let body = function.body.as_ref().unwrap();
    let hir::Statement::Variable(variable) = &body.statements[0] else {
        panic!("variable")
    };
    assert_eq!(variable.type_, hir::Type::Array(Box::new(hir::Type::Int)));
    assert!(matches!(
        variable.initializer.as_ref().unwrap().kind,
        hir::ExpressionKind::ArrayLiteral(_)
    ));
    let function = &compilation.mir.functions[0];
    assert!(
        function
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .any(|instruction| matches!(
                instruction,
                mir::Instruction::AllocateArray {
                    element_type: mir::Type::Int,
                    region: mir::AllocationRegion::Temporary,
                    ..
                }
            ))
    );
    assert!(
        function
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .any(|instruction| matches!(
                instruction,
                mir::Instruction::Assign {
                    target: mir::Place::Index { .. },
                    ..
                }
            ))
    );
}

#[test]
fn array_diagnostics_are_specific() {
    for (source, message) in [
        (
            "public int Run() { int[] a = [1]; return a[true]; }",
            "index must have type `int`",
        ),
        (
            "public int Run() { int[] a = [1]; a.Length = 2; return 0; }",
            "array Length is read-only",
        ),
        (
            "public int Run() { int[] a = [1]; return a.Missing; }",
            "array has no member `Missing`",
        ),
        (
            "public int Run() { int[] a = [1]; return a[-1]; }",
            "array index cannot be negative",
        ),
        (
            "public int Run() { int[] a = [1]; a[0] = false; return 0; }",
            "expected `int`, found `bool`",
        ),
        (
            "public int Run() { int[] a; return a.Length; }",
            "used before initialization",
        ),
        (
            "public int Run() { string[] a = new string[2]; return a.Length; }",
            "has no non-null default value",
        ),
    ] {
        let diagnostics = aster_compiler::compile(source).expect_err("source must be rejected");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains(message)),
            "missing `{message}` in {diagnostics:#?}"
        );
    }
}

#[test]
fn empty_reference_arrays_use_the_explicit_target_type() {
    let source = "public interface IValue { int Get(); } \
                  public class Value : IValue { public Value() {} public int Get() { return 1; } } \
                  public int Run() { \
                      string[] strings = []; \
                      string[] explicitStrings = new string[0]; \
                      Value[] classes = []; \
                      Value[] explicitClasses = new Value[0]; \
                      IValue[] interfaces = []; \
                      IValue[] explicitInterfaces = new IValue[0]; \
                      List<string>[] collections = []; \
                      List<string>[] explicitCollections = new List<string>[0]; \
                      int[] ints = new int[2]; \
                      bool[] flags = new bool[2]; \
                      return strings.Length + explicitStrings.Length + classes.Length + \
                          explicitClasses.Length + interfaces.Length + explicitInterfaces.Length + \
                          collections.Length + explicitCollections.Length + ints.Length + flags.Length; \
                  }";
    let compilation = aster_compiler::compile(source).expect("empty reference arrays are valid");

    let function = compilation
        .mir
        .functions
        .iter()
        .find(|function| function.name == "Run")
        .expect("Run MIR");
    let allocations = function
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| {
            let mir::Instruction::AllocateArray {
                element_type,
                length,
                initialization,
                ..
            } = instruction
            else {
                return None;
            };
            Some((element_type, length, *initialization))
        })
        .collect::<Vec<_>>();
    assert_eq!(allocations.len(), 10);
    assert!(
        allocations
            .iter()
            .take(8)
            .enumerate()
            .all(|(index, (_, length, initialization))| {
                matches!(
                    &length.kind,
                    mir::OperandKind::Constant(mir::Constant::Integer(value)) if value == "0"
                ) && *initialization
                    == if index % 2 == 0 {
                        mir::ArrayInitialization::Explicit
                    } else {
                        mir::ArrayInitialization::Empty
                    }
            })
    );
    assert!(
        allocations
            .iter()
            .skip(8)
            .all(|(_, _, initialization)| { *initialization == mir::ArrayInitialization::Default })
    );
    assert!(
        allocations
            .iter()
            .all(|(element, _, _)| **element != mir::Type::Unknown)
    );
}

#[test]
fn compile_time_zero_allows_empty_reference_array_construction() {
    aster_compiler::compile(
        "public int Run() { const int Zero = 0; string[] a = new string[1 - 1]; string[] b = new string[Zero]; return a.Length + b.Length; }",
    )
    .expect("all proven constant-zero lengths are valid");
}

#[test]
fn reference_array_defaults_stay_rejected_without_a_zero_proof() {
    for source in [
        "public int Run() { string[] a = new string[1]; return a.Length; }",
        "public int Run(int length) { string[] a = new string[length]; return a.Length; }",
        "public int Run() { int length = 0; string[] a = new string[length]; return a.Length; }",
        "public class Value { public Value() {} } public int Run() { Value[] a = new Value[1]; return a.Length; }",
        "public interface IValue { int Get(); } public int Run() { IValue[] a = new IValue[1]; return a.Length; }",
    ] {
        let diagnostics = aster_compiler::compile(source).expect_err("source must be rejected");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("has no non-null default value"))
        );
    }
}

#[test]
fn empty_array_literal_uses_assignment_return_and_argument_contexts() {
    for source in [
        "public int Run() { int[] values = [1]; values = []; return 0; }",
        "public int[] Values() { return []; }",
        "public void Use(int[] values) {} public int Run() { Use([]); return 0; }",
    ] {
        aster_compiler::compile(source).expect("an exact array target supplies the element type");
    }

    for source in [
        "public int Run() { var values = []; return 0; }",
        "public int Run() { []; return 0; }",
    ] {
        let diagnostics =
            aster_compiler::compile(source).expect_err("an untyped empty literal is ambiguous");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message
                .contains("cannot infer the element type of an empty array literal")
        }));
    }
}

#[test]
fn empty_reference_arrays_do_not_bypass_type_based_worker_transfer() {
    let diagnostics = aster_compiler::compile(
        "public int Count(string[] values) { return values.Length; } \
         public int Run() { return Task.Run(Count, new string[0]).Wait(); }",
    )
    .expect_err("zero runtime length does not make a reference array transferable");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message
            .contains("cannot cross a worker boundary as a `Task.Run` argument")
    }));
}

#[test]
fn foreach_is_typed_and_lowers_to_existing_array_cfg() {
    let compilation = aster_compiler::compile(
        "public int Run() { int[] values = [1, 2, 3]; int total = 0; foreach (int value in values) { total += value; } return total; }",
    )
    .expect("valid array foreach");
    let hir::Item::Function(function) = &compilation.hir.items[0] else {
        panic!("function");
    };
    assert!(matches!(
        function.body.as_ref().unwrap().statements[2],
        hir::Statement::ForEach { .. }
    ));
    let function = &compilation.mir.functions[0];
    assert!(
        function
            .blocks
            .iter()
            .any(|block| matches!(block.terminator, mir::Terminator::Branch { .. }))
    );
    assert!(
        function
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .any(|instruction| {
                matches!(
                    instruction,
                    mir::Instruction::Assign {
                        value: mir::Rvalue {
                            kind: mir::RvalueKind::ArrayLength(_),
                            ..
                        },
                        ..
                    }
                )
            })
    );
}

#[test]
fn foreach_over_a_list_is_typed_and_lowers_to_an_indexed_cfg() {
    // M3C: `List<T>` is now a valid `foreach` collection (M3B only accepted
    // arrays). Mirrors `foreach_is_typed_and_lowers_to_existing_array_cfg`
    // above, but confirms the version-checked shape `lower_foreach_over_list`
    // actually produces: a `ListLength` read, at least one `ListVersion`
    // read, and a `ListGet` (never `ArrayLength`/`Place::Index`).
    let compilation = aster_compiler::compile(
        "public int Run() { List<int> values = new List<int>(); values.Add(1); int total = 0; foreach (int value in values) { total += value; } return total; }",
    )
    .expect("valid list foreach");
    let function = &compilation.mir.functions[0];
    let instructions = function
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .collect::<Vec<_>>();
    assert!(instructions.iter().any(|instruction| matches!(
        instruction,
        mir::Instruction::Assign {
            value: mir::Rvalue {
                kind: mir::RvalueKind::ListLength(_),
                ..
            },
            ..
        }
    )));
    assert!(instructions.iter().any(|instruction| matches!(
        instruction,
        mir::Instruction::Assign {
            value: mir::Rvalue {
                kind: mir::RvalueKind::ListVersion(_),
                ..
            },
            ..
        }
    )));
    assert!(
        instructions
            .iter()
            .any(|instruction| matches!(instruction, mir::Instruction::ListGet { .. }))
    );
}

#[test]
fn foreach_over_a_string_is_typed_and_lowers_to_a_utf8_cursor_cfg() {
    // M3D: `string` is now a valid `foreach` collection (M3B/M3C only
    // accepted arrays/`List<T>`), always producing `char`. Confirms the
    // version-checked shape `lower_foreach_over_string` actually produces: a
    // `StringByteLength` read and a `StringDecodeNext` (never `ArrayLength`,
    // `ListLength`, `Place::Index`, or `ListGet`).
    let compilation = aster_compiler::compile(
        "public int Run() { string text = \"ab\"; int total = 0; foreach (char value in text) { total += 1; } return total; }",
    )
    .expect("valid string foreach");
    let function = &compilation.mir.functions[0];
    let instructions = function
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .collect::<Vec<_>>();
    assert!(instructions.iter().any(|instruction| matches!(
        instruction,
        mir::Instruction::Assign {
            value: mir::Rvalue {
                kind: mir::RvalueKind::StringByteLength(_),
                ..
            },
            ..
        }
    )));
    assert!(
        instructions
            .iter()
            .any(|instruction| matches!(instruction, mir::Instruction::StringDecodeNext { .. }))
    );
}

#[test]
fn foreach_diagnostics_preserve_array_only_and_readonly_rules() {
    for (source, message) in [
        (
            "public int Run() { int[] values = [1]; foreach (string value in values) { } return 0; }",
            "does not match array element type",
        ),
        (
            "public int Run() { string value = \"x\"; foreach (int item in value) { } return 0; }",
            "requires element type",
        ),
        (
            "public int Run() { int[] values = [1]; foreach (int value in values) { value = 2; } return 0; }",
            "foreach variable `value` is read-only",
        ),
        (
            "public struct Point { public int X; } public int Run() { Point[] values = [Point { X: 1 }]; foreach (Point value in values) { value.X = 2; } return 0; }",
            "foreach variable `value` is read-only",
        ),
    ] {
        let diagnostics = aster_compiler::compile(source).expect_err("source must be rejected");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains(message)),
            "missing `{message}` in {diagnostics:#?}"
        );
    }
}
