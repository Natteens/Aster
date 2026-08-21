use aster_codegen_cranelift::{ExecutionValue, execute, validate};
use aster_compiler::{compile, mir};

const SOURCE: &str = r"
    public int Run() {
        int[] values = new int[8];
        for (int i = 0; i < values.Length; i++) {
            values[i] = values[i] + 1;
        }
        return values[7];
    }
";

fn compiled() -> mir::Module {
    compile(SOURCE)
        .unwrap_or_else(|diagnostics| panic!("source must compile: {diagnostics:#?}"))
        .mir
}

fn compiled_two_arrays() -> mir::Module {
    compile(
        "public int Run() { int[] a = new int[5]; int[] b = new int[1]; for (int i = 0; i < a.Length; i++) { a[i] = i; int value = b[i]; } return a[4]; }",
    )
    .unwrap_or_else(|diagnostics| panic!("source must compile: {diagnostics:#?}"))
    .mir
}

fn proven_coordinates(
    module: &mir::Module,
) -> (
    mir::BasicBlockId,
    mir::BasicBlockId,
    mir::LocalId,
    mir::LocalId,
) {
    for function in &module.functions {
        for block in &function.blocks {
            for instruction in &block.instructions {
                let mir::Instruction::Assign {
                    target:
                        mir::Place::Index {
                            array,
                            index,
                            bounds: mir::ArrayBounds::Proven { loop_header },
                            ..
                        },
                    ..
                } = instruction
                else {
                    continue;
                };
                let mir::OperandKind::Copy(mir::Place::Local(array)) = array.kind else {
                    continue;
                };
                let mir::OperandKind::Copy(mir::Place::Local(index)) = index.kind else {
                    continue;
                };
                return (block.id, *loop_header, array, index);
            }
        }
    }
    panic!("expected a compiler-proven array assignment")
}

fn fresh_bool_local(function: &mut mir::Function) -> mir::LocalId {
    let id = function
        .parameters
        .iter()
        .chain(&function.locals)
        .map(|local| local.id.0)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .map(mir::LocalId)
        .expect("test local identifier fits");
    function.locals.push(mir::Local {
        id,
        symbol: None,
        name: "adulterated_condition".into(),
        type_: mir::Type::Bool,
        mutable: true,
        temporary: false,
    });
    id
}

fn true_assignment(local: mir::LocalId) -> mir::Instruction {
    mir::Instruction::Assign {
        target: mir::Place::Local(local),
        value: mir::Rvalue {
            type_: mir::Type::Bool,
            kind: mir::RvalueKind::Use(mir::Operand {
                type_: mir::Type::Bool,
                kind: mir::OperandKind::Constant(mir::Constant::Boolean(true)),
            }),
        },
    }
}

fn assert_invalid_proof(module: &mir::Module, scenario: &str) {
    let error = validate(module).expect_err(scenario);
    assert!(
        error
            .to_string()
            .contains("invalid proven array-bounds contract"),
        "{scenario}: {error}"
    );
}

fn first_proven_place_mut(module: &mut mir::Module) -> &mut mir::Place {
    for function in &mut module.functions {
        for block in &mut function.blocks {
            for instruction in &mut block.instructions {
                if let mir::Instruction::Assign { target, value } = instruction {
                    if matches!(
                        target,
                        mir::Place::Index {
                            bounds: mir::ArrayBounds::Proven { .. },
                            ..
                        }
                    ) {
                        return target;
                    }
                    if let mir::RvalueKind::Use(mir::Operand {
                        kind: mir::OperandKind::Copy(place),
                        ..
                    }) = &mut value.kind
                        && matches!(
                            place,
                            mir::Place::Index {
                                bounds: mir::ArrayBounds::Proven { .. },
                                ..
                            }
                        )
                    {
                        return place;
                    }
                }
            }
        }
    }
    panic!("expected a compiler-proven array index")
}

#[test]
fn validated_proven_access_executes_without_changing_results() {
    let module = compiled();
    validate(&module).expect("compiler-authorized array proof validates");
    assert_eq!(execute(&module, "Run"), Ok(ExecutionValue::Int(1)));
}

