//! Narrow scalar replacement for non-escaping, direct-local class objects.
//!
//! Escape analysis remains the lifetime authority: this pass considers only
//! object allocations already classified as `Temporary`. It then requires a
//! deliberately smaller representation-safe shape before replacing the
//! zeroed object fields with ordinary typed MIR locals.

use std::collections::{HashMap, HashSet};

use aster_mir as mir;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct InstructionPoint {
    block: mir::BasicBlockId,
    index: usize,
}

#[derive(Clone, Debug)]
struct Candidate {
    allocation: InstructionPoint,
    constructor: InstructionPoint,
    alias: Option<InstructionPoint>,
    origin: mir::LocalId,
    object: mir::LocalId,
    fields: Vec<mir::FieldDefinition>,
    initializations: Vec<FieldInitialization>,
}

#[derive(Clone, Debug)]
struct ConstructorPlan {
    class: mir::SymbolId,
    parameter_types: Vec<mir::Type>,
    initializations: Vec<ConstructorInitialization>,
}

#[derive(Clone, Debug)]
struct ConstructorInitialization {
    field: mir::SymbolId,
    source: InitializationSource,
}

#[derive(Clone, Debug)]
enum InitializationSource {
    Argument(usize),
    Constant(mir::Operand),
}

#[derive(Clone, Debug)]
struct FieldInitialization {
    field: mir::SymbolId,
    value: mir::Operand,
}

/// Replace the narrowest proven-unobservable object representation with
/// scalar MIR locals. This runs after escape-region assignment, so it never
/// independently decides whether an object escapes.
pub(super) fn eliminate(module: &mut mir::Module) {
    let scalar_classes = module
        .classes
        .iter()
        .filter(|class| class.fields.iter().all(|field| is_scalar(&field.type_)))
        .map(|class| (class.symbol, class.fields.clone()))
        .collect::<HashMap<_, _>>();
    let constructors = module
        .functions
        .iter()
        .filter_map(|function| {
            analyze_constructor(function, &scalar_classes)
                .map(|constructor| (function.symbol, constructor))
        })
        .collect::<HashMap<_, _>>();

    for function in &mut module.functions {
        let candidates = discover_candidates(function, &scalar_classes, &constructors)
            .into_iter()
            .filter(|candidate| candidate_uses_are_scalarizable(function, candidate))
            .collect::<Vec<_>>();
        if !candidates.is_empty() {
            scalarize(function, &candidates);
        }
    }
}

