//! Mechanical validation for compiler-authorized canonical-loop array access.
//!
//! This module never discovers optimization opportunities. It accepts only
//! the exact finite MIR proof contract emitted by the compiler pass and keeps
//! an adulterated `ArrayBounds::Proven` marker from reaching unsafe lowering.

use std::collections::{HashMap, HashSet};

use aster_mir as mir;

use super::super::BackendError;

#[derive(Clone, Debug)]
struct Access {
    block: mir::BasicBlockId,
    header: mir::BasicBlockId,
    array: mir::LocalId,
    array_type: mir::Type,
    index: mir::LocalId,
    index_type: mir::Type,
    element_type: mir::Type,
}

struct Cfg {
    blocks: HashMap<mir::BasicBlockId, usize>,
    successors: HashMap<mir::BasicBlockId, Vec<mir::BasicBlockId>>,
    predecessors: HashMap<mir::BasicBlockId, Vec<mir::BasicBlockId>>,
    dominators: HashMap<mir::BasicBlockId, HashSet<mir::BasicBlockId>>,
}

pub(super) fn validate(function: &mir::Function) -> Result<(), BackendError> {
    let accesses = collect_accesses(function)?;
    if accesses.is_empty() {
        return Ok(());
    }
    let cfg = Cfg::new(function)?;
    for access in accesses {
        validate_access(function, &cfg, access)?;
    }
    Ok(())
}

