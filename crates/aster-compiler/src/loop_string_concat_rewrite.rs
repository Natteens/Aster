//! Narrow rewrite of an unobservable loop-carried immutable concat chain.
//!
//! The pass recognizes one canonical four-block `while` shape and replaces
//! its intermediate `string` results with the existing `StringBuilder` MIR
//! operations. It runs before escape analysis, which remains responsible for
//! selecting regions for the compiler-created builder and final snapshot.

use aster_mir as mir;

const STRING_BUILDER_NAME: &str = "aster.core::StringBuilder";

/// Rewrites the smallest exact loop-carried string-concat shape.
pub(super) fn rewrite(module: &mut mir::Module) {
    let existing_class = module
        .classes
        .iter()
        .find(|class| class.name == STRING_BUILDER_NAME)
        .map(|class| class.symbol);
    let Some(class) = existing_class.or_else(|| fresh_symbol(module)) else {
        return;
    };

    let mut rewritten = false;
    for function in &mut module.functions {
        rewritten |= rewrite_function(function, class);
    }
    if rewritten && existing_class.is_none() {
        module.classes.push(mir::ClassDefinition {
            symbol: class,
            name: STRING_BUILDER_NAME.to_owned(),
            fields: Vec::new(),
        });
    }
}

fn fresh_symbol(module: &mir::Module) -> Option<mir::SymbolId> {
    let mut maximum = 0;
    let mut note = |symbol: mir::SymbolId| maximum = maximum.max(symbol.0);
    for class in &module.classes {
        note(class.symbol);
        for field in &class.fields {
            note(field.symbol);
        }
    }
    for definition in &module.structs {
        note(definition.symbol);
        for field in &definition.fields {
            note(field.symbol);
        }
    }
    for definition in &module.interfaces {
        note(definition.symbol);
        for method in &definition.methods {
            note(method.symbol);
        }
    }
    for definition in &module.enums {
        note(definition.symbol);
        for case in &definition.cases {
            note(case.symbol);
            for field in &case.fields {
                note(field.symbol);
            }
        }
    }
    for function in &module.functions {
        note(function.symbol);
        for local in function.parameters.iter().chain(&function.locals) {
            if let Some(symbol) = local.symbol {
                note(symbol);
            }
        }
    }
    maximum.checked_add(1).map(mir::SymbolId)
}

fn rewrite_function(function: &mut mir::Function, class: mir::SymbolId) -> bool {
    let Some(shape) = LoopShape::discover(function) else {
        return false;
    };
    let Some((builder, initialized)) = fresh_locals(function) else {
        return false;
    };

    let builder_operand = local_operand(mir::Type::Class(class), builder);
    let initialized_operand = local_operand(mir::Type::Bool, initialized);
    let Some([initialize_id, append_id, snapshot_id, finish_id]) = fresh_blocks(function) else {
        return false;
    };

    function.locals.push(mir::Local {
        id: builder,
        symbol: None,
        name: "_concat_builder".to_owned(),
        type_: mir::Type::Class(class),
        mutable: true,
        temporary: false,
    });
    function.locals.push(mir::Local {
        id: initialized,
        symbol: None,
        name: "_concat_builder_initialized".to_owned(),
        type_: mir::Type::Bool,
        mutable: true,
        temporary: true,
    });

    let preheader = block_mut(function, shape.preheader);
    preheader.instructions.push(assign(
        initialized,
        mir::Type::Bool,
        mir::Constant::Boolean(false),
    ));

    let body = block_mut(function, shape.body);
    let trailing = body.instructions.split_off(2);
    body.instructions.clear();
    body.terminator = mir::Terminator::Branch {
        condition: initialized_operand.clone(),
        then_block: append_id,
        else_block: initialize_id,
    };

    let exit = block_mut(function, shape.exit);
    let finish_instructions = std::mem::take(&mut exit.instructions);
    let finish_terminator = std::mem::replace(&mut exit.terminator, mir::Terminator::Unreachable);
    exit.terminator = mir::Terminator::Branch {
        condition: initialized_operand.clone(),
        then_block: snapshot_id,
        else_block: finish_id,
    };

    function.blocks.extend([
        mir::BasicBlock {
            id: initialize_id,
            instructions: vec![
                mir::Instruction::AllocateStringBuilder {
                    destination: mir::Place::Local(builder),
                    class,
                    region: mir::AllocationRegion::Persistent,
                },
                assign(initialized, mir::Type::Bool, mir::Constant::Boolean(true)),
            ],
            terminator: mir::Terminator::Goto(append_id),
        },
        mir::BasicBlock {
            id: append_id,
            instructions: std::iter::once(mir::Instruction::StringBuilderAppend {
                builder: builder_operand.clone(),
                value: shape.append,
                class,
            })
            .chain(trailing)
            .collect(),
            terminator: mir::Terminator::Goto(shape.header),
        },
        mir::BasicBlock {
            id: snapshot_id,
            instructions: vec![mir::Instruction::StringBuilderToString {
                destination: mir::Place::Local(shape.accumulator),
                builder: builder_operand,
                class,
                region: mir::AllocationRegion::Persistent,
            }],
            terminator: mir::Terminator::Goto(finish_id),
        },
        mir::BasicBlock {
            id: finish_id,
            instructions: finish_instructions,
            terminator: finish_terminator,
        },
    ]);
    true
}