#[test]
fn missing_header_and_nonlocal_index_are_rejected_before_codegen() {
    let mut missing_header = compiled();
    let mir::Place::Index { bounds, .. } = first_proven_place_mut(&mut missing_header) else {
        unreachable!()
    };
    *bounds = mir::ArrayBounds::Proven {
        loop_header: mir::BasicBlockId(u32::MAX),
    };
    validate(&missing_header).expect_err("unknown proof header must fail closed");

    let mut nonlocal = compiled();
    let mir::Place::Index { index, .. } = first_proven_place_mut(&mut nonlocal) else {
        unreachable!()
    };
    **index = mir::Operand {
        type_: mir::Type::Int,
        kind: mir::OperandKind::Constant(mir::Constant::Integer("-1".into())),
    };
    validate(&nonlocal).expect_err("a proven negative constant index must fail closed");
}

#[test]
fn wrong_array_and_non_dominating_use_are_rejected_before_codegen() {
    let mut wrong_array = compiled_two_arrays();
    let other_array = wrong_array.functions[0]
        .locals
        .iter()
        .find(|local| local.name == "b")
        .expect("array local")
        .id;
    let mir::Place::Index { array, .. } = first_proven_place_mut(&mut wrong_array) else {
        unreachable!()
    };
    **array = mir::Operand {
        type_: mir::Type::Array(Box::new(mir::Type::Int)),
        kind: mir::OperandKind::Copy(mir::Place::Local(other_array)),
    };
    assert_invalid_proof(&wrong_array, "a mismatched array identity must fail closed");

    let mut outside_loop = compiled();
    let header = match first_proven_place_mut(&mut outside_loop) {
        mir::Place::Index {
            bounds: mir::ArrayBounds::Proven { loop_header },
            ..
        } => *loop_header,
        _ => unreachable!(),
    };
    let function = &mut outside_loop.functions[0];
    let checked = function
        .blocks
        .iter_mut()
        .find_map(|block| {
            let mir::Terminator::Return(Some(mir::Operand {
                kind:
                    mir::OperandKind::Copy(
                        place @ mir::Place::Index {
                            bounds: mir::ArrayBounds::Checked,
                            ..
                        },
                    ),
                ..
            })) = &mut block.terminator
            else {
                return None;
            };
            Some(place)
        })
        .expect("post-loop checked index");
    let mir::Place::Index { bounds, .. } = checked else {
        unreachable!()
    };
    *bounds = mir::ArrayBounds::Proven {
        loop_header: header,
    };
    validate(&outside_loop).expect_err("proof must dominate the marked access");
}

#[test]
fn operand_and_element_types_must_match_the_proven_array_contract() {
    let mut wrong_array_type = compiled();
    let mir::Place::Index { array, .. } = first_proven_place_mut(&mut wrong_array_type) else {
        unreachable!()
    };
    array.type_ = mir::Type::Int;
    assert_invalid_proof(
        &wrong_array_type,
        "array operand type mismatch must fail closed",
    );

    let mut wrong_index_type = compiled();
    let mir::Place::Index { index, .. } = first_proven_place_mut(&mut wrong_index_type) else {
        unreachable!()
    };
    index.type_ = mir::Type::Long;
    assert_invalid_proof(
        &wrong_index_type,
        "index operand type mismatch must fail closed",
    );

    let mut wrong_element_type = compiled();
    let mir::Place::Index { element_type, .. } = first_proven_place_mut(&mut wrong_element_type)
    else {
        unreachable!()
    };
    *element_type = mir::Type::Long;
    assert_invalid_proof(
        &wrong_element_type,
        "element type mismatch must fail closed",
    );
}

