//! General backend-neutral simplification for typed MIR.
//!
//! The optimizer deliberately owns only scalar/control redundancy. Allocation,
//! escape, alias, lifetime, and call semantics remain with their existing
//! passes. Every transformation is monotonic: operands become constants or
//! simpler direct scalar copies, and instructions/edges/blocks only disappear.

use std::collections::{HashMap, HashSet, VecDeque};

use aster_mir as mir;

use crate::lifetime_analysis;

pub(super) fn optimize(module: &mut mir::Module) {
    for function in &mut module.functions {
        if contains_lifetime_markers(function) {
            continue;
        }
        optimize_function(function);
    }
}

fn optimize_function(function: &mut mir::Function) {
    loop {
        let mut changed = propagate_fold_and_simplify_branches(function);
        changed |= simplify_cfg(function);
        changed |= eliminate_dead_assignments(function);
        if !changed {
            break;
        }
    }
}

fn contains_lifetime_markers(function: &mir::Function) -> bool {
    function.blocks.iter().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                mir::Instruction::OwnedRegionEnter { .. }
                    | mir::Instruction::OwnedRegionExit { .. }
                    | mir::Instruction::TemporarySubregionEnter { .. }
                    | mir::Instruction::TemporarySubregionExit { .. }
            )
        })
    })
}

#[derive(Clone, Debug, PartialEq)]
enum ConstantFact {
    Constant(mir::Operand),
    EnumTag(u32),
}

type ConstantFacts = HashMap<mir::LocalId, ConstantFact>;

fn propagate_fold_and_simplify_branches(function: &mut mir::Function) -> bool {
    let entry_facts = constant_entry_facts(function);
    let block_indices = function
        .blocks
        .iter()
        .enumerate()
        .map(|(index, block)| (block.id, index))
        .collect::<HashMap<_, _>>();
    let mut changed = false;

    for block in &mut function.blocks {
        let Some(block_index) = block_indices.get(&block.id).copied() else {
            continue;
        };
        let Some(mut facts) = entry_facts[block_index].clone() else {
            continue;
        };
        let mut copies = HashMap::<mir::LocalId, mir::Operand>::new();

        for instruction in &mut block.instructions {
            changed |= rewrite_instruction_operands(instruction, &facts, &copies);

            match instruction {
                mir::Instruction::Assign {
                    target: mir::Place::Local(destination),
                    value,
                } if pure_non_failing_rvalue(value) => {
                    changed |= fold_rvalue(value);
                    invalidate_copy_facts(&mut copies, *destination);
                    let replacement = match &value.kind {
                        mir::RvalueKind::Use(operand)
                            if propagatable_type(&operand.type_)
                                && operand.type_ == value.type_
                                && !is_same_local(operand, *destination) =>
                        {
                            Some(operand.clone())
                        }
                        _ => None,
                    };
                    set_value_fact(&mut facts, *destination, value);
                    if let Some(replacement) = replacement {
                        copies.insert(*destination, replacement);
                    }
                }
                mir::Instruction::Assign {
                    target: mir::Place::Local(destination),
                    value,
                } if enum_tag_fact(value, &facts).is_some() => {
                    invalidate_copy_facts(&mut copies, *destination);
                    set_value_fact(&mut facts, *destination, value);
                }
                mir::Instruction::Assign { .. } => {
                    clear_facts(&mut facts, &mut copies);
                }
                _ => clear_facts(&mut facts, &mut copies),
            }
        }

        changed |= rewrite_terminator_operands(&mut block.terminator, &facts, &copies);
        if let mir::Terminator::Branch {
            condition:
                mir::Operand {
                    kind: mir::OperandKind::Constant(mir::Constant::Boolean(condition)),
                    ..
                },
            then_block,
            else_block,
        } = &block.terminator
        {
            block.terminator =
                mir::Terminator::Goto(if *condition { *then_block } else { *else_block });
            changed = true;
        }
    }

    changed
}

fn constant_entry_facts(function: &mir::Function) -> Vec<Option<ConstantFacts>> {
    let block_indices = function
        .blocks
        .iter()
        .enumerate()
        .map(|(index, block)| (block.id, index))
        .collect::<HashMap<_, _>>();
    let mut incoming = vec![None; function.blocks.len()];
    let Some(entry) = block_indices.get(&function.entry).copied() else {
        return incoming;
    };
    incoming[entry] = Some(ConstantFacts::new());
    let mut pending = VecDeque::from([entry]);
    let use_counts = direct_local_use_counts(function);

    while let Some(block_index) = pending.pop_front() {
        let Some(mut outgoing) = incoming[block_index].clone() else {
            continue;
        };
        transfer_constant_facts(&function.blocks[block_index].instructions, &mut outgoing);
        if let mir::Terminator::Branch { condition, .. } = &function.blocks[block_index].terminator
            && let mir::OperandKind::Copy(mir::Place::Local(condition)) = condition.kind
            && use_counts.get(&condition) == Some(&1)
        {
            // A single-use comparison temporary is fully consumed here.
            // Keeping one such result per arm made lowered switch chains
            // accumulate quadratically large dataflow states. A condition
            // referenced anywhere else remains available to successors.
            outgoing.remove(&condition);
        }
        for successor in successors(&function.blocks[block_index].terminator) {
            let Some(successor_index) = block_indices.get(&successor).copied() else {
                continue;
            };
            let merged = incoming[successor_index].as_ref().map_or_else(
                || outgoing.clone(),
                |current| meet_facts(current, &outgoing),
            );
            if incoming[successor_index].as_ref() != Some(&merged) {
                incoming[successor_index] = Some(merged);
                pending.push_back(successor_index);
            }
        }
    }

    incoming
}

fn direct_local_use_counts(function: &mir::Function) -> HashMap<mir::LocalId, usize> {
    let mut uses = HashMap::new();
    for block in &function.blocks {
        for instruction in &block.instructions {
            count_instruction_uses(instruction, &mut uses);
        }
        match &block.terminator {
            mir::Terminator::Branch { condition, .. } => count_operand_uses(condition, &mut uses),
            mir::Terminator::Return(Some(value)) => count_operand_uses(value, &mut uses),
            mir::Terminator::Goto(_)
            | mir::Terminator::Return(None)
            | mir::Terminator::End
            | mir::Terminator::Unreachable => {}
        }
    }
    uses
}

