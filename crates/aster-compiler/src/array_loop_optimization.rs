//! Conservative canonical-loop array optimization.
//!
//! The pass recognizes only the exact MIR shape emitted for an ascending
//! `0 .. array.Length` loop. It hoists the immutable length read into the
//! unique preheader and authorizes unchecked addressing only for the same
//! array value and induction local in body blocks dominated by the loop body
//! entry. All other accesses retain ordinary checked semantics.

use aster_mir as mir;

use crate::{lifetime_analysis, temporary_subregions};

#[derive(Clone, Copy, Debug)]
struct Plan {
    header: mir::BasicBlockId,
    preheader: mir::BasicBlockId,
    length_instruction: usize,
    array: mir::LocalId,
    index: mir::LocalId,
}

pub(super) fn optimize(module: &mut mir::Module) {
    for function in &mut module.functions {
        optimize_function(function);
    }
}

fn optimize_function(function: &mut mir::Function) {
    let loops = temporary_subregions::natural_loops(function);
    let plans = loops
        .iter()
        .filter(|loop_| {
            !loops.iter().any(|child| {
                child.header != loop_.header
                    && child.body.len() < loop_.body.len()
                    && child.body.is_subset(&loop_.body)
            })
        })
        .filter_map(|loop_| plan(function, loop_))
        .collect::<Vec<_>>();

    for plan in plans {
        apply(function, plan);
    }
}

#[allow(clippy::too_many_lines)]
fn plan(function: &mir::Function, loop_: &temporary_subregions::SimpleNaturalLoop) -> Option<Plan> {
    loop_.exits.is_empty().then_some(())?;
    let header = loop_.block(function, loop_.header);
    let mir::Terminator::Branch {
        condition,
        then_block,
        ..
    } = &header.terminator
    else {
        return None;
    };
    (*then_block == loop_.body_entry).then_some(())?;
    let condition = direct_local(condition)?;
    let (comparison_index, length) = header.instructions.iter().find_map(|instruction| {
        let mir::Instruction::Assign {
            target: mir::Place::Local(target),
            value:
                mir::Rvalue {
                    type_: mir::Type::Bool,
                    kind:
                        mir::RvalueKind::Binary {
                            left,
                            operator: mir::BinaryOperator::Less,
                            right,
                        },
                },
        } = instruction
        else {
            return None;
        };
        if *target == condition {
            Some((direct_local(left)?, direct_local(right)?))
        } else {
            None
        }
    })?;
    let (length_instruction, array) =
        header
            .instructions
            .iter()
            .enumerate()
            .find_map(|(index, instruction)| {
                let mir::Instruction::Assign {
                    target: mir::Place::Local(target),
                    value:
                        mir::Rvalue {
                            type_: mir::Type::Int,
                            kind: mir::RvalueKind::ArrayLength(array),
                        },
                } = instruction
                else {
                    return None;
                };
                if *target == length {
                    Some((index, direct_local(array)?))
                } else {
                    None
                }
            })?;

    (loop_.latches.len() == 1).then_some(())?;
    let latch = loop_.block(function, loop_.latches[0]);
    matches!(latch.terminator, mir::Terminator::Goto(target) if target == loop_.header)
        .then_some(())?;
    latch
        .instructions
        .iter()
        .any(|instruction| is_increment(instruction, comparison_index))
        .then_some(())?;

    let preheaders = function
        .blocks
        .iter()
        .filter(|block| {
            !loop_.body.contains(&block.id)
                && matches!(block.terminator, mir::Terminator::Goto(target) if target == loop_.header)
        })
        .collect::<Vec<_>>();
    (preheaders.len() == 1).then_some(())?;
    let preheader = preheaders[0];
    preheader
        .instructions
        .iter()
        .any(|instruction| is_zero_initialization(instruction, comparison_index))
        .then_some(())?;

    (definition_count(function, comparison_index) == 2).then_some(())?;
    (definition_count(function, length) == 1).then_some(())?;
    (definition_count(function, condition) == 1).then_some(())?;
    loop_
        .body
        .iter()
        .all(|block| {
            loop_
                .block(function, *block)
                .instructions
                .iter()
                .all(|instruction| {
                    !lifetime_analysis::instruction_defines_direct_local(
                        function,
                        instruction,
                        array,
                    )
                })
        })
        .then_some(())?;

    loop_
        .body
        .iter()
        .filter(|block| {
            **block != loop_.header
                && **block != loop_.latches[0]
                && loop_.dominates(loop_.body_entry, **block)
        })
        .any(|block| {
            loop_
                .block(function, *block)
                .instructions
                .iter()
                .any(|instruction| {
                    instruction_has_matching_index(instruction, array, comparison_index)
                })
        })
        .then_some(())?;

    Some(Plan {
        header: loop_.header,
        preheader: preheader.id,
        length_instruction,
        array,
        index: comparison_index,
    })
}