#[test]
fn incomplete_induction_and_wrong_array_length_are_rejected() {
    let mut missing_increment = compiled();
    let function = &mut missing_increment.functions[0];
    let proven_index = function
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .find_map(|instruction| {
            let mir::Instruction::Assign {
                target:
                    mir::Place::Index {
                        index,
                        bounds: mir::ArrayBounds::Proven { .. },
                        ..
                    },
                ..
            } = instruction
            else {
                return None;
            };
            let mir::OperandKind::Copy(mir::Place::Local(index)) = index.kind else {
                return None;
            };
            Some(index)
        })
        .expect("proven induction local");
    let increment = function
        .blocks
        .iter_mut()
        .flat_map(|block| &mut block.instructions)
        .find(|instruction| {
            matches!(
                instruction,
                mir::Instruction::Assign {
                    target: mir::Place::Local(target),
                    value: mir::Rvalue {
                        kind: mir::RvalueKind::Binary {
                            operator: mir::BinaryOperator::Add,
                            ..
                        },
                        ..
                    },
                    ..
                } if *target == proven_index
            )
        })
        .expect("induction increment");
    if let mir::Instruction::Assign { value, .. } = increment
        && let mir::RvalueKind::Binary { right, .. } = &mut value.kind
    {
        right.kind = mir::OperandKind::Constant(mir::Constant::Integer("2".into()));
    }
    assert_invalid_proof(&missing_increment, "non-unit induction must fail closed");

    let mut length_after_use = compiled();
    let function = &mut length_after_use.functions[0];
    for block in &mut function.blocks {
        if let Some(length_index) = block.instructions.iter().position(|instruction| {
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
        }) {
            let length = block.instructions.remove(length_index);
            block.instructions.insert(0, length);
            break;
        }
    }
    assert_invalid_proof(
        &length_after_use,
        "length before array definition must fail closed",
    );
}

#[test]
fn final_mir_reassignment_and_extra_induction_definition_are_rejected() {
    let mut reassigned = compiled();
    let (access_block, _, array, _) = proven_coordinates(&reassigned);
    let function = &mut reassigned.functions[0];
    let block = function
        .blocks
        .iter_mut()
        .find(|block| block.id == access_block)
        .expect("access block");
    block.instructions.insert(
        0,
        mir::Instruction::Assign {
            target: mir::Place::Local(array),
            value: mir::Rvalue {
                type_: mir::Type::Array(Box::new(mir::Type::Int)),
                kind: mir::RvalueKind::Use(mir::Operand {
                    type_: mir::Type::Array(Box::new(mir::Type::Int)),
                    kind: mir::OperandKind::Copy(mir::Place::Local(array)),
                }),
            },
        },
    );
    assert_invalid_proof(
        &reassigned,
        "a final-MIR array redefinition must fail closed",
    );

    let mut redefined_index = compiled();
    let (access_block, _, _, index) = proven_coordinates(&redefined_index);
    let block = redefined_index.functions[0]
        .blocks
        .iter_mut()
        .find(|block| block.id == access_block)
        .expect("access block");
    block.instructions.insert(
        0,
        mir::Instruction::Assign {
            target: mir::Place::Local(index),
            value: mir::Rvalue {
                type_: mir::Type::Int,
                kind: mir::RvalueKind::Use(mir::Operand {
                    type_: mir::Type::Int,
                    kind: mir::OperandKind::Constant(mir::Constant::Integer("0".into())),
                }),
            },
        },
    );
    assert_invalid_proof(
        &redefined_index,
        "an extra induction definition must fail closed",
    );
}