#[allow(clippy::too_many_lines)]
fn count_instruction_uses(instruction: &mir::Instruction, uses: &mut HashMap<mir::LocalId, usize>) {
    let operand = |value: &mir::Operand, uses: &mut HashMap<mir::LocalId, usize>| {
        count_operand_uses(value, uses);
    };
    let place = |value: &mir::Place, uses: &mut HashMap<mir::LocalId, usize>| {
        count_place_input_uses(value, uses);
    };
    match instruction {
        mir::Instruction::OwnedRegionEnter { .. }
        | mir::Instruction::OwnedRegionExit { .. }
        | mir::Instruction::TemporarySubregionEnter { .. }
        | mir::Instruction::TemporarySubregionExit { .. } => {}
        mir::Instruction::Assign { target, value } => {
            place(target, uses);
            count_rvalue_uses(value, uses);
        }
        mir::Instruction::Call {
            destination,
            arguments,
            ..
        }
        | mir::Instruction::ForeignCall {
            destination,
            arguments,
            ..
        }
        | mir::Instruction::CallIntrinsic {
            destination,
            arguments,
            ..
        } => {
            if let Some(destination) = destination {
                place(destination, uses);
            }
            for argument in arguments {
                operand(argument, uses);
            }
        }
        mir::Instruction::CallInterface {
            destination,
            receiver,
            arguments,
            ..
        } => {
            if let Some(destination) = destination {
                place(destination, uses);
            }
            operand(receiver, uses);
            for argument in arguments {
                operand(argument, uses);
            }
        }
        mir::Instruction::AllocateArray {
            destination,
            length,
            ..
        } => {
            place(destination, uses);
            operand(length, uses);
        }
        mir::Instruction::AllocateObject { destination, .. }
        | mir::Instruction::AllocateList { destination, .. }
        | mir::Instruction::AllocateDictionary { destination, .. }
        | mir::Instruction::AllocateStringBuilder { destination, .. } => place(destination, uses),
        mir::Instruction::StringBuilderAppend { builder, value, .. } => {
            operand(builder, uses);
            operand(value, uses);
        }
        mir::Instruction::StringBuilderToString {
            destination,
            builder,
            ..
        } => {
            place(destination, uses);
            operand(builder, uses);
        }
        mir::Instruction::DictionaryAdd {
            destination,
            dictionary,
            key,
            value,
        }
        | mir::Instruction::DictionarySet {
            destination,
            dictionary,
            key,
            value,
        } => {
            place(destination, uses);
            operand(dictionary, uses);
            operand(key, uses);
            operand(value, uses);
        }
        mir::Instruction::DictionaryTryGet {
            destination,
            dictionary,
            key,
            ..
        }
        | mir::Instruction::DictionaryContainsKey {
            destination,
            dictionary,
            key,
        }
        | mir::Instruction::DictionaryRemove {
            destination,
            dictionary,
            key,
        } => {
            place(destination, uses);
            operand(dictionary, uses);
            operand(key, uses);
        }
        mir::Instruction::DictionaryEntries {
            destination,
            dictionary,
            ..
        }
        | mir::Instruction::DictionaryKeys {
            destination,
            dictionary,
            ..
        }
        | mir::Instruction::DictionaryValues {
            destination,
            dictionary,
            ..
        } => {
            place(destination, uses);
            operand(dictionary, uses);
        }
        mir::Instruction::DictionaryClear { dictionary } => operand(dictionary, uses),
        mir::Instruction::ListAdd { list, value } => {
            operand(list, uses);
            operand(value, uses);
        }
        mir::Instruction::ListGet {
            destination,
            list,
            index,
            ..
        } => {
            place(destination, uses);
            operand(list, uses);
            operand(index, uses);
        }
        mir::Instruction::ListRemoveAt { list, index } => {
            operand(list, uses);
            operand(index, uses);
        }
        mir::Instruction::ListSet { list, index, value } => {
            operand(list, uses);
            operand(index, uses);
            operand(value, uses);
        }
        mir::Instruction::ListClear { list } => operand(list, uses),
        mir::Instruction::ListToArray {
            destination, list, ..
        } => {
            place(destination, uses);
            operand(list, uses);
        }
        mir::Instruction::StringDecodeNext {
            string,
            cursor,
            char_destination,
            next_cursor_destination,
            ok_destination,
        } => {
            operand(string, uses);
            operand(cursor, uses);
            place(char_destination, uses);
            place(next_cursor_destination, uses);
            place(ok_destination, uses);
        }
    }
}

fn count_rvalue_uses(value: &mir::Rvalue, uses: &mut HashMap<mir::LocalId, usize>) {
    match &value.kind {
        mir::RvalueKind::Use(value)
        | mir::RvalueKind::Discriminant(value)
        | mir::RvalueKind::ArrayLength(value)
        | mir::RvalueKind::ListLength(value)
        | mir::RvalueKind::DictionaryLength(value)
        | mir::RvalueKind::ListVersion(value)
        | mir::RvalueKind::StringByteLength(value)
        | mir::RvalueKind::Cast(value)
        | mir::RvalueKind::Unary { operand: value, .. } => count_operand_uses(value, uses),
        mir::RvalueKind::Aggregate(fields) | mir::RvalueKind::EnumConstruct { fields, .. } => {
            for field in fields {
                count_operand_uses(&field.value, uses);
            }
        }
        mir::RvalueKind::MakeInterface { object, .. } => count_operand_uses(object, uses),
        mir::RvalueKind::Binary { left, right, .. }
        | mir::RvalueKind::Equality { left, right, .. } => {
            count_operand_uses(left, uses);
            count_operand_uses(right, uses);
        }
    }
}

fn count_operand_uses(operand: &mir::Operand, uses: &mut HashMap<mir::LocalId, usize>) {
    if let mir::OperandKind::Copy(place) = &operand.kind {
        count_read_place_uses(place, uses);
    }
}

fn count_read_place_uses(place: &mir::Place, uses: &mut HashMap<mir::LocalId, usize>) {
    if let mir::Place::Local(local) = place {
        *uses.entry(*local).or_default() += 1;
    } else {
        count_place_input_uses(place, uses);
    }
}

fn count_place_input_uses(place: &mir::Place, uses: &mut HashMap<mir::LocalId, usize>) {
    match place {
        mir::Place::Local(_) | mir::Place::Symbol(_) => {}
        mir::Place::Field { base, .. } | mir::Place::EnumField { base, .. } => {
            count_read_place_uses(base, uses);
        }
        mir::Place::Index { array, index, .. } => {
            count_operand_uses(array, uses);
            count_operand_uses(index, uses);
        }
        mir::Place::ObjectField { object, .. } => count_operand_uses(object, uses),
    }
}

fn transfer_constant_facts(instructions: &[mir::Instruction], facts: &mut ConstantFacts) {
    for instruction in instructions {
        match instruction {
            mir::Instruction::Assign {
                target: mir::Place::Local(destination),
                value,
            } => {
                let next = if pure_non_failing_rvalue(value) {
                    constant_rvalue_with_facts(value, facts).map(ConstantFact::Constant)
                } else {
                    enum_tag_fact(value, facts).map(ConstantFact::EnumTag)
                };
                if let Some(next) = next {
                    facts.insert(*destination, next);
                } else {
                    facts.remove(destination);
                }
            }
            _ => facts.clear(),
        }
    }
}