fn apply(function: &mut mir::Function, plan: Plan) {
    let Some(header_index) = function
        .blocks
        .iter()
        .position(|block| block.id == plan.header)
    else {
        return;
    };
    let Some(preheader_index) = function
        .blocks
        .iter()
        .position(|block| block.id == plan.preheader)
    else {
        return;
    };
    let Some(loop_) = temporary_subregions::natural_loops(function)
        .into_iter()
        .find(|loop_| loop_.header == plan.header)
    else {
        return;
    };
    let Some(length) = function.blocks[header_index]
        .instructions
        .get(plan.length_instruction)
    else {
        return;
    };
    if !matches!(
        length,
        mir::Instruction::Assign {
            value: mir::Rvalue {
                type_: mir::Type::Int,
                kind: mir::RvalueKind::ArrayLength(array),
            },
            ..
        } if direct_local(array) == Some(plan.array)
    ) {
        return;
    }

    let length = function.blocks[header_index]
        .instructions
        .remove(plan.length_instruction);
    function.blocks[preheader_index].instructions.push(length);
    for block in &mut function.blocks {
        if block.id == loop_.header
            || loop_.latches.contains(&block.id)
            || !loop_.body.contains(&block.id)
            || !loop_.dominates(loop_.body_entry, block.id)
        {
            continue;
        }
        for instruction in &mut block.instructions {
            mark_instruction(instruction, plan.array, plan.index, plan.header);
        }
    }
}

fn definition_count(function: &mir::Function, local: mir::LocalId) -> usize {
    function
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter(|instruction| {
            lifetime_analysis::instruction_defines_direct_local(function, instruction, local)
        })
        .count()
}

fn direct_local(operand: &mir::Operand) -> Option<mir::LocalId> {
    let mir::OperandKind::Copy(mir::Place::Local(local)) = operand.kind else {
        return None;
    };
    Some(local)
}

fn integer_constant(operand: &mir::Operand, expected: &str) -> bool {
    matches!(
        &operand.kind,
        mir::OperandKind::Constant(mir::Constant::Integer(value)) if value == expected
    )
}

fn is_zero_initialization(instruction: &mir::Instruction, index: mir::LocalId) -> bool {
    matches!(
        instruction,
        mir::Instruction::Assign {
            target: mir::Place::Local(target),
            value: mir::Rvalue {
                type_: mir::Type::Int,
                kind: mir::RvalueKind::Use(value),
            },
        } if *target == index && integer_constant(value, "0")
    )
}

fn is_increment(instruction: &mir::Instruction, index: mir::LocalId) -> bool {
    matches!(
        instruction,
        mir::Instruction::Assign {
            target: mir::Place::Local(target),
            value: mir::Rvalue {
                type_: mir::Type::Int,
                kind: mir::RvalueKind::Binary {
                    left,
                    operator: mir::BinaryOperator::Add,
                    right,
                },
            },
        } if *target == index && direct_local(left) == Some(index) && integer_constant(right, "1")
    )
}

fn instruction_has_matching_index(
    instruction: &mir::Instruction,
    array: mir::LocalId,
    index: mir::LocalId,
) -> bool {
    let mir::Instruction::Assign { target, value } = instruction else {
        return false;
    };
    place_has_matching_index(target, array, index) || rvalue_has_matching_index(value, array, index)
}

fn mark_instruction(
    instruction: &mut mir::Instruction,
    array: mir::LocalId,
    index: mir::LocalId,
    header: mir::BasicBlockId,
) {
    let mir::Instruction::Assign { target, value } = instruction else {
        return;
    };
    mark_place(target, array, index, header);
    mark_rvalue(value, array, index, header);
}

fn place_has_matching_index(place: &mir::Place, array: mir::LocalId, index: mir::LocalId) -> bool {
    match place {
        mir::Place::Index {
            array: candidate_array,
            index: candidate_index,
            bounds: mir::ArrayBounds::Checked,
            ..
        } => {
            (direct_local(candidate_array) == Some(array)
                && direct_local(candidate_index) == Some(index))
                || operand_has_matching_index(candidate_array, array, index)
                || operand_has_matching_index(candidate_index, array, index)
        }
        mir::Place::Field { base, .. } | mir::Place::EnumField { base, .. } => {
            place_has_matching_index(base, array, index)
        }
        mir::Place::ObjectField { object, .. } => operand_has_matching_index(object, array, index),
        mir::Place::Local(_) | mir::Place::Symbol(_) => false,
        mir::Place::Index {
            array: a, index: i, ..
        } => {
            operand_has_matching_index(a, array, index)
                || operand_has_matching_index(i, array, index)
        }
    }
}

