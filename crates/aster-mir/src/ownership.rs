//! MEM-EXP-1 C5/C6 ownership pass (master plan §5.5, closure §4.B).
//!
//! Experiment only: the default build (C0) never calls it. It rewrites a
//! validated module so that every owning reference slot accounts for exactly
//! one strong count, using explicit [`crate::Instruction::Retain`] and
//! [`crate::Instruction::Release`]. Its output is checked by
//! [`crate::validate::ownership`], which shares no code with this pass.

use std::collections::{HashMap, HashSet};

use crate as mir;
use crate::validate::executable::{
    owned_instruction_reads, owned_operand_reads, owned_place_reads,
};
use mir::{
    BasicBlock, BasicBlockId, Function, Instruction, Intrinsic, Local, LocalId, Module, Operand,
    OperandKind, Place, Rvalue, RvalueKind, SymbolId, Terminator, Type,
};

/// Count operations inserted by [`insert_ownership`] (`retains`/`releases`
/// are those that remain in the output) and, for
/// [`insert_ownership_elided`], the `Retain`/`Release` instructions removed by
/// each C6 rule. A null capture counts only its `Release` (the capturing
/// `Assign` is not a count operation).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OwnershipStats {
    pub retains: usize,
    pub releases: usize,
    /// E1: releases of captures from definitely-null local leaves.
    pub elided_null_captures: usize,
    /// E2: retains, exit releases and capture releases of moved temporaries.
    pub elided_moves: usize,
    /// E3: entry retains and exit releases of leaf-function parameters.
    pub elided_parameters: usize,
}

/// Return a copy of `module` with ownership operations inserted and then
/// reduced by the local rules E1 (null capture), E2 (move from a single-use
/// lowering temporary) and E3 (leaf-function parameters) of the C6 contract.
#[must_use]
pub fn insert_ownership_elided(module: &mir::Module) -> (mir::Module, OwnershipStats) {
    let (mut output, mut stats) = insert_ownership(module);
    let native = string_builder_classes(module);
    for (function, original) in output.functions.iter_mut().zip(&module.functions) {
        let mut scratch = OwnershipStats::default();
        let context = Context {
            module,
            native: &native,
            function: original,
            next_local: 0,
            new_locals: Vec::new(),
            stats: &mut scratch,
        };
        let known: HashSet<LocalId> = original
            .parameters
            .iter()
            .chain(&original.locals)
            .map(|local| local.id)
            .collect();
        let captures: HashSet<LocalId> = function
            .locals
            .iter()
            .filter(|local| !known.contains(&local.id) && local.name.starts_with("$own.old"))
            .map(|local| local.id)
            .collect();
        let null = elide_null_captures(function, &captures);
        stats.retains -= null.retains;
        stats.releases -= null.releases;
        stats.elided_null_captures += null.retains + null.releases;
        let moves = context.elide_moves(function, &captures);
        stats.retains -= moves.retains;
        stats.releases -= moves.releases;
        stats.elided_moves += moves.retains + moves.releases;
        let parameters = context.elide_parameters(function);
        stats.retains -= parameters.retains;
        stats.releases -= parameters.releases;
        stats.elided_parameters += parameters.retains + parameters.releases;
    }
    (output, stats)
}

#[derive(Default)]
struct Removed {
    retains: usize,
    releases: usize,
}

fn is_call(instruction: &Instruction) -> bool {
    matches!(
        instruction,
        Instruction::Call { .. }
            | Instruction::CallInterface { .. }
            | Instruction::ForeignCall { .. }
            | Instruction::CallIntrinsic { .. }
    )
}

/// Every place an instruction writes.
fn destination_places(instruction: &Instruction) -> Vec<&Place> {
    match instruction {
        Instruction::Assign { target, .. }
        | Instruction::AllocateArray {
            destination: target,
            ..
        }
        | Instruction::AllocateObject {
            destination: target,
            ..
        }
        | Instruction::AllocateList {
            destination: target,
            ..
        }
        | Instruction::AllocateDictionary {
            destination: target,
            ..
        }
        | Instruction::AllocateStringBuilder {
            destination: target,
            ..
        }
        | Instruction::StringBuilderToString {
            destination: target,
            ..
        }
        | Instruction::DictionaryAdd {
            destination: target,
            ..
        }
        | Instruction::DictionarySet {
            destination: target,
            ..
        }
        | Instruction::DictionaryTryGet {
            destination: target,
            ..
        }
        | Instruction::DictionaryContainsKey {
            destination: target,
            ..
        }
        | Instruction::DictionaryRemove {
            destination: target,
            ..
        }
        | Instruction::DictionaryEntries {
            destination: target,
            ..
        }
        | Instruction::DictionaryKeys {
            destination: target,
            ..
        }
        | Instruction::DictionaryValues {
            destination: target,
            ..
        }
        | Instruction::ListGet {
            destination: target,
            ..
        }
        | Instruction::ListToArray {
            destination: target,
            ..
        } => vec![target],
        Instruction::Call { destination, .. }
        | Instruction::CallInterface { destination, .. }
        | Instruction::ForeignCall { destination, .. }
        | Instruction::CallIntrinsic { destination, .. } => destination.iter().collect(),
        Instruction::StringDecodeNext {
            char_destination,
            next_cursor_destination,
            ok_destination,
            ..
        } => vec![char_destination, next_cursor_destination, ok_destination],
        _ => Vec::new(),
    }
}

/// Whether a store to `place` writes an array element (it releases the old
/// element, so it counts as a releasing instruction for E3).
fn stores_into_array(place: &Place) -> bool {
    match place {
        Place::Index { .. } => true,
        Place::Field { base, .. } => stores_into_array(base),
        _ => false,
    }
}

/// The local a place is stored in, when the place is a local or a field path
/// of a local.
fn root_local(place: &Place) -> Option<LocalId> {
    match place {
        Place::Local(id) => Some(*id),
        Place::Field { base, .. } => root_local(base),
        _ => None,
    }
}

fn successor_blocks(terminator: &Terminator) -> Vec<BasicBlockId> {
    match terminator {
        Terminator::Goto(target) => vec![*target],
        Terminator::Branch {
            then_block,
            else_block,
            ..
        } => vec![*then_block, *else_block],
        _ => Vec::new(),
    }
}

fn is_release_of(instruction: &Instruction, local: LocalId) -> bool {
    matches!(
        instruction,
        Instruction::Release {
            value: Operand { kind: OperandKind::Copy(Place::Local(id)), .. }
        } if *id == local
    )
}

/// `Some(target)` for `target = Use(Copy(Local(local)))`.
fn copy_target(instruction: &Instruction, local: LocalId) -> Option<&Place> {
    match instruction {
        Instruction::Assign {
            target,
            value:
                Rvalue {
                    kind:
                        RvalueKind::Use(Operand {
                            kind: OperandKind::Copy(Place::Local(id)),
                            ..
                        }),
                    ..
                },
        } if *id == local => Some(target),
        _ => None,
    }
}

/// Return a copy of `module` with ownership operations inserted.
#[must_use]
pub fn insert_ownership(module: &mir::Module) -> (mir::Module, OwnershipStats) {
    let mut stats = OwnershipStats::default();
    let mut output = module.clone();
    let native = string_builder_classes(module);
    for function in &mut output.functions {
        let original = function.clone();
        let mut context = Context {
            module,
            native: &native,
            function: &original,
            next_local: next_id(
                function
                    .parameters
                    .iter()
                    .chain(&function.locals)
                    .map(|l| l.id.0),
            ),
            new_locals: Vec::new(),
            stats: &mut stats,
        };
        context.rewrite_function(function);
    }
    (output, stats)
}

fn next_id(ids: impl Iterator<Item = u32>) -> u32 {
    ids.max().map_or(0, |id| id + 1)
}

type Leaf = (Vec<SymbolId>, Type);

/// Argument positions of a storing intrinsic whose operand is kept by the
/// runtime (consumed); every other intrinsic argument is borrowed (design §9).
/// Positions follow the lowering: `ListInsert(list, index, value)`,
/// `DictionaryGetOrAdd(dictionary, key, value)`,
/// `AsyncStoreSlot(handle, slot, value)`.
fn stores_argument(intrinsic: &Intrinsic, position: usize) -> bool {
    match intrinsic {
        Intrinsic::ListInsert | Intrinsic::AsyncStoreSlot => position == 2,
        Intrinsic::DictionaryGetOrAdd => position == 1 || position == 2,
        _ => false,
    }
}

/// Intrinsics whose result is a value that already lives in a container or
/// task frame: the destination takes its own count after the call, whatever
/// the result type (design §9, coordinator correction).
fn returns_stored_value(intrinsic: &Intrinsic) -> bool {
    matches!(
        intrinsic,
        Intrinsic::DictionaryGetOr
            | Intrinsic::DictionaryGetOrAdd
            | Intrinsic::AsyncLoadSlot
            | Intrinsic::AsyncAwaitResult
            | Intrinsic::TaskWait
    )
}

/// `StringBuilder` objects use a native runtime header without a count word.
fn string_builder_classes(module: &Module) -> HashSet<SymbolId> {
    module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            Instruction::AllocateStringBuilder { class, .. }
            | Instruction::StringBuilderAppend { class, .. }
            | Instruction::StringBuilderToString { class, .. } => Some(*class),
            _ => None,
        })
        .collect()
}