fn is_scalar(type_: &mir::Type) -> bool {
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

fn analyze_constructor(
    function: &mir::Function,
    scalar_classes: &HashMap<mir::SymbolId, Vec<mir::FieldDefinition>>,
) -> Option<ConstructorPlan> {
    let class = function.owner?;
    let fields = scalar_classes.get(&class)?;
    let receiver = function.parameters.first()?;
    if !function.constructor
        || receiver.type_ != mir::Type::Class(class)
        || !function.locals.is_empty()
        || function.blocks.len() != 1
        || function.blocks[0].id != function.entry
        || !matches!(
            function.blocks[0].terminator,
            mir::Terminator::End | mir::Terminator::Return(None)
        )
    {
        return None;
    }

    let block = &function.blocks[0];
    if function.parameters.len() == 1 {
        return block.instructions.is_empty().then(|| ConstructorPlan {
            class,
            parameter_types: vec![receiver.type_.clone()],
            initializations: Vec::new(),
        });
    }
    if function.parameters[1..]
        .iter()
        .any(|parameter| !is_scalar(&parameter.type_))
    {
        return None;
    }

    let parameter_indexes = function
        .parameters
        .iter()
        .enumerate()
        .map(|(index, parameter)| (parameter.id, index))
        .collect::<HashMap<_, _>>();
    let mut assigned_fields = HashSet::new();
    let mut initializations = Vec::with_capacity(block.instructions.len());
    for instruction in &block.instructions {
        let mir::Instruction::Assign {
            target: mir::Place::ObjectField { object, field },
            value:
                mir::Rvalue {
                    type_: value_type,
                    kind: mir::RvalueKind::Use(value),
                },
        } = instruction
        else {
            return None;
        };
        if direct_local(object) != Some(receiver.id) || !assigned_fields.insert(*field) {
            return None;
        }
        let field_definition = fields.iter().find(|candidate| candidate.symbol == *field)?;
        if *value_type != field_definition.type_ || value.type_ != field_definition.type_ {
            return None;
        }
        let source = match &value.kind {
            mir::OperandKind::Copy(mir::Place::Local(local)) => {
                let index = *parameter_indexes.get(local)?;
                if index == 0 || function.parameters[index].type_ != field_definition.type_ {
                    return None;
                }
                InitializationSource::Argument(index)
            }
            mir::OperandKind::Constant(_) => InitializationSource::Constant(value.clone()),
            mir::OperandKind::Copy(_) | mir::OperandKind::Function(_) => return None,
        };
        initializations.push(ConstructorInitialization {
            field: *field,
            source,
        });
    }

    Some(ConstructorPlan {
        class,
        parameter_types: function
            .parameters
            .iter()
            .map(|parameter| parameter.type_.clone())
            .collect(),
        initializations,
    })
}

fn discover_candidates(
    function: &mir::Function,
    scalar_classes: &HashMap<mir::SymbolId, Vec<mir::FieldDefinition>>,
    constructors: &HashMap<mir::SymbolId, ConstructorPlan>,
) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    for block in &function.blocks {
        for (index, instruction) in block.instructions.iter().enumerate() {
            let mir::Instruction::AllocateObject {
                destination: mir::Place::Local(origin),
                class,
                region: mir::AllocationRegion::Temporary,
            } = instruction
            else {
                continue;
            };
            let Some(fields) = scalar_classes.get(class) else {
                continue;
            };
            let Some(mir::Instruction::Call {
                destination: None,
                function: constructor,
                arguments,
                return_type: mir::Type::Void,
            }) = block.instructions.get(index + 1)
            else {
                continue;
            };
            let Some(constructor) = constructors.get(constructor) else {
                continue;
            };
            let Some(initializations) =
                instantiate_constructor(constructor, arguments.as_slice(), *origin, *class)
            else {
                continue;
            };

            let (object, alias) = match block.instructions.get(index + 2) {
                Some(mir::Instruction::Assign {
                    target: mir::Place::Local(destination),
                    value:
                        mir::Rvalue {
                            type_: mir::Type::Class(alias_class),
                            kind: mir::RvalueKind::Use(source),
                        },
                }) if *alias_class == *class && direct_local(source) == Some(*origin) => (
                    *destination,
                    Some(InstructionPoint {
                        block: block.id,
                        index: index + 2,
                    }),
                ),
                _ => (*origin, None),
            };

            candidates.push(Candidate {
                allocation: InstructionPoint {
                    block: block.id,
                    index,
                },
                constructor: InstructionPoint {
                    block: block.id,
                    index: index + 1,
                },
                alias,
                origin: *origin,
                object,
                fields: fields.clone(),
                initializations,
            });
        }
    }
    candidates
}

fn instantiate_constructor(
    constructor: &ConstructorPlan,
    arguments: &[mir::Operand],
    local: mir::LocalId,
    class: mir::SymbolId,
) -> Option<Vec<FieldInitialization>> {
    if constructor.class != class
        || arguments.len() != constructor.parameter_types.len()
        || arguments
            .iter()
            .zip(&constructor.parameter_types)
            .any(|(argument, parameter)| argument.type_ != *parameter)
        || arguments.first().and_then(direct_local) != Some(local)
    {
        return None;
    }
    constructor
        .initializations
        .iter()
        .map(|initialization| {
            let value = match &initialization.source {
                InitializationSource::Argument(index) => arguments.get(*index)?.clone(),
                InitializationSource::Constant(value) => value.clone(),
            };
            Some(FieldInitialization {
                field: initialization.field,
                value,
            })
        })
        .collect()
}

fn direct_local(operand: &mir::Operand) -> Option<mir::LocalId> {
    let mir::OperandKind::Copy(mir::Place::Local(local)) = operand.kind else {
        return None;
    };
    Some(local)
}