fn operand_has_matching_index(
    operand: &mir::Operand,
    array: mir::LocalId,
    index: mir::LocalId,
) -> bool {
    matches!(&operand.kind, mir::OperandKind::Copy(place) if place_has_matching_index(place, array, index))
}

fn rvalue_has_matching_index(
    value: &mir::Rvalue,
    array: mir::LocalId,
    index: mir::LocalId,
) -> bool {
    match &value.kind {
        mir::RvalueKind::Use(operand)
        | mir::RvalueKind::Discriminant(operand)
        | mir::RvalueKind::ArrayLength(operand)
        | mir::RvalueKind::ListLength(operand)
        | mir::RvalueKind::DictionaryLength(operand)
        | mir::RvalueKind::ListVersion(operand)
        | mir::RvalueKind::StringByteLength(operand)
        | mir::RvalueKind::Cast(operand)
        | mir::RvalueKind::Unary { operand, .. } => {
            operand_has_matching_index(operand, array, index)
        }
        mir::RvalueKind::Aggregate(fields) | mir::RvalueKind::EnumConstruct { fields, .. } => {
            fields
                .iter()
                .any(|field| operand_has_matching_index(&field.value, array, index))
        }
        mir::RvalueKind::MakeInterface { object, .. } => {
            operand_has_matching_index(object, array, index)
        }
        mir::RvalueKind::Binary { left, right, .. }
        | mir::RvalueKind::Equality { left, right, .. } => {
            operand_has_matching_index(left, array, index)
                || operand_has_matching_index(right, array, index)
        }
    }
}

fn mark_place(
    place: &mut mir::Place,
    array: mir::LocalId,
    index: mir::LocalId,
    header: mir::BasicBlockId,
) {
    match place {
        mir::Place::Index {
            array: candidate_array,
            index: candidate_index,
            bounds,
            ..
        } => {
            mark_operand(candidate_array, array, index, header);
            mark_operand(candidate_index, array, index, header);
            if *bounds == mir::ArrayBounds::Checked
                && direct_local(candidate_array) == Some(array)
                && direct_local(candidate_index) == Some(index)
            {
                *bounds = mir::ArrayBounds::Proven {
                    loop_header: header,
                };
            }
        }
        mir::Place::Field { base, .. } | mir::Place::EnumField { base, .. } => {
            mark_place(base, array, index, header);
        }
        mir::Place::ObjectField { object, .. } => mark_operand(object, array, index, header),
        mir::Place::Local(_) | mir::Place::Symbol(_) => {}
    }
}

fn mark_operand(
    operand: &mut mir::Operand,
    array: mir::LocalId,
    index: mir::LocalId,
    header: mir::BasicBlockId,
) {
    if let mir::OperandKind::Copy(place) = &mut operand.kind {
        mark_place(place, array, index, header);
    }
}

fn mark_rvalue(
    value: &mut mir::Rvalue,
    array: mir::LocalId,
    index: mir::LocalId,
    header: mir::BasicBlockId,
) {
    match &mut value.kind {
        mir::RvalueKind::Use(operand)
        | mir::RvalueKind::Discriminant(operand)
        | mir::RvalueKind::ArrayLength(operand)
        | mir::RvalueKind::ListLength(operand)
        | mir::RvalueKind::DictionaryLength(operand)
        | mir::RvalueKind::ListVersion(operand)
        | mir::RvalueKind::StringByteLength(operand)
        | mir::RvalueKind::Cast(operand)
        | mir::RvalueKind::Unary { operand, .. } => mark_operand(operand, array, index, header),
        mir::RvalueKind::Aggregate(fields) | mir::RvalueKind::EnumConstruct { fields, .. } => {
            for field in fields {
                mark_operand(&mut field.value, array, index, header);
            }
        }
        mir::RvalueKind::MakeInterface { object, .. } => mark_operand(object, array, index, header),
        mir::RvalueKind::Binary { left, right, .. }
        | mir::RvalueKind::Equality { left, right, .. } => {
            mark_operand(left, array, index, header);
            mark_operand(right, array, index, header);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::optimize;

    const SOURCE: &str = "public int Run() { int[] values = new int[8]; for (int i = 0; i < values.Length; i++) { values[i] = values[i] + 1; } return values[7]; }";

    #[test]
    fn optimization_is_idempotent_after_the_full_compiler_pipeline() {
        let mut module = crate::compile(SOURCE).expect("source compiles").mir;
        let once = module.to_string();
        optimize(&mut module);
        assert_eq!(module.to_string(), once);
    }

    #[test]
    fn optimization_is_structurally_deterministic() {
        let first = crate::compile(SOURCE).expect("source compiles").mir;
        let second = crate::compile(SOURCE).expect("source compiles").mir;
        assert_eq!(first.to_string(), second.to_string());
    }
}