#[derive(Clone)]
struct LoopShape {
    preheader: mir::BasicBlockId,
    header: mir::BasicBlockId,
    body: mir::BasicBlockId,
    exit: mir::BasicBlockId,
    accumulator: mir::LocalId,
    append: mir::Operand,
}

impl LoopShape {
    fn discover(function: &mir::Function) -> Option<Self> {
        if function.blocks.len() != 4 {
            return None;
        }
        let preheader = block(function, function.entry)?;
        let mir::Terminator::Goto(header) = preheader.terminator else {
            return None;
        };
        let header_block = block(function, header)?;
        let mir::Terminator::Branch {
            condition,
            then_block: body,
            else_block: exit,
        } = &header_block.terminator
        else {
            return None;
        };
        let body_block = block(function, *body)?;
        if body_block.terminator != mir::Terminator::Goto(header) {
            return None;
        }
        let exit_block = block(function, *exit)?;
        let accumulator = empty_string_initializer(preheader)?;
        if !is_string_local(function, accumulator)
            || !assignments_avoid_local(&preheader.instructions, accumulator, Some(0))
            || !assignments_avoid_local(&header_block.instructions, accumulator, None)
            || operand_uses_local(condition, accumulator)
        {
            return None;
        }

        let [concat, assignment, trailing @ ..] = body_block.instructions.as_slice() else {
            return None;
        };
        let mir::Instruction::CallIntrinsic {
            destination: Some(mir::Place::Local(result)),
            intrinsic: mir::Intrinsic::StringConcat | mir::Intrinsic::StringConcatTemporary,
            arguments,
            return_type: mir::Type::String,
        } = concat
        else {
            return None;
        };
        let [left, append] = arguments.as_slice() else {
            return None;
        };
        if direct_local(left) != Some(accumulator)
            || !safe_append_operand(append, accumulator)
            || !matches!(
                assignment,
                mir::Instruction::Assign {
                    target: mir::Place::Local(target),
                    value: mir::Rvalue {
                        type_: mir::Type::String,
                        kind: mir::RvalueKind::Use(source),
                    },
                } if *target == accumulator && direct_local(source) == Some(*result)
            )
            || !assignments_avoid_local(trailing, accumulator, None)
            || !final_use_is_supported(exit_block, accumulator)
        {
            return None;
        }

        Some(Self {
            preheader: preheader.id,
            header,
            body: *body,
            exit: *exit,
            accumulator,
            append: append.clone(),
        })
    }
}

fn empty_string_initializer(block: &mir::BasicBlock) -> Option<mir::LocalId> {
    block
        .instructions
        .iter()
        .find_map(|instruction| match instruction {
            mir::Instruction::Assign {
                target: mir::Place::Local(local),
                value:
                    mir::Rvalue {
                        type_: mir::Type::String,
                        kind:
                            mir::RvalueKind::Use(mir::Operand {
                                type_: mir::Type::String,
                                kind: mir::OperandKind::Constant(mir::Constant::String(value)),
                            }),
                    },
            } if value.is_empty() => Some(*local),
            _ => None,
        })
}

fn is_string_local(function: &mir::Function, local: mir::LocalId) -> bool {
    function
        .locals
        .iter()
        .find(|candidate| candidate.id == local)
        .is_some_and(|candidate| candidate.type_ == mir::Type::String)
}

fn assignments_avoid_local(
    instructions: &[mir::Instruction],
    local: mir::LocalId,
    skip: Option<usize>,
) -> bool {
    instructions.iter().enumerate().all(|(index, instruction)| {
        if skip == Some(index) {
            return true;
        }
        matches!(instruction, mir::Instruction::Assign { .. })
            && !instruction_uses_local(instruction, local)
    })
}

fn final_use_is_supported(block: &mir::BasicBlock, accumulator: mir::LocalId) -> bool {
    match (&block.instructions[..], &block.terminator) {
        ([], mir::Terminator::Return(Some(value))) => direct_local(value) == Some(accumulator),
        (
            [
                mir::Instruction::CallIntrinsic {
                    destination: Some(mir::Place::Local(result)),
                    intrinsic: mir::Intrinsic::StringLength,
                    arguments,
                    return_type: mir::Type::Int,
                },
            ],
            mir::Terminator::Return(Some(value)),
        ) => {
            matches!(arguments.as_slice(), [argument] if direct_local(argument) == Some(accumulator))
                && direct_local(value) == Some(*result)
        }
        _ => false,
    }
}