fn candidate_uses_are_scalarizable(function: &mir::Function, candidate: &Candidate) -> bool {
    if candidate
        .initializations
        .iter()
        .any(|initialization| !operand_is_legal(&initialization.value, candidate))
    {
        return false;
    }
    for block in &function.blocks {
        for (index, instruction) in block.instructions.iter().enumerate() {
            let point = InstructionPoint {
                block: block.id,
                index,
            };
            if point == candidate.allocation
                || point == candidate.constructor
                || candidate.alias == Some(point)
            {
                continue;
            }
            if !instruction_is_legal(instruction, candidate) {
                return false;
            }
        }
        if !terminator_is_legal(&block.terminator, candidate) {
            return false;
        }
    }
    true
}

#[allow(clippy::too_many_lines)]
fn instruction_is_legal(instruction: &mir::Instruction, candidate: &Candidate) -> bool {
    match instruction {
        mir::Instruction::Assign { target, value } => {
            place_is_legal(target, candidate) && rvalue_is_legal(value, candidate)
        }
        mir::Instruction::Call {
            destination,
            arguments,
            ..
        }
        | mir::Instruction::CallIntrinsic {
            destination,
            arguments,
            ..
        } => {
            destination
                .as_ref()
                .is_none_or(|place| place_is_legal(place, candidate))
                && arguments
                    .iter()
                    .all(|operand| operand_is_legal(operand, candidate))
        }
        mir::Instruction::ForeignCall { .. } => false,
        mir::Instruction::CallInterface {
            destination,
            receiver,
            arguments,
            ..
        } => {
            destination
                .as_ref()
                .is_none_or(|place| place_is_legal(place, candidate))
                && operand_is_legal(receiver, candidate)
                && arguments
                    .iter()
                    .all(|operand| operand_is_legal(operand, candidate))
        }
        mir::Instruction::AllocateArray {
            destination,
            length,
            ..
        } => place_is_legal(destination, candidate) && operand_is_legal(length, candidate),
        mir::Instruction::AllocateObject { destination, .. }
        | mir::Instruction::AllocateList { destination, .. }
        | mir::Instruction::AllocateDictionary { destination, .. }
        | mir::Instruction::AllocateStringBuilder { destination, .. } => {
            place_is_legal(destination, candidate)
        }
        mir::Instruction::StringBuilderAppend { builder, value, .. } => {
            operand_is_legal(builder, candidate) && operand_is_legal(value, candidate)
        }
        mir::Instruction::StringBuilderToString {
            destination,
            builder,
            ..
        } => place_is_legal(destination, candidate) && operand_is_legal(builder, candidate),
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
            place_is_legal(destination, candidate)
                && operand_is_legal(dictionary, candidate)
                && operand_is_legal(key, candidate)
                && operand_is_legal(value, candidate)
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
            place_is_legal(destination, candidate)
                && operand_is_legal(dictionary, candidate)
                && operand_is_legal(key, candidate)
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
        } => place_is_legal(destination, candidate) && operand_is_legal(dictionary, candidate),
        mir::Instruction::DictionaryClear { dictionary } => operand_is_legal(dictionary, candidate),
        mir::Instruction::ListAdd { list, value } => {
            operand_is_legal(list, candidate) && operand_is_legal(value, candidate)
        }
        mir::Instruction::ListGet {
            destination,
            list,
            index,
            ..
        } => {
            place_is_legal(destination, candidate)
                && operand_is_legal(list, candidate)
                && operand_is_legal(index, candidate)
        }
        mir::Instruction::ListRemoveAt { list, index } => {
            operand_is_legal(list, candidate) && operand_is_legal(index, candidate)
        }
        mir::Instruction::ListSet { list, index, value } => {
            operand_is_legal(list, candidate)
                && operand_is_legal(index, candidate)
                && operand_is_legal(value, candidate)
        }
        mir::Instruction::ListClear { list } => operand_is_legal(list, candidate),
        mir::Instruction::ListToArray {
            destination, list, ..
        } => place_is_legal(destination, candidate) && operand_is_legal(list, candidate),
        mir::Instruction::StringDecodeNext {
            string,
            cursor,
            char_destination,
            next_cursor_destination,
            ok_destination,
        } => {
            operand_is_legal(string, candidate)
                && operand_is_legal(cursor, candidate)
                && place_is_legal(char_destination, candidate)
                && place_is_legal(next_cursor_destination, candidate)
                && place_is_legal(ok_destination, candidate)
        }
        mir::Instruction::TemporarySubregionEnter { .. }
        | mir::Instruction::TemporarySubregionExit { .. }
        | mir::Instruction::OwnedRegionEnter { .. }
        | mir::Instruction::OwnedRegionExit { .. } => true,
    }
}