fn meet_facts(left: &ConstantFacts, right: &ConstantFacts) -> ConstantFacts {
    let (smaller, larger) = if left.len() <= right.len() {
        (left, right)
    } else {
        (right, left)
    };
    smaller
        .iter()
        .filter(|(local, fact)| larger.get(local) == Some(*fact))
        .map(|(local, fact)| (*local, fact.clone()))
        .collect()
}

fn constant_rvalue_with_facts(value: &mir::Rvalue, facts: &ConstantFacts) -> Option<mir::Operand> {
    let mut value = value.clone();
    rewrite_rvalue_operands(&mut value, facts, &HashMap::new());
    fold_rvalue(&mut value);
    match value.kind {
        mir::RvalueKind::Use(operand)
            if propagatable_type(&operand.type_)
                && matches!(operand.kind, mir::OperandKind::Constant(_)) =>
        {
            Some(operand)
        }
        _ => None,
    }
}

fn set_value_fact(facts: &mut ConstantFacts, destination: mir::LocalId, value: &mir::Rvalue) {
    let enum_tag = enum_tag_fact(value, facts);
    let next = match &value.kind {
        mir::RvalueKind::Use(operand)
            if propagatable_type(&operand.type_)
                && matches!(operand.kind, mir::OperandKind::Constant(_)) =>
        {
            Some(ConstantFact::Constant(operand.clone()))
        }
        _ => enum_tag.map(ConstantFact::EnumTag),
    };
    if let Some(next) = next {
        facts.insert(destination, next);
    } else {
        facts.remove(&destination);
    }
}

fn enum_tag_fact(value: &mir::Rvalue, facts: &ConstantFacts) -> Option<u32> {
    match &value.kind {
        mir::RvalueKind::EnumConstruct { tag, .. } => Some(*tag),
        mir::RvalueKind::Use(mir::Operand {
            kind: mir::OperandKind::Copy(mir::Place::Local(local)),
            ..
        }) => {
            let ConstantFact::EnumTag(tag) = facts.get(local)? else {
                return None;
            };
            Some(*tag)
        }
        _ => None,
    }
}

fn clear_facts(facts: &mut ConstantFacts, copies: &mut HashMap<mir::LocalId, mir::Operand>) {
    facts.clear();
    copies.clear();
}

fn invalidate_copy_facts(
    copies: &mut HashMap<mir::LocalId, mir::Operand>,
    destination: mir::LocalId,
) {
    copies.retain(|local, operand| {
        *local != destination
            && !matches!(
                operand.kind,
                mir::OperandKind::Copy(mir::Place::Local(source)) if source == destination
            )
    });
}

fn is_same_local(operand: &mir::Operand, local: mir::LocalId) -> bool {
    matches!(
        operand.kind,
        mir::OperandKind::Copy(mir::Place::Local(source)) if source == local
    )
}

fn propagatable_type(type_: &mir::Type) -> bool {
    matches!(
        type_,
        mir::Type::Bool
            | mir::Type::SByte
            | mir::Type::Byte
            | mir::Type::Short
            | mir::Type::UShort
            | mir::Type::Int
            | mir::Type::UInt
            | mir::Type::Long
            | mir::Type::ULong
            | mir::Type::Float
            | mir::Type::Double
            | mir::Type::Char
    )
}

fn pure_non_failing_rvalue(value: &mir::Rvalue) -> bool {
    match &value.kind {
        mir::RvalueKind::Use(operand) => simple_operand(operand),
        mir::RvalueKind::Cast(operand) | mir::RvalueKind::Unary { operand, .. } => {
            propagatable_type(&value.type_) && simple_operand(operand)
        }
        mir::RvalueKind::Binary {
            left,
            operator,
            right,
        } => {
            simple_operand(left)
                && simple_operand(right)
                && match operator {
                    mir::BinaryOperator::Divide => {
                        matches!(left.type_, mir::Type::Float | mir::Type::Double)
                    }
                    mir::BinaryOperator::Remainder => false,
                    _ => true,
                }
        }
        mir::RvalueKind::Aggregate(_)
        | mir::RvalueKind::EnumConstruct { .. }
        | mir::RvalueKind::Discriminant(_)
        | mir::RvalueKind::ArrayLength(_)
        | mir::RvalueKind::ListLength(_)
        | mir::RvalueKind::DictionaryLength(_)
        | mir::RvalueKind::ListVersion(_)
        | mir::RvalueKind::StringByteLength(_)
        | mir::RvalueKind::MakeInterface { .. }
        | mir::RvalueKind::Equality { .. } => false,
    }
}

fn simple_operand(operand: &mir::Operand) -> bool {
    propagatable_type(&operand.type_)
        && matches!(
            operand.kind,
            mir::OperandKind::Constant(_) | mir::OperandKind::Copy(mir::Place::Local(_))
        )
}

