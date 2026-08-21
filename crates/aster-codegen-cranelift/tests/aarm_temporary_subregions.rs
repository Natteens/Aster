use aster_codegen_cranelift::{ExecutionValue, execute_with_stats, validate};
use aster_compiler::{compile, lower_aarm_temporary_subregions_for_research};
use aster_mir as mir;

const RUN: mir::SymbolId = mir::SymbolId(1);
const BUILD: mir::SymbolId = mir::SymbolId(2);
const BOX_CLASS: mir::SymbolId = mir::SymbolId(100);
const VALUE_FIELD: mir::SymbolId = mir::SymbolId(101);
const STRING_BUILDER_CLASS: mir::SymbolId = mir::SymbolId(102);
const BLOCK: mir::BasicBlockId = mir::BasicBlockId(0);

fn local(id: u32, name: &str, type_: mir::Type) -> mir::Local {
    mir::Local {
        id: mir::LocalId(id),
        symbol: None,
        name: name.to_owned(),
        type_,
        mutable: true,
        temporary: true,
    }
}

fn integer(value: i32) -> mir::Operand {
    mir::Operand {
        type_: mir::Type::Int,
        kind: mir::OperandKind::Constant(mir::Constant::Integer(value.to_string())),
    }
}

fn copy(local: u32, type_: mir::Type) -> mir::Operand {
    copy_place(mir::Place::Local(mir::LocalId(local)), type_)
}

fn copy_place(place: mir::Place, type_: mir::Type) -> mir::Operand {
    mir::Operand {
        type_,
        kind: mir::OperandKind::Copy(place),
    }
}

fn object_field(local: u32) -> mir::Place {
    mir::Place::ObjectField {
        object: Box::new(copy(local, mir::Type::Class(BOX_CLASS))),
        field: VALUE_FIELD,
    }
}

fn array_index(local: u32, index: i32) -> mir::Place {
    mir::Place::Index {
        array: Box::new(copy(local, mir::Type::Array(Box::new(mir::Type::Int)))),
        index: Box::new(integer(index)),
        element_type: mir::Type::Int,
        bounds: mir::ArrayBounds::Checked,
    }
}

fn use_operand(operand: mir::Operand) -> mir::Rvalue {
    mir::Rvalue {
        type_: operand.type_.clone(),
        kind: mir::RvalueKind::Use(operand),
    }
}

fn assign(target: mir::Place, operand: mir::Operand) -> mir::Instruction {
    mir::Instruction::Assign {
        target,
        value: use_operand(operand),
    }
}

fn add(target: u32, left: u32, right: u32) -> mir::Instruction {
    mir::Instruction::Assign {
        target: mir::Place::Local(mir::LocalId(target)),
        value: mir::Rvalue {
            type_: mir::Type::Int,
            kind: mir::RvalueKind::Binary {
                left: copy(left, mir::Type::Int),
                operator: mir::BinaryOperator::Add,
                right: copy(right, mir::Type::Int),
            },
        },
    }
}

fn less(target: u32, left: u32, limit: i32) -> mir::Instruction {
    mir::Instruction::Assign {
        target: mir::Place::Local(mir::LocalId(target)),
        value: mir::Rvalue {
            type_: mir::Type::Bool,
            kind: mir::RvalueKind::Binary {
                left: copy(left, mir::Type::Int),
                operator: mir::BinaryOperator::Less,
                right: integer(limit),
            },
        },
    }
}

fn allocate_object(local: u32) -> mir::Instruction {
    mir::Instruction::AllocateObject {
        destination: mir::Place::Local(mir::LocalId(local)),
        class: BOX_CLASS,
        region: mir::AllocationRegion::Persistent,
    }
}

fn temporary_object(local: u32) -> mir::Instruction {
    mir::Instruction::AllocateObject {
        destination: mir::Place::Local(mir::LocalId(local)),
        class: BOX_CLASS,
        region: mir::AllocationRegion::Temporary,
    }
}

fn temporary_array(local: u32, length: i32) -> mir::Instruction {
    mir::Instruction::AllocateArray {
        destination: mir::Place::Local(mir::LocalId(local)),
        element_type: mir::Type::Int,
        length: integer(length),
        initialization: mir::ArrayInitialization::Default,
        region: mir::AllocationRegion::Temporary,
    }
}

fn temporary_builder(local: u32) -> mir::Instruction {
    mir::Instruction::AllocateStringBuilder {
        destination: mir::Place::Local(mir::LocalId(local)),
        class: STRING_BUILDER_CLASS,
        region: mir::AllocationRegion::Temporary,
    }
}

fn builder_append(local: u32, value: &str) -> mir::Instruction {
    mir::Instruction::StringBuilderAppend {
        builder: copy(local, mir::Type::Class(STRING_BUILDER_CLASS)),
        value: mir::Operand {
            type_: mir::Type::String,
            kind: mir::OperandKind::Constant(mir::Constant::String(value.to_owned())),
        },
        class: STRING_BUILDER_CLASS,
    }
}

fn temporary_list(local: u32) -> mir::Instruction {
    mir::Instruction::AllocateList {
        destination: mir::Place::Local(mir::LocalId(local)),
        element_type: mir::Type::Int,
        region: mir::AllocationRegion::Temporary,
    }
}

fn list_add(local: u32, value: i32) -> mir::Instruction {
    mir::Instruction::ListAdd {
        list: copy(local, mir::Type::List(Box::new(mir::Type::Int))),
        value: integer(value),
    }
}

fn temporary_dictionary(local: u32) -> mir::Instruction {
    mir::Instruction::AllocateDictionary {
        destination: mir::Place::Local(mir::LocalId(local)),
        key_type: mir::Type::Int,
        value_type: mir::Type::Int,
        region: mir::AllocationRegion::Temporary,
    }
}

fn dictionary_set(local: u32, destination: u32, key: i32, value: i32) -> mir::Instruction {
    mir::Instruction::DictionarySet {
        destination: mir::Place::Local(mir::LocalId(destination)),
        dictionary: copy(
            local,
            mir::Type::Dictionary(Box::new(mir::Type::Int), Box::new(mir::Type::Int)),
        ),
        key: integer(key),
        value: integer(value),
    }
}

fn enter(id: u32) -> mir::Instruction {
    mir::Instruction::TemporarySubregionEnter {
        id: mir::TemporarySubregionId(id),
    }
}

fn exit(id: u32) -> mir::Instruction {
    mir::Instruction::TemporarySubregionExit {
        id: mir::TemporarySubregionId(id),
    }
}

fn function(
    symbol: mir::SymbolId,
    name: &str,
    visibility: mir::Visibility,
    locals: Vec<mir::Local>,
    return_type: mir::Type,
    instructions: Vec<mir::Instruction>,
    returned: Option<mir::Operand>,
) -> mir::Function {
    mir::Function {
        constructor: false,
        symbol,
        owner: None,
        name: name.to_owned(),
        visibility,
        parameters: Vec::new(),
        locals,
        return_type,
        entry: BLOCK,
        blocks: vec![mir::BasicBlock {
            id: BLOCK,
            instructions,
            terminator: mir::Terminator::Return(returned),
        }],
        temporary_subregion_candidates: Vec::new(),
    }
}

fn module(functions: Vec<mir::Function>) -> mir::Module {
    mir::Module {
        structs: Vec::new(),
        classes: vec![
            mir::ClassDefinition {
                symbol: BOX_CLASS,
                name: "Box".to_owned(),
                fields: vec![mir::FieldDefinition {
                    symbol: VALUE_FIELD,
                    name: "value".to_owned(),
                    type_: mir::Type::Int,
                }],
            },
            mir::ClassDefinition {
                symbol: STRING_BUILDER_CLASS,
                name: "aster.core::StringBuilder".to_owned(),
                fields: Vec::new(),
            },
        ],
        interfaces: Vec::new(),
        enums: Vec::new(),
        interface_implementations: Vec::new(),
        foreign_functions: Vec::new(),
        functions,
    }
}