fn terminator_is_legal(terminator: &mir::Terminator, candidate: &Candidate) -> bool {
    match terminator {
        mir::Terminator::Branch { condition, .. } => operand_is_legal(condition, candidate),
        mir::Terminator::Return(Some(operand)) => operand_is_legal(operand, candidate),
        mir::Terminator::Goto(_)
        | mir::Terminator::Return(None)
        | mir::Terminator::End
        | mir::Terminator::Unreachable => true,
    }
}

fn rvalue_is_legal(value: &mir::Rvalue, candidate: &Candidate) -> bool {
    match &value.kind {
        mir::RvalueKind::Use(operand)
        | mir::RvalueKind::Discriminant(operand)
        | mir::RvalueKind::ArrayLength(operand)
        | mir::RvalueKind::ListLength(operand)
        | mir::RvalueKind::DictionaryLength(operand)
        | mir::RvalueKind::ListVersion(operand)
        | mir::RvalueKind::StringByteLength(operand)
        | mir::RvalueKind::Cast(operand)
        | mir::RvalueKind::Unary { operand, .. } => operand_is_legal(operand, candidate),
        mir::RvalueKind::Aggregate(fields) | mir::RvalueKind::EnumConstruct { fields, .. } => {
            fields
                .iter()
                .all(|field| operand_is_legal(&field.value, candidate))
        }
        mir::RvalueKind::MakeInterface { object, .. } => operand_is_legal(object, candidate),
        mir::RvalueKind::Binary { left, right, .. }
        | mir::RvalueKind::Equality { left, right, .. } => {
            operand_is_legal(left, candidate) && operand_is_legal(right, candidate)
        }
    }
}

fn operand_is_legal(operand: &mir::Operand, candidate: &Candidate) -> bool {
    match &operand.kind {
        mir::OperandKind::Copy(place) => place_is_legal(place, candidate),
        mir::OperandKind::Constant(_) | mir::OperandKind::Function(_) => true,
    }
}

fn place_is_legal(place: &mir::Place, candidate: &Candidate) -> bool {
    match place {
        mir::Place::Local(local) => *local != candidate.origin && *local != candidate.object,
        mir::Place::Symbol(_) => true,
        mir::Place::Field { base, .. } | mir::Place::EnumField { base, .. } => {
            place_is_legal(base, candidate)
        }
        mir::Place::Index { array, index, .. } => {
            operand_is_legal(array, candidate) && operand_is_legal(index, candidate)
        }
        mir::Place::ObjectField { object, field }
            if direct_local(object) == Some(candidate.object) =>
        {
            candidate
                .fields
                .iter()
                .any(|candidate| candidate.symbol == *field)
        }
        mir::Place::ObjectField { object, .. } => operand_is_legal(object, candidate),
    }
}

fn scalarize(function: &mut mir::Function, candidates: &[Candidate]) {
    let mut next_local = function
        .parameters
        .iter()
        .chain(&function.locals)
        .map(|local| local.id.0)
        .max()
        .map_or(0, |id| {
            id.checked_add(1).expect("MIR local id space exhausted")
        });
    let mut field_locals = HashMap::new();

    for candidate in candidates {
        for field in &candidate.fields {
            let local = mir::LocalId(next_local);
            next_local = next_local
                .checked_add(1)
                .expect("MIR local id space exhausted");
            function.locals.push(mir::Local {
                id: local,
                symbol: None,
                name: format!("_scalarized_{}_{}", candidate.object.0, field.name),
                type_: field.type_.clone(),
                mutable: true,
                temporary: true,
            });
            field_locals.insert((candidate.object, field.symbol), local);
        }
    }

    let allocations = candidates
        .iter()
        .map(|candidate| (candidate.allocation, candidate))
        .collect::<HashMap<_, _>>();
    let constructors = candidates
        .iter()
        .map(|candidate| (candidate.constructor, candidate))
        .collect::<HashMap<_, _>>();
    let removed = candidates
        .iter()
        .filter_map(|candidate| candidate.alias)
        .collect::<HashSet<_>>();

    for block in &mut function.blocks {
        let original = std::mem::take(&mut block.instructions);
        let mut rewritten = Vec::with_capacity(original.len());
        for (index, mut instruction) in original.into_iter().enumerate() {
            let point = InstructionPoint {
                block: block.id,
                index,
            };
            if let Some(candidate) = allocations.get(&point) {
                for field in &candidate.fields {
                    rewritten.push(zero_field(
                        field_locals[&(candidate.object, field.symbol)],
                        &field.type_,
                    ));
                }
            } else if let Some(candidate) = constructors.get(&point) {
                for initialization in &candidate.initializations {
                    let mut instruction = mir::Instruction::Assign {
                        target: mir::Place::Local(
                            field_locals[&(candidate.object, initialization.field)],
                        ),
                        value: mir::Rvalue {
                            type_: initialization.value.type_.clone(),
                            kind: mir::RvalueKind::Use(initialization.value.clone()),
                        },
                    };
                    rewrite_instruction(&mut instruction, &field_locals);
                    rewritten.push(instruction);
                }
            } else if !removed.contains(&point) {
                rewrite_instruction(&mut instruction, &field_locals);
                rewritten.push(instruction);
            }
        }
        block.instructions = rewritten;
        rewrite_terminator(&mut block.terminator, &field_locals);
    }
}