#[allow(clippy::too_many_lines)]
fn rewrite_instruction_operands(
    instruction: &mut mir::Instruction,
    facts: &ConstantFacts,
    copies: &HashMap<mir::LocalId, mir::Operand>,
) -> bool {
    let mut changed = false;
    macro_rules! operand {
        ($operand:expr) => {
            changed |= rewrite_operand($operand, facts, copies)
        };
    }
    macro_rules! place {
        ($place:expr) => {
            changed |= rewrite_place_inputs($place, facts, copies)
        };
    }

    match instruction {
        mir::Instruction::Assign { target, value } => {
            place!(target);
            rewrite_rvalue_operands(value, facts, copies);
            changed |= fold_rvalue(value);
        }
        mir::Instruction::Call {
            destination,
            arguments,
            ..
        } => {
            if let Some(destination) = destination {
                place!(destination);
            }
            for argument in arguments {
                operand!(argument);
            }
        }
        mir::Instruction::CallIntrinsic { destination, .. }
        | mir::Instruction::ForeignCall { destination, .. } => {
            if let Some(destination) = destination {
                place!(destination);
            }
            // Runtime intrinsics and host calls may require exact operand
            // shapes, so their arguments stay opaque.
        }
        mir::Instruction::CallInterface {
            destination,
            receiver,
            arguments,
            ..
        } => {
            if let Some(destination) = destination {
                place!(destination);
            }
            operand!(receiver);
            for argument in arguments {
                operand!(argument);
            }
        }
        mir::Instruction::AllocateArray {
            destination,
            length,
            ..
        } => {
            place!(destination);
            operand!(length);
        }
        mir::Instruction::AllocateObject { destination, .. }
        | mir::Instruction::AllocateList { destination, .. }
        | mir::Instruction::AllocateDictionary { destination, .. }
        | mir::Instruction::AllocateStringBuilder { destination, .. } => place!(destination),
        mir::Instruction::StringBuilderAppend { builder, value, .. } => {
            operand!(builder);
            operand!(value);
        }
        mir::Instruction::StringBuilderToString {
            destination,
            builder,
            ..
        } => {
            place!(destination);
            operand!(builder);
        }
        mir::Instruction::DictionaryAdd {
            destination,
            dictionary,
            key,
            value,
        }
        | mir::Instruction::DictionarySet {
            destination,
            dictionary,
            key,
            value,
        } => {
            place!(destination);
            operand!(dictionary);
            operand!(key);
            operand!(value);
        }
        mir::Instruction::DictionaryTryGet {
            destination,
            dictionary,
            key,
            ..
        }
        | mir::Instruction::DictionaryContainsKey {
            destination,
            dictionary,
            key,
        }
        | mir::Instruction::DictionaryRemove {
            destination,
            dictionary,
            key,
        } => {
            place!(destination);
            operand!(dictionary);
            operand!(key);
        }
        mir::Instruction::DictionaryEntries {
            destination,
            dictionary,
            ..
        } => {
            place!(destination);
            operand!(dictionary);
        }
        mir::Instruction::OwnedRegionEnter { .. }
        | mir::Instruction::OwnedRegionExit { .. }
        | mir::Instruction::TemporarySubregionEnter { .. }
        | mir::Instruction::TemporarySubregionExit { .. }
        | mir::Instruction::DictionaryClear { .. }
        | mir::Instruction::DictionaryKeys { .. }
        | mir::Instruction::DictionaryValues { .. }
        | mir::Instruction::ListSet { .. }
        | mir::Instruction::ListClear { .. }
        | mir::Instruction::ListToArray { .. } => {
            // Lifetime and collection intrinsics stay opaque: no facts cross them.
        }
        mir::Instruction::ListAdd { list, value } => {
            operand!(list);
            operand!(value);
        }
        mir::Instruction::ListGet {
            destination,
            list,
            index,
            ..
        } => {
            place!(destination);
            operand!(list);
            operand!(index);
        }
        mir::Instruction::ListRemoveAt { list, index } => {
            operand!(list);
            operand!(index);
        }
        mir::Instruction::StringDecodeNext {
            string,
            cursor,
            char_destination,
            next_cursor_destination,
            ok_destination,
        } => {
            operand!(string);
            operand!(cursor);
            place!(char_destination);
            place!(next_cursor_destination);
            place!(ok_destination);
        }
    }
    changed
}

fn rewrite_rvalue_operands(
    value: &mut mir::Rvalue,
    facts: &ConstantFacts,
    copies: &HashMap<mir::LocalId, mir::Operand>,
) -> bool {
    let mut changed = false;
    let mut operand = |operand: &mut mir::Operand| {
        changed |= rewrite_operand(operand, facts, copies);
    };
    match &mut value.kind {
        mir::RvalueKind::Use(value)
        | mir::RvalueKind::Discriminant(value)
        | mir::RvalueKind::ArrayLength(value)
        | mir::RvalueKind::ListLength(value)
        | mir::RvalueKind::DictionaryLength(value)
        | mir::RvalueKind::ListVersion(value)
        | mir::RvalueKind::StringByteLength(value)
        | mir::RvalueKind::Cast(value)
        | mir::RvalueKind::Unary { operand: value, .. } => operand(value),
        mir::RvalueKind::Aggregate(fields) | mir::RvalueKind::EnumConstruct { fields, .. } => {
            for field in fields {
                operand(&mut field.value);
            }
        }
        mir::RvalueKind::MakeInterface { object, .. } => operand(object),
        mir::RvalueKind::Binary { left, right, .. }
        | mir::RvalueKind::Equality { left, right, .. } => {
            operand(left);
            operand(right);
        }
    }
    if let mir::RvalueKind::Discriminant(mir::Operand {
        kind: mir::OperandKind::Copy(mir::Place::Local(local)),
        ..
    }) = value.kind
        && let Some(ConstantFact::EnumTag(tag)) = facts.get(&local)
    {
        value.kind = mir::RvalueKind::Use(mir::Operand {
            type_: value.type_.clone(),
            kind: mir::OperandKind::Constant(mir::Constant::Integer(tag.to_string())),
        });
        changed = true;
    }
    changed
}

fn rewrite_terminator_operands(
    terminator: &mut mir::Terminator,
    facts: &ConstantFacts,
    copies: &HashMap<mir::LocalId, mir::Operand>,
) -> bool {
    match terminator {
        mir::Terminator::Branch { condition, .. } => rewrite_operand(condition, facts, copies),
        mir::Terminator::Return(Some(value)) => rewrite_operand(value, facts, copies),
        mir::Terminator::Goto(_)
        | mir::Terminator::Return(None)
        | mir::Terminator::End
        | mir::Terminator::Unreachable => false,
    }
}

fn rewrite_place_inputs(
    place: &mut mir::Place,
    facts: &ConstantFacts,
    copies: &HashMap<mir::LocalId, mir::Operand>,
) -> bool {
    match place {
        mir::Place::Local(_) | mir::Place::Symbol(_) => false,
        mir::Place::Field { base, .. } | mir::Place::EnumField { base, .. } => {
            rewrite_place_inputs(base, facts, copies)
        }
        mir::Place::Index { array, index, .. } => {
            rewrite_operand(array, facts, copies) | rewrite_operand(index, facts, copies)
        }
        mir::Place::ObjectField { object, .. } => rewrite_operand(object, facts, copies),
    }
}

fn rewrite_operand(
    operand: &mut mir::Operand,
    facts: &ConstantFacts,
    copies: &HashMap<mir::LocalId, mir::Operand>,
) -> bool {
    if !propagatable_type(&operand.type_) {
        return false;
    }
    let original = operand.clone();
    let mut seen = HashSet::new();
    while let mir::OperandKind::Copy(mir::Place::Local(local)) = operand.kind {
        if !seen.insert(local) {
            break;
        }
        if let Some(replacement) = copies.get(&local) {
            *operand = replacement.clone();
            continue;
        }
        let Some(ConstantFact::Constant(replacement)) = facts.get(&local) else {
            break;
        };
        *operand = replacement.clone();
    }
    *operand != original
}

fn fold_rvalue(value: &mut mir::Rvalue) -> bool {
    let Some(constant) = folded_constant(value) else {
        return false;
    };
    value.kind = mir::RvalueKind::Use(mir::Operand {
        type_: value.type_.clone(),
        kind: mir::OperandKind::Constant(constant),
    });
    true
}