fn marker_count(module: &mir::Module) -> usize {
    module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
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

#[test]
fn explicit_research_lowering_executes_array_reads_without_changing_default_compilation() {
    let source = "public int Run() { int[] values = [20, 22]; int result = values[0] + values[1] + values.Length - 2; return result; }";
    let compilation = compile(source).expect("array source compiles");
    let baseline = compilation.mir;
    assert_eq!(marker_count(&baseline), 0);

    let baseline_result = execute_with_stats(&baseline, "Run").expect("baseline executes");
    let mut lowered = baseline.clone();
    let report = lower_aarm_temporary_subregions_for_research(&mut lowered)
        .expect("validated array lifetime lowers");
    assert_eq!(report.validated_subregions_received, 1);
    assert_eq!(report.subregions_lowered, 1);
    assert_eq!(report.enter_instructions_inserted, 1);
    assert_eq!(report.exit_instructions_inserted, 1);
    assert_eq!(marker_count(&lowered), 2);
    assert!(
        lowered
            .functions
            .iter()
            .all(|function| function.temporary_subregion_candidates.is_empty())
    );

    let lowered_result =
        execute_with_stats(&lowered, "Run").expect("lowered array program executes");
    assert_eq!(baseline_result.0, ExecutionValue::Int(42));
    assert_eq!(lowered_result.0, baseline_result.0);
    assert_eq!(
        lowered_result.1.total_allocations,
        baseline_result.1.total_allocations
    );
    assert_eq!(
        lowered_result.1.requested_bytes,
        baseline_result.1.requested_bytes
    );
}

#[test]
fn acyclic_diamond_executes_both_paths_with_one_dynamic_fine_exit() {
    for (condition, expected) in [(true, 7), (false, 9)] {
        let mut run = function(
            RUN,
            "Run",
            mir::Visibility::Public,
            vec![
                local(0, "then_values", mir::Type::Array(Box::new(mir::Type::Int))),
                local(1, "else_values", mir::Type::Array(Box::new(mir::Type::Int))),
                local(2, "result", mir::Type::Int),
            ],
            mir::Type::Int,
            Vec::new(),
            Some(copy(2, mir::Type::Int)),
        );
        run.blocks = vec![
            mir::BasicBlock {
                id: mir::BasicBlockId(0),
                instructions: Vec::new(),
                terminator: mir::Terminator::Branch {
                    condition: mir::Operand {
                        type_: mir::Type::Bool,
                        kind: mir::OperandKind::Constant(mir::Constant::Boolean(condition)),
                    },
                    then_block: mir::BasicBlockId(1),
                    else_block: mir::BasicBlockId(2),
                },
            },
            mir::BasicBlock {
                id: mir::BasicBlockId(1),
                instructions: vec![
                    temporary_array(0, 8),
                    assign(mir::Place::Local(mir::LocalId(2)), integer(7)),
                ],
                terminator: mir::Terminator::Goto(mir::BasicBlockId(3)),
            },
            mir::BasicBlock {
                id: mir::BasicBlockId(2),
                instructions: vec![
                    temporary_array(1, 8),
                    assign(mir::Place::Local(mir::LocalId(2)), integer(9)),
                ],
                terminator: mir::Terminator::Goto(mir::BasicBlockId(3)),
            },
            mir::BasicBlock {
                id: mir::BasicBlockId(3),
                instructions: Vec::new(),
                terminator: mir::Terminator::Return(Some(copy(2, mir::Type::Int))),
            },
        ];
        let baseline = module(vec![run]);
        let baseline_result =
            execute_with_stats(&baseline, "Run").expect("baseline diamond executes");
        let mut lowered = baseline.clone();
        let report = lower_aarm_temporary_subregions_for_research(&mut lowered)
            .expect("acyclic diamond lowers");
        assert_eq!(report.subregions_lowered, 1);
        assert_eq!(report.enter_instructions_inserted, 1);
        assert_eq!(report.exit_instructions_inserted, 2);
        assert_eq!(marker_count(&lowered), 3);
        let lowered_result = execute_with_stats(&lowered, "Run").expect("lowered diamond executes");
        assert_eq!(baseline_result.0, ExecutionValue::Int(expected));
        assert_eq!(lowered_result.0, baseline_result.0);
    }
}

#[test]
fn simple_natural_array_loop_rewinds_each_iteration_without_changing_result() {
    let mut run = function(
        RUN,
        "Run",
        mir::Visibility::Public,
        vec![
            local(0, "values", mir::Type::Array(Box::new(mir::Type::Int))),
            local(1, "index", mir::Type::Int),
            local(2, "condition", mir::Type::Bool),
            local(3, "one", mir::Type::Int),
        ],
        mir::Type::Int,
        Vec::new(),
        None,
    );
    run.blocks = vec![
        mir::BasicBlock {
            id: mir::BasicBlockId(0),
            instructions: vec![assign(mir::Place::Local(mir::LocalId(1)), integer(0))],
            terminator: mir::Terminator::Goto(mir::BasicBlockId(1)),
        },
        mir::BasicBlock {
            id: mir::BasicBlockId(1),
            instructions: vec![less(2, 1, 64)],
            terminator: mir::Terminator::Branch {
                condition: copy(2, mir::Type::Bool),
                then_block: mir::BasicBlockId(2),
                else_block: mir::BasicBlockId(4),
            },
        },
        mir::BasicBlock {
            id: mir::BasicBlockId(2),
            instructions: vec![temporary_array(0, 256)],
            terminator: mir::Terminator::Goto(mir::BasicBlockId(3)),
        },
        mir::BasicBlock {
            id: mir::BasicBlockId(3),
            instructions: vec![
                assign(mir::Place::Local(mir::LocalId(3)), integer(1)),
                add(1, 1, 3),
            ],
            terminator: mir::Terminator::Goto(mir::BasicBlockId(1)),
        },
        mir::BasicBlock {
            id: mir::BasicBlockId(4),
            instructions: Vec::new(),
            terminator: mir::Terminator::Return(Some(copy(1, mir::Type::Int))),
        },
    ];
    let baseline = module(vec![run]);
    let baseline_result = execute_with_stats(&baseline, "Run").expect("baseline loop executes");
    let mut lowered = baseline.clone();
    let report = lower_aarm_temporary_subregions_for_research(&mut lowered)
        .expect("simple loop lowering succeeds");
    assert_eq!(
        (
            report.subregions_lowered,
            report.enter_instructions_inserted,
            report.exit_instructions_inserted,
        ),
        (1, 1, 1)
    );
    let lowered_result = execute_with_stats(&lowered, "Run").expect("lowered loop executes");
    assert_eq!(baseline_result.0, ExecutionValue::Int(64));
    assert_eq!(lowered_result.0, baseline_result.0);
    assert_eq!(
        lowered_result.1.requested_bytes,
        baseline_result.1.requested_bytes
    );
    assert!(lowered_result.1.peak_used_bytes < baseline_result.1.peak_used_bytes);

    let mut active_backedge = lowered;
    active_backedge.functions[0].blocks[3]
        .instructions
        .retain(|instruction| {
            !matches!(instruction, mir::Instruction::TemporarySubregionExit { .. })
        });
    let error = validate(&active_backedge)
        .expect_err("a loop backedge cannot carry an active fine checkpoint");
    assert!(error.message().contains("inconsistent") || error.message().contains("active"));
}

#[test]
fn loop_continue_paths_and_break_after_several_iterations_rewind_exactly_once() {
    let mut run = function(
        RUN,
        "Run",
        mir::Visibility::Public,
        vec![
            local(0, "values", mir::Type::Array(Box::new(mir::Type::Int))),
            local(1, "index", mir::Type::Int),
            local(2, "condition", mir::Type::Bool),
            local(3, "one", mir::Type::Int),
        ],
        mir::Type::Int,
        Vec::new(),
        None,
    );
    run.blocks = vec![
        mir::BasicBlock {
            id: mir::BasicBlockId(0),
            instructions: vec![assign(mir::Place::Local(mir::LocalId(1)), integer(0))],
            terminator: mir::Terminator::Goto(mir::BasicBlockId(1)),
        },
        mir::BasicBlock {
            id: mir::BasicBlockId(1),
            instructions: vec![less(2, 1, 8)],
            terminator: mir::Terminator::Branch {
                condition: copy(2, mir::Type::Bool),
                then_block: mir::BasicBlockId(2),
                else_block: mir::BasicBlockId(5),
            },
        },
        mir::BasicBlock {
            id: mir::BasicBlockId(2),
            instructions: vec![temporary_array(0, 256), less(2, 1, 3)],
            terminator: mir::Terminator::Branch {
                condition: copy(2, mir::Type::Bool),
                then_block: mir::BasicBlockId(3),
                else_block: mir::BasicBlockId(4),
            },
        },
        mir::BasicBlock {
            id: mir::BasicBlockId(3),
            instructions: vec![
                assign(mir::Place::Local(mir::LocalId(3)), integer(1)),
                add(1, 1, 3),
            ],
            terminator: mir::Terminator::Goto(mir::BasicBlockId(1)),
        },
        mir::BasicBlock {
            id: mir::BasicBlockId(4),
            instructions: vec![assign(
                mir::Place::Local(mir::LocalId(3)),
                copy(1, mir::Type::Int),
            )],
            terminator: mir::Terminator::Goto(mir::BasicBlockId(5)),
        },
        mir::BasicBlock {
            id: mir::BasicBlockId(5),
            instructions: Vec::new(),
            terminator: mir::Terminator::Return(Some(copy(1, mir::Type::Int))),
        },
    ];
    let baseline = module(vec![run]);
    let baseline_result = execute_with_stats(&baseline, "Run").expect("baseline executes");
    let mut lowered = baseline.clone();
    let report = lower_aarm_temporary_subregions_for_research(&mut lowered)
        .expect("continue and break loop lowers");
    assert_eq!(
        (
            report.subregions_lowered,
            report.enter_instructions_inserted,
            report.exit_instructions_inserted,
        ),
        (1, 1, 2)
    );
    let lowered_result = execute_with_stats(&lowered, "Run").expect("lowered executes");
    assert_eq!(baseline_result.0, ExecutionValue::Int(3));
    assert_eq!(lowered_result.0, baseline_result.0);
    assert_eq!(
        lowered_result.1.requested_bytes,
        baseline_result.1.requested_bytes
    );
    assert!(lowered_result.1.peak_used_bytes < baseline_result.1.peak_used_bytes);

    let mut active_break = lowered;
    active_break.functions[0].blocks[4]
        .instructions
        .retain(|instruction| {
            !matches!(instruction, mir::Instruction::TemporarySubregionExit { .. })
        });
    validate(&active_break).expect_err("a loop break target cannot receive an active fine state");
}

#[test]
fn multiple_latches_return_to_the_header_with_an_inactive_fine_state() {
    for select_first_latch in [true, false] {
        let mut run = function(
            RUN,
            "Run",
            mir::Visibility::Public,
            vec![
                local(0, "values", mir::Type::Array(Box::new(mir::Type::Int))),
                local(1, "index", mir::Type::Int),
                local(2, "condition", mir::Type::Bool),
                local(3, "one", mir::Type::Int),
            ],
            mir::Type::Int,
            Vec::new(),
            None,
        );
        run.blocks = vec![
            mir::BasicBlock {
                id: mir::BasicBlockId(0),
                instructions: vec![assign(mir::Place::Local(mir::LocalId(1)), integer(0))],
                terminator: mir::Terminator::Goto(mir::BasicBlockId(1)),
            },
            mir::BasicBlock {
                id: mir::BasicBlockId(1),
                instructions: vec![less(2, 1, 8)],
                terminator: mir::Terminator::Branch {
                    condition: copy(2, mir::Type::Bool),
                    then_block: mir::BasicBlockId(2),
                    else_block: mir::BasicBlockId(5),
                },
            },
            mir::BasicBlock {
                id: mir::BasicBlockId(2),
                instructions: vec![temporary_array(0, 128)],
                terminator: mir::Terminator::Branch {
                    condition: mir::Operand {
                        type_: mir::Type::Bool,
                        kind: mir::OperandKind::Constant(mir::Constant::Boolean(
                            select_first_latch,
                        )),
                    },
                    then_block: mir::BasicBlockId(3),
                    else_block: mir::BasicBlockId(4),
                },
            },
            mir::BasicBlock {
                id: mir::BasicBlockId(3),
                instructions: vec![
                    assign(mir::Place::Local(mir::LocalId(3)), integer(1)),
                    add(1, 1, 3),
                ],
                terminator: mir::Terminator::Goto(mir::BasicBlockId(1)),
            },
            mir::BasicBlock {
                id: mir::BasicBlockId(4),
                instructions: vec![
                    assign(mir::Place::Local(mir::LocalId(3)), integer(1)),
                    add(1, 1, 3),
                ],
                terminator: mir::Terminator::Goto(mir::BasicBlockId(1)),
            },
            mir::BasicBlock {
                id: mir::BasicBlockId(5),
                instructions: Vec::new(),
                terminator: mir::Terminator::Return(Some(copy(1, mir::Type::Int))),
            },
        ];
        let baseline = module(vec![run]);
        let baseline_result = execute_with_stats(&baseline, "Run").expect("baseline executes");
        let mut lowered = baseline.clone();
        let report = lower_aarm_temporary_subregions_for_research(&mut lowered)
            .expect("multiple-latch loop lowers");
        assert_eq!(
            (
                report.subregions_lowered,
                report.enter_instructions_inserted,
                report.exit_instructions_inserted,
            ),
            (1, 1, 2)
        );
        let lowered_result = execute_with_stats(&lowered, "Run").expect("lowered executes");
        assert_eq!(baseline_result.0, ExecutionValue::Int(8));
        assert_eq!(lowered_result.0, baseline_result.0);

        let mut active_latch = lowered;
        active_latch.functions[0].blocks[4]
            .instructions
            .retain(|instruction| {
                !matches!(instruction, mir::Instruction::TemporarySubregionExit { .. })
            });
        assert!(
            validate(&active_latch)
                .expect_err("every latch must return an inactive fine state")
                .message()
                .contains("inconsistent")
        );
    }
}

#[test]
fn early_return_from_a_loop_body_exits_the_active_fine_region() {
    let mut run = function(
        RUN,
        "Run",
        mir::Visibility::Public,
        vec![
            local(0, "values", mir::Type::Array(Box::new(mir::Type::Int))),
            local(1, "index", mir::Type::Int),
            local(2, "condition", mir::Type::Bool),
            local(3, "result", mir::Type::Int),
        ],
        mir::Type::Int,
        Vec::new(),
        None,
    );
    run.blocks = vec![
        mir::BasicBlock {
            id: mir::BasicBlockId(0),
            instructions: vec![assign(mir::Place::Local(mir::LocalId(1)), integer(0))],
            terminator: mir::Terminator::Goto(mir::BasicBlockId(1)),
        },
        mir::BasicBlock {
            id: mir::BasicBlockId(1),
            instructions: vec![less(2, 1, 8)],
            terminator: mir::Terminator::Branch {
                condition: copy(2, mir::Type::Bool),
                then_block: mir::BasicBlockId(2),
                else_block: mir::BasicBlockId(5),
            },
        },
        mir::BasicBlock {
            id: mir::BasicBlockId(2),
            instructions: vec![temporary_array(0, 64)],
            terminator: mir::Terminator::Branch {
                condition: mir::Operand {
                    type_: mir::Type::Bool,
                    kind: mir::OperandKind::Constant(mir::Constant::Boolean(true)),
                },
                then_block: mir::BasicBlockId(4),
                else_block: mir::BasicBlockId(3),
            },
        },
        mir::BasicBlock {
            id: mir::BasicBlockId(3),
            instructions: vec![
                assign(mir::Place::Local(mir::LocalId(3)), integer(1)),
                add(1, 1, 3),
            ],
            terminator: mir::Terminator::Goto(mir::BasicBlockId(1)),
        },
        mir::BasicBlock {
            id: mir::BasicBlockId(4),
            instructions: vec![assign(mir::Place::Local(mir::LocalId(3)), integer(7))],
            terminator: mir::Terminator::Return(Some(copy(3, mir::Type::Int))),
        },
        mir::BasicBlock {
            id: mir::BasicBlockId(5),
            instructions: Vec::new(),
            terminator: mir::Terminator::Return(Some(integer(0))),
        },
    ];
    let baseline = module(vec![run]);
    let baseline_result = execute_with_stats(&baseline, "Run").expect("baseline executes");
    let mut lowered = baseline.clone();
    let report = lower_aarm_temporary_subregions_for_research(&mut lowered)
        .expect("early-return loop lowers");
    assert_eq!(
        (
            report.subregions_lowered,
            report.enter_instructions_inserted,
            report.exit_instructions_inserted,
        ),
        (1, 1, 2)
    );
    let lowered_result = execute_with_stats(&lowered, "Run").expect("lowered executes");
    assert_eq!(baseline_result.0, ExecutionValue::Int(7));
    assert_eq!(lowered_result.0, baseline_result.0);

    let mut active_return = lowered;
    active_return.functions[0].blocks[4]
        .instructions
        .retain(|instruction| {
            !matches!(instruction, mir::Instruction::TemporarySubregionExit { .. })
        });
    validate(&active_return).expect_err("a loop-body Return cannot receive an active fine state");
}

#[test]
#[allow(clippy::too_many_lines)]
fn nested_outer_temporary_survives_repeated_inner_fine_rewinds() {
    let mut run = function(
        RUN,
        "Run",
        mir::Visibility::Public,
        vec![
            local(0, "outer", mir::Type::Class(BOX_CLASS)),
            local(1, "inner", mir::Type::Array(Box::new(mir::Type::Int))),
            local(2, "outer_index", mir::Type::Int),
            local(3, "inner_index", mir::Type::Int),
            local(4, "condition", mir::Type::Bool),
            local(5, "one", mir::Type::Int),
        ],
        mir::Type::Int,
        Vec::new(),
        None,
    );
    run.blocks = vec![
        mir::BasicBlock {
            id: mir::BasicBlockId(0),
            instructions: vec![
                assign(mir::Place::Local(mir::LocalId(2)), integer(0)),
                assign(mir::Place::Local(mir::LocalId(5)), integer(1)),
            ],
            terminator: mir::Terminator::Goto(mir::BasicBlockId(1)),
        },
        mir::BasicBlock {
            id: mir::BasicBlockId(1),
            instructions: vec![less(4, 2, 2)],
            terminator: mir::Terminator::Branch {
                condition: copy(4, mir::Type::Bool),
                then_block: mir::BasicBlockId(2),
                else_block: mir::BasicBlockId(6),
            },
        },
        mir::BasicBlock {
            id: mir::BasicBlockId(2),
            instructions: vec![
                temporary_object(0),
                assign(mir::Place::Local(mir::LocalId(3)), integer(0)),
            ],
            terminator: mir::Terminator::Goto(mir::BasicBlockId(3)),
        },
        mir::BasicBlock {
            id: mir::BasicBlockId(3),
            instructions: vec![less(4, 3, 3)],
            terminator: mir::Terminator::Branch {
                condition: copy(4, mir::Type::Bool),
                then_block: mir::BasicBlockId(4),
                else_block: mir::BasicBlockId(5),
            },
        },
        mir::BasicBlock {
            id: mir::BasicBlockId(4),
            instructions: vec![temporary_array(1, 128), add(3, 3, 5)],
            terminator: mir::Terminator::Goto(mir::BasicBlockId(3)),
        },
        mir::BasicBlock {
            id: mir::BasicBlockId(5),
            instructions: vec![
                assign(object_field(0), copy(3, mir::Type::Int)),
                assign(
                    mir::Place::Local(mir::LocalId(3)),
                    copy_place(object_field(0), mir::Type::Int),
                ),
                add(2, 2, 5),
            ],
            terminator: mir::Terminator::Goto(mir::BasicBlockId(1)),
        },
        mir::BasicBlock {
            id: mir::BasicBlockId(6),
            instructions: Vec::new(),
            terminator: mir::Terminator::Return(Some(copy(2, mir::Type::Int))),
        },
    ];
    let baseline = module(vec![run]);
    let baseline_result = execute_with_stats(&baseline, "Run").expect("baseline executes");
    let mut lowered = baseline.clone();
    let report = lower_aarm_temporary_subregions_for_research(&mut lowered)
        .expect("nested leaf loop lowers");
    assert_eq!(
        (
            report.subregions_lowered,
            report.enter_instructions_inserted,
            report.exit_instructions_inserted,
        ),
        (1, 1, 1)
    );
    let lowered_result = execute_with_stats(&lowered, "Run").expect("lowered executes");
    assert_eq!(baseline_result.0, ExecutionValue::Int(2));
    assert_eq!(lowered_result.0, baseline_result.0);
    assert_eq!(
        lowered_result.1.requested_bytes,
        baseline_result.1.requested_bytes
    );
    assert!(lowered_result.1.peak_used_bytes < baseline_result.1.peak_used_bytes);

    let mut active_inner_backedge = lowered;
    active_inner_backedge.functions[0].blocks[4]
        .instructions
        .retain(|instruction| {
            !matches!(instruction, mir::Instruction::TemporarySubregionExit { .. })
        });
    validate(&active_inner_backedge)
        .expect_err("an active inner mark cannot reach the inner or outer loop state");
}

#[test]
fn simple_natural_object_loop_reclaims_a_bounded_working_set() {
    let mut run = function(
        RUN,
        "Run",
        mir::Visibility::Public,
        vec![
            local(0, "value", mir::Type::Class(BOX_CLASS)),
            local(1, "index", mir::Type::Int),
            local(2, "condition", mir::Type::Bool),
            local(3, "one", mir::Type::Int),
        ],
        mir::Type::Int,
        Vec::new(),
        None,
    );
    run.blocks = vec![
        mir::BasicBlock {
            id: mir::BasicBlockId(0),
            instructions: vec![assign(mir::Place::Local(mir::LocalId(1)), integer(0))],
            terminator: mir::Terminator::Goto(mir::BasicBlockId(1)),
        },
        mir::BasicBlock {
            id: mir::BasicBlockId(1),
            instructions: vec![less(2, 1, 64)],
            terminator: mir::Terminator::Branch {
                condition: copy(2, mir::Type::Bool),
                then_block: mir::BasicBlockId(2),
                else_block: mir::BasicBlockId(4),
            },
        },
        mir::BasicBlock {
            id: mir::BasicBlockId(2),
            instructions: vec![temporary_object(0)],
            terminator: mir::Terminator::Goto(mir::BasicBlockId(3)),
        },
        mir::BasicBlock {
            id: mir::BasicBlockId(3),
            instructions: vec![
                assign(mir::Place::Local(mir::LocalId(3)), integer(1)),
                add(1, 1, 3),
            ],
            terminator: mir::Terminator::Goto(mir::BasicBlockId(1)),
        },
        mir::BasicBlock {
            id: mir::BasicBlockId(4),
            instructions: Vec::new(),
            terminator: mir::Terminator::Return(Some(copy(1, mir::Type::Int))),
        },
    ];
    let baseline = module(vec![run]);
    let baseline_result = execute_with_stats(&baseline, "Run").expect("baseline loop executes");
    let mut lowered = baseline.clone();
    let report = lower_aarm_temporary_subregions_for_research(&mut lowered)
        .expect("simple object loop lowering succeeds");
    assert_eq!(report.subregions_lowered, 1);
    let lowered_result = execute_with_stats(&lowered, "Run").expect("lowered loop executes");
    assert_eq!(lowered_result.0, baseline_result.0);
    assert_eq!(
        lowered_result.1.requested_bytes,
        baseline_result.1.requested_bytes
    );
    assert!(lowered_result.1.peak_used_bytes < baseline_result.1.peak_used_bytes);
}

#[test]
fn simple_natural_loop_supports_conditional_and_both_branch_temporary_work() {
    for condition in [true, false] {
        let mut run = function(
            RUN,
            "Run",
            mir::Visibility::Public,
            vec![
                local(0, "left", mir::Type::Array(Box::new(mir::Type::Int))),
                local(1, "right", mir::Type::Array(Box::new(mir::Type::Int))),
                local(2, "index", mir::Type::Int),
                local(3, "loop_condition", mir::Type::Bool),
                local(4, "one", mir::Type::Int),
            ],
            mir::Type::Int,
            Vec::new(),
            None,
        );
        run.blocks = vec![
            mir::BasicBlock {
                id: mir::BasicBlockId(0),
                instructions: vec![assign(mir::Place::Local(mir::LocalId(2)), integer(0))],
                terminator: mir::Terminator::Goto(mir::BasicBlockId(1)),
            },
            mir::BasicBlock {
                id: mir::BasicBlockId(1),
                instructions: vec![less(3, 2, 32)],
                terminator: mir::Terminator::Branch {
                    condition: copy(3, mir::Type::Bool),
                    then_block: mir::BasicBlockId(2),
                    else_block: mir::BasicBlockId(6),
                },
            },
            mir::BasicBlock {
                id: mir::BasicBlockId(2),
                instructions: Vec::new(),
                terminator: mir::Terminator::Branch {
                    condition: mir::Operand {
                        type_: mir::Type::Bool,
                        kind: mir::OperandKind::Constant(mir::Constant::Boolean(condition)),
                    },
                    then_block: mir::BasicBlockId(3),
                    else_block: mir::BasicBlockId(4),
                },
            },
            mir::BasicBlock {
                id: mir::BasicBlockId(3),
                instructions: vec![temporary_array(0, 64)],
                terminator: mir::Terminator::Goto(mir::BasicBlockId(5)),
            },
            mir::BasicBlock {
                id: mir::BasicBlockId(4),
                instructions: vec![temporary_array(1, 128)],
                terminator: mir::Terminator::Goto(mir::BasicBlockId(5)),
            },
            mir::BasicBlock {
                id: mir::BasicBlockId(5),
                instructions: vec![
                    assign(mir::Place::Local(mir::LocalId(4)), integer(1)),
                    add(2, 2, 4),
                ],
                terminator: mir::Terminator::Goto(mir::BasicBlockId(1)),
            },
            mir::BasicBlock {
                id: mir::BasicBlockId(6),
                instructions: Vec::new(),
                terminator: mir::Terminator::Return(Some(copy(2, mir::Type::Int))),
            },
        ];
        let baseline = module(vec![run]);
        let baseline_result = execute_with_stats(&baseline, "Run").expect("baseline executes");
        let mut lowered = baseline.clone();
        let report = lower_aarm_temporary_subregions_for_research(&mut lowered)
            .expect("branching loop lowering succeeds");
        assert_eq!(
            (
                report.subregions_lowered,
                report.enter_instructions_inserted,
                report.exit_instructions_inserted,
            ),
            (1, 1, 1)
        );
        let lowered_result = execute_with_stats(&lowered, "Run").expect("lowered loop executes");
        assert_eq!(lowered_result.0, ExecutionValue::Int(32));
        assert_eq!(lowered_result.0, baseline_result.0);
        assert!(lowered_result.1.peak_used_bytes < baseline_result.1.peak_used_bytes);
    }
}

#[test]
fn acyclic_early_returns_exit_the_active_fine_mark_on_each_path() {
    for (condition, expected) in [(true, 11), (false, 13)] {
        let mut run = function(
            RUN,
            "Run",
            mir::Visibility::Public,
            vec![
                local(0, "then_values", mir::Type::Array(Box::new(mir::Type::Int))),
                local(1, "else_values", mir::Type::Array(Box::new(mir::Type::Int))),
            ],
            mir::Type::Int,
            Vec::new(),
            None,
        );
        run.blocks = vec![
            mir::BasicBlock {
                id: mir::BasicBlockId(0),
                instructions: Vec::new(),
                terminator: mir::Terminator::Branch {
                    condition: mir::Operand {
                        type_: mir::Type::Bool,
                        kind: mir::OperandKind::Constant(mir::Constant::Boolean(condition)),
                    },
                    then_block: mir::BasicBlockId(1),
                    else_block: mir::BasicBlockId(2),
                },
            },
            mir::BasicBlock {
                id: mir::BasicBlockId(1),
                instructions: vec![temporary_array(0, 8)],
                terminator: mir::Terminator::Return(Some(integer(11))),
            },
            mir::BasicBlock {
                id: mir::BasicBlockId(2),
                instructions: vec![temporary_array(1, 8)],
                terminator: mir::Terminator::Return(Some(integer(13))),
            },
        ];
        let baseline = module(vec![run]);
        let baseline_result =
            execute_with_stats(&baseline, "Run").expect("baseline early return executes");
        let mut lowered = baseline.clone();
        let report = lower_aarm_temporary_subregions_for_research(&mut lowered)
            .expect("early-return CFG lowers");
        assert_eq!(
            (
                report.enter_instructions_inserted,
                report.exit_instructions_inserted
            ),
            (1, 2)
        );
        let lowered_result =
            execute_with_stats(&lowered, "Run").expect("lowered early return executes");
        assert_eq!(baseline_result.0, ExecutionValue::Int(expected));
        assert_eq!(lowered_result.0, baseline_result.0);
    }
}

#[test]
fn sequential_acyclic_diamonds_lower_to_disjoint_fine_regions() {
    let mut run = function(
        RUN,
        "Run",
        mir::Visibility::Public,
        vec![
            local(0, "a", mir::Type::Array(Box::new(mir::Type::Int))),
            local(1, "b", mir::Type::Array(Box::new(mir::Type::Int))),
            local(2, "c", mir::Type::Array(Box::new(mir::Type::Int))),
            local(3, "d", mir::Type::Array(Box::new(mir::Type::Int))),
        ],
        mir::Type::Int,
        Vec::new(),
        Some(integer(42)),
    );
    run.blocks = vec![
        mir::BasicBlock {
            id: mir::BasicBlockId(0),
            instructions: Vec::new(),
            terminator: mir::Terminator::Branch {
                condition: mir::Operand {
                    type_: mir::Type::Bool,
                    kind: mir::OperandKind::Constant(mir::Constant::Boolean(true)),
                },
                then_block: mir::BasicBlockId(1),
                else_block: mir::BasicBlockId(2),
            },
        },
        mir::BasicBlock {
            id: mir::BasicBlockId(1),
            instructions: vec![temporary_array(0, 32)],
            terminator: mir::Terminator::Goto(mir::BasicBlockId(3)),
        },
        mir::BasicBlock {
            id: mir::BasicBlockId(2),
            instructions: vec![temporary_array(1, 32)],
            terminator: mir::Terminator::Goto(mir::BasicBlockId(3)),
        },
        mir::BasicBlock {
            id: mir::BasicBlockId(3),
            instructions: Vec::new(),
            terminator: mir::Terminator::Branch {
                condition: mir::Operand {
                    type_: mir::Type::Bool,
                    kind: mir::OperandKind::Constant(mir::Constant::Boolean(false)),
                },
                then_block: mir::BasicBlockId(4),
                else_block: mir::BasicBlockId(5),
            },
        },
        mir::BasicBlock {
            id: mir::BasicBlockId(4),
            instructions: vec![temporary_array(2, 32)],
            terminator: mir::Terminator::Goto(mir::BasicBlockId(6)),
        },
        mir::BasicBlock {
            id: mir::BasicBlockId(5),
            instructions: vec![temporary_array(3, 32)],
            terminator: mir::Terminator::Goto(mir::BasicBlockId(6)),
        },
        mir::BasicBlock {
            id: mir::BasicBlockId(6),
            instructions: Vec::new(),
            terminator: mir::Terminator::Return(Some(integer(42))),
        },
    ];
    let baseline = module(vec![run]);
    let baseline_result = execute_with_stats(&baseline, "Run").expect("baseline executes");
    let mut lowered = baseline.clone();
    let report = lower_aarm_temporary_subregions_for_research(&mut lowered)
        .expect("sequential diamonds lower");
    assert_eq!(report.subregions_lowered, 2);
    assert_eq!(
        (
            report.enter_instructions_inserted,
            report.exit_instructions_inserted
        ),
        (2, 4)
    );
    let lowered_result =
        execute_with_stats(&lowered, "Run").expect("lowered sequential diamonds execute");
    assert_eq!(baseline_result.0, ExecutionValue::Int(42));
    assert_eq!(lowered_result.0, baseline_result.0);
    assert!(lowered_result.1.peak_used_bytes < baseline_result.1.peak_used_bytes);
}

#[test]
fn immediate_death_object_subregion_executes_without_a_stale_read() {
    let mut module = module(vec![function(
        RUN,
        "Run",
        mir::Visibility::Public,
        vec![local(0, "object", mir::Type::Class(BOX_CLASS))],
        mir::Type::Int,
        vec![allocate_object(0)],
        Some(integer(42)),
    )]);

    let report = lower_aarm_temporary_subregions_for_research(&mut module)
        .expect("an immediately dead object lowers");
    assert_eq!(report.subregions_lowered, 1);
    assert_eq!(marker_count(&module), 2);
    assert_eq!(
        execute_with_stats(&module, "Run")
            .expect("immediate-death object execution succeeds")
            .0,
        ExecutionValue::Int(42)
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn sequential_fine_rewinds_reduce_peak_and_retained_capacity_without_changing_results() {
    use std::time::Instant;

    let mut instructions = Vec::new();
    for (array, value) in [(0, 10), (2, 11), (4, 12), (6, 9)] {
        instructions.extend([
            temporary_array(array + 1, 20_000),
            assign(array_index(array + 1, 0), integer(value)),
            assign(
                mir::Place::Local(mir::LocalId(array)),
                copy_place(array_index(array + 1, 0), mir::Type::Int),
            ),
        ]);
    }
    instructions.extend([add(8, 0, 2), add(9, 4, 6), add(10, 8, 9)]);
    let locals = (0..8)
        .map(|id| {
            if id % 2 == 0 {
                local(id, &format!("value_{id}"), mir::Type::Int)
            } else {
                local(
                    id,
                    &format!("array_{id}"),
                    mir::Type::Array(Box::new(mir::Type::Int)),
                )
            }
        })
        .chain([
            local(8, "left", mir::Type::Int),
            local(9, "right", mir::Type::Int),
            local(10, "result", mir::Type::Int),
        ])
        .collect();
    let baseline = module(vec![function(
        RUN,
        "Run",
        mir::Visibility::Public,
        locals,
        mir::Type::Int,
        instructions,
        Some(copy(10, mir::Type::Int)),
    )]);
    let mut lowered = baseline.clone();
    let report = lower_aarm_temporary_subregions_for_research(&mut lowered)
        .expect("sequential array lifetimes lower");
    assert_eq!(report.subregions_lowered, 4);

    let baseline_started = Instant::now();
    let baseline_result = execute_with_stats(&baseline, "Run").expect("baseline executes");
    let baseline_elapsed = baseline_started.elapsed();
    let lowered_started = Instant::now();
    let lowered_result = execute_with_stats(&lowered, "Run").expect("lowered MIR executes");
    let lowered_elapsed = lowered_started.elapsed();

    assert_eq!(baseline_result.0, ExecutionValue::Int(42));
    assert_eq!(lowered_result.0, baseline_result.0);
    assert_eq!(lowered_result.1.used_bytes, baseline_result.1.used_bytes);
    assert_eq!(lowered_result.1.used_bytes, 0);
    assert_eq!(
        lowered_result.1.requested_bytes,
        baseline_result.1.requested_bytes
    );
    assert_eq!(
        lowered_result.1.total_allocations,
        baseline_result.1.total_allocations
    );
    assert!(lowered_result.1.peak_used_bytes < baseline_result.1.peak_used_bytes);
    assert!(lowered_result.1.reserved_bytes < baseline_result.1.reserved_bytes);

    eprintln!(
        "AARM-5D sequential evidence: baseline={{result:{:?}, elapsed:{baseline_elapsed:?}, peak_used:{}, final_used:{}, reserved:{}, requested:{}, allocations:{}}} lowered={{result:{:?}, elapsed:{lowered_elapsed:?}, peak_used:{}, final_used:{}, reserved:{}, requested:{}, allocations:{}}}",
        baseline_result.0,
        baseline_result.1.peak_used_bytes,
        baseline_result.1.used_bytes,
        baseline_result.1.reserved_bytes,
        baseline_result.1.requested_bytes,
        baseline_result.1.total_allocations,
        lowered_result.0,
        lowered_result.1.peak_used_bytes,
        lowered_result.1.used_bytes,
        lowered_result.1.reserved_bytes,
        lowered_result.1.requested_bytes,
        lowered_result.1.total_allocations,
    );

    #[cfg(feature = "aarm-telemetry")]
    {
        use aster_codegen_cranelift::execute_with_aarm_telemetry;

        let (_, baseline_telemetry) =
            execute_with_aarm_telemetry(&baseline, "Run").expect("baseline telemetry executes");
        let (_, lowered_telemetry) =
            execute_with_aarm_telemetry(&lowered, "Run").expect("lowered telemetry executes");
        let baseline_fresh = baseline_telemetry
            .temporary
            .events
            .fresh_regular_page_allocations
            + baseline_telemetry
                .temporary
                .events
                .fresh_oversized_page_allocations;
        let lowered_fresh = lowered_telemetry
            .temporary
            .events
            .fresh_regular_page_allocations
            + lowered_telemetry
                .temporary
                .events
                .fresh_oversized_page_allocations;
        assert_eq!(
            lowered_telemetry.requested_bytes,
            baseline_telemetry.requested_bytes
        );
        assert_eq!(lowered_telemetry.temporary.live_used_bytes, 0);
        assert!(
            lowered_telemetry.temporary.peak_live_used_bytes
                < baseline_telemetry.temporary.peak_live_used_bytes
        );
        assert!(
            lowered_telemetry.temporary.arena_capacity_bytes
                < baseline_telemetry.temporary.arena_capacity_bytes
        );
        assert!(lowered_fresh < baseline_fresh);
        eprintln!(
            "AARM-5D arena evidence: baseline={{peak_live:{}, final_live:{}, logical_capacity:{}, requested:{}, fresh_pages:{baseline_fresh}}} lowered={{peak_live:{}, final_live:{}, logical_capacity:{}, requested:{}, fresh_pages:{lowered_fresh}}}",
            baseline_telemetry.temporary.peak_live_used_bytes,
            baseline_telemetry.temporary.live_used_bytes,
            baseline_telemetry.temporary.arena_capacity_bytes,
            baseline_telemetry.requested_bytes,
            lowered_telemetry.temporary.peak_live_used_bytes,
            lowered_telemetry.temporary.live_used_bytes,
            lowered_telemetry.temporary.arena_capacity_bytes,
            lowered_telemetry.requested_bytes,
        );
    }
}

#[test]
fn older_temporary_and_persistent_storage_survive_their_proven_fine_rewinds() {
    let mut older = module(vec![function(
        RUN,
        "Run",
        mir::Visibility::Public,
        vec![
            local(0, "older", mir::Type::Class(BOX_CLASS)),
            local(1, "younger", mir::Type::Class(BOX_CLASS)),
            local(2, "younger_value", mir::Type::Int),
            local(3, "older_value", mir::Type::Int),
            local(4, "result", mir::Type::Int),
        ],
        mir::Type::Int,
        vec![
            allocate_object(0),
            assign(object_field(0), integer(20)),
            allocate_object(1),
            assign(object_field(1), integer(22)),
            assign(
                mir::Place::Local(mir::LocalId(2)),
                copy_place(object_field(1), mir::Type::Int),
            ),
            assign(
                mir::Place::Local(mir::LocalId(3)),
                copy_place(object_field(0), mir::Type::Int),
            ),
            add(4, 2, 3),
        ],
        Some(copy(4, mir::Type::Int)),
    )]);
    let report = lower_aarm_temporary_subregions_for_research(&mut older)
        .expect("younger object lifetime lowers");
    assert_eq!(report.subregions_lowered, 1);
    assert_eq!(
        execute_with_stats(&older, "Run")
            .expect("older object remains valid")
            .0,
        ExecutionValue::Int(42)
    );

    let build = function(
        BUILD,
        "Build",
        mir::Visibility::Internal,
        vec![
            local(0, "temporary", mir::Type::Class(BOX_CLASS)),
            local(1, "persistent", mir::Type::Class(BOX_CLASS)),
            local(2, "temporary_value", mir::Type::Int),
        ],
        mir::Type::Class(BOX_CLASS),
        vec![
            allocate_object(0),
            assign(object_field(0), integer(20)),
            allocate_object(1),
            assign(object_field(1), integer(42)),
            assign(
                mir::Place::Local(mir::LocalId(2)),
                copy_place(object_field(0), mir::Type::Int),
            ),
        ],
        Some(copy(1, mir::Type::Class(BOX_CLASS))),
    );
    let run = function(
        RUN,
        "Run",
        mir::Visibility::Public,
        vec![
            local(0, "persistent", mir::Type::Class(BOX_CLASS)),
            local(1, "result", mir::Type::Int),
        ],
        mir::Type::Int,
        vec![
            mir::Instruction::Call {
                destination: Some(mir::Place::Local(mir::LocalId(0))),
                function: BUILD,
                arguments: Vec::new(),
                return_type: mir::Type::Class(BOX_CLASS),
            },
            assign(
                mir::Place::Local(mir::LocalId(1)),
                copy_place(object_field(0), mir::Type::Int),
            ),
        ],
        Some(copy(1, mir::Type::Int)),
    );
    let mut persistent = module(vec![build, run]);
    let report = lower_aarm_temporary_subregions_for_research(&mut persistent)
        .expect("temporary object around persistent allocation lowers");
    assert_eq!(report.subregions_lowered, 1);
    assert_eq!(
        execute_with_stats(&persistent, "Run")
            .expect("persistent object survives fine rewind")
            .0,
        ExecutionValue::Int(42)
    );
}

#[test]
fn sequential_fine_rewinds_zero_and_reuse_retained_temporary_capacity() {
    let mut module = module(vec![function(
        RUN,
        "Run",
        mir::Visibility::Public,
        vec![
            local(0, "first", mir::Type::Class(BOX_CLASS)),
            local(1, "second", mir::Type::Class(BOX_CLASS)),
            local(2, "result", mir::Type::Int),
        ],
        mir::Type::Int,
        vec![
            allocate_object(0),
            assign(object_field(0), integer(99)),
            allocate_object(1),
            assign(
                mir::Place::Local(mir::LocalId(2)),
                copy_place(object_field(1), mir::Type::Int),
            ),
        ],
        Some(copy(2, mir::Type::Int)),
    )]);
    let report = lower_aarm_temporary_subregions_for_research(&mut module)
        .expect("sequential object lifetimes lower");
    assert_eq!(report.subregions_lowered, 2);
    assert_eq!(
        execute_with_stats(&module, "Run")
            .expect("reused bytes are zero")
            .0,
        ExecutionValue::Int(0)
    );

    #[cfg(feature = "aarm-telemetry")]
    {
        use std::sync::Arc;

        use aster_codegen_cranelift::execute_with_aarm_parallel_governor;
        use aster_runtime::{ExecutionContext, MemoryGovernor};

        let page = ExecutionContext::AARM_MIN_PAGE_CAPACITY_BYTES;
        let governor = Arc::new(MemoryGovernor::new(page));
        let (value, telemetry, plans, workers) =
            execute_with_aarm_parallel_governor(&module, "Run", 1, Arc::clone(&governor))
                .expect("one retained page serves both fine subregions");
        assert_eq!(value, ExecutionValue::Int(0));
        assert!(plans.is_empty());
        assert!(workers.is_empty());
        let during = telemetry.governor.expect("execution is governed");
        assert_eq!(during.current_capacity_bytes, page as u64);
        assert_eq!(during.grant_events, 1);
        assert_eq!(during.release_events, 0);
        assert_eq!(telemetry.temporary.events.fresh_regular_page_allocations, 1);
        assert_eq!(telemetry.temporary.events.inactive_page_reuse_events, 1);
        assert!(telemetry.temporary.events.rewound_bytes > 0);

        let after = governor.telemetry();
        assert_eq!(after.current_capacity_bytes, 0);
        assert_eq!(after.grant_events, 1);
        assert_eq!(after.release_events, 1);
    }
}

#[cfg(feature = "aarm-telemetry")]
#[test]
fn allocation_failures_inside_a_fine_subregion_run_balanced_cleanup() {
    use std::sync::Arc;

    use aster_codegen_cranelift::execute_with_aarm_parallel_governor;
    use aster_runtime::{ExecutionContext, MemoryGovernor};

    let object_module = module(vec![function(
        RUN,
        "Run",
        mir::Visibility::Public,
        vec![local(0, "object", mir::Type::Class(BOX_CLASS))],
        mir::Type::Int,
        vec![enter(0), temporary_object(0), exit(0)],
        Some(integer(42)),
    )]);
    validate(&object_module).expect("the object failure path is structurally valid");

    let object_governor = Arc::new(MemoryGovernor::new(1));
    let object_error =
        execute_with_aarm_parallel_governor(&object_module, "Run", 1, Arc::clone(&object_governor))
            .expect_err("a fresh object page must exceed a one-byte hard limit");
    assert!(
        object_error
            .message()
            .contains("shared execution memory budget")
    );
    let object_after = object_governor.telemetry();
    assert_eq!(object_after.current_capacity_bytes, 0);
    assert_eq!(object_after.grant_events, 0);
    assert_eq!(object_after.release_events, 0);

    let array_module = module(vec![function(
        RUN,
        "Run",
        mir::Visibility::Public,
        vec![
            local(0, "object", mir::Type::Class(BOX_CLASS)),
            local(1, "array", mir::Type::Array(Box::new(mir::Type::Int))),
        ],
        mir::Type::Int,
        vec![
            enter(0),
            temporary_object(0),
            temporary_array(1, 100_000),
            exit(0),
        ],
        Some(integer(42)),
    )]);
    validate(&array_module).expect("the array failure path is structurally valid");

    let page = ExecutionContext::AARM_MIN_PAGE_CAPACITY_BYTES;
    let governor = Arc::new(MemoryGovernor::new(page));
    let error = execute_with_aarm_parallel_governor(&array_module, "Run", 1, Arc::clone(&governor))
        .expect_err("oversized array must hit the shared hard limit");
    assert!(error.message().contains("shared execution memory budget"));

    let after = governor.telemetry();
    assert_eq!(after.current_capacity_bytes, 0);
    assert_eq!(after.grant_events, 1);
    assert_eq!(after.release_events, 1);
}

#[test]
fn public_research_lowering_rejects_preexisting_executable_markers_atomically() {
    let mut module = module(vec![function(
        RUN,
        "Run",
        mir::Visibility::Public,
        vec![local(0, "object", mir::Type::Class(BOX_CLASS))],
        mir::Type::Int,
        vec![enter(0), temporary_object(0), exit(0)],
        Some(integer(42)),
    )]);
    let original = module.clone();

    lower_aarm_temporary_subregions_for_research(&mut module)
        .expect_err("preexisting executable authority must fail closed");
    assert_eq!(module, original);
}

#[test]
#[allow(clippy::too_many_lines)]
fn public_research_lowering_emits_no_markers_for_proof_barriers() {
    let assert_rejected = |mut module: mir::Module, case: &str| {
        let report = lower_aarm_temporary_subregions_for_research(&mut module)
            .unwrap_or_else(|error| panic!("{case} research analysis failed: {error}"));
        assert_eq!(report.subregions_lowered, 0, "{case}");
        assert_eq!(marker_count(&module), 0, "{case}");
        assert!(
            module
                .functions
                .iter()
                .all(|function| function.temporary_subregion_candidates.is_empty()),
            "{case}"
        );
    };

    assert_rejected(
        module(vec![function(
            RUN,
            "Run",
            mir::Visibility::Public,
            vec![
                local(0, "text", mir::Type::String),
                local(1, "length", mir::Type::Int),
            ],
            mir::Type::Int,
            vec![
                mir::Instruction::CallIntrinsic {
                    destination: Some(mir::Place::Local(mir::LocalId(0))),
                    intrinsic: mir::Intrinsic::StringFromLongTemporary,
                    arguments: vec![mir::Operand {
                        type_: mir::Type::Long,
                        kind: mir::OperandKind::Constant(mir::Constant::Integer("42".to_owned())),
                    }],
                    return_type: mir::Type::String,
                },
                mir::Instruction::CallIntrinsic {
                    destination: Some(mir::Place::Local(mir::LocalId(1))),
                    intrinsic: mir::Intrinsic::StringLength,
                    arguments: vec![copy(0, mir::Type::String)],
                    return_type: mir::Type::Int,
                },
            ],
            Some(copy(1, mir::Type::Int)),
        )]),
        "string result used after its candidate boundary",
    );

    let build = function(
        BUILD,
        "Build",
        mir::Visibility::Internal,
        Vec::new(),
        mir::Type::Int,
        Vec::new(),
        Some(integer(1)),
    );
    let run_with_call = function(
        RUN,
        "Run",
        mir::Visibility::Public,
        vec![
            local(0, "object", mir::Type::Class(BOX_CLASS)),
            local(1, "side", mir::Type::Int),
            local(2, "observed", mir::Type::Int),
        ],
        mir::Type::Int,
        vec![
            allocate_object(0),
            mir::Instruction::Call {
                destination: Some(mir::Place::Local(mir::LocalId(1))),
                function: BUILD,
                arguments: Vec::new(),
                return_type: mir::Type::Int,
            },
            assign(
                mir::Place::Local(mir::LocalId(2)),
                copy_place(object_field(0), mir::Type::Int),
            ),
        ],
        Some(copy(2, mir::Type::Int)),
    );
    assert_rejected(module(vec![build, run_with_call]), "direct call");

    let mut branch = module(vec![function(
        RUN,
        "Run",
        mir::Visibility::Public,
        vec![local(0, "object", mir::Type::Class(BOX_CLASS))],
        mir::Type::Int,
        vec![allocate_object(0)],
        Some(integer(42)),
    )]);
    branch.functions[0].blocks[0].terminator = mir::Terminator::Branch {
        condition: mir::Operand {
            type_: mir::Type::Bool,
            kind: mir::OperandKind::Constant(mir::Constant::Boolean(true)),
        },
        then_block: mir::BasicBlockId(1),
        else_block: mir::BasicBlockId(2),
    };
    branch.functions[0].blocks.extend([
        mir::BasicBlock {
            id: mir::BasicBlockId(1),
            instructions: Vec::new(),
            terminator: mir::Terminator::Return(Some(integer(42))),
        },
        mir::BasicBlock {
            id: mir::BasicBlockId(2),
            instructions: Vec::new(),
            terminator: mir::Terminator::Return(Some(integer(42))),
        },
    ]);
    assert_rejected(branch, "branching CFG");

    let mut loop_module = module(vec![function(
        RUN,
        "Run",
        mir::Visibility::Public,
        vec![local(0, "object", mir::Type::Class(BOX_CLASS))],
        mir::Type::Int,
        vec![allocate_object(0)],
        Some(integer(42)),
    )]);
    loop_module.functions[0].blocks[0].terminator = mir::Terminator::Goto(BLOCK);
    assert_rejected(loop_module, "loop CFG");
}

#[test]
fn immutable_temporary_concat_reclaims_without_invalidating_an_older_string() {
    let string = mir::Type::String;
    let mut module = module(vec![function(
        RUN,
        "Run",
        mir::Visibility::Public,
        vec![
            local(0, "older", string.clone()),
            local(1, "result", string.clone()),
            local(2, "length", mir::Type::Int),
        ],
        mir::Type::Int,
        vec![
            mir::Instruction::CallIntrinsic {
                destination: Some(mir::Place::Local(mir::LocalId(0))),
                intrinsic: mir::Intrinsic::StringFromLongTemporary,
                arguments: vec![mir::Operand {
                    type_: mir::Type::Long,
                    kind: mir::OperandKind::Constant(mir::Constant::Integer("7".to_owned())),
                }],
                return_type: string.clone(),
            },
            mir::Instruction::CallIntrinsic {
                destination: Some(mir::Place::Local(mir::LocalId(1))),
                intrinsic: mir::Intrinsic::StringConcatTemporary,
                arguments: vec![
                    copy(0, string.clone()),
                    mir::Operand {
                        type_: string.clone(),
                        kind: mir::OperandKind::Constant(mir::Constant::String("x".to_owned())),
                    },
                ],
                return_type: string.clone(),
            },
            mir::Instruction::CallIntrinsic {
                destination: Some(mir::Place::Local(mir::LocalId(2))),
                intrinsic: mir::Intrinsic::StringLength,
                arguments: vec![copy(0, string)],
                return_type: mir::Type::Int,
            },
        ],
        Some(copy(2, mir::Type::Int)),
    )]);
    let baseline = execute_with_stats(&module, "Run").expect("baseline executes");
    let report = lower_aarm_temporary_subregions_for_research(&mut module)
        .expect("immutable string lowering succeeds");
    let lowered = execute_with_stats(&module, "Run").expect("lowered executes");

    assert_eq!(baseline.0, ExecutionValue::Int(1));
    assert_eq!(lowered.0, baseline.0);
    assert_eq!(lowered.1.requested_bytes, baseline.1.requested_bytes);
    assert_eq!(report.subregions_lowered, 1);
    assert_eq!(marker_count(&module), 2);
}

#[test]
fn immutable_temporary_formatting_reclaims_each_loop_iteration() {
    let string = mir::Type::String;
    let mut run = function(
        RUN,
        "Run",
        mir::Visibility::Public,
        vec![
            local(0, "text", string.clone()),
            local(1, "index", mir::Type::Int),
            local(2, "condition", mir::Type::Bool),
            local(3, "one", mir::Type::Int),
        ],
        mir::Type::Int,
        Vec::new(),
        None,
    );
    run.blocks = vec![
        mir::BasicBlock {
            id: mir::BasicBlockId(0),
            instructions: vec![
                assign(mir::Place::Local(mir::LocalId(1)), integer(0)),
                assign(mir::Place::Local(mir::LocalId(3)), integer(1)),
            ],
            terminator: mir::Terminator::Goto(mir::BasicBlockId(1)),
        },
        mir::BasicBlock {
            id: mir::BasicBlockId(1),
            instructions: vec![less(2, 1, 128)],
            terminator: mir::Terminator::Branch {
                condition: copy(2, mir::Type::Bool),
                then_block: mir::BasicBlockId(2),
                else_block: mir::BasicBlockId(3),
            },
        },
        mir::BasicBlock {
            id: mir::BasicBlockId(2),
            instructions: vec![
                mir::Instruction::CallIntrinsic {
                    destination: Some(mir::Place::Local(mir::LocalId(0))),
                    intrinsic: mir::Intrinsic::StringFromLongTemporary,
                    arguments: vec![mir::Operand {
                        type_: mir::Type::Long,
                        kind: mir::OperandKind::Constant(mir::Constant::Integer("42".to_owned())),
                    }],
                    return_type: string,
                },
                add(1, 1, 3),
            ],
            terminator: mir::Terminator::Goto(mir::BasicBlockId(1)),
        },
        mir::BasicBlock {
            id: mir::BasicBlockId(3),
            instructions: Vec::new(),
            terminator: mir::Terminator::Return(Some(copy(1, mir::Type::Int))),
        },
    ];
    let baseline = module(vec![run]);
    let baseline_result = execute_with_stats(&baseline, "Run").expect("baseline executes");
    let mut lowered = baseline.clone();
    let report = lower_aarm_temporary_subregions_for_research(&mut lowered)
        .expect("string loop lowering succeeds");
    let lowered_result = execute_with_stats(&lowered, "Run").expect("lowered executes");

    assert_eq!(baseline_result.0, ExecutionValue::Int(128));
    assert_eq!(lowered_result.0, baseline_result.0);
    assert_eq!(
        lowered_result.1.requested_bytes,
        baseline_result.1.requested_bytes
    );
    assert!(lowered_result.1.peak_used_bytes < baseline_result.1.peak_used_bytes);
    assert_eq!(report.subregions_lowered, 1);
}

#[test]
fn fine_owned_string_builder_growth_and_persistent_snapshot_execute_safely() {
    let string = mir::Type::String;
    let baseline = module(vec![function(
        RUN,
        "Run",
        mir::Visibility::Public,
        vec![
            local(0, "builder", mir::Type::Class(STRING_BUILDER_CLASS)),
            local(1, "snapshot", string.clone()),
        ],
        string,
        vec![
            temporary_builder(0),
            builder_append(0, "abcdefghijklmnop"),
            builder_append(0, "qrstuvwxyz0123456789"),
            builder_append(0, "ABCDEFGHIJKLMNOPQRSTUVWXYZ"),
            mir::Instruction::StringBuilderToString {
                destination: mir::Place::Local(mir::LocalId(1)),
                builder: copy(0, mir::Type::Class(STRING_BUILDER_CLASS)),
                class: STRING_BUILDER_CLASS,
                region: mir::AllocationRegion::Persistent,
            },
        ],
        Some(copy(1, mir::Type::String)),
    )]);
    assert_eq!(marker_count(&baseline), 0);
    let baseline_result = execute_with_stats(&baseline, "Run").expect("baseline builder executes");

    let mut lowered = baseline.clone();
    let report = lower_aarm_temporary_subregions_for_research(&mut lowered)
        .expect("fine-owned builder lowers");
    let lowered_result =
        execute_with_stats(&lowered, "Run").expect("lowered builder snapshot executes");

    assert_eq!(lowered_result.0, baseline_result.0);
    assert_eq!(
        lowered_result.1.requested_bytes,
        baseline_result.1.requested_bytes
    );
    assert_eq!(report.subregions_lowered, 1, "{report:#?}");
    assert_eq!(marker_count(&lowered), 2);
}

#[test]
fn fine_owned_list_and_dictionary_growth_execute_safely() {
    let list_type = mir::Type::List(Box::new(mir::Type::Int));
    let dictionary_type = mir::Type::Dictionary(Box::new(mir::Type::Int), Box::new(mir::Type::Int));
    let mut module = module(vec![function(
        RUN,
        "Run",
        mir::Visibility::Public,
        vec![
            local(0, "list", list_type),
            local(1, "dictionary", dictionary_type),
            local(2, "result", mir::Type::Bool),
        ],
        mir::Type::Int,
        vec![
            temporary_list(0),
            list_add(0, 20),
            list_add(0, 22),
            temporary_dictionary(1),
            dictionary_set(1, 2, 1, 42),
            mir::Instruction::DictionaryContainsKey {
                destination: mir::Place::Local(mir::LocalId(2)),
                dictionary: copy(
                    1,
                    mir::Type::Dictionary(Box::new(mir::Type::Int), Box::new(mir::Type::Int)),
                ),
                key: integer(1),
            },
        ],
        Some(integer(42)),
    )]);
    let baseline = execute_with_stats(&module, "Run").expect("collection baseline executes");
    let report = lower_aarm_temporary_subregions_for_research(&mut module)
        .expect("fine-owned collections lower");
    let lowered = execute_with_stats(&module, "Run").expect("fine-owned collections execute");

    assert_eq!(baseline.0, ExecutionValue::Int(42));
    assert_eq!(lowered.0, baseline.0);
    assert_eq!(lowered.1.requested_bytes, baseline.1.requested_bytes);
    assert_eq!(report.subregions_lowered, 2, "{report:#?}");
}

#[test]
#[allow(clippy::too_many_lines)]
fn fine_owned_hidden_backing_loop_reclaim_is_bounded_and_checksum_stable() {
    let iterations = std::env::var("ASTER_AARM_HIDDEN_ITERATIONS")
        .ok()
        .and_then(|value| value.parse::<i32>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(100_000);
    let mut run = function(
        RUN,
        "Run",
        mir::Visibility::Public,
        vec![
            local(0, "builder", mir::Type::Class(STRING_BUILDER_CLASS)),
            local(1, "list", mir::Type::List(Box::new(mir::Type::Int))),
            local(
                2,
                "dictionary",
                mir::Type::Dictionary(Box::new(mir::Type::Int), Box::new(mir::Type::Int)),
            ),
            local(3, "mutation", mir::Type::Bool),
            local(4, "index", mir::Type::Int),
            local(5, "condition", mir::Type::Bool),
            local(6, "one", mir::Type::Int),
        ],
        mir::Type::Int,
        Vec::new(),
        None,
    );
    run.blocks = vec![
        mir::BasicBlock {
            id: mir::BasicBlockId(0),
            instructions: vec![
                assign(mir::Place::Local(mir::LocalId(4)), integer(0)),
                assign(mir::Place::Local(mir::LocalId(6)), integer(1)),
            ],
            terminator: mir::Terminator::Goto(mir::BasicBlockId(1)),
        },
        mir::BasicBlock {
            id: mir::BasicBlockId(1),
            instructions: vec![less(5, 4, iterations)],
            terminator: mir::Terminator::Branch {
                condition: copy(5, mir::Type::Bool),
                then_block: mir::BasicBlockId(2),
                else_block: mir::BasicBlockId(3),
            },
        },
        mir::BasicBlock {
            id: mir::BasicBlockId(2),
            instructions: vec![
                temporary_builder(0),
                builder_append(0, "abcdefghijklmnop-qrstuvwxyz-0123456789"),
                temporary_list(1),
                list_add(1, 42),
                temporary_dictionary(2),
                dictionary_set(2, 3, 1, 42),
                add(4, 4, 6),
            ],
            terminator: mir::Terminator::Goto(mir::BasicBlockId(1)),
        },
        mir::BasicBlock {
            id: mir::BasicBlockId(3),
            instructions: Vec::new(),
            terminator: mir::Terminator::Return(Some(copy(4, mir::Type::Int))),
        },
    ];
    let baseline = module(vec![run]);
    let baseline_result = execute_with_stats(&baseline, "Run").expect("baseline loop executes");
    let mut lowered = baseline.clone();
    let report = lower_aarm_temporary_subregions_for_research(&mut lowered)
        .expect("fine-owned hidden-backing loop lowers");
    let lowered_result = execute_with_stats(&lowered, "Run").expect("lowered loop executes");

    assert_eq!(baseline_result.0, ExecutionValue::Int(iterations));
    assert_eq!(lowered_result.0, baseline_result.0);
    assert_eq!(
        lowered_result.1.requested_bytes,
        baseline_result.1.requested_bytes
    );
    assert!(lowered_result.1.peak_used_bytes < baseline_result.1.peak_used_bytes);
    assert!(lowered_result.1.reserved_bytes <= baseline_result.1.reserved_bytes);
    assert_eq!(report.subregions_lowered, 1, "{report:#?}");
    let timing = if cfg!(debug_assertions) {
        None
    } else {
        let median = |module: &mir::Module| {
            let mut samples = (0..5)
                .map(|_| {
                    let started = std::time::Instant::now();
                    let result = execute_with_stats(module, "Run").expect("timed loop executes");
                    assert_eq!(result.0, ExecutionValue::Int(iterations));
                    started.elapsed().as_secs_f64() * 1_000.0
                })
                .collect::<Vec<_>>();
            samples.sort_by(f64::total_cmp);
            samples[samples.len() / 2]
        };
        Some((median(&baseline), median(&lowered)))
    };
    eprintln!(
        "hidden-backing-loop iterations={iterations} requested={} baseline_peak={} aarm_peak={} baseline_capacity={} aarm_capacity={} median_ms={timing:?}",
        baseline_result.1.requested_bytes,
        baseline_result.1.peak_used_bytes,
        lowered_result.1.peak_used_bytes,
        baseline_result.1.reserved_bytes,
        lowered_result.1.reserved_bytes,
    );
}

#[test]
fn mixed_object_array_and_immutable_string_subregions_preserve_results() {
    let string = mir::Type::String;
    let mut module = module(vec![function(
        RUN,
        "Run",
        mir::Visibility::Public,
        vec![
            local(0, "object", mir::Type::Class(BOX_CLASS)),
            local(1, "array", mir::Type::Array(Box::new(mir::Type::Int))),
            local(2, "text", string.clone()),
            local(3, "length", mir::Type::Int),
        ],
        mir::Type::Int,
        vec![
            temporary_object(0),
            assign(object_field(0), integer(7)),
            temporary_array(1, 4),
            mir::Instruction::Assign {
                target: mir::Place::Local(mir::LocalId(3)),
                value: mir::Rvalue {
                    type_: mir::Type::Int,
                    kind: mir::RvalueKind::ArrayLength(copy(
                        1,
                        mir::Type::Array(Box::new(mir::Type::Int)),
                    )),
                },
            },
            mir::Instruction::CallIntrinsic {
                destination: Some(mir::Place::Local(mir::LocalId(2))),
                intrinsic: mir::Intrinsic::StringFromBoolTemporary,
                arguments: vec![mir::Operand {
                    type_: mir::Type::Bool,
                    kind: mir::OperandKind::Constant(mir::Constant::Boolean(true)),
                }],
                return_type: string,
            },
        ],
        Some(copy(3, mir::Type::Int)),
    )]);
    let baseline = execute_with_stats(&module, "Run").expect("baseline executes");
    let report =
        lower_aarm_temporary_subregions_for_research(&mut module).expect("mixed lowering succeeds");
    let lowered = execute_with_stats(&module, "Run").expect("lowered executes");

    assert_eq!(baseline.0, ExecutionValue::Int(4));
    assert_eq!(lowered.0, baseline.0);
    assert_eq!(lowered.1.requested_bytes, baseline.1.requested_bytes);
    assert!(lowered.1.peak_used_bytes < baseline.1.peak_used_bytes);
    assert!(lowered.1.reserved_bytes <= baseline.1.reserved_bytes);
    assert_eq!(report.subregions_lowered, 3);
}

#[test]
#[allow(clippy::too_many_lines)]
fn malformed_executable_subregions_fail_closed_before_codegen() {
    let valid = || {
        module(vec![function(
            RUN,
            "Run",
            mir::Visibility::Public,
            vec![local(0, "object", mir::Type::Class(BOX_CLASS))],
            mir::Type::Int,
            vec![enter(0), temporary_object(0), exit(0)],
            Some(integer(42)),
        )])
    };

    let mut cases = Vec::new();

    let mut exit_without_enter = valid();
    exit_without_enter.functions[0].blocks[0]
        .instructions
        .remove(0);
    cases.push(exit_without_enter);

    let mut enter_without_exit = valid();
    enter_without_exit.functions[0].blocks[0].instructions.pop();
    cases.push(enter_without_exit);

    let mut mismatched = valid();
    mismatched.functions[0].blocks[0].instructions[2] = exit(1);
    cases.push(mismatched);

    let mut nested = valid();
    nested.functions[0].blocks[0]
        .instructions
        .insert(1, enter(1));
    nested.functions[0].blocks[0]
        .instructions
        .insert(3, exit(1));
    cases.push(nested);

    let mut crossing = valid();
    crossing.functions[0]
        .locals
        .push(local(1, "second_object", mir::Type::Class(BOX_CLASS)));
    crossing.functions[0].blocks[0].instructions = vec![
        enter(0),
        temporary_object(0),
        enter(1),
        temporary_object(1),
        exit(0),
        exit(1),
    ];
    cases.push(crossing);

    let mut duplicate_enter = valid();
    duplicate_enter.functions[0].blocks[0]
        .instructions
        .insert(1, enter(0));
    cases.push(duplicate_enter);

    let mut duplicate_exit = valid();
    duplicate_exit.functions[0].blocks[0]
        .instructions
        .push(exit(0));
    cases.push(duplicate_exit);

    let mut no_allocation = valid();
    no_allocation.functions[0].blocks[0].instructions.remove(1);
    cases.push(no_allocation);

    let mut unlowered_metadata = valid();
    unlowered_metadata.functions[0]
        .temporary_subregion_candidates
        .push(mir::TemporarySubregionCandidate {
            id: mir::TemporarySubregionId(0),
            checkpoint: mir::MirPoint {
                block: BLOCK,
                instruction_boundary: 0,
            },
            rewinds: vec![mir::MirPoint {
                block: BLOCK,
                instruction_boundary: 1,
            }],
            allocations: vec![mir::MirAllocationSite {
                function: RUN,
                block: BLOCK,
                instruction_index: 0,
            }],
        });
    cases.push(unlowered_metadata);

    let mut multi_block = valid();
    multi_block.functions[0].blocks.push(mir::BasicBlock {
        id: mir::BasicBlockId(1),
        instructions: Vec::new(),
        terminator: mir::Terminator::Return(Some(integer(42))),
    });
    cases.push(multi_block);

    // A balanced Exit in a successor block is a valid AARM-5E1 shape, so it
    // intentionally is no longer listed among malformed single-block cases.

    let mut branch_around_exit = valid();
    branch_around_exit.functions[0].blocks[0].instructions.pop();
    branch_around_exit.functions[0].blocks[0].terminator = mir::Terminator::Branch {
        condition: mir::Operand {
            type_: mir::Type::Bool,
            kind: mir::OperandKind::Constant(mir::Constant::Boolean(true)),
        },
        then_block: mir::BasicBlockId(1),
        else_block: mir::BasicBlockId(2),
    };
    branch_around_exit.functions[0].blocks.extend([
        mir::BasicBlock {
            id: mir::BasicBlockId(1),
            instructions: vec![exit(0)],
            terminator: mir::Terminator::Return(Some(integer(42))),
        },
        mir::BasicBlock {
            id: mir::BasicBlockId(2),
            instructions: Vec::new(),
            terminator: mir::Terminator::Return(Some(integer(42))),
        },
    ]);
    cases.push(branch_around_exit);

    let mut return_before_exit = valid();
    return_before_exit.functions[0].blocks[0].instructions.pop();
    return_before_exit.functions[0]
        .blocks
        .push(mir::BasicBlock {
            id: mir::BasicBlockId(1),
            instructions: vec![exit(0)],
            terminator: mir::Terminator::Unreachable,
        });
    cases.push(return_before_exit);

    for malformed in cases {
        validate(&malformed).expect_err("malformed executable subregion must fail closed");
    }
}

#[test]
fn hidden_alias_call_and_concurrency_operations_are_rejected() {
    let list_type = mir::Type::List(Box::new(mir::Type::Int));
    let mut module = module(vec![function(
        RUN,
        "Run",
        mir::Visibility::Public,
        vec![
            local(0, "object", mir::Type::Class(BOX_CLASS)),
            local(1, "list", list_type.clone()),
            local(2, "copy", list_type.clone()),
        ],
        mir::Type::Int,
        vec![
            enter(0),
            temporary_object(0),
            assign(mir::Place::Local(mir::LocalId(2)), copy(1, list_type)),
            exit(0),
        ],
        Some(integer(42)),
    )]);
    validate(&module).expect_err("a hidden List copy is outside the executable subset");

    module.functions[0].locals[1] = local(1, "string", mir::Type::String);
    module.functions[0].locals[2] = local(2, "copy", mir::Type::String);
    module.functions[0].blocks[0].instructions[2] = assign(
        mir::Place::Local(mir::LocalId(2)),
        copy(1, mir::Type::String),
    );
    validate(&module).expect_err("a hidden string copy is outside the executable subset");

    module.functions[0].blocks[0].instructions[2] = mir::Instruction::Call {
        destination: None,
        function: BUILD,
        arguments: Vec::new(),
        return_type: mir::Type::Void,
    };
    validate(&module).expect_err("a direct call inside a fine subregion is rejected");

    module.functions[0].blocks[0].instructions[2] = mir::Instruction::CallInterface {
        destination: None,
        receiver: copy(0, mir::Type::Class(BOX_CLASS)),
        method: VALUE_FIELD,
        arguments: Vec::new(),
        return_type: mir::Type::Void,
    };
    validate(&module).expect_err("an interface call inside a fine subregion is rejected");

    module.functions[0].blocks[0].instructions[2] = mir::Instruction::AllocateList {
        destination: mir::Place::Local(mir::LocalId(2)),
        element_type: mir::Type::Int,
        region: mir::AllocationRegion::Temporary,
    };
    validate(&module).expect_err("a List allocation inside a fine subregion is rejected");

    module.functions[0].blocks[0].instructions[2] = mir::Instruction::AllocateDictionary {
        destination: mir::Place::Local(mir::LocalId(2)),
        key_type: mir::Type::Int,
        value_type: mir::Type::Int,
        region: mir::AllocationRegion::Temporary,
    };
    validate(&module).expect_err("a Dictionary allocation inside a fine subregion is rejected");

    module.functions[0].blocks[0].instructions[2] = mir::Instruction::AllocateStringBuilder {
        destination: mir::Place::Local(mir::LocalId(2)),
        class: BOX_CLASS,
        region: mir::AllocationRegion::Temporary,
    };
    validate(&module).expect_err("a StringBuilder allocation inside a fine subregion is rejected");

    module.functions[0].blocks[0].instructions[2] = mir::Instruction::CallIntrinsic {
        destination: Some(mir::Place::Local(mir::LocalId(2))),
        intrinsic: mir::Intrinsic::StringFromLongTemporary,
        arguments: vec![integer(42)],
        return_type: mir::Type::String,
    };
    validate(&module).expect("a supported signed integer string conversion is executable");

    module.functions[0].blocks[0].instructions[2] = mir::Instruction::CallIntrinsic {
        destination: None,
        intrinsic: mir::Intrinsic::TaskRun,
        arguments: Vec::new(),
        return_type: mir::Type::Void,
    };
    validate(&module).expect_err("concurrency is rejected from the executable function");
}