impl Cfg {
    fn new(function: &mir::Function) -> Result<Self, BackendError> {
        let mut blocks = HashMap::new();
        for (index, block) in function.blocks.iter().enumerate() {
            if blocks.insert(block.id, index).is_some() {
                return Err(invalid(function));
            }
        }
        if !blocks.contains_key(&function.entry) {
            return Err(invalid(function));
        }
        let mut successors = HashMap::new();
        for block in &function.blocks {
            let targets = match block.terminator {
                mir::Terminator::Goto(target) => vec![target],
                mir::Terminator::Branch {
                    then_block,
                    else_block,
                    ..
                } => vec![then_block, else_block],
                mir::Terminator::Return(_) | mir::Terminator::End => Vec::new(),
                mir::Terminator::Unreachable => return Err(invalid(function)),
            };
            if targets.iter().any(|target| !blocks.contains_key(target)) {
                return Err(invalid(function));
            }
            successors.insert(block.id, targets);
        }
        let mut reachable = HashSet::new();
        let mut pending = vec![function.entry];
        while let Some(block) = pending.pop() {
            if reachable.insert(block) {
                pending.extend(successors[&block].iter().copied());
            }
        }
        if reachable.len() != blocks.len() {
            return Err(invalid(function));
        }
        let mut predecessors = blocks
            .keys()
            .copied()
            .map(|block| (block, Vec::new()))
            .collect::<HashMap<_, _>>();
        for (block, targets) in &successors {
            for target in targets {
                predecessors
                    .get_mut(target)
                    .expect("validated target")
                    .push(*block);
            }
        }
        for values in predecessors.values_mut() {
            values.sort_unstable_by_key(|block| block.0);
        }
        let mut order = reachable.iter().copied().collect::<Vec<_>>();
        order.sort_unstable_by_key(|block| block.0);
        let mut dominators = order
            .iter()
            .map(|block| {
                (
                    *block,
                    if *block == function.entry {
                        HashSet::from([*block])
                    } else {
                        reachable.clone()
                    },
                )
            })
            .collect::<HashMap<_, _>>();
        loop {
            let mut changed = false;
            for block in &order {
                if *block == function.entry {
                    continue;
                }
                let mut next = predecessors[block]
                    .iter()
                    .map(|predecessor| dominators[predecessor].clone())
                    .reduce(|left, right| left.intersection(&right).copied().collect())
                    .unwrap_or_default();
                next.insert(*block);
                if next != dominators[block] {
                    dominators.insert(*block, next);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        Ok(Self {
            blocks,
            successors,
            predecessors,
            dominators,
        })
    }

    fn block<'a>(
        &self,
        function: &'a mir::Function,
        id: mir::BasicBlockId,
    ) -> Option<&'a mir::BasicBlock> {
        self.blocks.get(&id).map(|index| &function.blocks[*index])
    }

    fn dominates(&self, dominator: mir::BasicBlockId, block: mir::BasicBlockId) -> bool {
        self.dominators
            .get(&block)
            .is_some_and(|set| set.contains(&dominator))
    }
}

#[allow(clippy::too_many_lines)]
fn validate_access(
    function: &mir::Function,
    cfg: &Cfg,
    access: Access,
) -> Result<(), BackendError> {
    let header = cfg
        .block(function, access.header)
        .ok_or_else(|| invalid(function))?;
    let mir::Terminator::Branch {
        condition,
        then_block,
        else_block,
    } = &header.terminator
    else {
        return Err(invalid(function));
    };
    let condition = direct_local(condition).ok_or_else(|| invalid(function))?;
    let (comparison_index, length) = unique_assignment(function, condition)
        .and_then(|(_, _, value)| {
            let mir::Rvalue {
                type_: mir::Type::Bool,
                kind:
                    mir::RvalueKind::Binary {
                        left,
                        operator: mir::BinaryOperator::Less,
                        right,
                    },
            } = value
            else {
                return None;
            };
            Some((direct_local(left)?, direct_local(right)?))
        })
        .ok_or_else(|| invalid(function))?;
    if comparison_index != access.index {
        return Err(invalid(function));
    }

    let mut latches = cfg.predecessors[&access.header]
        .iter()
        .copied()
        .filter(|predecessor| cfg.dominates(access.header, *predecessor))
        .collect::<Vec<_>>();
    latches.sort_unstable_by_key(|block| block.0);
    latches.dedup();
    if latches.len() != 1 || latches[0] == access.header {
        return Err(invalid(function));
    }
    let latch = latches[0];
    let latch_block = cfg
        .block(function, latch)
        .ok_or_else(|| invalid(function))?;
    if !matches!(latch_block.terminator, mir::Terminator::Goto(target) if target == access.header)
        || !latch_block
            .instructions
            .iter()
            .any(|instruction| is_increment(instruction, access.index))
    {
        return Err(invalid(function));
    }

    let mut body = HashSet::from([access.header]);
    let mut pending = vec![latch];
    while let Some(block) = pending.pop() {
        if body.insert(block) {
            pending.extend(
                cfg.predecessors[&block]
                    .iter()
                    .copied()
                    .filter(|predecessor| *predecessor != access.header),
            );
        }
    }
    if body
        .iter()
        .any(|block| !cfg.dominates(access.header, *block))
    {
        return Err(invalid(function));
    }
    let (body_entry, _loop_exit) = match (body.contains(then_block), body.contains(else_block)) {
        (true, false) => (*then_block, *else_block),
        _ => return Err(invalid(function)),
    };
    if body.iter().any(|block| {
        *block != access.header
            && cfg.predecessors[block]
                .iter()
                .any(|predecessor| !body.contains(predecessor))
    }) {
        return Err(invalid(function));
    }
    if body.iter().any(|block| {
        *block != access.header
            && cfg.successors[block]
                .iter()
                .any(|successor| !body.contains(successor) && *successor != access.header)
    }) {
        return Err(invalid(function));
    }
    if !body.contains(&access.block)
        || access.block == access.header
        || access.block == latch
        || !cfg.dominates(body_entry, access.block)
    {
        return Err(invalid(function));
    }

    let preheaders = cfg.predecessors[&access.header]
        .iter()
        .copied()
        .filter(|predecessor| !body.contains(predecessor))
        .collect::<Vec<_>>();
    if preheaders.len() != 1 {
        return Err(invalid(function));
    }
    let preheader = cfg
        .block(function, preheaders[0])
        .ok_or_else(|| invalid(function))?;
    if !matches!(preheader.terminator, mir::Terminator::Goto(target) if target == access.header) {
        return Err(invalid(function));
    }
    let (length_block, length_instruction, length_value) =
        unique_assignment(function, length).ok_or_else(|| invalid(function))?;
    let mir::Rvalue {
        type_: mir::Type::Int,
        kind: mir::RvalueKind::ArrayLength(length_array),
    } = length_value
    else {
        return Err(invalid(function));
    };
    if length_block != preheader.id || direct_local(length_array) != Some(access.array) {
        return Err(invalid(function));
    }
    let zero_instruction = preheader
        .instructions
        .iter()
        .position(|instruction| is_zero_initialization(instruction, access.index))
        .ok_or_else(|| invalid(function))?;
    if zero_instruction >= length_instruction
        || preheader
            .instructions
            .iter()
            .enumerate()
            .any(|(index, instruction)| {
                index > length_instruction && defines_direct_local(instruction, access.array)
            })
    {
        return Err(invalid(function));
    }

    if definition_count(function, access.index) != 2
        || definition_count(function, length) != 1
        || definition_count(function, condition) != 1
        || body.iter().any(|block| {
            cfg.block(function, *block).is_none_or(|block| {
                block
                    .instructions
                    .iter()
                    .any(|instruction| defines_direct_local(instruction, access.array))
            })
        })
    {
        return Err(invalid(function));
    }

    let locals = function
        .parameters
        .iter()
        .chain(&function.locals)
        .map(|local| (local.id, &local.type_))
        .collect::<HashMap<_, _>>();
    let expected_array = mir::Type::Array(Box::new(access.element_type));
    if access.index_type != mir::Type::Int
        || access.array_type != expected_array
        || locals.get(&access.index) != Some(&&mir::Type::Int)
        || locals.get(&access.array) != Some(&&expected_array)
    {
        return Err(invalid(function));
    }
    Ok(())
}

fn invalid(function: &mir::Function) -> BackendError {
    BackendError::new(format!(
        "function `{}` contains an invalid proven array-bounds contract",
        function.name
    ))
}

fn unique_assignment(
    function: &mir::Function,
    local: mir::LocalId,
) -> Option<(mir::BasicBlockId, usize, &mir::Rvalue)> {
    let mut matches = function.blocks.iter().flat_map(|block| {
        block
            .instructions
            .iter()
            .enumerate()
            .filter_map(move |(index, instruction)| {
                let mir::Instruction::Assign {
                    target: mir::Place::Local(target),
                    value,
                } = instruction
                else {
                    return None;
                };
                (*target == local).then_some((block.id, index, value))
            })
    });
    let value = matches.next()?;
    matches.next().is_none().then_some(value)
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

fn definition_count(function: &mir::Function, local: mir::LocalId) -> usize {
    function
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter(|instruction| defines_direct_local(instruction, local))
        .count()
}

#[allow(clippy::too_many_lines)]
fn defines_direct_local(instruction: &mir::Instruction, local: mir::LocalId) -> bool {
    let destination = match instruction {
        mir::Instruction::Assign { target, .. }
        | mir::Instruction::AllocateArray {
            destination: target,
            ..
        }
        | mir::Instruction::AllocateObject {
            destination: target,
            ..
        }
        | mir::Instruction::AllocateList {
            destination: target,
            ..
        }
        | mir::Instruction::AllocateDictionary {
            destination: target,
            ..
        }
        | mir::Instruction::AllocateStringBuilder {
            destination: target,
            ..
        }
        | mir::Instruction::StringBuilderToString {
            destination: target,
            ..
        }
        | mir::Instruction::DictionaryAdd {
            destination: target,
            ..
        }
        | mir::Instruction::DictionarySet {
            destination: target,
            ..
        }
        | mir::Instruction::DictionaryTryGet {
            destination: target,
            ..
        }
        | mir::Instruction::DictionaryContainsKey {
            destination: target,
            ..
        }
        | mir::Instruction::DictionaryRemove {
            destination: target,
            ..
        }
        | mir::Instruction::DictionaryEntries {
            destination: target,
            ..
        }
        | mir::Instruction::DictionaryKeys {
            destination: target,
            ..
        }
        | mir::Instruction::DictionaryValues {
            destination: target,
            ..
        }
        | mir::Instruction::ListGet {
            destination: target,
            ..
        }
        | mir::Instruction::ListToArray {
            destination: target,
            ..
        } => Some(target),
        mir::Instruction::Call { destination, .. }
        | mir::Instruction::ForeignCall { destination, .. }
        | mir::Instruction::CallInterface { destination, .. }
        | mir::Instruction::CallIntrinsic { destination, .. } => destination.as_ref(),
        mir::Instruction::StringDecodeNext {
            char_destination,
            next_cursor_destination,
            ok_destination,
            ..
        } => {
            return [char_destination, next_cursor_destination, ok_destination]
                .iter()
                .any(|place| matches!(place, mir::Place::Local(candidate) if *candidate == local));
        }
        mir::Instruction::OwnedRegionEnter { .. }
        | mir::Instruction::OwnedRegionExit { .. }
        | mir::Instruction::TemporarySubregionEnter { .. }
        | mir::Instruction::TemporarySubregionExit { .. }
        | mir::Instruction::StringBuilderAppend { .. }
        | mir::Instruction::DictionaryClear { .. }
        | mir::Instruction::ListAdd { .. }
        | mir::Instruction::ListRemoveAt { .. }
        | mir::Instruction::ListSet { .. }
        | mir::Instruction::ListClear { .. } => None,
    };
    matches!(destination, Some(mir::Place::Local(candidate)) if *candidate == local)
}

fn collect_accesses(function: &mir::Function) -> Result<Vec<Access>, BackendError> {
    let mut accesses = Vec::new();
    for block in &function.blocks {
        for instruction in &block.instructions {
            let mir::Instruction::Assign { target, value } = instruction else {
                continue;
            };
            let mut collect = |place: &mir::Place| {
                if let mir::Place::Index {
                    array,
                    index,
                    element_type,
                    bounds: mir::ArrayBounds::Proven { loop_header },
                } = place
                {
                    accesses.push(Access {
                        block: block.id,
                        header: *loop_header,
                        array: direct_local(array).ok_or_else(|| invalid(function))?,
                        array_type: array.type_.clone(),
                        index: direct_local(index).ok_or_else(|| invalid(function))?,
                        index_type: index.type_.clone(),
                        element_type: element_type.clone(),
                    });
                }
                Ok(())
            };
            visit_place(target, &mut collect)?;
            visit_rvalue_places(value, &mut collect)?;
        }
    }
    Ok(accesses)
}

fn visit_operand(
    operand: &mir::Operand,
    visitor: &mut impl FnMut(&mir::Place) -> Result<(), BackendError>,
) -> Result<(), BackendError> {
    if let mir::OperandKind::Copy(place) = &operand.kind {
        visit_place(place, visitor)?;
    }
    Ok(())
}

fn visit_place(
    place: &mir::Place,
    visitor: &mut impl FnMut(&mir::Place) -> Result<(), BackendError>,
) -> Result<(), BackendError> {
    visitor(place)?;
    match place {
        mir::Place::Field { base, .. } | mir::Place::EnumField { base, .. } => {
            visit_place(base, visitor)
        }
        mir::Place::Index { array, index, .. } => {
            visit_operand(array, visitor)?;
            visit_operand(index, visitor)
        }
        mir::Place::ObjectField { object, .. } => visit_operand(object, visitor),
        mir::Place::Local(_) | mir::Place::Symbol(_) => Ok(()),
    }
}

fn visit_rvalue_places(
    value: &mir::Rvalue,
    visitor: &mut impl FnMut(&mir::Place) -> Result<(), BackendError>,
) -> Result<(), BackendError> {
    match &value.kind {
        mir::RvalueKind::Use(operand)
        | mir::RvalueKind::Discriminant(operand)
        | mir::RvalueKind::ArrayLength(operand)
        | mir::RvalueKind::ListLength(operand)
        | mir::RvalueKind::DictionaryLength(operand)
        | mir::RvalueKind::ListVersion(operand)
        | mir::RvalueKind::StringByteLength(operand)
        | mir::RvalueKind::Cast(operand)
        | mir::RvalueKind::Unary { operand, .. } => visit_operand(operand, visitor),
        mir::RvalueKind::Aggregate(fields) | mir::RvalueKind::EnumConstruct { fields, .. } => {
            for field in fields {
                visit_operand(&field.value, visitor)?;
            }
            Ok(())
        }
        mir::RvalueKind::MakeInterface { object, .. } => visit_operand(object, visitor),
        mir::RvalueKind::Binary { left, right, .. }
        | mir::RvalueKind::Equality { left, right, .. } => {
            visit_operand(left, visitor)?;
            visit_operand(right, visitor)
        }
    }
}