fn folded_constant(value: &mir::Rvalue) -> Option<mir::Constant> {
    match &value.kind {
        mir::RvalueKind::Unary { operator, operand } => {
            fold_unary(&operand.type_, *operator, operand_constant(operand)?)
        }
        mir::RvalueKind::Binary {
            left,
            operator,
            right,
        } => fold_binary(
            &left.type_,
            *operator,
            operand_constant(left)?,
            operand_constant(right)?,
        ),
        mir::RvalueKind::Equality {
            left,
            right,
            negated,
        } => fold_equality(
            &left.type_,
            operand_constant(left)?,
            operand_constant(right)?,
            *negated,
        ),
        _ => None,
    }
}

fn operand_constant(operand: &mir::Operand) -> Option<&mir::Constant> {
    let mir::OperandKind::Constant(constant) = &operand.kind else {
        return None;
    };
    Some(constant)
}

fn fold_unary(
    type_: &mir::Type,
    operator: mir::UnaryOperator,
    operand: &mir::Constant,
) -> Option<mir::Constant> {
    match (operator, operand) {
        (mir::UnaryOperator::Not, mir::Constant::Boolean(value)) => {
            Some(mir::Constant::Boolean(!value))
        }
        (mir::UnaryOperator::Negate, mir::Constant::Float(value)) => match type_ {
            mir::Type::Float => Some(mir::Constant::Float(
                (-value.parse::<f32>().ok()?).to_string(),
            )),
            mir::Type::Double => Some(mir::Constant::Float(
                (-value.parse::<f64>().ok()?).to_string(),
            )),
            _ => None,
        },
        (mir::UnaryOperator::Negate, mir::Constant::Integer(value)) => {
            fold_integer_negate(type_, value)
        }
        _ => None,
    }
}

fn fold_integer_negate(type_: &mir::Type, value: &str) -> Option<mir::Constant> {
    macro_rules! negate {
        ($integer:ty) => {{
            let value = value.parse::<$integer>().ok()?;
            Some(mir::Constant::Integer(value.wrapping_neg().to_string()))
        }};
    }
    match type_ {
        mir::Type::SByte => negate!(i8),
        mir::Type::Byte => negate!(u8),
        mir::Type::Short => negate!(i16),
        mir::Type::UShort => negate!(u16),
        mir::Type::Int => negate!(i32),
        mir::Type::UInt => negate!(u32),
        mir::Type::Long => negate!(i64),
        mir::Type::ULong => negate!(u64),
        _ => None,
    }
}

fn fold_binary(
    type_: &mir::Type,
    operator: mir::BinaryOperator,
    left: &mir::Constant,
    right: &mir::Constant,
) -> Option<mir::Constant> {
    match (left, right) {
        (mir::Constant::Integer(left), mir::Constant::Integer(right)) => {
            fold_integer_binary(type_, operator, left, right)
        }
        (mir::Constant::Float(left), mir::Constant::Float(right)) => {
            fold_float_binary(type_, operator, left, right)
        }
        (mir::Constant::Boolean(left), mir::Constant::Boolean(right)) => {
            let value = match operator {
                mir::BinaryOperator::Equal => *left == *right,
                mir::BinaryOperator::NotEqual => *left != *right,
                mir::BinaryOperator::LogicalAnd => *left && *right,
                mir::BinaryOperator::LogicalOr => *left || *right,
                _ => return None,
            };
            Some(mir::Constant::Boolean(value))
        }
        (mir::Constant::Character(left), mir::Constant::Character(right)) => {
            fold_ordering(operator, left, right).map(mir::Constant::Boolean)
        }
        _ => None,
    }
}

fn fold_integer_binary(
    type_: &mir::Type,
    operator: mir::BinaryOperator,
    left: &str,
    right: &str,
) -> Option<mir::Constant> {
    macro_rules! integer {
        ($integer:ty) => {{
            let left = left.parse::<$integer>().ok()?;
            let right = right.parse::<$integer>().ok()?;
            match operator {
                mir::BinaryOperator::Multiply => {
                    Some(mir::Constant::Integer(left.wrapping_mul(right).to_string()))
                }
                mir::BinaryOperator::Add => {
                    Some(mir::Constant::Integer(left.wrapping_add(right).to_string()))
                }
                mir::BinaryOperator::Subtract => {
                    Some(mir::Constant::Integer(left.wrapping_sub(right).to_string()))
                }
                mir::BinaryOperator::Less => Some(mir::Constant::Boolean(left < right)),
                mir::BinaryOperator::LessEqual => Some(mir::Constant::Boolean(left <= right)),
                mir::BinaryOperator::Greater => Some(mir::Constant::Boolean(left > right)),
                mir::BinaryOperator::GreaterEqual => Some(mir::Constant::Boolean(left >= right)),
                mir::BinaryOperator::Equal => Some(mir::Constant::Boolean(left == right)),
                mir::BinaryOperator::NotEqual => Some(mir::Constant::Boolean(left != right)),
                mir::BinaryOperator::Divide
                | mir::BinaryOperator::Remainder
                | mir::BinaryOperator::LogicalAnd
                | mir::BinaryOperator::LogicalOr => None,
            }
        }};
    }
    match type_ {
        mir::Type::SByte => integer!(i8),
        mir::Type::Byte => integer!(u8),
        mir::Type::Short => integer!(i16),
        mir::Type::UShort => integer!(u16),
        mir::Type::Int => integer!(i32),
        mir::Type::UInt => integer!(u32),
        mir::Type::Long => integer!(i64),
        mir::Type::ULong => integer!(u64),
        _ => None,
    }
}

#[allow(clippy::float_cmp)] // Exact IEEE comparisons are the ASTER operator semantics.
fn fold_float_binary(
    type_: &mir::Type,
    operator: mir::BinaryOperator,
    left: &str,
    right: &str,
) -> Option<mir::Constant> {
    macro_rules! float {
        ($float:ty) => {{
            let left = left.parse::<$float>().ok()?;
            let right = right.parse::<$float>().ok()?;
            match operator {
                mir::BinaryOperator::Multiply => {
                    Some(mir::Constant::Float((left * right).to_string()))
                }
                mir::BinaryOperator::Divide => {
                    Some(mir::Constant::Float((left / right).to_string()))
                }
                mir::BinaryOperator::Add => Some(mir::Constant::Float((left + right).to_string())),
                mir::BinaryOperator::Subtract => {
                    Some(mir::Constant::Float((left - right).to_string()))
                }
                mir::BinaryOperator::Less => Some(mir::Constant::Boolean(left < right)),
                mir::BinaryOperator::LessEqual => Some(mir::Constant::Boolean(left <= right)),
                mir::BinaryOperator::Greater => Some(mir::Constant::Boolean(left > right)),
                mir::BinaryOperator::GreaterEqual => Some(mir::Constant::Boolean(left >= right)),
                mir::BinaryOperator::Equal => Some(mir::Constant::Boolean(left == right)),
                mir::BinaryOperator::NotEqual => Some(mir::Constant::Boolean(left != right)),
                mir::BinaryOperator::Remainder
                | mir::BinaryOperator::LogicalAnd
                | mir::BinaryOperator::LogicalOr => None,
            }
        }};
    }
    match type_ {
        mir::Type::Float => float!(f32),
        mir::Type::Double => float!(f64),
        _ => None,
    }
}