#[test]
fn final_mir_preheader_and_early_exit_mutations_are_rejected() {
    let mut branched_preheader = compiled();
    let (_, header, _, _) = proven_coordinates(&branched_preheader);
    let function = &mut branched_preheader.functions[0];
    let mir::Terminator::Branch {
        else_block: loop_exit,
        ..
    } = function
        .blocks
        .iter()
        .find(|block| block.id == header)
        .expect("loop header")
        .terminator
    else {
        panic!("canonical header branches");
    };
    let condition = fresh_bool_local(function);
    let preheader = function
        .blocks
        .iter_mut()
        .find(|block| {
            block.instructions.iter().any(|instruction| {
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
        })
        .expect("hoisted-length preheader");
    preheader.instructions.push(true_assignment(condition));
    preheader.terminator = mir::Terminator::Branch {
        condition: mir::Operand {
            type_: mir::Type::Bool,
            kind: mir::OperandKind::Copy(mir::Place::Local(condition)),
        },
        then_block: header,
        else_block: loop_exit,
    };
    assert_invalid_proof(
        &branched_preheader,
        "a noncanonical final-MIR preheader must fail closed",
    );

    let mut early_exit = compiled();
    let (access_block, header, _, _) = proven_coordinates(&early_exit);
    let function = &mut early_exit.functions[0];
    let mir::Terminator::Branch {
        else_block: loop_exit,
        ..
    } = function
        .blocks
        .iter()
        .find(|block| block.id == header)
        .expect("loop header")
        .terminator
    else {
        panic!("canonical header branches");
    };
    let condition = fresh_bool_local(function);
    let block = function
        .blocks
        .iter_mut()
        .find(|block| block.id == access_block)
        .expect("access block");
    let mir::Terminator::Goto(normal_successor) = block.terminator else {
        panic!("canonical body jumps to its latch");
    };
    block.instructions.push(true_assignment(condition));
    block.terminator = mir::Terminator::Branch {
        condition: mir::Operand {
            type_: mir::Type::Bool,
            kind: mir::OperandKind::Copy(mir::Place::Local(condition)),
        },
        then_block: normal_successor,
        else_block: loop_exit,
    };
    assert_invalid_proof(
        &early_exit,
        "an early-exit edge added after proof must fail closed",
    );
}

#[test]
fn ordinary_noncanonical_accesses_keep_controlled_bounds_failures() {
    let module = compile(
        "public int Run() { int[] values = new int[1]; for (int i = 0; i < values.Length; i++) { values[i] = 7; } return values[1]; }",
    )
    .expect("source compiles")
    .mir;
    let error = execute(&module, "Run").expect_err("post-loop access must remain checked");
    assert!(error.to_string().contains("array index"), "{error}");
}

#[test]
fn proven_and_checked_arrays_coexist_and_the_shorter_array_still_fails() {
    let module = compiled_two_arrays();
    let debug = module.to_string();
    assert!(debug.contains("bounds: Proven"));
    assert!(debug.contains("bounds: Checked"));
    let error = execute(&module, "Run").expect_err("the shorter second array must stay checked");
    assert!(error.to_string().contains("array index"), "{error}");
}

#[test]
fn negative_noncanonical_access_keeps_the_controlled_error_path() {
    let module = compile(
        "public int Run() { int[] values = new int[1]; int index = -1; return values[index]; }",
    )
    .expect("source compiles")
    .mir;
    let error = execute(&module, "Run").expect_err("negative access must remain checked");
    assert!(error.to_string().contains("array index"), "{error}");
}

#[test]
fn canonical_zero_and_single_element_loops_preserve_bounds_and_results() {
    let zero = compile(
        "public int Run() { int[] values = new int[0]; for (int i = 0; i < values.Length; i++) { values[i] = 9; } return values.Length; }",
    )
    .expect("zero-length loop compiles")
    .mir;
    assert_eq!(execute(&zero, "Run"), Ok(ExecutionValue::Int(0)));

    let one = compile(
        "public int Run() { int[] values = new int[1]; for (int i = 0; i < values.Length; i++) { values[i] = 9; } return values[0]; }",
    )
    .expect("single-element loop compiles")
    .mir;
    assert_eq!(execute(&one, "Run"), Ok(ExecutionValue::Int(9)));
}

#[test]
fn final_aarm_and_owned_region_markers_preserve_a_still_valid_proof() {
    let aarm = compile(
        "public int Run() { int[] values = new int[4]; for (int i = 0; i < values.Length; i++) { int[] a = new int[1]; int[] b = new int[1]; int[] c = new int[1]; a[0] = i; b[0] = i; c[0] = i; values[i] = a[0] + b[0] + c[0]; } return values[3]; }",
    )
    .expect("AARM composition source compiles")
    .mir;
    let aarm_debug = aarm.to_string();
    assert!(aarm_debug.contains("TemporarySubregionEnter"));
    assert!(aarm_debug.contains("bounds: Proven"));
    validate(&aarm).expect("final MIR with AARM markers validates");
    assert_eq!(execute(&aarm, "Run"), Ok(ExecutionValue::Int(9)));

    let owned = compile(
        "internal int[] Make(int value) { return [value]; } public int Run() { int[] values = new int[4]; for (int i = 0; i < values.Length; i++) { int[] item = Make(i); values[i] = item[0]; } return values[3]; }",
    )
    .expect("owned-region composition source compiles")
    .mir;
    let owned_debug = owned.to_string();
    assert!(owned_debug.contains("OwnedRegionEnter"));
    assert!(owned_debug.contains("bounds: Proven"));
    validate(&owned).expect("final MIR with owned-region markers validates");
    assert_eq!(execute(&owned, "Run"), Ok(ExecutionValue::Int(3)));
}