fn safe_append_operand(operand: &mir::Operand, accumulator: mir::LocalId) -> bool {
    operand.type_ == mir::Type::String
        && direct_local(operand) != Some(accumulator)
        && matches!(
            operand.kind,
            mir::OperandKind::Copy(mir::Place::Local(_))
                | mir::OperandKind::Constant(mir::Constant::String(_))
        )
}

fn instruction_uses_local(instruction: &mir::Instruction, local: mir::LocalId) -> bool {
    let mir::Instruction::Assign { target, value } = instruction else {
        return true;
    };
    place_uses_local(target, local) || rvalue_uses_local(value, local)
}

fn rvalue_uses_local(value: &mir::Rvalue, local: mir::LocalId) -> bool {
    match &value.kind {
        mir::RvalueKind::Use(operand)
        | mir::RvalueKind::Discriminant(operand)
        | mir::RvalueKind::ArrayLength(operand)
        | mir::RvalueKind::ListLength(operand)
        | mir::RvalueKind::DictionaryLength(operand)
        | mir::RvalueKind::ListVersion(operand)
        | mir::RvalueKind::StringByteLength(operand)
        | mir::RvalueKind::Cast(operand)
        | mir::RvalueKind::Unary { operand, .. } => operand_uses_local(operand, local),
        mir::RvalueKind::Aggregate(fields) | mir::RvalueKind::EnumConstruct { fields, .. } => {
            fields
                .iter()
                .any(|field| operand_uses_local(&field.value, local))
        }
        mir::RvalueKind::MakeInterface { object, .. } => operand_uses_local(object, local),
        mir::RvalueKind::Binary { left, right, .. }
        | mir::RvalueKind::Equality { left, right, .. } => {
            operand_uses_local(left, local) || operand_uses_local(right, local)
        }
    }
}

fn operand_uses_local(operand: &mir::Operand, local: mir::LocalId) -> bool {
    matches!(direct_local(operand), Some(found) if found == local)
}

fn place_uses_local(place: &mir::Place, local: mir::LocalId) -> bool {
    match place {
        mir::Place::Local(found) => *found == local,
        mir::Place::Symbol(_) => false,
        mir::Place::Field { base, .. } | mir::Place::EnumField { base, .. } => {
            place_uses_local(base, local)
        }
        mir::Place::Index { array, index, .. } => {
            operand_uses_local(array, local) || operand_uses_local(index, local)
        }
        mir::Place::ObjectField { object, .. } => operand_uses_local(object, local),
    }
}

fn direct_local(operand: &mir::Operand) -> Option<mir::LocalId> {
    let mir::OperandKind::Copy(mir::Place::Local(local)) = operand.kind else {
        return None;
    };
    Some(local)
}

fn local_operand(type_: mir::Type, local: mir::LocalId) -> mir::Operand {
    mir::Operand {
        type_,
        kind: mir::OperandKind::Copy(mir::Place::Local(local)),
    }
}

fn assign(local: mir::LocalId, type_: mir::Type, value: mir::Constant) -> mir::Instruction {
    mir::Instruction::Assign {
        target: mir::Place::Local(local),
        value: mir::Rvalue {
            type_: type_.clone(),
            kind: mir::RvalueKind::Use(mir::Operand {
                type_,
                kind: mir::OperandKind::Constant(value),
            }),
        },
    }
}

fn fresh_locals(function: &mir::Function) -> Option<(mir::LocalId, mir::LocalId)> {
    let maximum = function
        .parameters
        .iter()
        .chain(&function.locals)
        .map(|local| local.id.0)
        .max()
        .unwrap_or(0);
    Some((
        mir::LocalId(maximum.checked_add(1)?),
        mir::LocalId(maximum.checked_add(2)?),
    ))
}

fn fresh_blocks(function: &mir::Function) -> Option<[mir::BasicBlockId; 4]> {
    let maximum = function
        .blocks
        .iter()
        .map(|block| block.id.0)
        .max()
        .expect("function has an entry block");
    Some([
        mir::BasicBlockId(maximum.checked_add(1)?),
        mir::BasicBlockId(maximum.checked_add(2)?),
        mir::BasicBlockId(maximum.checked_add(3)?),
        mir::BasicBlockId(maximum.checked_add(4)?),
    ])
}

fn block(function: &mir::Function, id: mir::BasicBlockId) -> Option<&mir::BasicBlock> {
    function.blocks.iter().find(|block| block.id == id)
}

fn block_mut(function: &mut mir::Function, id: mir::BasicBlockId) -> &mut mir::BasicBlock {
    function
        .blocks
        .iter_mut()
        .find(|block| block.id == id)
        .expect("discovered block remains present")
}