fn fold_ordering<T: PartialOrd + PartialEq>(
    operator: mir::BinaryOperator,
    left: &T,
    right: &T,
) -> Option<bool> {
    Some(match operator {
        mir::BinaryOperator::Less => left < right,
        mir::BinaryOperator::LessEqual => left <= right,
        mir::BinaryOperator::Greater => left > right,
        mir::BinaryOperator::GreaterEqual => left >= right,
        mir::BinaryOperator::Equal => left == right,
        mir::BinaryOperator::NotEqual => left != right,
        _ => return None,
    })
}

fn fold_equality(
    type_: &mir::Type,
    left: &mir::Constant,
    right: &mir::Constant,
    negated: bool,
) -> Option<mir::Constant> {
    let equal = match (left, right) {
        (mir::Constant::Integer(left), mir::Constant::Integer(right)) => {
            let mir::Constant::Boolean(equal) =
                fold_integer_binary(type_, mir::BinaryOperator::Equal, left, right)?
            else {
                return None;
            };
            equal
        }
        (mir::Constant::Float(left), mir::Constant::Float(right)) => {
            let mir::Constant::Boolean(equal) =
                fold_float_binary(type_, mir::BinaryOperator::Equal, left, right)?
            else {
                return None;
            };
            equal
        }
        (mir::Constant::Boolean(left), mir::Constant::Boolean(right)) => left == right,
        (mir::Constant::Character(left), mir::Constant::Character(right)) => left == right,
        (mir::Constant::String(left), mir::Constant::String(right)) => left == right,
        _ => return None,
    };
    Some(mir::Constant::Boolean(equal ^ negated))
}

fn eliminate_dead_assignments(function: &mut mir::Function) -> bool {
    let liveness = lifetime_analysis::reference_liveness(function);
    let mut changed = false;
    for block in &mut function.blocks {
        let original = std::mem::take(&mut block.instructions);
        block.instructions = original
            .into_iter()
            .enumerate()
            .filter_map(|(instruction_index, instruction)| {
                let removable = match &instruction {
                    mir::Instruction::Assign {
                        target: mir::Place::Local(destination),
                        value,
                    } if pure_non_failing_rvalue(value) => liveness
                        .local_is_live_after(block.id, instruction_index, *destination)
                        .is_some_and(|live| !live),
                    _ => false,
                };
                if removable {
                    changed = true;
                    None
                } else {
                    Some(instruction)
                }
            })
            .collect();
    }
    changed
}

fn simplify_cfg(function: &mut mir::Function) -> bool {
    let trampolines = function
        .blocks
        .iter()
        .filter_map(|block| {
            if block.instructions.is_empty()
                && let mir::Terminator::Goto(target) = block.terminator
            {
                return Some((block.id, target));
            }
            None
        })
        .collect::<HashMap<_, _>>();
    let forwarded = forwarded_targets(&trampolines);
    let mut changed = false;

    for block in &mut function.blocks {
        match &mut block.terminator {
            mir::Terminator::Goto(target) => {
                let next = forwarded.get(target).copied().unwrap_or(*target);
                changed |= next != *target;
                *target = next;
            }
            mir::Terminator::Branch {
                then_block,
                else_block,
                ..
            } => {
                let forwarded_then = forwarded.get(then_block).copied().unwrap_or(*then_block);
                let forwarded_else = forwarded.get(else_block).copied().unwrap_or(*else_block);
                changed |= forwarded_then != *then_block || forwarded_else != *else_block;
                *then_block = forwarded_then;
                *else_block = forwarded_else;
                if *then_block == *else_block {
                    block.terminator = mir::Terminator::Goto(*then_block);
                    changed = true;
                }
            }
            mir::Terminator::Return(_) | mir::Terminator::End | mir::Terminator::Unreachable => {}
        }
    }

    let reachable = reachable_blocks(function);
    let before = function.blocks.len();
    function
        .blocks
        .retain(|block| reachable.contains(&block.id));
    changed | (before != function.blocks.len())
}

fn forwarded_targets(
    trampolines: &HashMap<mir::BasicBlockId, mir::BasicBlockId>,
) -> HashMap<mir::BasicBlockId, mir::BasicBlockId> {
    let mut resolved = HashMap::<mir::BasicBlockId, Option<mir::BasicBlockId>>::new();
    for start in trampolines.keys().copied() {
        if resolved.contains_key(&start) {
            continue;
        }
        let mut path = Vec::new();
        let mut positions = HashMap::new();
        let mut current = start;
        let terminal = loop {
            if let Some(known) = resolved.get(&current).copied() {
                break known;
            }
            if positions.insert(current, path.len()).is_some() {
                break None;
            }
            path.push(current);
            let Some(next) = trampolines.get(&current).copied() else {
                break Some(current);
            };
            current = next;
        };
        for block in path {
            resolved.insert(block, terminal);
        }
    }
    resolved
        .into_iter()
        .filter_map(|(block, target)| target.map(|target| (block, target)))
        .collect()
}

fn reachable_blocks(function: &mir::Function) -> HashSet<mir::BasicBlockId> {
    let blocks = function
        .blocks
        .iter()
        .map(|block| (block.id, &block.terminator))
        .collect::<HashMap<_, _>>();
    let mut reachable = HashSet::new();
    let mut pending = VecDeque::from([function.entry]);
    while let Some(block) = pending.pop_front() {
        if !reachable.insert(block) {
            continue;
        }
        if let Some(terminator) = blocks.get(&block) {
            pending.extend(successors(terminator));
        }
    }
    reachable
}

