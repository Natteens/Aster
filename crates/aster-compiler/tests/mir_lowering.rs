use aster_compiler::{compile, compile_without_mir_optimizer_for_research, mir};

fn compile_valid(source: &str) -> aster_compiler::Compilation {
    compile_without_mir_optimizer_for_research(source)
        .unwrap_or_else(|diagnostics| panic!("expected valid source: {diagnostics:#?}"))
}

fn function<'a>(module: &'a mir::Module, name: &str) -> &'a mir::Function {
    module
        .functions
        .iter()
        .find(|function| function.name == name)
        .unwrap_or_else(|| panic!("missing MIR function {name}"))
}

#[test]
fn lowers_simple_function_return() {
    let compilation = compile_valid("public int Value() { return 42; }");
    let function = function(&compilation.mir, "Value");
    assert_eq!(function.return_type, mir::Type::Int);
    assert!(matches!(
        function.blocks[0].terminator,
        mir::Terminator::Return(Some(mir::Operand {
            kind: mir::OperandKind::Constant(mir::Constant::Integer(ref value)),
            ..
        })) if value == "42"
    ));
}

#[test]
fn lowers_variables_and_assignments() {
    let compilation =
        compile_valid("public int Work() { int value = 1; value += 2; return value; }");
    let function = function(&compilation.mir, "Work");
    let value = function
        .locals
        .iter()
        .find(|local| local.name == "value")
        .expect("source local should be preserved");
    assert_eq!(value.type_, mir::Type::Int);
    assert!(value.symbol.is_some());
    assert!(function.blocks[0].instructions.iter().any(|instruction| {
        matches!(
            instruction,
            mir::Instruction::Assign {
                target: mir::Place::Local(target),
                value: mir::Rvalue {
                    kind: mir::RvalueKind::Binary { operator: mir::BinaryOperator::Add, .. },
                    ..
                }
            } if *target == value.id
        )
    }));
}

#[test]
fn lowers_standard_math_domain_failures_to_a_typed_intrinsic() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("examples/math_basics.aster");
    let compilation = aster_compiler::compile_project(&root).expect("math example should compile");
    let hir_intrinsics = compilation
        .compilation
        .hir
        .items
        .iter()
        .filter_map(|item| {
            let aster_compiler::hir::Item::Function(function) = item else {
                return None;
            };
            function.intrinsic
        })
        .collect::<Vec<_>>();
    assert_eq!(hir_intrinsics.len(), 3);

    let mir_intrinsics = compilation
        .compilation
        .mir
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| {
            let mir::Instruction::CallIntrinsic {
                intrinsic: mir::Intrinsic::ReportRuntimeError(kind),
                ..
            } = instruction
            else {
                return None;
            };
            Some(*kind)
        })
        .collect::<Vec<_>>();
    for expected in [
        mir::RuntimeErrorKind::MathAbsIntOverflow,
        mir::RuntimeErrorKind::MathAbsLongOverflow,
        mir::RuntimeErrorKind::MathClampInvalidRange,
    ] {
        assert!(mir_intrinsics.contains(&expected));
    }
    assert!(
        !compilation
            .compilation
            .mir
            .functions
            .iter()
            .any(|function| {
                function.name.contains("__Abs") || function.name.contains("__ClampInvalidRange")
            })
    );
}

#[test]
fn preserves_local_constants_as_immutable_typed_locals() {
    let compilation =
        compile_valid("public int Work() { const int MaxScore = 100; return MaxScore; }");
    let function = function(&compilation.mir, "Work");
    let constant = function
        .locals
        .iter()
        .find(|local| local.name == "MaxScore")
        .expect("constant should have a MIR local");
    assert_eq!(constant.type_, mir::Type::Int);
    assert!(!constant.mutable);
    assert!(constant.symbol.is_some());
}

#[test]
fn lowers_if_else_to_branches() {
    let compilation = compile_valid(
        "public int Choose(bool ready) { if (ready) { return 1; } else { return 2; } }",
    );
    let function = function(&compilation.mir, "Choose");
    assert_eq!(function.parameters[0].type_, mir::Type::Bool);
    assert!(matches!(
        function.blocks[0].terminator,
        mir::Terminator::Branch { .. }
    ));
    assert_eq!(
        function
            .blocks
            .iter()
            .filter(|block| matches!(block.terminator, mir::Terminator::Return(_)))
            .count(),
        2
    );
}

#[test]
fn lowers_while_to_a_control_flow_cycle() {
    let compilation = compile_valid("public void Work(bool ready) { while (ready) { break; } }");
    let function = function(&compilation.mir, "Work");
    assert!(matches!(
        function.blocks[0].terminator,
        mir::Terminator::Goto(_)
    ));
    assert!(
        function
            .blocks
            .iter()
            .any(|block| matches!(block.terminator, mir::Terminator::Branch { .. }))
    );
}

#[test]
fn lowers_for_to_condition_body_update_and_exit_blocks() {
    let compilation = compile_valid(
        "public void Work() { for (int index = 0; index < 3; index += 1) { Log(\"tick\"); } }",
    );
    let function = function(&compilation.mir, "Work");
    assert!(function.locals.iter().any(|local| local.name == "index"));
    assert!(function.blocks.len() >= 5);
    assert!(
        function
            .blocks
            .iter()
            .any(|block| matches!(block.terminator, mir::Terminator::Branch { .. }))
    );
    assert!(function.blocks.iter().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                mir::Instruction::Assign {
                    value: mir::Rvalue {
                        kind: mir::RvalueKind::Binary {
                            operator: mir::BinaryOperator::Less,
                            ..
                        },
                        ..
                    },
                    ..
                }
            )
        })
    }));
}

#[test]
fn lowers_resolved_function_calls() {
    let compilation = compile_valid(
        "public int Add(int left, int right) { return left + right; } public int Use() { return Add(1, 2); }",
    );
    let add = function(&compilation.mir, "Add");
    let use_function = function(&compilation.mir, "Use");
    assert!(use_function.blocks.iter().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(instruction, mir::Instruction::Call { function, return_type: mir::Type::Int, .. } if *function == add.symbol)
        })
    }));
}

#[test]
fn void_function_ends_without_a_return_value() {
    let compilation = compile_valid("public void Work() { Log(\"working\"); }");
    let function = function(&compilation.mir, "Work");
    assert!(function.blocks.iter().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                mir::Instruction::CallIntrinsic {
                    destination: None,
                    intrinsic: mir::Intrinsic::Log,
                    return_type: mir::Type::Void,
                    ..
                }
            )
        })
    }));
    assert!(matches!(
        function.blocks.last().unwrap().terminator,
        mir::Terminator::End
    ));
}

#[test]
fn rejects_invalid_code_before_mir_lowering() {
    let diagnostics = compile("public int Value() { return false; }")
        .expect_err("semantic errors must prevent MIR construction");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("expected `int`, found `bool`"))
    );
}