fn zero_field(local: mir::LocalId, type_: &mir::Type) -> mir::Instruction {
    let constant = match type_ {
        mir::Type::Bool => mir::Constant::Boolean(false),
        mir::Type::SByte
        | mir::Type::Byte
        | mir::Type::Short
        | mir::Type::UShort
        | mir::Type::Int
        | mir::Type::UInt
        | mir::Type::Long
        | mir::Type::ULong => mir::Constant::Integer("0".to_owned()),
        mir::Type::Float | mir::Type::Double => mir::Constant::Float("0".to_owned()),
        mir::Type::Char => mir::Constant::Character('\0'),
        _ => unreachable!("only scalar class fields are replaced"),
    };
    mir::Instruction::Assign {
        target: mir::Place::Local(local),
        value: mir::Rvalue {
            type_: type_.clone(),
            kind: mir::RvalueKind::Use(mir::Operand {
                type_: type_.clone(),
                kind: mir::OperandKind::Constant(constant),
            }),
        },
    }
}

#[allow(clippy::too_many_lines)]
fn rewrite_instruction(
    instruction: &mut mir::Instruction,
    fields: &HashMap<(mir::LocalId, mir::SymbolId), mir::LocalId>,
) {
    match instruction {
        mir::Instruction::Assign { target, value } => {
            rewrite_place(target, fields);
            rewrite_rvalue(value, fields);
        }
        mir::Instruction::Call {
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
                rewrite_place(destination, fields);
            }
            for operand in arguments {
                rewrite_operand(operand, fields);
            }
        }
        mir::Instruction::CallInterface {
            destination,
            receiver,
            arguments,
            ..
        } => {
            if let Some(destination) = destination {
                rewrite_place(destination, fields);
            }
            rewrite_operand(receiver, fields);
            for operand in arguments {
                rewrite_operand(operand, fields);
            }
        }
        mir::Instruction::AllocateArray {
            destination,
            length,
            ..
        } => {
            rewrite_place(destination, fields);
            rewrite_operand(length, fields);
        }
        mir::Instruction::AllocateObject { destination, .. }
        | mir::Instruction::AllocateList { destination, .. }
        | mir::Instruction::AllocateDictionary { destination, .. }
        | mir::Instruction::AllocateStringBuilder { destination, .. } => {
            rewrite_place(destination, fields);
        }
        mir::Instruction::StringBuilderAppend { builder, value, .. } => {
            rewrite_operand(builder, fields);
            rewrite_operand(value, fields);
        }
        mir::Instruction::StringBuilderToString {
            destination,
            builder,
            ..
        } => {
            rewrite_place(destination, fields);
            rewrite_operand(builder, fields);
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
            rewrite_place(destination, fields);
            rewrite_operand(dictionary, fields);
            rewrite_operand(key, fields);
            rewrite_operand(value, fields);
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
            rewrite_place(destination, fields);
            rewrite_operand(dictionary, fields);
            rewrite_operand(key, fields);
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
            rewrite_place(destination, fields);
            rewrite_operand(dictionary, fields);
        }
        mir::Instruction::DictionaryClear { dictionary } => rewrite_operand(dictionary, fields),
        mir::Instruction::ListAdd { list, value } => {
            rewrite_operand(list, fields);
            rewrite_operand(value, fields);
        }
        mir::Instruction::ListGet {
            destination,
            list,
            index,
            ..
        } => {
            rewrite_place(destination, fields);
            rewrite_operand(list, fields);
            rewrite_operand(index, fields);
        }
        mir::Instruction::ListRemoveAt { list, index } => {
            rewrite_operand(list, fields);
            rewrite_operand(index, fields);
        }
        mir::Instruction::ListSet { list, index, value } => {
            rewrite_operand(list, fields);
            rewrite_operand(index, fields);
            rewrite_operand(value, fields);
        }
        mir::Instruction::ListClear { list } => rewrite_operand(list, fields),
        mir::Instruction::ListToArray {
            destination, list, ..
        } => {
            rewrite_place(destination, fields);
            rewrite_operand(list, fields);
        }
        mir::Instruction::StringDecodeNext {
            string,
            cursor,
            char_destination,
            next_cursor_destination,
            ok_destination,
        } => {
            rewrite_operand(string, fields);
            rewrite_operand(cursor, fields);
            rewrite_place(char_destination, fields);
            rewrite_place(next_cursor_destination, fields);
            rewrite_place(ok_destination, fields);
        }
        mir::Instruction::TemporarySubregionEnter { .. }
        | mir::Instruction::TemporarySubregionExit { .. }
        | mir::Instruction::OwnedRegionEnter { .. }
        | mir::Instruction::OwnedRegionExit { .. }
        | mir::Instruction::ForeignCall { .. } => {}
    }
}