fn successors(terminator: &mir::Terminator) -> Vec<mir::BasicBlockId> {
    match terminator {
        mir::Terminator::Goto(target) => vec![*target],
        mir::Terminator::Branch {
            then_block,
            else_block,
            ..
        } => vec![*then_block, *else_block],
        mir::Terminator::Return(_) | mir::Terminator::End | mir::Terminator::Unreachable => {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bool_operand(local: mir::LocalId) -> mir::Operand {
        mir::Operand {
            type_: mir::Type::Bool,
            kind: mir::OperandKind::Copy(mir::Place::Local(local)),
        }
    }

    fn branch_fact_function(successor_uses_condition: bool) -> mir::Function {
        let condition = mir::LocalId(0);
        let observed = mir::LocalId(1);
        mir::Function {
            constructor: false,
            symbol: mir::SymbolId(1),
            owner: None,
            name: "BranchFacts".to_owned(),
            visibility: mir::Visibility::Public,
            parameters: Vec::new(),
            locals: vec![
                mir::Local {
                    id: condition,
                    symbol: None,
                    name: "condition".to_owned(),
                    type_: mir::Type::Bool,
                    mutable: true,
                    temporary: true,
                },
                mir::Local {
                    id: observed,
                    symbol: None,
                    name: "observed".to_owned(),
                    type_: mir::Type::Bool,
                    mutable: true,
                    temporary: true,
                },
            ],
            return_type: mir::Type::Void,
            entry: mir::BasicBlockId(0),
            blocks: vec![
                mir::BasicBlock {
                    id: mir::BasicBlockId(0),
                    instructions: vec![mir::Instruction::Assign {
                        target: mir::Place::Local(condition),
                        value: mir::Rvalue {
                            type_: mir::Type::Bool,
                            kind: mir::RvalueKind::Use(mir::Operand {
                                type_: mir::Type::Bool,
                                kind: mir::OperandKind::Constant(mir::Constant::Boolean(true)),
                            }),
                        },
                    }],
                    terminator: mir::Terminator::Branch {
                        condition: bool_operand(condition),
                        then_block: mir::BasicBlockId(1),
                        else_block: mir::BasicBlockId(2),
                    },
                },
                mir::BasicBlock {
                    id: mir::BasicBlockId(1),
                    instructions: if successor_uses_condition {
                        vec![mir::Instruction::Assign {
                            target: mir::Place::Local(observed),
                            value: mir::Rvalue {
                                type_: mir::Type::Bool,
                                kind: mir::RvalueKind::Use(bool_operand(condition)),
                            },
                        }]
                    } else {
                        Vec::new()
                    },
                    terminator: mir::Terminator::Return(None),
                },
                mir::BasicBlock {
                    id: mir::BasicBlockId(2),
                    instructions: Vec::new(),
                    terminator: mir::Terminator::Return(None),
                },
            ],
            temporary_subregion_candidates: Vec::new(),
        }
    }

    fn int_fact(value: &str) -> ConstantFact {
        ConstantFact::Constant(mir::Operand {
            type_: mir::Type::Int,
            kind: mir::OperandKind::Constant(mir::Constant::Integer(value.to_owned())),
        })
    }

    #[test]
    fn sparse_constant_meet_keeps_only_identical_facts() {
        let shared = mir::LocalId(1);
        let conflict = mir::LocalId(2);
        let one_sided = mir::LocalId(3);
        let left = HashMap::from([
            (shared, int_fact("7")),
            (conflict, int_fact("8")),
            (one_sided, int_fact("9")),
        ]);
        let right = HashMap::from([(shared, int_fact("7")), (conflict, int_fact("10"))]);

        assert_eq!(
            meet_facts(&left, &right),
            HashMap::from([(shared, int_fact("7"))])
        );
        assert!(meet_facts(&HashMap::new(), &right).is_empty());
    }

    #[test]
    fn switch_condition_fact_is_removed_only_when_the_branch_is_its_last_use() {
        let condition = mir::LocalId(0);
        let single_use = branch_fact_function(false);
        assert_eq!(
            direct_local_use_counts(&single_use).get(&condition),
            Some(&1)
        );
        assert!(
            constant_entry_facts(&single_use)[1]
                .as_ref()
                .is_some_and(|facts| !facts.contains_key(&condition))
        );

        let successor_use = branch_fact_function(true);
        assert_eq!(
            direct_local_use_counts(&successor_use).get(&condition),
            Some(&2)
        );
        assert_eq!(
            constant_entry_facts(&successor_use)[1]
                .as_ref()
                .and_then(|facts| facts.get(&condition)),
            Some(&ConstantFact::Constant(mir::Operand {
                type_: mir::Type::Bool,
                kind: mir::OperandKind::Constant(mir::Constant::Boolean(true)),
            }))
        );
    }

    #[test]
    fn optimizer_is_deterministic_and_idempotent() {
        let original = branch_fact_function(true);
        let mut left = original.clone();
        let mut right = original;
        optimize_function(&mut left);
        optimize_function(&mut right);
        assert_eq!(left, right);

        let once = left.clone();
        optimize_function(&mut left);
        assert_eq!(left, once);
    }

    #[test]
    fn trampoline_forwarding_compresses_long_chains_and_declines_cycles() {
        let block = mir::BasicBlockId;
        let chain = HashMap::from([
            (block(1), block(2)),
            (block(2), block(3)),
            (block(3), block(4)),
        ]);
        let forwarded = forwarded_targets(&chain);
        assert_eq!(forwarded.get(&block(1)), Some(&block(4)));
        assert_eq!(forwarded.get(&block(2)), Some(&block(4)));
        assert_eq!(forwarded.get(&block(3)), Some(&block(4)));

        for cycle in [
            HashMap::from([(block(1), block(1))]),
            HashMap::from([(block(1), block(2)), (block(2), block(1))]),
            HashMap::from([
                (block(0), block(1)),
                (block(1), block(2)),
                (block(2), block(1)),
            ]),
        ] {
            assert!(forwarded_targets(&cycle).is_empty());
        }
    }

    fn integer_value(
        type_: &mir::Type,
        operator: mir::BinaryOperator,
        left: &str,
        right: &str,
    ) -> String {
        let Some(mir::Constant::Integer(value)) = fold_integer_binary(type_, operator, left, right)
        else {
            panic!("integer operation must fold")
        };
        value
    }

    fn boolean_value(
        type_: &mir::Type,
        operator: mir::BinaryOperator,
        left: &str,
        right: &str,
    ) -> bool {
        let Some(mir::Constant::Boolean(value)) = fold_integer_binary(type_, operator, left, right)
        else {
            panic!("integer comparison must fold")
        };
        value
    }

    #[test]
    fn integer_folding_uses_each_concrete_width_and_signedness() {
        for (type_, minimum, maximum) in [
            (mir::Type::SByte, "-128", "127"),
            (mir::Type::Short, "-32768", "32767"),
            (mir::Type::Int, "-2147483648", "2147483647"),
            (
                mir::Type::Long,
                "-9223372036854775808",
                "9223372036854775807",
            ),
        ] {
            assert_eq!(
                integer_value(&type_, mir::BinaryOperator::Add, maximum, "1"),
                minimum
            );
            assert_eq!(
                integer_value(&type_, mir::BinaryOperator::Subtract, minimum, "1"),
                maximum
            );
            assert_eq!(
                integer_value(&type_, mir::BinaryOperator::Multiply, maximum, "2"),
                "-2"
            );
            assert_eq!(
                fold_integer_negate(&type_, minimum),
                Some(mir::Constant::Integer(minimum.to_owned()))
            );
            assert!(boolean_value(&type_, mir::BinaryOperator::Less, "-1", "1"));
        }

        for (type_, maximum, doubled) in [
            (mir::Type::Byte, "255", "254"),
            (mir::Type::UShort, "65535", "65534"),
            (mir::Type::UInt, "4294967295", "4294967294"),
            (
                mir::Type::ULong,
                "18446744073709551615",
                "18446744073709551614",
            ),
        ] {
            assert_eq!(
                integer_value(&type_, mir::BinaryOperator::Add, maximum, "1"),
                "0"
            );
            assert_eq!(
                integer_value(&type_, mir::BinaryOperator::Subtract, "0", "1"),
                maximum
            );
            assert_eq!(
                integer_value(&type_, mir::BinaryOperator::Multiply, maximum, "2"),
                doubled
            );
            assert!(!boolean_value(
                &type_,
                mir::BinaryOperator::Less,
                maximum,
                "1"
            ));
        }

        for type_ in [
            mir::Type::SByte,
            mir::Type::Byte,
            mir::Type::Short,
            mir::Type::UShort,
            mir::Type::Int,
            mir::Type::UInt,
            mir::Type::Long,
            mir::Type::ULong,
        ] {
            assert_eq!(
                fold_integer_binary(&type_, mir::BinaryOperator::Divide, "1", "0"),
                None
            );
            assert_eq!(
                fold_integer_binary(&type_, mir::BinaryOperator::Remainder, "1", "0"),
                None
            );
        }
    }

    #[test]
    fn float_folding_preserves_ieee_edge_values() {
        assert_eq!(
            fold_unary(
                &mir::Type::Double,
                mir::UnaryOperator::Negate,
                &mir::Constant::Float("0".to_owned())
            ),
            Some(mir::Constant::Float("-0".to_owned()))
        );
        assert_eq!(
            fold_float_binary(&mir::Type::Double, mir::BinaryOperator::Equal, "NaN", "NaN"),
            Some(mir::Constant::Boolean(false))
        );
        assert_eq!(
            fold_float_binary(&mir::Type::Double, mir::BinaryOperator::Equal, "-0", "0"),
            Some(mir::Constant::Boolean(true))
        );
        assert_eq!(
            fold_float_binary(&mir::Type::Double, mir::BinaryOperator::Divide, "1", "-0"),
            Some(mir::Constant::Float("-inf".to_owned()))
        );
        assert_eq!(
            fold_float_binary(&mir::Type::Double, mir::BinaryOperator::Add, "inf", "-inf"),
            Some(mir::Constant::Float("NaN".to_owned()))
        );

        let subnormal = f32::from_bits(1).to_string();
        assert_eq!(
            fold_float_binary(&mir::Type::Float, mir::BinaryOperator::Add, &subnormal, "0"),
            Some(mir::Constant::Float(subnormal))
        );
    }

    #[test]
    fn character_folding_only_compares_existing_unicode_scalars() {
        assert_eq!(
            fold_binary(
                &mir::Type::Char,
                mir::BinaryOperator::Greater,
                &mir::Constant::Character('\u{10ffff}'),
                &mir::Constant::Character('A')
            ),
            Some(mir::Constant::Boolean(true))
        );
    }

    #[test]
    fn runtime_intrinsic_arguments_are_opaque_fact_barriers() {
        let local = mir::LocalId(0);
        let facts = HashMap::from([(
            local,
            ConstantFact::Constant(mir::Operand {
                type_: mir::Type::Int,
                kind: mir::OperandKind::Constant(mir::Constant::Integer("7".to_owned())),
            }),
        )]);
        let copies = HashMap::new();

        for intrinsic in [
            mir::Intrinsic::Log,
            mir::Intrinsic::TaskRun,
            mir::Intrinsic::TaskWait,
            mir::Intrinsic::TaskWaitAll,
            mir::Intrinsic::TaskCancel,
            mir::Intrinsic::TaskCancellationRequested,
            mir::Intrinsic::AsyncSpawn,
            mir::Intrinsic::AsyncState,
            mir::Intrinsic::AsyncSetState,
            mir::Intrinsic::AsyncStoreSlot,
            mir::Intrinsic::AsyncLoadSlot,
            mir::Intrinsic::AsyncSpawnInner,
            mir::Intrinsic::AsyncAwaitResult,
            mir::Intrinsic::AsyncSetResult,
            mir::Intrinsic::ParallelFor,
            mir::Intrinsic::ParallelForEach,
            mir::Intrinsic::ParallelReduce,
            mir::Intrinsic::StringTrim,
            mir::Intrinsic::StringReplace,
            mir::Intrinsic::StringSplit,
            mir::Intrinsic::MathUnaryFloat,
            mir::Intrinsic::MathUnaryDouble,
            mir::Intrinsic::MathPowFloat,
            mir::Intrinsic::MathPowDouble,
        ] {
            let mut instruction = mir::Instruction::CallIntrinsic {
                destination: None,
                intrinsic,
                arguments: vec![mir::Operand {
                    type_: mir::Type::Int,
                    kind: mir::OperandKind::Copy(mir::Place::Local(local)),
                }],
                return_type: mir::Type::Void,
            };
            assert!(!rewrite_instruction_operands(
                &mut instruction,
                &facts,
                &copies
            ));
            let mir::Instruction::CallIntrinsic { arguments, .. } = instruction else {
                unreachable!()
            };
            assert!(matches!(
                arguments[0].kind,
                mir::OperandKind::Copy(mir::Place::Local(id)) if id == local
            ));
        }
    }

    #[test]
    fn standard_library_collection_operations_are_opaque_fact_barriers() {
        let scalar = mir::LocalId(0);
        let list = mir::LocalId(1);
        let facts = HashMap::from([(
            scalar,
            ConstantFact::Constant(mir::Operand {
                type_: mir::Type::Int,
                kind: mir::OperandKind::Constant(mir::Constant::Integer("7".to_owned())),
            }),
        )]);
        let mut instruction = mir::Instruction::ListSet {
            list: mir::Operand {
                type_: mir::Type::List(Box::new(mir::Type::Int)),
                kind: mir::OperandKind::Copy(mir::Place::Local(list)),
            },
            index: mir::Operand {
                type_: mir::Type::Int,
                kind: mir::OperandKind::Copy(mir::Place::Local(scalar)),
            },
            value: mir::Operand {
                type_: mir::Type::Int,
                kind: mir::OperandKind::Copy(mir::Place::Local(scalar)),
            },
        };
        assert!(!rewrite_instruction_operands(
            &mut instruction,
            &facts,
            &HashMap::new()
        ));
        let mir::Instruction::ListSet { index, value, .. } = instruction else {
            unreachable!()
        };
        assert!(matches!(
            index.kind,
            mir::OperandKind::Copy(mir::Place::Local(id)) if id == scalar
        ));
        assert!(matches!(
            value.kind,
            mir::OperandKind::Copy(mir::Place::Local(id)) if id == scalar
        ));
    }
}
