//! Backend-neutral checks that region, ownership, and concurrency metadata in
//! MIR is executable: owned Persistent regions, fine Temporary subregions,
//! worker host-operation reachability, nominal construction shapes, and CFG
//! block targets. Moved unchanged from the Cranelift backend (T-ARCH-2c); the
//! backend still calls them, and compiler passes may.

#![allow(
    clippy::implicit_hasher,
    clippy::missing_errors_doc,
    clippy::must_use_candidate,
    reason = "internal validator API shared by the compiler and backends"
)]

use std::collections::{BTreeMap, HashMap, HashSet};

use super::ValidationError;
use crate as mir;

/// Host operations and foreign calls are rejected anywhere reachable from a
/// `Task.Run`/`Parallel.For`/`ForEach`/`Reduce` worker body: output order,
/// input consumption, and file access would be non-deterministic across
/// workers, and neither backend is shared or synchronized. Builds the whole
/// module's direct call graph once (`Instruction::Call` edges), marks every
/// function whose body directly calls a host I/O intrinsic or foreign binding,
/// then, for every worker entry point found anywhere in the module, walks
/// the graph from its `Function` operand(s) to see whether an I/O-using
/// function is reachable. Host operations currently include terminal/filesystem
/// I/O, clock reads, and the entropy seed read.
#[allow(clippy::too_many_lines)]
pub fn validate_no_console_io_in_workers(module: &mir::Module) -> Result<(), ValidationError> {
    let mut callees: HashMap<mir::SymbolId, Vec<mir::SymbolId>> = HashMap::new();
    let mut io_users: HashSet<mir::SymbolId> = HashSet::new();
    let mut foreign_users: HashSet<mir::SymbolId> = HashSet::new();
    for function in &module.functions {
        let mut direct = Vec::new();
        for block in &function.blocks {
            for instruction in &block.instructions {
                match instruction {
                    mir::Instruction::Call {
                        function: callee, ..
                    } => direct.push(*callee),
                    mir::Instruction::CallIntrinsic {
                        intrinsic:
                            mir::Intrinsic::ConsoleWrite
                            | mir::Intrinsic::ConsoleWriteLine
                            | mir::Intrinsic::ConsoleReadLine
                            | mir::Intrinsic::ConsoleReadLineTemporary
                            | mir::Intrinsic::FileReadAllText(_)
                            | mir::Intrinsic::FileReadAllTextTemporary(_)
                            | mir::Intrinsic::FileWriteAllText(_)
                            | mir::Intrinsic::FileAppendAllText(_)
                            | mir::Intrinsic::FileListFiles(_)
                            | mir::Intrinsic::FileListFilesTemporary(_)
                            | mir::Intrinsic::FileListDirectories(_)
                            | mir::Intrinsic::FileListDirectoriesTemporary(_)
                            | mir::Intrinsic::FileExists(_)
                            | mir::Intrinsic::DirectoryExists(_)
                            | mir::Intrinsic::FileCreateDirectory(_)
                            | mir::Intrinsic::FileDeleteFile(_)
                            | mir::Intrinsic::FileDeleteDirectory(_)
                            | mir::Intrinsic::TimeMonotonicMilliseconds
                            | mir::Intrinsic::TimeUnixMilliseconds
                            | mir::Intrinsic::RandomEntropySeed,
                        ..
                    } => {
                        io_users.insert(function.symbol);
                    }
                    mir::Instruction::ForeignCall { .. } => {
                        foreign_users.insert(function.symbol);
                    }
                    _ => {}
                }
            }
        }
        callees.insert(function.symbol, direct);
    }
    let reaches = |root: mir::SymbolId, users: &HashSet<mir::SymbolId>| -> bool {
        let mut visited: HashSet<mir::SymbolId> = HashSet::new();
        let mut stack = vec![root];
        while let Some(symbol) = stack.pop() {
            if !visited.insert(symbol) {
                continue;
            }
            if users.contains(&symbol) {
                return true;
            }
            if let Some(direct) = callees.get(&symbol) {
                stack.extend(direct.iter().copied());
            }
        }
        false
    };
    for function in &module.functions {
        for block in &function.blocks {
            for instruction in &block.instructions {
                let mir::Instruction::CallIntrinsic {
                    intrinsic,
                    arguments,
                    ..
                } = instruction
                else {
                    continue;
                };
                let worker_name = match intrinsic {
                    mir::Intrinsic::TaskRun => "Task.Run",
                    mir::Intrinsic::ParallelFor => "Parallel.For",
                    mir::Intrinsic::ParallelForEach => "Parallel.ForEach",
                    mir::Intrinsic::ParallelReduce => "Parallel.Reduce",
                    _ => continue,
                };
                for argument in arguments {
                    if let mir::OperandKind::Function(target) = argument.kind
                        && reaches(target, &foreign_users)
                    {
                        return Err(ValidationError::Rejected(format!(
                            "function `{}` uses `{worker_name}` with a worker body that (directly or transitively) performs a foreign call, which is rejected in this version",
                            function.name
                        )));
                    }
                    if let mir::OperandKind::Function(target) = argument.kind
                        && reaches(target, &io_users)
                    {
                        return Err(ValidationError::Rejected(format!(
                            "function `{}` uses `{worker_name}` with a worker body that (directly or transitively) calls `aster.io.Write`/`WriteLine`/`ReadLine`/`ReadAllText`/`WriteAllText`/`ListFiles`, another filesystem operation, a clock host operation through `aster.time.Clock`, or an entropy host operation through `Random.FromEntropy`, which is rejected in this version",
                            function.name
                        )));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Every `EnumConstruct` (built by enum literals, `switch` desugaring, and
/// postfix `?`'s `Result`/`Option` propagation alike) must name a `case`
/// symbol that is actually one of `value.type_`'s declared cases, with a
/// matching `tag` and exactly the fields that case declares (by symbol and
/// type, in order). Runs once per function over the whole module's enum
/// definitions, the same data `validate_string_try_parse_targets` uses,
/// rather than a construct-site-specific check duplicated per caller.
pub fn validate_enum_construct_shapes(
    function: &mir::Function,
    enum_definitions: &HashMap<mir::SymbolId, &mir::EnumDefinition>,
) -> Result<(), ValidationError> {
    for block in &function.blocks {
        for instruction in &block.instructions {
            let mir::Instruction::Assign { value, .. } = instruction else {
                continue;
            };
            let mir::RvalueKind::EnumConstruct { case, tag, fields } = &value.kind else {
                continue;
            };
            let mir::Type::Enum(symbol) = &value.type_ else {
                return Err(ValidationError::invalid(format!(
                    "function `{}` has an `EnumConstruct` whose declared type is not an enum",
                    function.name
                )));
            };
            let definition = enum_definitions.get(symbol).ok_or_else(|| {
                ValidationError::invalid(format!(
                    "function `{}` has an `EnumConstruct` for an unknown enum type",
                    function.name
                ))
            })?;
            let matched = definition
                .cases
                .iter()
                .find(|case_definition| case_definition.symbol == *case);
            let Some(matched) = matched else {
                return Err(ValidationError::invalid(format!(
                    "function `{}` has an `EnumConstruct` whose case symbol is not one of `{}`'s cases",
                    function.name, definition.name
                )));
            };
            if matched.tag != *tag {
                return Err(ValidationError::invalid(format!(
                    "function `{}` has an `EnumConstruct` for case `{}` with tag {tag}, expected {}",
                    function.name, matched.name, matched.tag
                )));
            }
            if fields.len() != matched.fields.len() {
                return Err(ValidationError::invalid(format!(
                    "function `{}` has an `EnumConstruct` for case `{}` with {} field(s), expected {}",
                    function.name,
                    matched.name,
                    fields.len(),
                    matched.fields.len()
                )));
            }
            for (provided, expected) in fields.iter().zip(&matched.fields) {
                if provided.field != expected.symbol || provided.value.type_ != expected.type_ {
                    return Err(ValidationError::invalid(format!(
                        "function `{}` has an `EnumConstruct` for case `{}` with a field that does not match its declaration",
                        function.name, matched.name
                    )));
                }
            }
        }
    }
    Ok(())
}

/// Every `Aggregate` (a struct literal, e.g. `IOError { Kind: ..., OsCode:
/// ... }`) must name a `Type::User` struct that actually exists, with
/// exactly the fields that struct declares, by symbol, order, and type.
/// Mirrors [`validate_enum_construct_shapes`]: previously `Aggregate`'s only
/// check was that each field operand was itself well-formed
/// (`validate_rvalue`), never that the *set* of fields matched the struct's
/// real declaration, so adulterated MIR reached Cranelift's own verifier
/// instead of a controlled `ValidationError`.
pub fn validate_struct_literal_shapes(
    function: &mir::Function,
    struct_definitions: &HashMap<mir::SymbolId, &mir::StructDefinition>,
) -> Result<(), ValidationError> {
    for block in &function.blocks {
        for instruction in &block.instructions {
            let mir::Instruction::Assign { value, .. } = instruction else {
                continue;
            };
            let mir::RvalueKind::Aggregate(fields) = &value.kind else {
                continue;
            };
            let mir::Type::User(symbol) = &value.type_ else {
                return Err(ValidationError::invalid(format!(
                    "function `{}` has an `Aggregate` whose declared type is not a struct",
                    function.name
                )));
            };
            let definition = struct_definitions.get(symbol).ok_or_else(|| {
                ValidationError::invalid(format!(
                    "function `{}` has an `Aggregate` for an unknown struct type",
                    function.name
                ))
            })?;
            if fields.len() != definition.fields.len() {
                return Err(ValidationError::invalid(format!(
                    "function `{}` has an `Aggregate` for `{}` with {} field(s), expected {}",
                    function.name,
                    definition.name,
                    fields.len(),
                    definition.fields.len()
                )));
            }
            for (provided, expected) in fields.iter().zip(&definition.fields) {
                if provided.field != expected.symbol || provided.value.type_ != expected.type_ {
                    return Err(ValidationError::invalid(format!(
                        "function `{}` has an `Aggregate` for `{}` with a field that does not match its declaration",
                        function.name, definition.name
                    )));
                }
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
pub fn validate_executable_owned_regions(
    function: &mir::Function,
    effects: &HashMap<mir::SymbolId, bool>,
) -> Result<(), ValidationError> {
    let locals = function
        .parameters
        .iter()
        .chain(&function.locals)
        .map(|local| (local.id, &local.type_))
        .collect::<HashMap<_, _>>();
    let mut entered = HashSet::new();

    for block in &function.blocks {
        let mut active: Option<(mir::OwnedRegionId, HashSet<mir::LocalId>)> = None;
        for instruction in &block.instructions {
            match instruction {
                mir::Instruction::OwnedRegionEnter { id } => {
                    if active.is_some() || !entered.insert(*id) {
                        return Err(ValidationError::unsupported(
                            &function.name,
                            "nested or duplicate executable owned-region enter",
                        ));
                    }
                    active = Some((*id, HashSet::new()));
                }
                mir::Instruction::OwnedRegionExit { id, invalidated } => {
                    let Some((active_id, produced)) = active.take() else {
                        return Err(ValidationError::unsupported(
                            &function.name,
                            "owned-region exit without a matching enter",
                        ));
                    };
                    let invalidated_set = invalidated.iter().copied().collect::<HashSet<_>>();
                    if active_id != *id
                        || invalidated.is_empty()
                        || invalidated_set.len() != invalidated.len()
                        || invalidated_set.iter().any(|local| {
                            locals
                                .get(local)
                                .is_none_or(|type_| !owned_region_result_type(type_))
                        })
                        || produced.is_empty()
                        || !produced.is_subset(&invalidated_set)
                    {
                        return Err(ValidationError::unsupported(
                            &function.name,
                            "mismatched executable owned-region exit",
                        ));
                    }
                }
                mir::Instruction::TemporarySubregionEnter { .. }
                | mir::Instruction::TemporarySubregionExit { .. }
                    if active.is_some() =>
                {
                    return Err(ValidationError::unsupported(
                        &function.name,
                        "Temporary reclaim inside an executable owned region",
                    ));
                }
                instruction if active.is_some() => {
                    let (_, aliases) = active.as_mut().expect("owned region is active");
                    if aliases.is_empty() {
                        let mir::Instruction::Call {
                            destination: Some(mir::Place::Local(destination)),
                            return_type,
                            ..
                        } = instruction
                        else {
                            return Err(ValidationError::unsupported(
                                &function.name,
                                "owned-region checkpoint not followed by its producer call",
                            ));
                        };
                        if !owned_region_result_type(return_type) {
                            return Err(ValidationError::unsupported(
                                &function.name,
                                "owned-region producer with a non-reference result",
                            ));
                        }
                        aliases.insert(*destination);
                        continue;
                    }
                    if owned_region_barrier(instruction, effects) {
                        return Err(ValidationError::unsupported(
                            &function.name,
                            "unrelated Persistent effect inside an executable owned region",
                        ));
                    }
                    if let mir::Instruction::Assign {
                        target: mir::Place::Local(destination),
                        value,
                    } = instruction
                        && owned_alias_source(value, aliases)
                    {
                        aliases.insert(*destination);
                    }
                }
                _ => {}
            }
        }
        if active.is_some() {
            return Err(ValidationError::unsupported(
                &function.name,
                "executable owned region crosses a basic-block boundary",
            ));
        }
    }

    if entered.is_empty() {
        return Ok(());
    }
    validate_owned_region_invalidated_uses(function)
}

pub fn owned_region_effects(
    module: &mir::Module,
    signatures: &HashMap<mir::SymbolId, &mir::Function>,
) -> HashMap<mir::SymbolId, bool> {
    // This is a fail-closed check over the explicit typed MIR contract, not a
    // second ownership selector: source aliases and last-use stay compiler-owned.
    let mut effects = module
        .functions
        .iter()
        .map(|function| {
            let direct = function
                .blocks
                .iter()
                .flat_map(|block| &block.instructions)
                .any(|instruction| match instruction {
                    mir::Instruction::Call { function, .. } => !signatures.contains_key(function),
                    _ => owned_region_direct_barrier(instruction),
                });
            (function.symbol, direct)
        })
        .collect::<HashMap<_, _>>();

    loop {
        let mut changed = false;
        for function in &module.functions {
            if effects[&function.symbol] {
                continue;
            }
            let transitive = function
                .blocks
                .iter()
                .flat_map(|block| &block.instructions)
                .any(|instruction| {
                    matches!(instruction,
                        mir::Instruction::Call { function, .. }
                            if effects.get(function).copied().unwrap_or(true))
                });
            if transitive {
                effects.insert(function.symbol, true);
                changed = true;
            }
        }
        if !changed {
            return effects;
        }
    }
}

pub fn owned_region_barrier(
    instruction: &mir::Instruction,
    effects: &HashMap<mir::SymbolId, bool>,
) -> bool {
    match instruction {
        mir::Instruction::Call { function, .. } => effects.get(function).copied().unwrap_or(true),
        _ => owned_region_direct_barrier(instruction),
    }
}

pub fn owned_region_direct_barrier(instruction: &mir::Instruction) -> bool {
    let persistent_allocation = match instruction {
        mir::Instruction::AllocateObject { region, .. }
        | mir::Instruction::AllocateArray { region, .. }
        | mir::Instruction::AllocateList { region, .. }
        | mir::Instruction::AllocateDictionary { region, .. }
        | mir::Instruction::AllocateStringBuilder { region, .. }
        | mir::Instruction::StringBuilderToString { region, .. }
        | mir::Instruction::DictionaryEntries { region, .. }
        | mir::Instruction::DictionaryKeys { region, .. }
        | mir::Instruction::DictionaryValues { region, .. }
        | mir::Instruction::ListToArray { region, .. } => {
            *region == mir::AllocationRegion::Persistent
        }
        mir::Instruction::CallIntrinsic { intrinsic, .. } => {
            intrinsic.allocation_region() == Some(mir::AllocationRegion::Persistent)
                || is_concurrency_intrinsic(*intrinsic)
        }
        _ => false,
    };
    persistent_allocation
        || matches!(
            instruction,
            mir::Instruction::CallInterface { .. }
                | mir::Instruction::ListAdd { .. }
                | mir::Instruction::DictionaryAdd { .. }
                | mir::Instruction::DictionarySet { .. }
                | mir::Instruction::StringBuilderAppend { .. }
        )
}

pub fn owned_alias_source(value: &mir::Rvalue, aliases: &HashSet<mir::LocalId>) -> bool {
    matches!(
        &value.kind,
        mir::RvalueKind::Use(mir::Operand {
            kind: mir::OperandKind::Copy(mir::Place::Local(local)),
            ..
        }) | mir::RvalueKind::Cast(mir::Operand {
            kind: mir::OperandKind::Copy(mir::Place::Local(local)),
            ..
        }) if owned_region_result_type(&value.type_) && aliases.contains(local)
    )
}

pub fn owned_region_result_type(type_: &mir::Type) -> bool {
    matches!(
        type_,
        mir::Type::String
            | mir::Type::Array(_)
            | mir::Type::Class(_)
            | mir::Type::List(_)
            | mir::Type::Dictionary(_, _)
    )
}

pub fn validate_owned_region_invalidated_uses(
    function: &mir::Function,
) -> Result<(), ValidationError> {
    let blocks = function
        .blocks
        .iter()
        .map(|block| (block.id, block))
        .collect::<HashMap<_, _>>();
    let mut incoming = HashMap::from([(function.entry, HashSet::<mir::LocalId>::new())]);
    let mut pending = vec![function.entry];
    while let Some(block_id) = pending.pop() {
        let Some(block) = blocks.get(&block_id) else {
            return Err(ValidationError::unsupported(
                &function.name,
                "malformed owned-region CFG",
            ));
        };
        let mut invalid = incoming[&block_id].clone();
        for instruction in &block.instructions {
            let mut reads = HashSet::new();
            owned_instruction_reads(instruction, &mut reads);
            if reads.iter().any(|local| invalid.contains(local)) {
                return Err(ValidationError::unsupported(
                    &function.name,
                    "use of a reclaimed owned-region local before redefinition",
                ));
            }
            let mut definitions = HashSet::new();
            owned_instruction_definitions(instruction, &mut definitions);
            invalid.retain(|local| !definitions.contains(local));
            if let mir::Instruction::OwnedRegionExit { invalidated, .. } = instruction {
                invalid.extend(invalidated.iter().copied());
            }
        }
        let mut terminator_reads = HashSet::new();
        match &block.terminator {
            mir::Terminator::Branch { condition, .. }
            | mir::Terminator::Return(Some(condition)) => {
                owned_operand_reads(condition, &mut terminator_reads);
            }
            _ => {}
        }
        if terminator_reads.iter().any(|local| invalid.contains(local)) {
            return Err(ValidationError::unsupported(
                &function.name,
                "use of a reclaimed owned-region local in a terminator",
            ));
        }
        let successors = match block.terminator {
            mir::Terminator::Goto(target) => vec![target],
            mir::Terminator::Branch {
                then_block,
                else_block,
                ..
            } => vec![then_block, else_block],
            mir::Terminator::Return(_) | mir::Terminator::End | mir::Terminator::Unreachable => {
                Vec::new()
            }
        };
        for successor in successors {
            if !blocks.contains_key(&successor) {
                return Err(ValidationError::unsupported(
                    &function.name,
                    "malformed owned-region CFG",
                ));
            }
            if let Some(state) = incoming.get_mut(&successor) {
                let old_len = state.len();
                state.extend(invalid.iter().copied());
                if state.len() != old_len {
                    pending.push(successor);
                }
            } else {
                incoming.insert(successor, invalid.clone());
                pending.push(successor);
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
pub fn owned_instruction_reads(instruction: &mir::Instruction, reads: &mut HashSet<mir::LocalId>) {
    match instruction {
        mir::Instruction::OwnedRegionEnter { .. }
        | mir::Instruction::OwnedRegionExit { .. }
        | mir::Instruction::TemporarySubregionEnter { .. }
        | mir::Instruction::TemporarySubregionExit { .. } => {}
        mir::Instruction::Assign { target, value } => {
            if !matches!(target, mir::Place::Local(_)) {
                owned_place_reads(target, reads);
            }
            owned_rvalue_reads(value, reads);
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
            if let Some(destination) = destination
                && !matches!(destination, mir::Place::Local(_))
            {
                owned_place_reads(destination, reads);
            }
            for argument in arguments {
                owned_operand_reads(argument, reads);
            }
        }
        mir::Instruction::CallInterface {
            destination,
            receiver,
            arguments,
            ..
        } => {
            if let Some(destination) = destination
                && !matches!(destination, mir::Place::Local(_))
            {
                owned_place_reads(destination, reads);
            }
            owned_operand_reads(receiver, reads);
            for argument in arguments {
                owned_operand_reads(argument, reads);
            }
        }
        mir::Instruction::AllocateArray {
            destination,
            length,
            ..
        } => {
            if !matches!(destination, mir::Place::Local(_)) {
                owned_place_reads(destination, reads);
            }
            owned_operand_reads(length, reads);
        }
        mir::Instruction::AllocateObject { destination, .. }
        | mir::Instruction::AllocateList { destination, .. }
        | mir::Instruction::AllocateDictionary { destination, .. }
        | mir::Instruction::AllocateStringBuilder { destination, .. } => {
            owned_destination_reads(destination, reads);
        }
        mir::Instruction::StringBuilderAppend { builder, value, .. } => {
            owned_operand_reads(builder, reads);
            owned_operand_reads(value, reads);
        }
        mir::Instruction::StringBuilderToString {
            destination,
            builder,
            ..
        } => {
            if !matches!(destination, mir::Place::Local(_)) {
                owned_place_reads(destination, reads);
            }
            owned_operand_reads(builder, reads);
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
            owned_destination_reads(destination, reads);
            owned_operand_reads(dictionary, reads);
            owned_operand_reads(key, reads);
            owned_operand_reads(value, reads);
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
            owned_destination_reads(destination, reads);
            owned_operand_reads(dictionary, reads);
            owned_operand_reads(key, reads);
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
            owned_destination_reads(destination, reads);
            owned_operand_reads(dictionary, reads);
        }
        mir::Instruction::DictionaryClear { dictionary } => {
            owned_operand_reads(dictionary, reads);
        }
        mir::Instruction::ListAdd { list, value } => {
            owned_operand_reads(list, reads);
            owned_operand_reads(value, reads);
        }
        mir::Instruction::ListGet {
            destination,
            list,
            index,
            ..
        } => {
            owned_destination_reads(destination, reads);
            owned_operand_reads(list, reads);
            owned_operand_reads(index, reads);
        }
        mir::Instruction::ListRemoveAt { list, index } => {
            owned_operand_reads(list, reads);
            owned_operand_reads(index, reads);
        }
        mir::Instruction::ListSet { list, index, value } => {
            owned_operand_reads(list, reads);
            owned_operand_reads(index, reads);
            owned_operand_reads(value, reads);
        }
        mir::Instruction::ListClear { list } => owned_operand_reads(list, reads),
        mir::Instruction::ListToArray {
            destination, list, ..
        } => {
            owned_destination_reads(destination, reads);
            owned_operand_reads(list, reads);
        }
        mir::Instruction::StringDecodeNext {
            string,
            cursor,
            char_destination,
            next_cursor_destination,
            ok_destination,
        } => {
            owned_operand_reads(string, reads);
            owned_operand_reads(cursor, reads);
            owned_destination_reads(char_destination, reads);
            owned_destination_reads(next_cursor_destination, reads);
            owned_destination_reads(ok_destination, reads);
        }
        mir::Instruction::Retain { value } | mir::Instruction::Release { value } => {
            owned_operand_reads(value, reads);
        }
    }
}

pub fn owned_instruction_definitions(
    instruction: &mir::Instruction,
    definitions: &mut HashSet<mir::LocalId>,
) {
    match instruction {
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
        } => owned_place_definition(target, definitions),
        mir::Instruction::Call { destination, .. }
        | mir::Instruction::CallInterface { destination, .. }
        | mir::Instruction::CallIntrinsic { destination, .. } => {
            if let Some(destination) = destination {
                owned_place_definition(destination, definitions);
            }
        }
        mir::Instruction::StringDecodeNext {
            char_destination,
            next_cursor_destination,
            ok_destination,
            ..
        } => {
            owned_place_definition(char_destination, definitions);
            owned_place_definition(next_cursor_destination, definitions);
            owned_place_definition(ok_destination, definitions);
        }
        _ => {}
    }
}

pub fn owned_place_definition(place: &mir::Place, definitions: &mut HashSet<mir::LocalId>) {
    if let mir::Place::Local(local) = place {
        definitions.insert(*local);
    }
}

pub fn owned_destination_reads(place: &mir::Place, reads: &mut HashSet<mir::LocalId>) {
    if !matches!(place, mir::Place::Local(_)) {
        owned_place_reads(place, reads);
    }
}

pub fn owned_rvalue_reads(value: &mir::Rvalue, reads: &mut HashSet<mir::LocalId>) {
    match &value.kind {
        mir::RvalueKind::Use(operand)
        | mir::RvalueKind::Discriminant(operand)
        | mir::RvalueKind::ArrayLength(operand)
        | mir::RvalueKind::ListLength(operand)
        | mir::RvalueKind::DictionaryLength(operand)
        | mir::RvalueKind::ListVersion(operand)
        | mir::RvalueKind::StringByteLength(operand)
        | mir::RvalueKind::Cast(operand)
        | mir::RvalueKind::Upcast(operand)
        | mir::RvalueKind::Unary { operand, .. } => owned_operand_reads(operand, reads),
        mir::RvalueKind::Aggregate(fields) | mir::RvalueKind::EnumConstruct { fields, .. } => {
            for field in fields {
                owned_operand_reads(&field.value, reads);
            }
        }
        mir::RvalueKind::MakeInterface { object, .. } => owned_operand_reads(object, reads),
        mir::RvalueKind::Binary { left, right, .. }
        | mir::RvalueKind::Equality { left, right, .. } => {
            owned_operand_reads(left, reads);
            owned_operand_reads(right, reads);
        }
    }
}

pub fn owned_operand_reads(operand: &mir::Operand, reads: &mut HashSet<mir::LocalId>) {
    if let mir::OperandKind::Copy(place) = &operand.kind {
        owned_place_reads(place, reads);
    }
}

pub fn owned_place_reads(place: &mir::Place, reads: &mut HashSet<mir::LocalId>) {
    match place {
        mir::Place::Local(local) => {
            reads.insert(*local);
        }
        mir::Place::Field { base, .. } | mir::Place::EnumField { base, .. } => {
            owned_place_reads(base, reads);
        }
        mir::Place::Index { array, index, .. } => {
            owned_operand_reads(array, reads);
            owned_operand_reads(index, reads);
        }
        mir::Place::ObjectField { object, .. } => owned_operand_reads(object, reads),
        mir::Place::Symbol(_) => {}
    }
}

#[allow(clippy::too_many_lines)]
pub fn validate_executable_temporary_subregions(
    function: &mir::Function,
) -> Result<(), ValidationError> {
    let contains_subregion = function
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .any(|instruction| {
            matches!(
                instruction,
                mir::Instruction::TemporarySubregionEnter { .. }
                    | mir::Instruction::TemporarySubregionExit { .. }
            )
        });
    if !contains_subregion {
        return Ok(());
    }

    if function
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .any(|instruction| {
            matches!(
                instruction,
                mir::Instruction::CallIntrinsic { intrinsic, .. }
                    if is_concurrency_intrinsic(*intrinsic)
            )
        })
    {
        return Err(ValidationError::unsupported(
            &function.name,
            "concurrency in an executable AARM temporary subregion function",
        ));
    }

    validate_executable_temporary_subregion_cfg(function)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FineState {
    Inactive,
    Active(mir::TemporarySubregionId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FineExecutionState {
    fine: FineState,
    owned: BTreeMap<u32, FineOwnedKind>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FineOwnedKind {
    StringBuilder,
    List,
    Dictionary,
}

impl FineExecutionState {
    fn inactive() -> Self {
        Self {
            fine: FineState::Inactive,
            owned: BTreeMap::new(),
        }
    }

    fn enter(&mut self, id: mir::TemporarySubregionId) -> Result<(), &'static str> {
        if self.fine != FineState::Inactive {
            return Err("nested executable AARM temporary subregion enter");
        }
        self.fine = FineState::Active(id);
        self.owned.clear();
        Ok(())
    }

    fn exit(&mut self, id: mir::TemporarySubregionId) -> Result<(), &'static str> {
        if self.fine != FineState::Active(id) {
            return Err("unmatched executable AARM temporary subregion exit");
        }
        *self = Self::inactive();
        Ok(())
    }

    fn validate_builder_instruction(
        &mut self,
        instruction: &mir::Instruction,
    ) -> Result<(), &'static str> {
        if self.fine == FineState::Inactive {
            return Ok(());
        }
        match instruction {
            mir::Instruction::AllocateStringBuilder {
                destination: mir::Place::Local(local),
                region: mir::AllocationRegion::Temporary,
                ..
            } => {
                if self
                    .owned
                    .insert(local.0, FineOwnedKind::StringBuilder)
                    .is_some()
                {
                    return Err("duplicate executable AARM StringBuilder local");
                }
            }
            mir::Instruction::StringBuilderAppend { builder, .. }
            | mir::Instruction::StringBuilderToString { builder, .. } => {
                let mir::OperandKind::Copy(mir::Place::Local(local)) = builder.kind else {
                    return Err("non-local executable AARM StringBuilder receiver");
                };
                if self.owned.get(&local.0) != Some(&FineOwnedKind::StringBuilder) {
                    return Err("unowned executable AARM StringBuilder receiver");
                }
            }
            mir::Instruction::AllocateList {
                destination: mir::Place::Local(local),
                region: mir::AllocationRegion::Temporary,
                ..
            } => {
                if self.owned.insert(local.0, FineOwnedKind::List).is_some() {
                    return Err("duplicate executable AARM List local");
                }
            }
            mir::Instruction::AllocateDictionary {
                destination: mir::Place::Local(local),
                region: mir::AllocationRegion::Temporary,
                ..
            } => {
                if self
                    .owned
                    .insert(local.0, FineOwnedKind::Dictionary)
                    .is_some()
                {
                    return Err("duplicate executable AARM Dictionary local");
                }
            }
            mir::Instruction::ListAdd { list, .. }
            | mir::Instruction::ListGet { list, .. }
            | mir::Instruction::ListRemoveAt { list, .. } => {
                if !fine_receiver_is_owned(list, FineOwnedKind::List, &self.owned) {
                    return Err("unowned executable AARM List receiver");
                }
            }
            mir::Instruction::DictionaryAdd { dictionary, .. }
            | mir::Instruction::DictionarySet { dictionary, .. }
            | mir::Instruction::DictionaryTryGet { dictionary, .. }
            | mir::Instruction::DictionaryContainsKey { dictionary, .. }
            | mir::Instruction::DictionaryRemove { dictionary, .. } => {
                if !fine_receiver_is_owned(dictionary, FineOwnedKind::Dictionary, &self.owned) {
                    return Err("unowned executable AARM Dictionary receiver");
                }
            }
            mir::Instruction::Assign { target, value }
                if fine_rvalue_mentions_owned_builder(value, &self.owned)
                    || matches!(target, mir::Place::Local(local) if self.owned.contains_key(&local.0)) =>
            {
                return Err("aliased or overwritten executable AARM StringBuilder local");
            }
            _ => {}
        }
        Ok(())
    }
}

pub fn fine_receiver_is_owned(
    operand: &mir::Operand,
    expected: FineOwnedKind,
    owned: &BTreeMap<u32, FineOwnedKind>,
) -> bool {
    matches!(operand.kind, mir::OperandKind::Copy(mir::Place::Local(local))
        if owned.get(&local.0) == Some(&expected))
}

pub fn fine_rvalue_mentions_owned_builder(
    value: &mir::Rvalue,
    owned: &BTreeMap<u32, FineOwnedKind>,
) -> bool {
    matches!(&value.kind, mir::RvalueKind::Use(operand) | mir::RvalueKind::Cast(operand)
        if matches!(operand.kind, mir::OperandKind::Copy(mir::Place::Local(local)) if owned.contains_key(&local.0)))
}

#[allow(clippy::too_many_lines)]
pub fn validate_executable_temporary_subregion_cfg(
    function: &mir::Function,
) -> Result<(), ValidationError> {
    let mut blocks = HashMap::new();
    for (index, block) in function.blocks.iter().enumerate() {
        if blocks.insert(block.id, index).is_some() {
            return Err(ValidationError::unsupported(
                &function.name,
                "duplicate basic blocks in executable AARM temporary subregions",
            ));
        }
    }
    if !blocks.contains_key(&function.entry) {
        return Err(ValidationError::unsupported(
            &function.name,
            "missing executable AARM temporary subregion entry",
        ));
    }
    let successors = function
        .blocks
        .iter()
        .map(|block| {
            let successors = match block.terminator {
                mir::Terminator::Goto(target) => vec![target],
                mir::Terminator::Branch {
                    then_block,
                    else_block,
                    ..
                } => vec![then_block, else_block],
                mir::Terminator::Return(_) | mir::Terminator::End => Vec::new(),
                mir::Terminator::Unreachable => return None,
            };
            successors
                .iter()
                .all(|target| blocks.contains_key(target))
                .then_some((block.id, successors))
        })
        .collect::<Option<HashMap<_, _>>>()
        .ok_or_else(|| {
            ValidationError::unsupported(
                &function.name,
                "malformed executable AARM temporary subregion CFG",
            )
        })?;
    let mut reachable = HashSet::new();
    let mut pending = vec![function.entry];
    while let Some(block) = pending.pop() {
        if !reachable.insert(block) {
            continue;
        }
        pending.extend(successors[&block].iter().copied());
    }
    if reachable.len() != blocks.len() {
        return Err(ValidationError::unsupported(
            &function.name,
            "unreachable executable AARM temporary subregion block",
        ));
    }
    validate_executable_temporary_subregion_cycles(function, &successors, &reachable)?;
    let mut state = HashMap::from([(function.entry, FineExecutionState::inactive())]);
    let mut entered = HashMap::new();
    let mut allocations = HashSet::new();
    let mut pending = vec![function.entry];
    while let Some(block_id) = pending.pop() {
        let mut current = state[&block_id].clone();
        let block = &function.blocks[blocks[&block_id]];
        for (instruction_index, instruction) in block.instructions.iter().enumerate() {
            match instruction {
                mir::Instruction::TemporarySubregionEnter { id } => {
                    if entered
                        .insert(*id, (block_id, instruction_index))
                        .is_some_and(|previous| previous != (block_id, instruction_index))
                        || current.enter(*id).is_err()
                    {
                        return Err(ValidationError::unsupported(
                            &function.name,
                            "nested or duplicate executable AARM temporary subregion enter",
                        ));
                    }
                }
                mir::Instruction::TemporarySubregionExit { id } => {
                    if current.exit(*id).is_err() {
                        return Err(ValidationError::unsupported(
                            &function.name,
                            "unmatched executable AARM temporary subregion exit",
                        ));
                    }
                }
                instruction if current.fine != FineState::Inactive => {
                    current
                        .validate_builder_instruction(instruction)
                        .map_err(|feature| ValidationError::unsupported(&function.name, feature))?;
                    if temporary_subregion_allocation_is_executable(instruction) {
                        allocations.insert(current.fine);
                    }
                    if !temporary_subregion_instruction_is_executable(instruction) {
                        return Err(ValidationError::unsupported(
                            &function.name,
                            "instruction inside an executable AARM temporary subregion",
                        ));
                    }
                }
                _ => {}
            }
        }
        if matches!(
            block.terminator,
            mir::Terminator::Return(_) | mir::Terminator::End
        ) && current.fine != FineState::Inactive
        {
            return Err(ValidationError::unsupported(
                &function.name,
                "Return or End with an active executable AARM temporary subregion",
            ));
        }
        for successor in &successors[&block_id] {
            if let Some(existing) = state.insert(*successor, current.clone()) {
                if existing != current {
                    return Err(ValidationError::unsupported(
                        &function.name,
                        "inconsistent executable AARM temporary subregion state at CFG join",
                    ));
                }
            } else {
                pending.push(*successor);
            }
        }
    }
    if entered
        .keys()
        .any(|id| !allocations.contains(&FineState::Active(*id)))
    {
        return Err(ValidationError::unsupported(
            &function.name,
            "executable AARM temporary subregion without a Temporary allocation",
        ));
    }
    Ok(())
}

pub fn temporary_subregion_allocation_is_executable(instruction: &mir::Instruction) -> bool {
    matches!(
        instruction,
        mir::Instruction::AllocateObject {
            region: mir::AllocationRegion::Temporary,
            ..
        } | mir::Instruction::AllocateArray {
            region: mir::AllocationRegion::Temporary,
            ..
        } | mir::Instruction::AllocateStringBuilder {
            region: mir::AllocationRegion::Temporary,
            ..
        } | mir::Instruction::StringBuilderToString {
            region: mir::AllocationRegion::Temporary,
            ..
        } | mir::Instruction::AllocateList {
            region: mir::AllocationRegion::Temporary,
            ..
        } | mir::Instruction::AllocateDictionary {
            region: mir::AllocationRegion::Temporary,
            ..
        }
    ) || matches!(
        instruction,
        mir::Instruction::CallIntrinsic {
            intrinsic: mir::Intrinsic::StringConcatTemporary
                | mir::Intrinsic::StringJoinTemporary
                | mir::Intrinsic::StringSubstringFromTemporary
                | mir::Intrinsic::StringSubstringRangeTemporary
                | mir::Intrinsic::StringFromLongTemporary
                | mir::Intrinsic::StringFromULongTemporary
                | mir::Intrinsic::StringFromDoubleTemporary
                | mir::Intrinsic::StringFromFloatTemporary
                | mir::Intrinsic::StringFromBoolTemporary
                | mir::Intrinsic::StringFromCharTemporary,
            ..
        }
    )
}

#[allow(clippy::too_many_lines)]
pub fn validate_executable_temporary_subregion_cycles(
    function: &mir::Function,
    successors: &HashMap<mir::BasicBlockId, Vec<mir::BasicBlockId>>,
    reachable: &HashSet<mir::BasicBlockId>,
) -> Result<(), ValidationError> {
    let mut predecessors = reachable
        .iter()
        .copied()
        .map(|block| (block, Vec::new()))
        .collect::<HashMap<_, _>>();
    for (block, targets) in successors {
        for target in targets {
            predecessors
                .get_mut(target)
                .expect("validated target")
                .push(*block);
        }
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
    let mut backedges = successors
        .iter()
        .flat_map(|(source, targets)| targets.iter().map(move |target| (*source, *target)))
        .filter(|(source, target)| dominators[source].contains(target))
        .collect::<Vec<_>>();
    backedges.sort_unstable_by_key(|(source, target)| (source.0, target.0));
    let backedges = backedges.into_iter().collect::<HashSet<_>>();
    let mut indegree = reachable
        .iter()
        .copied()
        .map(|block| (block, 0_usize))
        .collect::<HashMap<_, _>>();
    for block in reachable {
        for successor in &successors[block] {
            if !backedges.contains(&(*block, *successor)) {
                *indegree
                    .get_mut(successor)
                    .expect("reachable successor is in the CFG") += 1;
            }
        }
    }
    let mut ready = indegree
        .iter()
        .filter_map(|(block, degree)| (*degree == 0).then_some(*block))
        .collect::<Vec<_>>();
    ready.sort_unstable_by_key(|block| std::cmp::Reverse(block.0));
    let mut visited = 0;
    while let Some(block) = ready.pop() {
        visited += 1;
        for successor in &successors[&block] {
            if backedges.contains(&(block, *successor)) {
                continue;
            }
            let degree = indegree
                .get_mut(successor)
                .expect("reachable successor is in the CFG");
            *degree -= 1;
            if *degree == 0 {
                ready.push(*successor);
            }
        }
        ready.sort_unstable_by_key(|block| std::cmp::Reverse(block.0));
    }
    if visited != reachable.len() {
        return Err(ValidationError::unsupported(
            &function.name,
            "irreducible executable AARM temporary-subregion CFG",
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
pub fn temporary_subregion_instruction_is_executable(instruction: &mir::Instruction) -> bool {
    match instruction {
        mir::Instruction::Assign { target, value } => {
            temporary_subregion_place_is_executable(target)
                && temporary_subregion_rvalue_is_executable(value)
        }
        mir::Instruction::AllocateArray {
            destination,
            element_type,
            length,
            ..
        } => {
            matches!(destination, mir::Place::Local(_))
                && temporary_subregion_type_is_executable(element_type)
                && temporary_subregion_operand_is_executable(length)
        }
        mir::Instruction::AllocateObject { destination, .. } => {
            matches!(destination, mir::Place::Local(_))
        }
        mir::Instruction::AllocateStringBuilder {
            destination,
            region: mir::AllocationRegion::Temporary,
            ..
        } => matches!(destination, mir::Place::Local(_)),
        mir::Instruction::AllocateList {
            destination,
            element_type,
            region: mir::AllocationRegion::Temporary,
        } => {
            matches!(destination, mir::Place::Local(_))
                && temporary_subregion_type_is_executable(element_type)
        }
        mir::Instruction::AllocateDictionary {
            destination,
            key_type,
            value_type,
            region: mir::AllocationRegion::Temporary,
        } => {
            matches!(destination, mir::Place::Local(_))
                && temporary_subregion_type_is_executable(key_type)
                && temporary_subregion_type_is_executable(value_type)
        }
        mir::Instruction::StringBuilderAppend { builder, .. } => {
            matches!(builder.kind, mir::OperandKind::Copy(mir::Place::Local(_)))
        }
        mir::Instruction::StringBuilderToString {
            destination,
            builder,
            ..
        } => {
            matches!(destination, mir::Place::Local(_))
                && matches!(builder.kind, mir::OperandKind::Copy(mir::Place::Local(_)))
        }
        mir::Instruction::ListAdd { list, value } => {
            matches!(list.kind, mir::OperandKind::Copy(mir::Place::Local(_)))
                && temporary_subregion_operand_is_executable(value)
        }
        mir::Instruction::ListGet {
            destination,
            list,
            index,
            element_type,
        } => {
            matches!(destination, mir::Place::Local(_))
                && matches!(list.kind, mir::OperandKind::Copy(mir::Place::Local(_)))
                && temporary_subregion_operand_is_executable(index)
                && temporary_subregion_type_is_executable(element_type)
        }
        mir::Instruction::ListRemoveAt { list, index } => {
            matches!(list.kind, mir::OperandKind::Copy(mir::Place::Local(_)))
                && temporary_subregion_operand_is_executable(index)
        }
        mir::Instruction::ListSet { list, index, value } => {
            matches!(list.kind, mir::OperandKind::Copy(mir::Place::Local(_)))
                && temporary_subregion_operand_is_executable(index)
                && temporary_subregion_operand_is_executable(value)
        }
        mir::Instruction::ListClear { list } => {
            matches!(list.kind, mir::OperandKind::Copy(mir::Place::Local(_)))
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
            matches!(destination, mir::Place::Local(_))
                && matches!(
                    dictionary.kind,
                    mir::OperandKind::Copy(mir::Place::Local(_))
                )
                && temporary_subregion_operand_is_executable(key)
                && temporary_subregion_operand_is_executable(value)
        }
        mir::Instruction::DictionaryContainsKey {
            destination,
            dictionary,
            key,
        }
        | mir::Instruction::DictionaryRemove {
            destination,
            dictionary,
            key,
        } => {
            matches!(destination, mir::Place::Local(_))
                && matches!(
                    dictionary.kind,
                    mir::OperandKind::Copy(mir::Place::Local(_))
                )
                && temporary_subregion_operand_is_executable(key)
        }
        mir::Instruction::DictionaryTryGet {
            destination,
            dictionary,
            key,
            value_type,
            ..
        } => {
            matches!(destination, mir::Place::Local(_))
                && matches!(
                    dictionary.kind,
                    mir::OperandKind::Copy(mir::Place::Local(_))
                )
                && temporary_subregion_operand_is_executable(key)
                && temporary_subregion_type_is_executable(value_type)
        }
        mir::Instruction::DictionaryClear { dictionary } => matches!(
            dictionary.kind,
            mir::OperandKind::Copy(mir::Place::Local(_))
        ),
        mir::Instruction::CallIntrinsic {
            destination,
            intrinsic,
            arguments,
            return_type,
        } => temporary_subregion_immutable_string_intrinsic_is_executable(
            destination.as_ref(),
            *intrinsic,
            arguments,
            return_type,
        ),
        mir::Instruction::TemporarySubregionEnter { .. }
        | mir::Instruction::TemporarySubregionExit { .. }
        | mir::Instruction::OwnedRegionEnter { .. }
        | mir::Instruction::OwnedRegionExit { .. }
        | mir::Instruction::Call { .. }
        | mir::Instruction::ForeignCall { .. }
        | mir::Instruction::CallInterface { .. }
        | mir::Instruction::AllocateList {
            region: mir::AllocationRegion::Persistent,
            ..
        }
        | mir::Instruction::AllocateDictionary {
            region: mir::AllocationRegion::Persistent,
            ..
        }
        | mir::Instruction::AllocateStringBuilder {
            region: mir::AllocationRegion::Persistent,
            ..
        }
        | mir::Instruction::DictionaryEntries { .. }
        | mir::Instruction::DictionaryKeys { .. }
        | mir::Instruction::DictionaryValues { .. }
        | mir::Instruction::ListToArray { .. }
        | mir::Instruction::StringDecodeNext { .. }
        | mir::Instruction::Retain { .. }
        | mir::Instruction::Release { .. } => false,
    }
}

pub fn temporary_subregion_immutable_string_intrinsic_is_executable(
    destination: Option<&mir::Place>,
    intrinsic: mir::Intrinsic,
    arguments: &[mir::Operand],
    return_type: &mir::Type,
) -> bool {
    matches!(destination, Some(mir::Place::Local(_)))
        && return_type == &mir::Type::String
        && match intrinsic {
            mir::Intrinsic::StringConcatTemporary => {
                matches!(arguments, [left, right] if temporary_subregion_string_input_is_executable(left) && temporary_subregion_string_input_is_executable(right))
            }
            mir::Intrinsic::StringJoinTemporary => arguments
                .iter()
                .all(temporary_subregion_string_input_is_executable),
            mir::Intrinsic::StringSubstringFromTemporary => {
                matches!(arguments, [value, start]
                    if temporary_subregion_string_input_is_executable(value)
                        && start.type_ == mir::Type::Int
                        && temporary_subregion_operand_is_executable(start))
            }
            mir::Intrinsic::StringSubstringRangeTemporary => {
                matches!(arguments, [value, start, length]
                    if temporary_subregion_string_input_is_executable(value)
                        && start.type_ == mir::Type::Int
                        && length.type_ == mir::Type::Int
                        && temporary_subregion_operand_is_executable(start)
                        && temporary_subregion_operand_is_executable(length))
            }
            mir::Intrinsic::StringFromLongTemporary | mir::Intrinsic::StringFromULongTemporary => {
                matches!(arguments, [value]
                if match intrinsic {
                    mir::Intrinsic::StringFromLongTemporary => matches!(
                        value.type_,
                        mir::Type::SByte | mir::Type::Short | mir::Type::Int | mir::Type::Long
                    ),
                    mir::Intrinsic::StringFromULongTemporary => matches!(
                        value.type_,
                        mir::Type::Byte | mir::Type::UShort | mir::Type::UInt | mir::Type::ULong
                    ),
                    _ => false,
                })
            }
            mir::Intrinsic::StringFromDoubleTemporary => {
                matches!(arguments, [value] if value.type_ == mir::Type::Double)
            }
            mir::Intrinsic::StringFromFloatTemporary => {
                matches!(arguments, [value] if value.type_ == mir::Type::Float)
            }
            mir::Intrinsic::StringFromBoolTemporary => {
                matches!(arguments, [value] if value.type_ == mir::Type::Bool)
            }
            mir::Intrinsic::StringFromCharTemporary => {
                matches!(arguments, [value] if value.type_ == mir::Type::Char)
            }
            _ => false,
        }
}

pub fn temporary_subregion_string_input_is_executable(operand: &mir::Operand) -> bool {
    operand.type_ == mir::Type::String
        && matches!(
            operand.kind,
            mir::OperandKind::Constant(mir::Constant::String(_)) | mir::OperandKind::Copy(_)
        )
}

pub fn temporary_subregion_rvalue_is_executable(value: &mir::Rvalue) -> bool {
    if !temporary_subregion_type_is_executable(&value.type_) {
        return false;
    }
    match &value.kind {
        mir::RvalueKind::Use(operand)
        | mir::RvalueKind::Cast(operand)
        | mir::RvalueKind::Unary { operand, .. } => {
            temporary_subregion_operand_is_executable(operand)
        }
        mir::RvalueKind::Binary {
            left,
            operator,
            right,
        } => {
            !matches!(
                operator,
                mir::BinaryOperator::Divide | mir::BinaryOperator::Remainder
            ) && temporary_subregion_operand_is_executable(left)
                && temporary_subregion_operand_is_executable(right)
        }
        mir::RvalueKind::Equality { left, right, .. } => {
            temporary_subregion_equality_type_is_executable(&left.type_)
                && temporary_subregion_operand_is_executable(left)
                && temporary_subregion_operand_is_executable(right)
        }
        mir::RvalueKind::ArrayLength(operand) => {
            matches!(operand.type_, mir::Type::Array(_))
                && temporary_subregion_operand_is_executable(operand)
        }
        mir::RvalueKind::Aggregate(_)
        | mir::RvalueKind::EnumConstruct { .. }
        | mir::RvalueKind::Discriminant(_)
        | mir::RvalueKind::ListLength(_)
        | mir::RvalueKind::DictionaryLength(_)
        | mir::RvalueKind::ListVersion(_)
        | mir::RvalueKind::StringByteLength(_)
        | mir::RvalueKind::Upcast(_)
        | mir::RvalueKind::MakeInterface { .. } => false,
    }
}

pub fn temporary_subregion_operand_is_executable(operand: &mir::Operand) -> bool {
    temporary_subregion_type_is_executable(&operand.type_)
        && match &operand.kind {
            mir::OperandKind::Constant(mir::Constant::String(_)) => false,
            mir::OperandKind::Constant(_) | mir::OperandKind::Function(_) => true,
            mir::OperandKind::Copy(place) => temporary_subregion_place_is_executable(place),
        }
}

pub fn temporary_subregion_place_is_executable(place: &mir::Place) -> bool {
    match place {
        mir::Place::Local(_) => true,
        mir::Place::Index {
            array,
            index,
            element_type,
            ..
        } => {
            matches!(
                &array.type_,
                mir::Type::Array(array_element) if array_element.as_ref() == element_type
            ) && temporary_subregion_type_is_executable(element_type)
                && temporary_subregion_operand_is_executable(array)
                && temporary_subregion_operand_is_executable(index)
        }
        mir::Place::ObjectField { object, .. } => {
            matches!(object.type_, mir::Type::Class(_))
                && temporary_subregion_operand_is_executable(object)
        }
        mir::Place::Symbol(_) | mir::Place::Field { .. } | mir::Place::EnumField { .. } => false,
    }
}

pub fn temporary_subregion_type_is_executable(type_: &mir::Type) -> bool {
    match type_ {
        mir::Type::Array(element) => temporary_subregion_type_is_executable(element),
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
        | mir::Type::Class(_) => true,
        mir::Type::String
        | mir::Type::User(_)
        | mir::Type::Interface(_)
        | mir::Type::Enum(_)
        | mir::Type::Task(_)
        | mir::Type::List(_)
        | mir::Type::Dictionary(_, _)
        | mir::Type::Void
        | mir::Type::Decimal
        | mir::Type::Unknown => false,
    }
}

pub fn temporary_subregion_equality_type_is_executable(type_: &mir::Type) -> bool {
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
            | mir::Type::Class(_)
            | mir::Type::Array(_)
    )
}

pub fn is_concurrency_intrinsic(intrinsic: mir::Intrinsic) -> bool {
    matches!(
        intrinsic,
        mir::Intrinsic::TaskRun
            | mir::Intrinsic::TaskWait
            | mir::Intrinsic::AsyncSpawn
            | mir::Intrinsic::AsyncState
            | mir::Intrinsic::AsyncSetState
            | mir::Intrinsic::AsyncStoreSlot
            | mir::Intrinsic::AsyncLoadSlot
            | mir::Intrinsic::AsyncSpawnInner
            | mir::Intrinsic::AsyncAwaitResult
            | mir::Intrinsic::AsyncSetResult
            | mir::Intrinsic::ParallelFor
            | mir::Intrinsic::ParallelForEach
            | mir::Intrinsic::ParallelReduce
    )
}

pub fn validate_block_target(
    target: mir::BasicBlockId,
    function_name: &str,
    blocks: &HashSet<mir::BasicBlockId>,
) -> Result<(), ValidationError> {
    if blocks.contains(&target) {
        Ok(())
    } else {
        Err(ValidationError::invalid(format!(
            "function `{function_name}` references unknown basic block {target:?}"
        )))
    }
}

pub type InterfaceMethods<'a> =
    HashMap<mir::SymbolId, (mir::SymbolId, &'a mir::InterfaceMethodDefinition)>;

pub fn validate_interface_metadata<'a>(
    module: &'a mir::Module,
    signatures: &HashMap<mir::SymbolId, &'a mir::Function>,
    classes: &HashSet<mir::SymbolId>,
) -> Result<
    (
        InterfaceMethods<'a>,
        HashSet<(mir::SymbolId, mir::SymbolId)>,
    ),
    ValidationError,
> {
    let mut methods = HashMap::new();
    let interfaces = module
        .interfaces
        .iter()
        .map(|interface| {
            for method in &interface.methods {
                methods.insert(method.symbol, (interface.symbol, method));
            }
            (interface.symbol, interface)
        })
        .collect::<HashMap<_, _>>();
    let bases = module
        .classes
        .iter()
        .filter_map(|class| Some((class.symbol, class.base?)))
        .collect::<HashMap<_, _>>();
    // The class itself, then its base chain (bounded: hierarchy validation
    // already rejected cycles, the bound keeps this walk total regardless).
    let in_chain = |class: mir::SymbolId, owner: mir::SymbolId| {
        let mut current = class;
        for _ in 0..=module.classes.len() {
            if current == owner {
                return true;
            }
            match bases.get(&current) {
                Some(base) => current = *base,
                None => return false,
            }
        }
        false
    };
    let mut implementations = HashSet::new();
    for implementation in &module.interface_implementations {
        if !classes.contains(&implementation.class) {
            return Err(ValidationError::invalid(
                "interface implementation references an unknown class",
            ));
        }
        let interface = interfaces.get(&implementation.interface).ok_or_else(|| {
            ValidationError::invalid("interface implementation references an unknown interface")
        })?;
        if !implementations.insert((implementation.class, implementation.interface)) {
            return Err(ValidationError::invalid(
                "duplicate interface implementation in MIR",
            ));
        }
        if implementation.methods.len() != interface.methods.len() {
            return Err(ValidationError::invalid(
                "interface implementation has the wrong method count",
            ));
        }
        for (required, concrete_symbol) in interface.methods.iter().zip(&implementation.methods) {
            let concrete = signatures.get(concrete_symbol).ok_or_else(|| {
                ValidationError::invalid("interface implementation references an unknown method")
            })?;
            // The concrete method belongs to the implementing class or to a
            // base class it inherits it from (no member is virtual yet, so the
            // inherited method is exactly what a derived object runs).
            let owner = concrete.owner;
            if owner.is_none_or(|owner| !in_chain(implementation.class, owner))
                || concrete.visibility != mir::Visibility::Public
                || concrete.return_type != required.return_type
                || concrete.parameters.first().map(|receiver| &receiver.type_)
                    != owner.map(mir::Type::Class).as_ref()
                || concrete
                    .parameters
                    .iter()
                    .skip(1)
                    .map(|parameter| &parameter.type_)
                    .ne(required.parameters.iter())
            {
                return Err(ValidationError::invalid(format!(
                    "concrete method `{}` does not match interface method `{}`",
                    concrete.name, required.name
                )));
            }
        }
    }
    Ok((methods, implementations))
}

/// Pipeline phase of a MIR module: which region instructions may exist.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Phase {
    /// After MIR lowering and before region lowering: no fine subregions or
    /// owned regions yet.
    Lowered,
    /// After the AARM pass: fine Temporary subregions may exist.
    TemporarySubregions,
    /// After owned-region lowering: the module a backend receives.
    Final,
}

/// Structural CFG of one function: unique block identities, an existing
/// entry, and every terminator target declared.
pub fn validate_cfg(function: &mir::Function) -> Result<(), ValidationError> {
    let mut blocks = HashSet::new();
    for block in &function.blocks {
        if !blocks.insert(block.id) {
            return Err(ValidationError::invalid(format!(
                "function `{}` declares basic block {:?} twice",
                function.name, block.id
            )));
        }
    }
    if !function.blocks.is_empty() {
        validate_block_target(function.entry, &function.name, &blocks)?;
    }
    for block in &function.blocks {
        match &block.terminator {
            mir::Terminator::Goto(target) => {
                validate_block_target(*target, &function.name, &blocks)?;
            }
            mir::Terminator::Branch {
                then_block,
                else_block,
                ..
            } => {
                validate_block_target(*then_block, &function.name, &blocks)?;
                validate_block_target(*else_block, &function.name, &blocks)?;
            }
            mir::Terminator::Return(_) | mir::Terminator::End | mir::Terminator::Unreachable => {}
        }
    }
    Ok(())
}

/// Every backend-neutral executability check that applies at `phase`:
/// interface metadata, CFG structure, enum and struct construction shapes,
/// worker host-operation reachability, and the region checks once their
/// pass has run. Typing is [`super::validate_types`].
pub fn validate_module(module: &mir::Module, phase: Phase) -> Result<(), ValidationError> {
    let signatures = module
        .functions
        .iter()
        .map(|function| (function.symbol, function))
        .collect::<HashMap<_, _>>();
    let classes = module
        .classes
        .iter()
        .map(|class| class.symbol)
        .collect::<HashSet<_>>();
    validate_interface_metadata(module, &signatures, &classes)?;
    let enum_definitions = module
        .enums
        .iter()
        .map(|definition| (definition.symbol, definition))
        .collect::<HashMap<_, _>>();
    let struct_definitions = module
        .structs
        .iter()
        .map(|definition| (definition.symbol, definition))
        .collect::<HashMap<_, _>>();
    let owned_effects = (phase == Phase::Final).then(|| owned_region_effects(module, &signatures));
    for function in &module.functions {
        validate_cfg(function)?;
        validate_enum_construct_shapes(function, &enum_definitions)?;
        validate_struct_literal_shapes(function, &struct_definitions)?;
        if phase >= Phase::TemporarySubregions {
            validate_executable_temporary_subregions(function)?;
        }
        if let Some(effects) = &owned_effects {
            validate_executable_owned_regions(function, effects)?;
        }
    }
    validate_no_console_io_in_workers(module)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fine_builder_allocation(local: u32, region: mir::AllocationRegion) -> mir::Instruction {
        mir::Instruction::AllocateStringBuilder {
            destination: mir::Place::Local(mir::LocalId(local)),
            class: mir::SymbolId(900),
            region,
        }
    }

    fn fine_builder_append(local: u32) -> mir::Instruction {
        mir::Instruction::StringBuilderAppend {
            builder: mir::Operand {
                type_: mir::Type::Class(mir::SymbolId(900)),
                kind: mir::OperandKind::Copy(mir::Place::Local(mir::LocalId(local))),
            },
            value: mir::Operand {
                type_: mir::Type::String,
                kind: mir::OperandKind::Constant(mir::Constant::String("x".to_owned())),
            },
            class: mir::SymbolId(900),
        }
    }

    #[test]
    fn fine_builder_provenance_state_is_exact_at_joins_and_resets_per_region() {
        let id = mir::TemporarySubregionId(7);
        let allocation = fine_builder_allocation(1, mir::AllocationRegion::Temporary);
        let append = fine_builder_append(1);
        let mut state = FineExecutionState::inactive();

        state.enter(id).expect("fine Enter starts ownership domain");
        assert!(state.owned.is_empty());
        state
            .validate_builder_instruction(&allocation)
            .expect("Temporary direct-local builder becomes owned");
        assert_eq!(
            state.owned,
            BTreeMap::from([(1, FineOwnedKind::StringBuilder)])
        );
        state
            .validate_builder_instruction(&append)
            .expect("Append on the owned local is structurally valid");

        let identical_join = state.clone();
        assert_eq!(state, identical_join, "identical incoming sets join");
        let mut mismatched_join = state.clone();
        mismatched_join
            .owned
            .insert(2, FineOwnedKind::StringBuilder);
        assert_ne!(
            state, mismatched_join,
            "different incoming sets cannot join"
        );

        state
            .exit(id)
            .expect("matching Exit closes ownership domain");
        assert_eq!(state, FineExecutionState::inactive());
        state
            .enter(id)
            .expect("next loop iteration starts a new domain");
        assert!(state.owned.is_empty());
        assert!(state.validate_builder_instruction(&append).is_err());
        assert!(state.enter(mir::TemporarySubregionId(8)).is_err());
        assert!(state.exit(mir::TemporarySubregionId(8)).is_err());

        let persistent = fine_builder_allocation(1, mir::AllocationRegion::Persistent);
        let mut persistent_state = FineExecutionState::inactive();
        persistent_state.enter(id).expect("fine Enter");
        persistent_state
            .validate_builder_instruction(&persistent)
            .expect("Persistent builder is observed but not owned");
        assert!(persistent_state.owned.is_empty());
        assert!(
            persistent_state
                .validate_builder_instruction(&append)
                .is_err()
        );

        assert!(temporary_subregion_instruction_is_executable(&allocation));
        assert!(temporary_subregion_instruction_is_executable(&append));
    }
}