fn leaves(module: &Module, native: &HashSet<SymbolId>, type_: &Type) -> Vec<Leaf> {
    match type_ {
        Type::Class(symbol) if native.contains(symbol) => Vec::new(),
        // An array is one leaf whatever its element type: an `int[]` must still
        // be released to free its blocks (design §8). A string is a leaf too
        // (design §9); string constants are immortal and never counted.
        // A list is one leaf whatever its element type (design §10), and so is
        // a dictionary (design §11).
        Type::Class(_)
        | Type::Interface(_)
        | Type::Array(_)
        | Type::String
        | Type::List(_)
        | Type::Dictionary(_, _) => vec![(Vec::new(), type_.clone())],
        Type::User(symbol) => module
            .structs
            .iter()
            .find(|definition| definition.symbol == *symbol)
            .into_iter()
            .flat_map(|definition| &definition.fields)
            .flat_map(|field| {
                leaves(module, native, &field.type_)
                    .into_iter()
                    .map(move |(mut path, leaf)| {
                        path.insert(0, field.symbol);
                        (path, leaf)
                    })
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn leaf_place(base: &Place, path: &[SymbolId]) -> Place {
    path.iter().fold(base.clone(), |base, field| Place::Field {
        base: Box::new(base),
        field: *field,
    })
}

fn copy(place: Place, type_: &Type) -> Operand {
    Operand {
        type_: type_.clone(),
        kind: OperandKind::Copy(place),
    }
}

fn owning(place: &Place) -> bool {
    match place {
        Place::Local(_) | Place::ObjectField { .. } | Place::Index { .. } => true,
        Place::Field { base, .. } => owning(base),
        Place::Symbol(_) | Place::EnumField { .. } => false,
    }
}

struct Context<'a> {
    module: &'a Module,
    native: &'a HashSet<SymbolId>,
    function: &'a Function,
    next_local: u32,
    new_locals: Vec<Local>,
    stats: &'a mut OwnershipStats,
}

impl Context<'_> {
    fn leaf_operands(&self, place: &Place, type_: &Type) -> Vec<Operand> {
        leaves(self.module, self.native, type_)
            .into_iter()
            .map(|(path, leaf)| copy(leaf_place(place, &path), &leaf))
            .collect()
    }

    fn operand_leaves(&self, operand: &Operand) -> Vec<Operand> {
        match &operand.kind {
            OperandKind::Copy(place) => self.leaf_operands(place, &operand.type_),
            OperandKind::Constant(_) | OperandKind::Function(_) => Vec::new(),
        }
    }

    fn place_type(&self, place: &Place) -> Option<Type> {
        match place {
            Place::Local(id) => self
                .function
                .parameters
                .iter()
                .chain(&self.function.locals)
                .find(|local| local.id == *id)
                .map(|local| local.type_.clone()),
            Place::Symbol(_) => None,
            Place::Field { base, field } => {
                let Type::User(symbol) = self.place_type(base)? else {
                    return None;
                };
                let definition = self.module.structs.iter().find(|d| d.symbol == symbol)?;
                definition
                    .fields
                    .iter()
                    .find(|f| f.symbol == *field)
                    .map(|f| f.type_.clone())
            }
            Place::Index { element_type, .. } => Some(element_type.clone()),
            Place::ObjectField { object, field } => {
                let Type::Class(symbol) = &object.type_ else {
                    return None;
                };
                let definition = self.module.classes.iter().find(|d| d.symbol == *symbol)?;
                definition
                    .fields
                    .iter()
                    .find(|f| f.symbol == *field)
                    .map(|f| f.type_.clone())
            }
            Place::EnumField { case, field, .. } => self
                .module
                .enums
                .iter()
                .flat_map(|definition| &definition.cases)
                .find(|candidate| candidate.symbol == *case)?
                .fields
                .iter()
                .find(|f| f.symbol == *field)
                .map(|f| f.type_.clone()),
        }
    }

    fn fresh(&mut self, type_: &Type, name: &str) -> LocalId {
        let id = LocalId(self.next_local);
        self.next_local += 1;
        self.new_locals.push(Local {
            id,
            symbol: None,
            name: format!("{name}.{}", id.0),
            type_: type_.clone(),
            mutable: true,
            temporary: true,
        });
        id
    }

    fn retain(&mut self, out: &mut Vec<Instruction>, value: Operand) {
        self.stats.retains += 1;
        out.push(Instruction::Retain { value });
    }

    fn release(&mut self, out: &mut Vec<Instruction>, value: Operand) {
        self.stats.releases += 1;
        out.push(Instruction::Release { value });
    }

    fn owning_destinations(&self, place: &Place) -> Vec<Operand> {
        if !owning(place) {
            return Vec::new();
        }
        self.place_type(place)
            .map(|type_| self.leaf_operands(place, &type_))
            .unwrap_or_default()
    }

    fn consumed_and_destinations(
        &self,
        instruction: &Instruction,
    ) -> (Vec<Operand>, Vec<Operand>, bool) {
        let mut sources: Vec<&Operand> = Vec::new();
        let mut destination: Option<&Place> = None;
        let mut borrowed_result = false;
        match instruction {
            Instruction::Assign { target, value } => {
                destination = Some(target);
                match &value.kind {
                    RvalueKind::Use(operand)
                    | RvalueKind::Cast(operand)
                    | RvalueKind::MakeInterface {
                        object: operand, ..
                    } => sources.push(operand),
                    RvalueKind::Aggregate(fields) | RvalueKind::EnumConstruct { fields, .. } => {
                        sources.extend(fields.iter().map(|field| &field.value));
                    }
                    _ => {}
                }
            }
            Instruction::Call { destination: d, .. }
            | Instruction::CallInterface { destination: d, .. } => destination = d.as_ref(),
            Instruction::ForeignCall {
                destination: d,
                arguments,
                ..
            } => {
                destination = d.as_ref();
                sources.extend(arguments);
            }
            Instruction::CallIntrinsic {
                destination: d,
                arguments,
                return_type,
                intrinsic,
                ..
            } => {
                destination = d.as_ref();
                // An array or string result is a fresh object owned by the
                // destination, unless the intrinsic returns a value that is
                // already stored somewhere (design §9).
                borrowed_result = returns_stored_value(intrinsic)
                    || !matches!(return_type, Type::Array(_) | Type::String);
                // Arguments are borrowed; only stored operands are consumed.
                sources.extend(
                    arguments
                        .iter()
                        .enumerate()
                        .filter(|(position, _)| stores_argument(intrinsic, *position))
                        .map(|(_, argument)| argument),
                );
            }
            Instruction::AllocateObject { destination: d, .. }
            | Instruction::AllocateArray { destination: d, .. }
            | Instruction::AllocateList { destination: d, .. }
            | Instruction::AllocateDictionary { destination: d, .. }
            | Instruction::ListToArray { destination: d, .. }
            | Instruction::DictionaryKeys { destination: d, .. }
            | Instruction::DictionaryValues { destination: d, .. }
            | Instruction::StringBuilderToString { destination: d, .. }
            | Instruction::DictionaryEntries { destination: d, .. } => destination = Some(d),
            Instruction::ListAdd { value, .. } | Instruction::ListSet { value, .. } => {
                sources.push(value);
            }
            Instruction::ListGet { destination: d, .. }
            | Instruction::DictionaryTryGet { destination: d, .. } => {
                destination = Some(d);
                borrowed_result = true;
            }
            Instruction::DictionaryAdd { key, value, .. }
            | Instruction::DictionarySet { key, value, .. } => sources.extend([key, value]),
            _ => {}
        }
        let consumed = sources
            .into_iter()
            .flat_map(|operand| self.operand_leaves(operand))
            .collect();
        let destinations = destination
            .map(|place| self.owning_destinations(place))
            .unwrap_or_default();
        (consumed, destinations, borrowed_result)
    }

    fn rewrite_instruction(&mut self, instruction: Instruction, out: &mut Vec<Instruction>) {
        // Region rewinds in the middle of a function would leave counted
        // locals pointing into reclaimed memory; removal only stops reclamation.
        if matches!(
            instruction,
            Instruction::OwnedRegionEnter { .. }
                | Instruction::OwnedRegionExit { .. }
                | Instruction::TemporarySubregionEnter { .. }
                | Instruction::TemporarySubregionExit { .. }
        ) {
            return;
        }
        let (consumed, destinations, borrowed_result) =
            self.consumed_and_destinations(&instruction);
        for operand in consumed {
            self.retain(out, operand);
        }
        let mut captured = Vec::new();
        for destination in &destinations {
            let temporary = self.fresh(&destination.type_, "$own.old");
            captured.push(copy(Place::Local(temporary), &destination.type_));
            out.push(Instruction::Assign {
                target: Place::Local(temporary),
                value: Rvalue {
                    type_: destination.type_.clone(),
                    kind: RvalueKind::Use(destination.clone()),
                },
            });
        }
        out.push(instruction);
        if borrowed_result {
            for destination in destinations {
                self.retain(out, destination);
            }
        }
        for old in captured {
            self.release(out, old);
        }
    }

    fn release_slots(&mut self, out: &mut Vec<Instruction>) {
        // `self.function` is the pre-pass function, so pass temporaries never appear here.
        let function = self.function;
        for local in function.parameters.iter().chain(&function.locals) {
            for slot in self.leaf_operands(&Place::Local(local.id), &local.type_) {
                self.release(out, slot);
            }
        }
    }

    fn rewrite_terminator(
        &mut self,
        terminator: Terminator,
        out: &mut Vec<Instruction>,
    ) -> Terminator {
        match terminator {
            // Any copied operand is read before the exit releases: even a
            // scalar such as `this.value` reads through a reference.
            Terminator::Return(Some(operand))
                if matches!(operand.kind, OperandKind::Copy(_))
                    || !leaves(self.module, self.native, &operand.type_).is_empty() =>
            {
                let result = self.fresh(&operand.type_, "$own.ret");
                out.push(Instruction::Assign {
                    target: Place::Local(result),
                    value: Rvalue {
                        type_: operand.type_.clone(),
                        kind: RvalueKind::Use(operand.clone()),
                    },
                });
                for leaf in self.leaf_operands(&Place::Local(result), &operand.type_) {
                    self.retain(out, leaf);
                }
                self.release_slots(out);
                Terminator::Return(Some(copy(Place::Local(result), &operand.type_)))
            }
            terminator @ (Terminator::Return(_) | Terminator::End) => {
                self.release_slots(out);
                terminator
            }
            other => other,
        }
    }

    fn rewrite_function(&mut self, function: &mut Function) {
        let blocks = std::mem::take(&mut function.blocks);
        function.blocks = blocks
            .into_iter()
            .map(|block| {
                let mut instructions = Vec::new();
                for instruction in block.instructions {
                    self.rewrite_instruction(instruction, &mut instructions);
                }
                let terminator = self.rewrite_terminator(block.terminator, &mut instructions);
                BasicBlock {
                    id: block.id,
                    instructions,
                    terminator,
                }
            })
            .collect();
        let parameter_slots: Vec<Operand> = function
            .parameters
            .iter()
            .flat_map(|parameter| self.leaf_operands(&Place::Local(parameter.id), &parameter.type_))
            .collect();
        if !parameter_slots.is_empty() {
            let id = BasicBlockId(next_id(function.blocks.iter().map(|block| block.id.0)));
            let mut instructions = Vec::new();
            for slot in parameter_slots {
                self.retain(&mut instructions, slot);
            }
            function.blocks.insert(
                0,
                BasicBlock {
                    id,
                    instructions,
                    terminator: Terminator::Goto(function.entry),
                },
            );
            function.entry = id;
        }
        function.locals.append(&mut self.new_locals);
    }
}

/// Forward "definitely null" analysis: for each block (by index) the locals
/// that were never written on any path from the function entry. `None`
/// marks a block unreachable from the entry.
fn null_entry_states(function: &Function) -> Vec<Option<HashSet<LocalId>>> {
    let index: HashMap<BasicBlockId, usize> = function
        .blocks
        .iter()
        .enumerate()
        .map(|(position, block)| (block.id, position))
        .collect();
    let parameters: HashSet<LocalId> = function.parameters.iter().map(|p| p.id).collect();
    let mut states: Vec<Option<HashSet<LocalId>>> = vec![None; function.blocks.len()];
    if let Some(&entry) = index.get(&function.entry) {
        states[entry] = Some(
            function
                .locals
                .iter()
                .map(|local| local.id)
                .filter(|id| !parameters.contains(id))
                .collect(),
        );
    }
    loop {
        let mut changed = false;
        for (position, block) in function.blocks.iter().enumerate() {
            let Some(mut state) = states[position].clone() else {
                continue;
            };
            for instruction in &block.instructions {
                for place in destination_places(instruction) {
                    if let Some(local) = root_local(place) {
                        state.remove(&local);
                    }
                }
            }
            for target in successor_blocks(&block.terminator) {
                let Some(&next) = index.get(&target) else {
                    continue;
                };
                match &mut states[next] {
                    None => {
                        states[next] = Some(state.clone());
                        changed = true;
                    }
                    Some(existing) => {
                        let before = existing.len();
                        existing.retain(|local| state.contains(local));
                        changed |= existing.len() != before;
                    }
                }
            }
        }
        if !changed {
            return states;
        }
    }
}

/// E1: drop a capture of a local leaf that is definitely null, and its release.
fn elide_null_captures(function: &mut Function, captures: &HashSet<LocalId>) -> Removed {
    let states = null_entry_states(function);
    let mut removed = Removed::default();
    for (block, entry) in function.blocks.iter_mut().zip(states) {
        let mut state = entry.unwrap_or_default();
        let mut elided = HashSet::new();
        let mut kept = Vec::with_capacity(block.instructions.len());
        for instruction in std::mem::take(&mut block.instructions) {
            if let Instruction::Assign {
                target: Place::Local(temporary),
                value:
                    Rvalue {
                        kind:
                            RvalueKind::Use(Operand {
                                kind: OperandKind::Copy(source),
                                ..
                            }),
                        ..
                    },
            } = &instruction
                && captures.contains(temporary)
                && root_local(source).is_some_and(|local| state.contains(&local))
            {
                elided.insert(*temporary);
                continue;
            }
            if elided
                .iter()
                .any(|temporary| is_release_of(&instruction, *temporary))
            {
                removed.releases += 1;
                continue;
            }
            for place in destination_places(&instruction) {
                if let Some(local) = root_local(place) {
                    state.remove(&local);
                }
            }
            kept.push(instruction);
        }
        block.instructions = kept;
    }
    removed
}

impl Context<'_> {
    /// Shape check on the pre-pass function: `local` is written once, read
    /// once, and the read is a copy into an owning place that does not itself
    /// mention `local`.
    fn single_use_temporary(&self, local: LocalId) -> bool {
        let mut writes = 0;
        let mut readers = 0;
        let mut owning_copy = false;
        for block in &self.function.blocks {
            for instruction in &block.instructions {
                if destination_places(instruction)
                    .iter()
                    .any(|place| root_local(place) == Some(local))
                {
                    writes += 1;
                }
                let mut reads = HashSet::new();
                owned_instruction_reads(instruction, &mut reads);
                if reads.contains(&local) {
                    readers += 1;
                    if let Some(target) = copy_target(instruction, local)
                        && owning(target)
                    {
                        let mut target_reads = HashSet::new();
                        owned_place_reads(target, &mut target_reads);
                        owning_copy = !target_reads.contains(&local);
                    }
                }
            }
            let mut reads = HashSet::new();
            match &block.terminator {
                Terminator::Branch { condition, .. } => owned_operand_reads(condition, &mut reads),
                Terminator::Return(Some(operand)) => owned_operand_reads(operand, &mut reads),
                _ => {}
            }
            if reads.contains(&local) {
                return false;
            }
        }
        writes == 1 && readers == 1 && owning_copy
    }

    /// E2: for a single-use lowering temporary `t` copied into an owning
    /// destination later in the write's block, drop the copy's retain, the
    /// exit releases of `t` and the capture of `t` at its write.
    fn elide_moves(&self, function: &mut Function, captures: &HashSet<LocalId>) -> Removed {
        let mut removed = Removed::default();
        for candidate in &self.function.locals {
            let found = leaves(self.module, self.native, &candidate.type_);
            if !candidate.temporary
                || found.len() != 1
                || !found[0].0.is_empty()
                || !self.single_use_temporary(candidate.id)
            {
                continue;
            }
            Self::move_temporary(function, captures, candidate.id, &mut removed);
        }
        removed
    }

    fn move_temporary(
        function: &mut Function,
        captures: &HashSet<LocalId>,
        local: LocalId,
        removed: &mut Removed,
    ) {
        let is_capture = |instruction: &Instruction| {
            matches!(
                instruction,
                Instruction::Assign { target: Place::Local(id), .. } if captures.contains(id)
            )
        };
        let mut writer = None;
        let mut copy_site = None;
        let mut retains = Vec::new();
        for (block_index, block) in function.blocks.iter().enumerate() {
            for (index, instruction) in block.instructions.iter().enumerate() {
                if destination_places(instruction)
                    .iter()
                    .any(|place| root_local(place) == Some(local))
                {
                    if writer.is_some() {
                        return;
                    }
                    writer = Some((block_index, index));
                }
                if let Some(target) = copy_target(instruction, local)
                    && !matches!(target, Place::Local(id) if captures.contains(id))
                {
                    if copy_site.is_some() {
                        return;
                    }
                    copy_site = Some((block_index, index));
                }
                if matches!(
                    instruction,
                    Instruction::Retain {
                        value: Operand { kind: OperandKind::Copy(Place::Local(id)), .. }
                    } if *id == local
                ) {
                    retains.push((block_index, index));
                }
            }
        }
        let (Some((writer_block, writer_index)), Some((copy_block, copy_index))) =
            (writer, copy_site)
        else {
            return;
        };
        let [(retain_block, retain_index)] = retains[..] else {
            return;
        };
        if writer_block != copy_block
            || retain_block != copy_block
            || !(writer_index < retain_index && retain_index < copy_index)
            || !function.blocks[copy_block].instructions[retain_index + 1..copy_index]
                .iter()
                .all(is_capture)
        {
            return;
        }
        let capture_temporaries: HashSet<LocalId> = function.blocks[writer_block].instructions
            [..writer_index]
            .iter()
            .filter_map(|instruction| match instruction {
                Instruction::Assign {
                    target: Place::Local(temporary),
                    ..
                } if captures.contains(temporary) && copy_target(instruction, local).is_some() => {
                    Some(*temporary)
                }
                _ => None,
            })
            .collect();
        for (block_index, block) in function.blocks.iter_mut().enumerate() {
            let mut kept = Vec::with_capacity(block.instructions.len());
            for (index, instruction) in std::mem::take(&mut block.instructions)
                .into_iter()
                .enumerate()
            {
                if block_index == retain_block && index == retain_index {
                    removed.retains += 1;
                } else if is_release_of(&instruction, local)
                    || capture_temporaries
                        .iter()
                        .any(|temporary| is_release_of(&instruction, *temporary))
                {
                    removed.releases += 1;
                } else if !(block_index == writer_block
                    && index < writer_index
                    && matches!(
                        &instruction,
                        Instruction::Assign { target: Place::Local(temporary), .. }
                            if capture_temporaries.contains(temporary)
                    ))
                {
                    kept.push(instruction);
                }
            }
            block.instructions = kept;
        }
    }

    /// E3: in a function that can release nothing but its own parameters,
    /// drop the entry retain and exit release of every parameter.
    fn elide_parameters(&self, function: &mut Function) -> Removed {
        let mut removed = Removed::default();
        let parameters: HashSet<LocalId> = function.parameters.iter().map(|p| p.id).collect();
        if !function
            .parameters
            .iter()
            .any(|p| !leaves(self.module, self.native, &p.type_).is_empty())
        {
            return removed;
        }
        let qualifies = function
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .all(|instruction| {
                !is_call(instruction)
                    && !matches!(
                        instruction,
                        Instruction::ListSet { .. }
                            | Instruction::ListRemoveAt { .. }
                            | Instruction::ListClear { .. }
                            | Instruction::DictionaryAdd { .. }
                            | Instruction::DictionarySet { .. }
                            | Instruction::DictionaryRemove { .. }
                            | Instruction::DictionaryClear { .. }
                    )
                    && !destination_places(instruction)
                        .into_iter()
                        .any(stores_into_array)
                    && destination_places(instruction)
                        .iter()
                        .all(|place| root_local(place).is_none_or(|l| !parameters.contains(&l)))
                    && match instruction {
                        Instruction::Release { value } => matches!(
                            &value.kind,
                            OperandKind::Copy(place)
                                if root_local(place).is_some_and(|l| parameters.contains(&l))
                        ),
                        _ => true,
                    }
            });
        let Some(position) = function
            .blocks
            .iter()
            .position(|block| block.id == function.entry)
        else {
            return removed;
        };
        let entry = &function.blocks[position];
        let Terminator::Goto(next) = entry.terminator else {
            return removed;
        };
        if !qualifies
            || !entry
                .instructions
                .iter()
                .all(|instruction| matches!(instruction, Instruction::Retain { .. }))
        {
            return removed;
        }
        removed.retains += entry.instructions.len();
        function.entry = next;
        function.blocks.remove(position);
        for block in &mut function.blocks {
            let before = block.instructions.len();
            block
                .instructions
                .retain(|instruction| !matches!(instruction, Instruction::Release { .. }));
            removed.releases += before - block.instructions.len();
        }
        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validate::ownership::validate_ownership;
    use mir::{FieldDefinition, Visibility};

    fn class() -> Type {
        Type::Class(SymbolId(10))
    }

    fn iface() -> Type {
        Type::Interface(SymbolId(30))
    }

    fn struct_type() -> Type {
        Type::User(SymbolId(20))
    }

    fn int_array() -> Type {
        Type::Array(Box::new(Type::Int))
    }

    fn int_list() -> Type {
        Type::List(Box::new(Type::Int))
    }

    fn int_dictionary() -> Type {
        Type::Dictionary(Box::new(Type::Int), Box::new(Type::Int))
    }

    fn class_dictionary() -> Type {
        Type::Dictionary(Box::new(class()), Box::new(class()))
    }

    fn dictionary_field(id: u32) -> Place {
        Place::ObjectField {
            object: Box::new(copy(lp(id), &class())),
            field: SymbolId(14),
        }
    }

    fn allocate_dictionary(destination: u32) -> Instruction {
        Instruction::AllocateDictionary {
            destination: lp(destination),
            key_type: Type::Int,
            value_type: Type::Int,
            region: mir::AllocationRegion::Persistent,
        }
    }

    fn list_field(id: u32) -> Place {
        Place::ObjectField {
            object: Box::new(copy(lp(id), &class())),
            field: SymbolId(13),
        }
    }

    fn allocate_list(destination: u32) -> Instruction {
        Instruction::AllocateList {
            destination: lp(destination),
            element_type: Type::Int,
            region: mir::AllocationRegion::Persistent,
        }
    }

    fn class_array() -> Type {
        Type::Array(Box::new(class()))
    }

    fn array_struct() -> Type {
        Type::User(SymbolId(22))
    }

    fn element(array: u32, element_type: &Type) -> Place {
        Place::Index {
            array: Box::new(copy(
                lp(array),
                &Type::Array(Box::new(element_type.clone())),
            )),
            index: Box::new(zero()),
            element_type: element_type.clone(),
            bounds: mir::ArrayBounds::Checked,
        }
    }

    fn allocate_array(destination: u32, element_type: &Type) -> Instruction {
        Instruction::AllocateArray {
            destination: lp(destination),
            element_type: element_type.clone(),
            length: zero(),
            initialization: mir::ArrayInitialization::Default,
            region: mir::AllocationRegion::Persistent,
        }
    }

    fn local(id: u32, type_: Type, temporary: bool) -> Local {
        Local {
            id: LocalId(id),
            symbol: None,
            name: format!("l{id}"),
            type_,
            mutable: true,
            temporary,
        }
    }

    fn lp(id: u32) -> Place {
        Place::Local(LocalId(id))
    }

    fn field(id: u32) -> Place {
        Place::ObjectField {
            object: Box::new(copy(lp(id), &class())),
            field: SymbolId(11),
        }
    }

    fn assign(target: Place, type_: &Type, source: Operand) -> Instruction {
        Instruction::Assign {
            target,
            value: Rvalue {
                type_: type_.clone(),
                kind: RvalueKind::Use(source),
            },
        }
    }

    fn retain(place: Place, type_: &Type) -> Instruction {
        Instruction::Retain {
            value: copy(place, type_),
        }
    }

    fn release(place: Place, type_: &Type) -> Instruction {
        Instruction::Release {
            value: copy(place, type_),
        }
    }

    fn field_definition(symbol: u32, type_: Type) -> FieldDefinition {
        FieldDefinition {
            symbol: SymbolId(symbol),
            name: format!("f{symbol}"),
            type_,
        }
    }

    fn run(
        parameters: Vec<Local>,
        locals: Vec<Local>,
        instructions: Vec<Instruction>,
        terminator: Terminator,
        return_type: Type,
    ) -> (Function, OwnershipStats) {
        let module = Module {
            structs: vec![
                mir::StructDefinition {
                    symbol: SymbolId(20),
                    name: "S".into(),
                    fields: vec![field_definition(21, class())],
                },
                mir::StructDefinition {
                    symbol: SymbolId(22),
                    name: "A".into(),
                    fields: vec![field_definition(23, int_array())],
                },
            ],
            classes: vec![mir::ClassDefinition {
                symbol: SymbolId(10),
                name: "C".into(),
                fields: vec![
                    field_definition(11, class()),
                    field_definition(12, Type::String),
                    field_definition(13, int_list()),
                    field_definition(14, int_dictionary()),
                ],
            }],
            interfaces: Vec::new(),
            enums: Vec::new(),
            interface_implementations: Vec::new(),
            foreign_functions: Vec::new(),
            functions: vec![Function {
                constructor: false,
                symbol: SymbolId(1),
                owner: None,
                name: "f".into(),
                visibility: Visibility::Public,
                parameters,
                locals,
                return_type,
                entry: BasicBlockId(0),
                blocks: vec![BasicBlock {
                    id: BasicBlockId(0),
                    instructions,
                    terminator,
                }],
                temporary_subregion_candidates: Vec::new(),
            }],
        };
        let (output, stats) = insert_ownership(&module);
        assert_eq!(validate_ownership(&output), Ok(()), "{output:#?}");
        (output.functions.into_iter().next().unwrap(), stats)
    }

    fn run_void(
        parameters: Vec<Local>,
        locals: Vec<Local>,
        instructions: Vec<Instruction>,
    ) -> (Function, OwnershipStats) {
        run(
            parameters,
            locals,
            instructions,
            Terminator::Return(None),
            Type::Void,
        )
    }

    struct Both {
        c5: Function,
        c5_stats: OwnershipStats,
        elided: Function,
        elided_stats: OwnershipStats,
    }

    fn block(id: u32, instructions: Vec<Instruction>, terminator: Terminator) -> BasicBlock {
        BasicBlock {
            id: BasicBlockId(id),
            instructions,
            terminator,
        }
    }

    fn counts(function: &Function) -> (usize, usize) {
        let instructions = || function.blocks.iter().flat_map(|b| &b.instructions);
        (
            instructions()
                .filter(|i| matches!(i, Instruction::Retain { .. }))
                .count(),
            instructions()
                .filter(|i| matches!(i, Instruction::Release { .. }))
                .count(),
        )
    }

    /// Run C5 and C6 on one function; both outputs must validate and the
    /// reported `retains`/`releases` must match what remains in each output.
    fn both(
        parameters: Vec<Local>,
        locals: Vec<Local>,
        blocks: Vec<BasicBlock>,
        return_type: Type,
    ) -> Both {
        let module = Module {
            structs: vec![
                mir::StructDefinition {
                    symbol: SymbolId(20),
                    name: "S".into(),
                    fields: vec![field_definition(21, class())],
                },
                mir::StructDefinition {
                    symbol: SymbolId(22),
                    name: "A".into(),
                    fields: vec![field_definition(23, int_array())],
                },
            ],
            classes: vec![mir::ClassDefinition {
                symbol: SymbolId(10),
                name: "C".into(),
                fields: vec![
                    field_definition(11, class()),
                    field_definition(12, Type::String),
                    field_definition(13, int_list()),
                    field_definition(14, int_dictionary()),
                ],
            }],
            interfaces: Vec::new(),
            enums: Vec::new(),
            interface_implementations: Vec::new(),
            foreign_functions: Vec::new(),
            functions: vec![Function {
                constructor: false,
                symbol: SymbolId(1),
                owner: None,
                name: "f".into(),
                visibility: Visibility::Public,
                parameters,
                locals,
                return_type,
                entry: BasicBlockId(0),
                blocks,
                temporary_subregion_candidates: Vec::new(),
            }],
        };
        let (c5, c5_stats) = insert_ownership(&module);
        assert_eq!(validate_ownership(&c5), Ok(()), "C5: {c5:#?}");
        let (elided, elided_stats) = insert_ownership_elided(&module);
        assert_eq!(validate_ownership(&elided), Ok(()), "C6: {elided:#?}");
        let c5 = c5.functions.into_iter().next().unwrap();
        let elided = elided.functions.into_iter().next().unwrap();
        assert_eq!((c5_stats.retains, c5_stats.releases), counts(&c5));
        assert_eq!(
            (elided_stats.retains, elided_stats.releases),
            counts(&elided)
        );
        assert_eq!(
            c5_stats.retains + c5_stats.releases,
            elided_stats.retains
                + elided_stats.releases
                + elided_stats.elided_null_captures
                + elided_stats.elided_moves
                + elided_stats.elided_parameters
        );
        Both {
            c5,
            c5_stats,
            elided,
            elided_stats,
        }
    }

    fn call(destination: Option<Place>, return_type: Type) -> Instruction {
        Instruction::Call {
            destination,
            function: SymbolId(2),
            arguments: Vec::new(),
            return_type,
        }
    }

    fn branch(then_block: u32, else_block: u32) -> Terminator {
        Terminator::Branch {
            condition: Operand {
                type_: Type::Bool,
                kind: OperandKind::Constant(mir::Constant::Boolean(true)),
            },
            then_block: BasicBlockId(then_block),
            else_block: BasicBlockId(else_block),
        }
    }

    #[test]
    fn e3_getter_elides_parameter_count_operations() {
        let result = both(
            vec![local(0, class(), false)],
            Vec::new(),
            vec![block(
                0,
                Vec::new(),
                Terminator::Return(Some(copy(field(0), &class()))),
            )],
            class(),
        );
        assert_eq!(
            result.c5_stats,
            OwnershipStats {
                retains: 2,
                releases: 1,
                ..OwnershipStats::default()
            }
        );
        assert_eq!(
            result.elided_stats,
            OwnershipStats {
                retains: 1,
                releases: 0,
                elided_parameters: 2,
                ..OwnershipStats::default()
            }
        );
        assert_eq!(result.elided.entry, BasicBlockId(0));
        assert_eq!(result.elided.blocks.len(), 1);
        assert_eq!(
            result.elided.blocks[0].instructions,
            vec![
                assign(lp(1), &class(), copy(field(0), &class())),
                retain(lp(1), &class()),
            ]
        );
    }

    #[test]
    fn e3_does_not_apply_to_a_function_with_a_call() {
        let result = both(
            vec![local(0, class(), false)],
            Vec::new(),
            vec![block(
                0,
                vec![call(None, Type::Void)],
                Terminator::Return(None),
            )],
            Type::Void,
        );
        assert_eq!(result.elided_stats.elided_parameters, 0);
        assert_eq!(result.elided, result.c5);
    }

    #[test]
    fn e1_call_result_into_a_never_written_local_drops_the_capture() {
        let result = both(
            Vec::new(),
            vec![local(0, class(), false)],
            vec![block(
                0,
                vec![call(Some(lp(0)), class())],
                Terminator::Return(None),
            )],
            Type::Void,
        );
        assert_eq!(
            result.c5.blocks[0].instructions,
            vec![
                assign(lp(1), &class(), copy(lp(0), &class())),
                call(Some(lp(0)), class()),
                release(lp(1), &class()),
                release(lp(0), &class()),
            ]
        );
        assert_eq!(
            result.elided.blocks[0].instructions,
            vec![call(Some(lp(0)), class()), release(lp(0), &class())]
        );
        assert_eq!(
            result.elided_stats,
            OwnershipStats {
                retains: 0,
                releases: 1,
                elided_null_captures: 1,
                ..OwnershipStats::default()
            }
        );
    }

    #[test]
    fn e2_move_from_a_lowering_temporary_drops_retain_and_exit_release() {
        // t = this.f; x = t
        let result = both(
            vec![local(0, class(), false)],
            vec![local(1, class(), true), local(2, class(), false)],
            vec![block(
                0,
                vec![
                    assign(lp(1), &class(), copy(field(0), &class())),
                    assign(lp(2), &class(), copy(lp(1), &class())),
                ],
                Terminator::Return(None),
            )],
            Type::Void,
        );
        assert_eq!(
            result.c5_stats,
            OwnershipStats {
                retains: 3,
                releases: 5,
                ..OwnershipStats::default()
            }
        );
        assert_eq!(
            result.elided_stats,
            OwnershipStats {
                retains: 2,
                releases: 2,
                elided_null_captures: 2,
                elided_moves: 2,
                elided_parameters: 0,
            }
        );
        assert_eq!(
            result.elided.blocks[1].instructions,
            vec![
                retain(field(0), &class()),
                assign(lp(1), &class(), copy(field(0), &class())),
                assign(lp(2), &class(), copy(lp(1), &class())),
                release(lp(0), &class()),
                release(lp(2), &class()),
            ]
        );
    }

    #[test]
    fn e2_in_a_loop_also_drops_the_capture_of_the_moved_temporary() {
        let result = both(
            vec![local(0, class(), false)],
            vec![local(1, class(), true), local(2, class(), false)],
            vec![
                block(0, Vec::new(), Terminator::Goto(BasicBlockId(1))),
                block(
                    1,
                    vec![
                        assign(lp(1), &class(), copy(field(0), &class())),
                        assign(lp(2), &class(), copy(lp(1), &class())),
                    ],
                    branch(1, 2),
                ),
                block(2, Vec::new(), Terminator::Return(None)),
            ],
            Type::Void,
        );
        // No capture is null at the loop header (E1 does not apply); E2
        // removes the copy's retain, the exit release of t, and t's capture.
        assert_eq!(result.elided_stats.elided_null_captures, 0);
        assert_eq!(result.elided_stats.elided_moves, 3);
    }

    #[test]
    fn e1_does_not_apply_after_the_first_write_inside_a_loop() {
        // B0 -> B1 (loop) -> B2: x = p0 is written on every iteration.
        let result = both(
            vec![local(0, class(), false)],
            vec![local(1, class(), false)],
            vec![
                block(0, Vec::new(), Terminator::Goto(BasicBlockId(1))),
                block(
                    1,
                    vec![assign(lp(1), &class(), copy(lp(0), &class()))],
                    branch(1, 2),
                ),
                block(2, Vec::new(), Terminator::Return(None)),
            ],
            Type::Void,
        );
        assert_eq!(result.elided_stats.elided_null_captures, 0);
        assert_eq!(result.elided, result.c5);
        // A write before the loop makes the in-loop capture non-null, while
        // the first write itself still is a null capture.
        let result = both(
            vec![local(0, class(), false)],
            vec![local(1, class(), false)],
            vec![
                block(
                    0,
                    vec![assign(lp(1), &class(), copy(lp(0), &class()))],
                    Terminator::Goto(BasicBlockId(1)),
                ),
                block(
                    1,
                    vec![assign(lp(1), &class(), copy(lp(0), &class()))],
                    branch(1, 2),
                ),
                block(2, Vec::new(), Terminator::Return(None)),
            ],
            Type::Void,
        );
        assert_eq!(result.elided_stats.elided_null_captures, 1);
    }

    #[test]
    fn local_assignment_captures_and_releases_old_value() {
        let (function, stats) = run_void(
            vec![local(0, class(), false)],
            vec![local(1, class(), false)],
            vec![assign(lp(1), &class(), copy(lp(0), &class()))],
        );
        assert_eq!(function.entry, BasicBlockId(1));
        assert_eq!(
            function.blocks[0].instructions,
            vec![retain(lp(0), &class())]
        );
        assert_eq!(
            function.blocks[1].instructions,
            vec![
                retain(lp(0), &class()),
                assign(lp(2), &class(), copy(lp(1), &class())),
                assign(lp(1), &class(), copy(lp(0), &class())),
                release(lp(2), &class()),
                release(lp(0), &class()),
                release(lp(1), &class()),
            ]
        );
        assert!(function.locals[1].temporary);
        assert_eq!(
            stats,
            OwnershipStats {
                retains: 2,
                releases: 3,
                ..OwnershipStats::default()
            }
        );
    }

    #[test]
    fn self_assignment_of_field_retains_before_capture() {
        let (function, _) = run_void(
            vec![local(0, class(), false)],
            Vec::new(),
            vec![assign(field(0), &class(), copy(field(0), &class()))],
        );
        assert_eq!(
            function.blocks[1].instructions,
            vec![
                retain(field(0), &class()),
                assign(lp(1), &class(), copy(field(0), &class())),
                assign(field(0), &class(), copy(field(0), &class())),
                release(lp(1), &class()),
                release(lp(0), &class()),
            ]
        );
    }

    #[test]
    fn field_alias_assignment_validates() {
        run_void(
            vec![local(0, class(), false), local(1, class(), false)],
            Vec::new(),
            vec![assign(field(0), &class(), copy(field(1), &class()))],
        );
    }

    #[test]
    fn field_overwrite_with_call_result_validates() {
        let (function, _) = run_void(
            vec![local(0, class(), false)],
            Vec::new(),
            vec![Instruction::Call {
                destination: Some(field(0)),
                function: SymbolId(2),
                arguments: Vec::new(),
                return_type: class(),
            }],
        );
        let retains = function.blocks[1]
            .instructions
            .iter()
            .filter(|i| matches!(i, Instruction::Retain { .. }))
            .count();
        assert_eq!(retains, 0);
    }

    #[test]
    fn returning_a_parameter_retains_the_result_local() {
        run(
            vec![local(0, class(), false)],
            Vec::new(),
            Vec::new(),
            Terminator::Return(Some(copy(lp(0), &class()))),
            class(),
        );
    }

    #[test]
    fn returning_a_field_load_validates() {
        run(
            vec![local(0, class(), false)],
            Vec::new(),
            Vec::new(),
            Terminator::Return(Some(copy(field(0), &class()))),
            class(),
        );
    }

    #[test]
    fn struct_local_with_class_field_copy_validates() {
        let (function, _) = run_void(
            vec![local(0, struct_type(), false)],
            vec![local(1, struct_type(), false)],
            vec![assign(lp(1), &struct_type(), copy(lp(0), &struct_type()))],
        );
        let leaf = Place::Field {
            base: Box::new(lp(0)),
            field: SymbolId(21),
        };
        assert_eq!(
            function.blocks[0].instructions,
            vec![retain(leaf, &class())]
        );
    }

    #[test]
    fn interface_value_from_make_interface_validates() {
        run(
            vec![local(0, class(), false)],
            vec![local(1, iface(), false)],
            vec![Instruction::Assign {
                target: lp(1),
                value: Rvalue {
                    type_: iface(),
                    kind: RvalueKind::MakeInterface {
                        object: copy(lp(0), &class()),
                        class: SymbolId(10),
                        interface: SymbolId(30),
                    },
                },
            }],
            Terminator::Return(Some(copy(lp(1), &iface()))),
            iface(),
        );
    }

    #[test]
    fn list_add_of_class_value_consumes_a_retain() {
        let list = Type::List(Box::new(class()));
        let (function, _) = run_void(
            vec![local(0, list.clone(), false), local(1, class(), false)],
            Vec::new(),
            vec![Instruction::ListAdd {
                list: copy(lp(0), &list),
                value: copy(lp(1), &class()),
            }],
        );
        assert_eq!(function.blocks[1].instructions[0], retain(lp(1), &class()));
    }

    #[test]
    fn call_argument_object_field_load_is_borrowed() {
        let (function, _) = run_void(
            vec![local(0, class(), false)],
            Vec::new(),
            vec![Instruction::Call {
                destination: None,
                function: SymbolId(2),
                arguments: vec![copy(field(0), &class())],
                return_type: Type::Void,
            }],
        );
        assert_eq!(function.blocks[1].instructions.len(), 2);
    }

    fn list_instructions(ints: Operand) -> (Instruction, Instruction) {
        let list = Type::List(Box::new(class()));
        (
            Instruction::ListGet {
                destination: lp(2),
                list: copy(lp(0), &list),
                index: ints.clone(),
                element_type: class(),
            },
            Instruction::ListSet {
                list: copy(lp(0), &list),
                index: ints,
                value: copy(lp(1), &class()),
            },
        )
    }

    fn zero() -> Operand {
        Operand {
            type_: Type::Int,
            kind: OperandKind::Constant(mir::Constant::Integer("0".into())),
        }
    }

    #[test]
    fn list_get_is_retained_right_after_the_read_and_survives_list_set() {
        let list = Type::List(Box::new(class()));
        let (get, set) = list_instructions(zero());
        let (function, _) = run_void(
            vec![local(0, list, false), local(1, class(), false)],
            vec![local(2, class(), false)],
            vec![get.clone(), set, call(None, Type::Void)],
        );
        let all: Vec<&Instruction> = function
            .blocks
            .iter()
            .flat_map(|b| &b.instructions)
            .collect();
        let at = all.iter().position(|i| **i == get).unwrap();
        assert_eq!(*all[at + 1], retain(lp(2), &class()));
    }

    #[test]
    fn e3_does_not_apply_to_a_function_with_list_set() {
        let list = Type::List(Box::new(class()));
        let (_, set) = list_instructions(zero());
        let result = both(
            vec![local(0, list, false), local(1, class(), false)],
            Vec::new(),
            vec![block(
                0,
                vec![set],
                Terminator::Return(Some(copy(field(1), &class()))),
            )],
            class(),
        );
        assert_eq!(result.elided_stats.elided_parameters, 0);
    }

    fn is_capture_of(instruction: &Instruction, source: &Place) -> bool {
        matches!(
            instruction,
            Instruction::Assign {
                value: Rvalue {
                    kind: RvalueKind::Use(Operand { kind: OperandKind::Copy(place), .. }),
                    ..
                },
                ..
            } if place == source
        )
    }

    fn body(function: &Function) -> &[Instruction] {
        &function.blocks.last().unwrap().instructions
    }

    #[test]
    fn array_local_reassigned_in_a_loop_releases_the_old_array() {
        let result = both(
            Vec::new(),
            vec![local(0, int_array(), false)],
            vec![
                block(0, Vec::new(), Terminator::Goto(BasicBlockId(1))),
                block(1, vec![allocate_array(0, &Type::Int)], branch(1, 2)),
                block(2, Vec::new(), Terminator::Return(None)),
            ],
            Type::Void,
        );
        // capture, allocate, release the capture; the exit releases the local.
        let loop_body = &result.c5.blocks[1].instructions;
        assert!(is_capture_of(&loop_body[0], &lp(0)), "{loop_body:#?}");
        assert_eq!(loop_body[1], allocate_array(0, &Type::Int));
        assert_eq!(loop_body[2], release(lp(1), &int_array()));
        // Fresh result: no retain anywhere.
        assert_eq!(counts(&result.c5).0, 0);
        assert_eq!(result.c5_stats.releases, 2);
    }

    #[test]
    fn array_element_overwrite_with_a_new_object_releases_the_old_element() {
        let target = element(0, &class());
        let (function, stats) = run_void(
            vec![local(0, class_array(), false)],
            vec![local(1, class(), false)],
            vec![
                Instruction::AllocateObject {
                    destination: lp(1),
                    class: SymbolId(10),
                    region: mir::AllocationRegion::Persistent,
                },
                assign(target.clone(), &class(), copy(lp(1), &class())),
            ],
        );
        let instructions = body(&function);
        // The allocation is owned (capture of the null slot, no retain); the
        // store retains the borrowed source, captures the old element, stores
        // and releases the capture.
        let store = instructions
            .iter()
            .position(|i| *i == assign(target.clone(), &class(), copy(lp(1), &class())))
            .unwrap();
        assert_eq!(instructions[store - 2], retain(lp(1), &class()));
        assert!(is_capture_of(&instructions[store - 1], &target));
        assert!(matches!(
            instructions[store + 1],
            Instruction::Release { .. }
        ));
        assert_eq!(stats.retains, 2);
    }

    #[test]
    fn struct_containing_an_array_copy_counts_the_array_leaf() {
        let (function, _) = run_void(
            vec![local(0, array_struct(), false)],
            vec![local(1, array_struct(), false)],
            vec![assign(lp(1), &array_struct(), copy(lp(0), &array_struct()))],
        );
        let leaf = |id: u32| Place::Field {
            base: Box::new(lp(id)),
            field: SymbolId(23),
        };
        assert_eq!(
            function.blocks[0].instructions,
            vec![retain(leaf(0), &int_array())]
        );
        assert!(body(&function).contains(&release(leaf(1), &int_array())));
    }

    #[test]
    fn list_to_array_result_reassigned_captures_and_releases_the_old_array() {
        let list = Type::List(Box::new(class()));
        let to_array = || Instruction::ListToArray {
            destination: lp(1),
            list: copy(lp(0), &list),
            element_type: class(),
            region: mir::AllocationRegion::Persistent,
        };
        let (function, stats) = run_void(
            vec![local(0, list.clone(), false)],
            vec![local(1, class_array(), false)],
            vec![to_array(), to_array()],
        );
        let instructions = body(&function);
        assert!(is_capture_of(&instructions[0], &lp(1)));
        assert_eq!(instructions[1], to_array());
        assert_eq!(instructions[2], release(lp(2), &class_array()));
        assert!(is_capture_of(&instructions[3], &lp(1)));
        assert_eq!(instructions[4], to_array());
        assert_eq!(instructions[5], release(lp(3), &class_array()));
        // Only the entry retain of the list parameter (a list is a leaf).
        assert_eq!(stats.retains, 1);
    }

    #[test]
    fn dictionary_snapshots_are_owned_destination_writers() {
        let dictionary = Type::Dictionary(Box::new(Type::Int), Box::new(Type::Int));
        let keys = Instruction::DictionaryKeys {
            destination: lp(1),
            dictionary: copy(lp(0), &dictionary),
            key_type: Type::Int,
            region: mir::AllocationRegion::Persistent,
        };
        let values = Instruction::DictionaryValues {
            destination: lp(1),
            dictionary: copy(lp(0), &dictionary),
            value_type: Type::Int,
            region: mir::AllocationRegion::Persistent,
        };
        let (function, stats) = run_void(
            vec![local(0, dictionary, false)],
            vec![local(1, int_array(), false)],
            vec![keys.clone(), values.clone()],
        );
        let instructions = body(&function);
        assert!(is_capture_of(&instructions[0], &lp(1)));
        assert_eq!(instructions[1], keys);
        assert!(is_capture_of(&instructions[3], &lp(1)));
        assert_eq!(instructions[4], values);
        // Only the entry retain of the dictionary parameter (a dictionary is a leaf).
        assert_eq!(stats.retains, 1);
    }

    #[test]
    fn intrinsic_returning_an_array_is_owned() {
        let chars = Type::Array(Box::new(Type::Char));
        let intrinsic = |destination: u32, return_type: Type| Instruction::CallIntrinsic {
            destination: Some(lp(destination)),
            intrinsic: mir::Intrinsic::StringToChars,
            arguments: Vec::new(),
            return_type,
        };
        let (function, stats) = run_void(
            Vec::new(),
            vec![local(0, chars.clone(), false)],
            vec![intrinsic(0, chars.clone()), intrinsic(0, chars.clone())],
        );
        let instructions = &function.blocks[0].instructions;
        assert!(is_capture_of(&instructions[0], &lp(0)));
        assert_eq!(instructions[1], intrinsic(0, chars.clone()));
        // Fresh result: the capture release follows directly, no retain.
        assert_eq!(instructions[2], release(lp(1), &chars));
        assert!(is_capture_of(&instructions[3], &lp(0)));
        assert_eq!(stats.retains, 0);
    }

    #[test]
    fn e3_does_not_apply_to_a_function_with_an_array_element_store() {
        let result = both(
            vec![local(0, int_array(), false)],
            Vec::new(),
            vec![block(
                0,
                vec![assign(element(0, &Type::Int), &Type::Int, zero())],
                Terminator::Return(None),
            )],
            Type::Void,
        );
        assert_eq!(result.elided_stats.elided_parameters, 0);
        // A read-only array parameter still qualifies.
        let result = both(
            vec![local(0, int_array(), false)],
            vec![local(1, Type::Int, false)],
            vec![block(
                0,
                vec![assign(
                    lp(1),
                    &Type::Int,
                    copy(element(0, &Type::Int), &Type::Int),
                )],
                Terminator::Return(None),
            )],
            Type::Void,
        );
        assert_eq!(result.elided_stats.elided_parameters, 2);
    }

    fn string_local(id: u32) -> Local {
        local(id, Type::String, false)
    }

    fn string_constant(text: &str) -> Operand {
        Operand {
            type_: Type::String,
            kind: OperandKind::Constant(mir::Constant::String(text.into())),
        }
    }

    fn string_intrinsic(
        destination: u32,
        intrinsic: mir::Intrinsic,
        arguments: Vec<Operand>,
    ) -> Instruction {
        Instruction::CallIntrinsic {
            destination: Some(lp(destination)),
            intrinsic,
            arguments,
            return_type: Type::String,
        }
    }

    fn retains_in(instructions: &[Instruction]) -> usize {
        instructions
            .iter()
            .filter(|i| matches!(i, Instruction::Retain { .. }))
            .count()
    }

    #[test]
    fn string_concat_reassigned_in_a_loop_releases_the_old_value_without_retains() {
        let concat = string_intrinsic(
            1,
            mir::Intrinsic::StringConcat,
            vec![
                copy(lp(1), &Type::String),
                copy(lp(0), &Type::String),
                string_constant("x"),
            ],
        );
        let result = both(
            vec![string_local(0)],
            vec![string_local(1)],
            vec![
                block(0, Vec::new(), Terminator::Goto(BasicBlockId(1))),
                block(1, vec![concat.clone()], branch(1, 2)),
                block(2, Vec::new(), Terminator::Return(None)),
            ],
            Type::Void,
        );
        let loop_block = result
            .c5
            .blocks
            .iter()
            .find(|b| b.id == BasicBlockId(1))
            .unwrap();
        assert!(is_capture_of(&loop_block.instructions[0], &lp(1)));
        assert_eq!(loop_block.instructions[1], concat);
        assert!(matches!(
            loop_block.instructions[2],
            Instruction::Release { .. }
        ));
        assert_eq!(loop_block.instructions.len(), 3);
    }

    #[test]
    fn string_join_result_is_owned_and_constants_are_not_counted() {
        let join = string_intrinsic(
            1,
            mir::Intrinsic::StringJoin,
            vec![
                string_constant("a"),
                copy(lp(0), &Type::String),
                string_constant("b"),
            ],
        );
        let (function, stats) = run_void(
            vec![string_local(0)],
            vec![string_local(1)],
            vec![join.clone()],
        );
        let instructions = body(&function);
        assert!(is_capture_of(&instructions[0], &lp(1)));
        assert_eq!(instructions[1], join);
        assert_eq!(instructions[2], release(lp(2), &Type::String));
        // Only the parameter entry retain remains.
        assert_eq!(stats.retains, 1);
        assert_eq!(retains_in(instructions), 0);
    }

    #[test]
    fn constant_string_assignment_needs_no_count() {
        let (function, stats) = run_void(
            Vec::new(),
            vec![string_local(0)],
            vec![assign(lp(0), &Type::String, string_constant("k"))],
        );
        assert_eq!(stats.retains, 0);
        assert_eq!(retains_in(body(&function)), 0);
    }

    #[test]
    fn string_builder_to_string_is_an_owned_destination_writer() {
        let builder_class = SymbolId(40);
        let builder = Type::Class(builder_class);
        let to_string = || Instruction::StringBuilderToString {
            destination: lp(1),
            builder: copy(lp(0), &builder),
            class: builder_class,
            region: mir::AllocationRegion::Persistent,
        };
        let (function, stats) = run_void(
            vec![local(0, builder.clone(), false)],
            vec![string_local(1)],
            vec![to_string(), to_string()],
        );
        let instructions = body(&function);
        assert!(is_capture_of(&instructions[0], &lp(1)));
        assert_eq!(instructions[1], to_string());
        assert_eq!(instructions[2], release(lp(2), &Type::String));
        assert!(is_capture_of(&instructions[3], &lp(1)));
        assert_eq!(stats.retains, 0);
    }

    #[test]
    fn string_field_overwrite_retains_the_source_and_releases_the_old_value() {
        let string_field = Place::ObjectField {
            object: Box::new(copy(lp(0), &class())),
            field: SymbolId(12),
        };
        let (function, _) = run_void(
            vec![local(0, class(), false)],
            vec![string_local(1)],
            vec![assign(
                string_field.clone(),
                &Type::String,
                copy(lp(1), &Type::String),
            )],
        );
        let instructions = body(&function);
        assert_eq!(instructions[0], retain(lp(1), &Type::String));
        assert!(is_capture_of(&instructions[1], &string_field));
        assert!(matches!(instructions[3], Instruction::Release { .. }));
    }

    #[test]
    fn list_insert_keeps_the_consumed_retain_of_a_string_element_only() {
        let list = Type::List(Box::new(Type::String));
        let insert = Instruction::CallIntrinsic {
            destination: None,
            intrinsic: mir::Intrinsic::ListInsert,
            arguments: vec![copy(lp(0), &list), zero(), copy(lp(1), &Type::String)],
            return_type: Type::Void,
        };
        let (function, _) = run_void(
            vec![local(0, list, false)],
            vec![string_local(1)],
            vec![insert.clone()],
        );
        let instructions = body(&function);
        assert_eq!(instructions[0], retain(lp(1), &Type::String));
        assert_eq!(instructions[1], insert);
        assert_eq!(retains_in(instructions), 1);
    }

    #[test]
    fn dictionary_get_or_result_is_borrowed_and_retained_after_the_call() {
        let dictionary = Type::Dictionary(Box::new(Type::String), Box::new(Type::String));
        let arguments = vec![
            copy(lp(0), &dictionary),
            copy(lp(1), &Type::String),
            copy(lp(1), &Type::String),
        ];
        let get_or = string_intrinsic(2, mir::Intrinsic::DictionaryGetOr, arguments.clone());
        let (function, _) = run_void(
            vec![local(0, dictionary.clone(), false)],
            vec![string_local(1), string_local(2)],
            vec![get_or.clone()],
        );
        let instructions = body(&function);
        assert!(is_capture_of(&instructions[0], &lp(2)));
        assert_eq!(instructions[1], get_or);
        assert_eq!(instructions[2], retain(lp(2), &Type::String));
        assert_eq!(retains_in(instructions), 1);
        // GetOrAdd stores key and value (two consumed retains) and borrows
        // its result.
        let get_or_add = string_intrinsic(2, mir::Intrinsic::DictionaryGetOrAdd, arguments);
        let (function, _) = run_void(
            vec![local(0, dictionary, false)],
            vec![string_local(1), string_local(2)],
            vec![get_or_add.clone()],
        );
        let instructions = body(&function);
        assert_eq!(retains_in(instructions), 3);
        assert_eq!(instructions[0], retain(lp(1), &Type::String));
        assert_eq!(instructions[1], retain(lp(1), &Type::String));
        assert!(is_capture_of(&instructions[2], &lp(2)));
        assert_eq!(instructions[3], get_or_add);
        assert_eq!(instructions[4], retain(lp(2), &Type::String));
    }

    #[test]
    fn list_local_reassigned_in_a_loop_releases_the_old_list_without_retains() {
        let result = both(
            Vec::new(),
            vec![local(0, int_list(), false)],
            vec![
                block(0, Vec::new(), Terminator::Goto(BasicBlockId(1))),
                block(1, vec![allocate_list(0)], branch(1, 2)),
                block(2, Vec::new(), Terminator::Return(None)),
            ],
            Type::Void,
        );
        let loop_block = result.c5.blocks.iter().find(|b| b.id == BasicBlockId(1));
        let instructions = &loop_block.unwrap().instructions;
        assert_eq!(retains_in(instructions), 0);
        assert!(
            instructions
                .iter()
                .any(|i| matches!(i, Instruction::Release { .. })),
            "{instructions:#?}"
        );
        assert_eq!(counts(&result.c5), (0, 2));
    }

    #[test]
    fn list_stored_in_a_class_field_is_retained_and_the_old_list_released() {
        let (function, _) = run_void(
            vec![local(0, class(), false)],
            vec![local(1, int_list(), false)],
            vec![
                allocate_list(1),
                assign(list_field(0), &int_list(), copy(lp(1), &int_list())),
                assign(list_field(0), &int_list(), copy(lp(1), &int_list())),
            ],
        );
        let instructions = body(&function);
        // Each store keeps its own count (retain), captures and releases the
        // previous field value.
        assert_eq!(
            instructions
                .iter()
                .filter(|i| matches!(i, Instruction::Retain { value } if value.type_ == int_list()))
                .count(),
            2
        );
    }

    #[test]
    fn list_passed_to_a_callee_is_borrowed() {
        let (function, _) = run_void(
            vec![local(0, int_list(), false)],
            Vec::new(),
            vec![Instruction::Call {
                destination: None,
                function: SymbolId(2),
                arguments: vec![copy(lp(0), &int_list())],
                return_type: Type::Void,
            }],
        );
        // Only the entry retain of the parameter; the call adds none.
        assert_eq!(retains_in(body(&function)), 0);
        assert_eq!(counts(&function), (1, 1));
    }

    #[test]
    fn list_instructions_borrow_the_list_and_retain_only_the_stored_value() {
        let list = int_list();
        let strings = Type::List(Box::new(Type::String));
        let (function, _) = run_void(
            vec![local(0, strings.clone(), false), string_local(1)],
            Vec::new(),
            vec![
                Instruction::ListAdd {
                    list: copy(lp(0), &strings),
                    value: copy(lp(1), &Type::String),
                },
                Instruction::ListClear {
                    list: copy(lp(0), &strings),
                },
            ],
        );
        let instructions = body(&function);
        assert_eq!(retains_in(instructions), 1);
        assert!(instructions.contains(&retain(lp(1), &Type::String)));
        assert!(!instructions.contains(&retain(lp(0), &list)));
    }

    #[test]
    fn list_in_a_lowering_temporary_can_be_moved_to_a_local() {
        // t = new List; x = t
        let result = both(
            Vec::new(),
            vec![local(0, int_list(), true), local(1, int_list(), false)],
            vec![block(
                0,
                vec![
                    allocate_list(0),
                    assign(lp(1), &int_list(), copy(lp(0), &int_list())),
                ],
                Terminator::Return(None),
            )],
            Type::Void,
        );
        assert!(
            result.elided_stats.elided_moves > 0,
            "{:?}",
            result.elided_stats
        );
    }

    #[test]
    fn e3_getter_over_a_list_field_length_keeps_elision() {
        let length = Instruction::Assign {
            target: lp(1),
            value: Rvalue {
                type_: Type::Int,
                kind: RvalueKind::ListLength(copy(list_field(0), &int_list())),
            },
        };
        let result = both(
            vec![local(0, class(), false)],
            vec![local(1, Type::Int, false)],
            vec![block(
                0,
                vec![length],
                Terminator::Return(Some(copy(lp(1), &Type::Int))),
            )],
            Type::Int,
        );
        assert_eq!(counts(&result.elided), (0, 0));
        assert!(result.elided_stats.elided_parameters > 0);
    }

    #[test]
    fn e3_applies_to_a_list_parameter_that_is_only_read() {
        let length = Instruction::Assign {
            target: lp(1),
            value: Rvalue {
                type_: Type::Int,
                kind: RvalueKind::ListLength(copy(lp(0), &int_list())),
            },
        };
        let result = both(
            vec![local(0, int_list(), false)],
            vec![local(1, Type::Int, false)],
            vec![block(
                0,
                vec![length],
                Terminator::Return(Some(copy(lp(1), &Type::Int))),
            )],
            Type::Int,
        );
        assert_eq!(counts(&result.c5), (1, 1));
        assert_eq!(counts(&result.elided), (0, 0));
    }

    #[test]
    fn dictionary_local_reassigned_in_a_loop_releases_the_old_dictionary() {
        let result = both(
            Vec::new(),
            vec![local(0, int_dictionary(), false)],
            vec![
                block(0, Vec::new(), Terminator::Goto(BasicBlockId(1))),
                block(1, vec![allocate_dictionary(0)], branch(1, 2)),
                block(2, Vec::new(), Terminator::Return(None)),
            ],
            Type::Void,
        );
        let loop_block = result.c5.blocks.iter().find(|b| b.id == BasicBlockId(1));
        let instructions = &loop_block.unwrap().instructions;
        assert_eq!(retains_in(instructions), 0);
        assert!(
            instructions
                .iter()
                .any(|i| matches!(i, Instruction::Release { .. })),
            "{instructions:#?}"
        );
        assert_eq!(counts(&result.c5), (0, 2));
    }

    #[test]
    fn dictionary_stored_in_a_class_field_is_retained_and_the_old_one_released() {
        let (function, _) = run_void(
            vec![local(0, class(), false)],
            vec![local(1, int_dictionary(), false)],
            vec![
                allocate_dictionary(1),
                assign(
                    dictionary_field(0),
                    &int_dictionary(),
                    copy(lp(1), &int_dictionary()),
                ),
                assign(
                    dictionary_field(0),
                    &int_dictionary(),
                    copy(lp(1), &int_dictionary()),
                ),
            ],
        );
        let instructions = body(&function);
        let of_dictionary = |count: fn(&Instruction) -> Option<&Operand>| {
            instructions
                .iter()
                .filter_map(count)
                .filter(|value| value.type_ == int_dictionary())
                .count()
        };
        assert_eq!(
            of_dictionary(|i| match i {
                Instruction::Retain { value } => Some(value),
                _ => None,
            }),
            2
        );
        // Captures of the old local and of the two old field values, and the
        // exit release of the local.
        assert_eq!(
            of_dictionary(|i| match i {
                Instruction::Release { value } => Some(value),
                _ => None,
            }),
            4
        );
    }

    fn dictionary_try_get(destination: u32) -> Instruction {
        Instruction::DictionaryTryGet {
            destination: lp(destination),
            dictionary: copy(lp(0), &class_dictionary()),
            key: copy(lp(1), &class()),
            value_type: class(),
            option_layout: mir::DictionaryOptionLayout {
                some_case: SymbolId(50),
                some_field: SymbolId(51),
                some_tag: 1,
                none_case: SymbolId(52),
                none_tag: 0,
            },
        }
    }

    fn dictionary_set() -> Instruction {
        Instruction::DictionarySet {
            destination: lp(4),
            dictionary: copy(lp(0), &class_dictionary()),
            key: copy(lp(1), &class()),
            value: copy(lp(2), &class()),
        }
    }

    #[test]
    fn dictionary_try_get_is_retained_after_the_read_and_survives_a_set() {
        let use_result = Instruction::Call {
            destination: None,
            function: SymbolId(2),
            arguments: vec![copy(lp(3), &class())],
            return_type: Type::Void,
        };
        let get = dictionary_try_get(3);
        let (function, _) = run_void(
            vec![
                local(0, class_dictionary(), false),
                local(1, class(), false),
                local(2, class(), false),
            ],
            vec![local(3, class(), false), local(4, Type::Bool, false)],
            vec![get.clone(), dictionary_set(), use_result],
        );
        let all: Vec<&Instruction> = function
            .blocks
            .iter()
            .flat_map(|b| &b.instructions)
            .collect();
        let at = all.iter().position(|i| **i == get).unwrap();
        assert_eq!(*all[at + 1], retain(lp(3), &class()));
        // Key and value are consumed by the set; the dictionary is borrowed.
        let set = all.iter().position(|i| **i == dictionary_set()).unwrap();
        assert_eq!(*all[set - 1], retain(lp(2), &class()));
        assert_eq!(*all[set - 2], retain(lp(1), &class()));
        let dictionary_retains = all
            .iter()
            .filter(|i| matches!(i, Instruction::Retain { value } if matches!(value.type_, Type::Dictionary(..))))
            .count();
        // Only the entry retain of the dictionary parameter.
        assert_eq!(dictionary_retains, 1);
    }

    #[test]
    fn e3_applies_to_a_dictionary_getter_that_only_reads_the_count() {
        let count = Instruction::Assign {
            target: lp(1),
            value: Rvalue {
                type_: Type::Int,
                kind: RvalueKind::DictionaryLength(copy(lp(0), &int_dictionary())),
            },
        };
        let result = both(
            vec![local(0, int_dictionary(), false)],
            vec![local(1, Type::Int, false)],
            vec![block(
                0,
                vec![count],
                Terminator::Return(Some(copy(lp(1), &Type::Int))),
            )],
            Type::Int,
        );
        assert_eq!(counts(&result.c5), (1, 1));
        assert_eq!(counts(&result.elided), (0, 0));
        assert!(result.elided_stats.elided_parameters > 0);
    }

    #[test]
    fn e3_does_not_apply_to_a_function_mutating_a_dictionary() {
        let dictionary = || copy(lp(0), &int_dictionary());
        for mutation in [
            Instruction::DictionaryAdd {
                destination: lp(1),
                dictionary: dictionary(),
                key: zero(),
                value: zero(),
            },
            Instruction::DictionarySet {
                destination: lp(1),
                dictionary: dictionary(),
                key: zero(),
                value: zero(),
            },
            Instruction::DictionaryRemove {
                destination: lp(1),
                dictionary: dictionary(),
                key: zero(),
            },
            Instruction::DictionaryClear {
                dictionary: dictionary(),
            },
        ] {
            let result = both(
                vec![local(0, int_dictionary(), false)],
                vec![local(1, Type::Bool, false)],
                vec![block(0, vec![mutation], Terminator::Return(None))],
                Type::Void,
            );
            assert_eq!(result.elided_stats.elided_parameters, 0);
            assert_eq!(counts(&result.elided), (1, 1));
        }
    }
}