fn rewrite_terminator(
    terminator: &mut mir::Terminator,
    fields: &HashMap<(mir::LocalId, mir::SymbolId), mir::LocalId>,
) {
    match terminator {
        mir::Terminator::Branch { condition, .. } => rewrite_operand(condition, fields),
        mir::Terminator::Return(Some(operand)) => rewrite_operand(operand, fields),
        mir::Terminator::Goto(_)
        | mir::Terminator::Return(None)
        | mir::Terminator::End
        | mir::Terminator::Unreachable => {}
    }
}

fn rewrite_rvalue(
    value: &mut mir::Rvalue,
    fields: &HashMap<(mir::LocalId, mir::SymbolId), mir::LocalId>,
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
        | mir::RvalueKind::Unary { operand, .. } => rewrite_operand(operand, fields),
        mir::RvalueKind::Aggregate(values)
        | mir::RvalueKind::EnumConstruct { fields: values, .. } => {
            for field in values {
                rewrite_operand(&mut field.value, fields);
            }
        }
        mir::RvalueKind::MakeInterface { object, .. } => rewrite_operand(object, fields),
        mir::RvalueKind::Binary { left, right, .. }
        | mir::RvalueKind::Equality { left, right, .. } => {
            rewrite_operand(left, fields);
            rewrite_operand(right, fields);
        }
    }
}

fn rewrite_operand(
    operand: &mut mir::Operand,
    fields: &HashMap<(mir::LocalId, mir::SymbolId), mir::LocalId>,
) {
    if let mir::OperandKind::Copy(place) = &mut operand.kind {
        rewrite_place(place, fields);
    }
}

fn rewrite_place(
    place: &mut mir::Place,
    fields: &HashMap<(mir::LocalId, mir::SymbolId), mir::LocalId>,
) {
    if let mir::Place::ObjectField { object, field } = place {
        if let Some(local) = direct_local(object).and_then(|object| fields.get(&(object, *field))) {
            *place = mir::Place::Local(*local);
            return;
        }
    }
    match place {
        mir::Place::Field { base, .. } | mir::Place::EnumField { base, .. } => {
            rewrite_place(base, fields);
        }
        mir::Place::Index { array, index, .. } => {
            rewrite_operand(array, fields);
            rewrite_operand(index, fields);
        }
        mir::Place::ObjectField { object, .. } => rewrite_operand(object, fields),
        mir::Place::Local(_) | mir::Place::Symbol(_) => {}
    }
}
