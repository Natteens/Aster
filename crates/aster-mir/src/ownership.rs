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
    /// E4: retains, capture releases and exit releases of borrowed locals.
    pub elided_borrows: usize,
    /// E5: entry retains and exit releases of the parameters of non-releasing
    /// functions that E3 did not already cover.
    pub elided_summary_parameters: usize,
}

/// Return a copy of `module` with ownership operations inserted and then
/// reduced by the local rules E1 (null capture), E2 (move from a single-use
/// lowering temporary) and E3 (leaf-function parameters) of the C6 contract,
/// and by the summary rules E4 (borrow windows) and E5 (parameters of
/// non-releasing functions) of design section 14.
#[must_use]
pub fn insert_ownership_elided(module: &mir::Module) -> (mir::Module, OwnershipStats) {
    let (output, stats, _) = insert_ownership_elided_with_rounds(module);
    (output, stats)
}

/// [`insert_ownership_elided`], also reporting how many rounds the joint
/// fixpoint of the non-releasing summary and the borrowed-local set needed.
#[doc(hidden)]
#[must_use]
pub fn insert_ownership_elided_with_rounds(
    module: &mir::Module,
) -> (mir::Module, OwnershipStats, usize) {
    let (mut output, mut stats) = insert_ownership(module);
    let shape = Shape::new(module);
    let mut all_captures = Vec::with_capacity(output.functions.len());
    for (function, original) in output.functions.iter_mut().zip(&module.functions) {
        let mut scratch = OwnershipStats::default();
        let context = Context {
            shape: &shape,
            function: original,
            locals: local_types(original),
            next_local: 0,
            new_locals: Vec::new(),
            stats: &mut scratch,
        };
        // The pass appended its own locals after the original ones.
        let captures: HashSet<LocalId> = function.locals[original.locals.len()..]
            .iter()
            .filter(|local| local.name.starts_with("$own.old"))
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
        all_captures.push(captures);
    }
    let rounds = elide_with_summaries(module, &shape, &mut output, &all_captures, &mut stats);
    (output, stats, rounds)
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
            | Instruction::CallVirtual { .. }
            | Instruction::ForeignCall { .. }
            | Instruction::CallIntrinsic { .. }
    )
}

/// Every place an instruction writes.
fn destination_places(instruction: &Instruction) -> impl Iterator<Item = &Place> {
    let places: [Option<&Place>; 3] = match instruction {
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
        } => [Some(target), None, None],
        Instruction::Call { destination, .. }
        | Instruction::CallInterface { destination, .. }
        | Instruction::CallVirtual { destination, .. }
        | Instruction::ForeignCall { destination, .. }
        | Instruction::CallIntrinsic { destination, .. } => [destination.as_ref(), None, None],
        Instruction::StringDecodeNext {
            char_destination,
            next_cursor_destination,
            ok_destination,
            ..
        } => [
            Some(char_destination),
            Some(next_cursor_destination),
            Some(ok_destination),
        ],
        _ => [None, None, None],
    };
    places.into_iter().flatten()
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
    let shape = Shape::new(module);
    for (function, original) in output.functions.iter_mut().zip(&module.functions) {
        let mut context = Context {
            shape: &shape,
            function: original,
            locals: local_types(original),
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

/// Indexed view of the module's type definitions, built once per pass.
struct Shape<'a> {
    native: HashSet<SymbolId>,
    structs: HashMap<SymbolId, &'a mir::StructDefinition>,
    /// Field types of structs and classes by `(owner, field)`.
    field_types: HashMap<(SymbolId, SymbolId), &'a Type>,
    /// Field types of enum cases by `(case, field)`.
    case_fields: HashMap<(SymbolId, SymbolId), &'a Type>,
}

impl<'a> Shape<'a> {
    fn new(module: &'a Module) -> Self {
        let mut field_types = HashMap::new();
        for (owner, fields) in module
            .structs
            .iter()
            .map(|definition| (definition.symbol, &definition.fields))
            .chain(
                module
                    .classes
                    .iter()
                    .map(|definition| (definition.symbol, &definition.fields)),
            )
        {
            for field in fields {
                field_types
                    .entry((owner, field.symbol))
                    .or_insert(&field.type_);
            }
        }
        let mut case_fields = HashMap::new();
        for case in module.enums.iter().flat_map(|definition| &definition.cases) {
            for field in &case.fields {
                case_fields
                    .entry((case.symbol, field.symbol))
                    .or_insert(&field.type_);
            }
        }
        Shape {
            native: string_builder_classes(module),
            structs: module
                .structs
                .iter()
                .rev()
                .map(|definition| (definition.symbol, definition))
                .collect(),
            field_types,
            case_fields,
        }
    }
}

fn has_leaves(shape: &Shape<'_>, type_: &Type) -> bool {
    match type_ {
        Type::Class(symbol) => !shape.native.contains(symbol),
        Type::Interface(_)
        | Type::Array(_)
        | Type::String
        | Type::List(_)
        | Type::Dictionary(_, _) => true,
        Type::User(symbol) => shape.structs.get(symbol).is_some_and(|definition| {
            definition
                .fields
                .iter()
                .any(|field| has_leaves(shape, &field.type_))
        }),
        _ => false,
    }
}

fn leaves(shape: &Shape<'_>, type_: &Type) -> Vec<Leaf> {
    match type_ {
        Type::Class(symbol) if shape.native.contains(symbol) => Vec::new(),
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
        Type::User(symbol) => shape
            .structs
            .get(symbol)
            .into_iter()
            .flat_map(|definition| &definition.fields)
            .flat_map(|field| {
                leaves(shape, &field.type_)
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
    shape: &'a Shape<'a>,
    function: &'a Function,
    /// Types of the function's parameters and locals by id.
    locals: Vec<Option<&'a Type>>,
    next_local: u32,
    new_locals: Vec<Local>,
    stats: &'a mut OwnershipStats,
}

/// Types of the parameters and locals, indexed by local id (the first
/// declaration of an id wins).
fn local_types(function: &Function) -> Vec<Option<&Type>> {
    let mut types: Vec<Option<&Type>> = Vec::new();
    for local in function.parameters.iter().chain(&function.locals) {
        let id = local.id.0 as usize;
        if types.len() <= id {
            types.resize(id + 1, None);
        }
        types[id].get_or_insert(&local.type_);
    }
    types
}

impl Context<'_> {
    fn leaf_operands(&self, place: &Place, type_: &Type) -> Vec<Operand> {
        leaves(self.shape, type_)
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
                .locals
                .get(id.0 as usize)
                .and_then(|type_| type_.cloned()),
            Place::Symbol(_) => None,
            Place::Field { base, field } => {
                let Type::User(symbol) = self.place_type(base)? else {
                    return None;
                };
                self.shape
                    .field_types
                    .get(&(symbol, *field))
                    .map(|type_| (*type_).clone())
            }
            Place::Index { element_type, .. } => Some(element_type.clone()),
            Place::ObjectField { object, field } => {
                let Type::Class(symbol) = &object.type_ else {
                    return None;
                };
                self.shape
                    .field_types
                    .get(&(*symbol, *field))
                    .map(|type_| (*type_).clone())
            }
            Place::EnumField { case, field, .. } => self
                .shape
                .case_fields
                .get(&(*case, *field))
                .map(|type_| (*type_).clone()),
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
                    | RvalueKind::Upcast(operand)
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
            | Instruction::CallInterface { destination: d, .. }
            | Instruction::CallVirtual { destination: d, .. } => destination = d.as_ref(),
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
                    || !leaves(self.shape, &operand.type_).is_empty() =>
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

/// Dense lookup of a function's blocks by id.
fn block_positions(function: &Function) -> Vec<usize> {
    let mut positions: Vec<usize> = Vec::new();
    for (position, block) in function.blocks.iter().enumerate() {
        let id = block.id.0 as usize;
        if positions.len() <= id {
            positions.resize(id + 1, usize::MAX);
        }
        if positions[id] == usize::MAX {
            positions[id] = position;
        }
    }
    positions
}

fn block_position(positions: &[usize], id: BasicBlockId) -> Option<usize> {
    positions
        .get(id.0 as usize)
        .copied()
        .filter(|position| *position != usize::MAX)
}

/// Forward "definitely null" analysis: for each block (by index) the locals
/// that were never written on any path from the function entry, as flags
/// indexed by `slots` (non-parameter local id to flag index, `usize::MAX`
/// otherwise). `None` marks a block unreachable from the entry.
fn null_entry_states(
    function: &Function,
    slots: &[usize],
    slot_count: usize,
) -> Vec<Option<Vec<bool>>> {
    let positions = block_positions(function);
    let slot_of = |place: &Place| {
        root_local(place)
            .and_then(|local| slots.get(local.0 as usize))
            .copied()
            .filter(|slot| *slot != usize::MAX)
    };
    let mut states: Vec<Option<Vec<bool>>> = vec![None; function.blocks.len()];
    if let Some(entry) = block_position(&positions, function.entry) {
        states[entry] = Some(vec![true; slot_count]);
    }
    let mut state: Vec<bool> = Vec::with_capacity(slot_count);
    loop {
        let mut changed = false;
        for (position, block) in function.blocks.iter().enumerate() {
            let Some(entering) = &states[position] else {
                continue;
            };
            state.clear();
            state.extend_from_slice(entering);
            for instruction in &block.instructions {
                for place in destination_places(instruction) {
                    if let Some(slot) = slot_of(place) {
                        state[slot] = false;
                    }
                }
            }
            for target in successor_blocks(&block.terminator) {
                let Some(next) = block_position(&positions, target) else {
                    continue;
                };
                match &mut states[next] {
                    None => {
                        states[next] = Some(state.clone());
                        changed = true;
                    }
                    Some(existing) => {
                        for (old, new) in existing.iter_mut().zip(&state) {
                            if *old && !*new {
                                *old = false;
                                changed = true;
                            }
                        }
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
    let mut slots: Vec<usize> = Vec::new();
    let mut slot_count = 0;
    for local in &function.locals {
        let id = local.id.0 as usize;
        if slots.len() <= id {
            slots.resize(id + 1, usize::MAX);
        }
        if slots[id] == usize::MAX {
            slots[id] = slot_count;
            slot_count += 1;
        }
    }
    // Parameters are written on entry, never null.
    for parameter in &function.parameters {
        if let Some(slot) = slots.get_mut(parameter.id.0 as usize) {
            *slot = usize::MAX;
        }
    }
    let states = null_entry_states(function, &slots, slot_count);
    let slot_of = |place: &Place| {
        root_local(place)
            .and_then(|local| slots.get(local.0 as usize))
            .copied()
            .filter(|slot| *slot != usize::MAX)
    };
    let mut removed = Removed::default();
    for (block, entry) in function.blocks.iter_mut().zip(states) {
        let mut state = entry.unwrap_or_else(|| vec![false; slot_count]);
        let mut elided: Vec<LocalId> = Vec::new();
        let mut dropped: Vec<usize> = Vec::new();
        for (index, instruction) in block.instructions.iter().enumerate() {
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
            } = instruction
                && captures.contains(temporary)
                && slot_of(source).is_some_and(|slot| state[slot])
            {
                elided.push(*temporary);
                dropped.push(index);
                continue;
            }
            if elided
                .iter()
                .any(|temporary| is_release_of(instruction, *temporary))
            {
                removed.releases += 1;
                dropped.push(index);
                continue;
            }
            for place in destination_places(instruction) {
                if let Some(slot) = slot_of(place) {
                    state[slot] = false;
                }
            }
        }
        if !dropped.is_empty() {
            let mut index = 0;
            block.instructions.retain(|_| {
                index += 1;
                dropped.binary_search(&(index - 1)).is_err()
            });
        }
    }
    removed
}

impl Context<'_> {
    /// The locals of the pre-pass function that are written once and read
    /// once, where the read is a copy into an owning place that does not
    /// itself mention the local (the shape E2 needs). One pass over the
    /// function.
    fn single_use_temporaries(&self) -> HashSet<LocalId> {
        #[derive(Default)]
        struct Use {
            writes: usize,
            readers: usize,
            owning_copy: bool,
            blocked: bool,
        }
        let mut uses: HashMap<LocalId, Use> = HashMap::new();
        let mut reads = HashSet::new();
        for block in &self.function.blocks {
            for instruction in &block.instructions {
                for place in destination_places(instruction) {
                    if let Some(local) = root_local(place) {
                        uses.entry(local).or_default().writes += 1;
                    }
                }
                reads.clear();
                owned_instruction_reads(instruction, &mut reads);
                for local in &reads {
                    let entry = uses.entry(*local).or_default();
                    entry.readers += 1;
                    if let Some(target) = copy_target(instruction, *local)
                        && owning(target)
                    {
                        let mut target_reads = HashSet::new();
                        owned_place_reads(target, &mut target_reads);
                        entry.owning_copy = !target_reads.contains(local);
                    }
                }
            }
            reads.clear();
            match &block.terminator {
                Terminator::Branch { condition, .. } => owned_operand_reads(condition, &mut reads),
                Terminator::Return(Some(operand)) => owned_operand_reads(operand, &mut reads),
                _ => {}
            }
            for local in &reads {
                uses.entry(*local).or_default().blocked = true;
            }
        }
        uses.into_iter()
            .filter(|(_, usage)| {
                !usage.blocked && usage.writes == 1 && usage.readers == 1 && usage.owning_copy
            })
            .map(|(local, _)| local)
            .collect()
    }

    /// E2: for a single-use lowering temporary `t` copied into an owning
    /// destination later in the write's block, drop the copy's retain, the
    /// exit releases of `t` and the capture of `t` at its write.
    fn elide_moves(&self, function: &mut Function, captures: &HashSet<LocalId>) -> Removed {
        let mut removed = Removed::default();
        let single_use = self.single_use_temporaries();
        for candidate in &self.function.locals {
            if !candidate.temporary
                || !single_use.contains(&candidate.id)
                || !self.single_leaf(&candidate.type_)
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
                if destination_places(instruction).any(|place| root_local(place) == Some(local)) {
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
            .any(|p| !leaves(self.shape, &p.type_).is_empty())
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
                    && !destination_places(instruction).any(stores_into_array)
                    && destination_places(instruction)
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

/// Intrinsics that only read their arguments or allocate fresh objects: they
/// run no user code and release no object that existed before the call
/// (design section 14). Every intrinsic outside this list is releasing.
fn non_releasing_intrinsic(intrinsic: &Intrinsic) -> bool {
    use Intrinsic as N;
    matches!(
        intrinsic,
        N::Log
            | N::LogWarning
            | N::LogError
            | N::StringEquals
            | N::StringConcat
            | N::StringConcatTemporary
            | N::StringLength
            | N::StringContains
            | N::StringStartsWith
            | N::StringEndsWith
            | N::StringIndexOf
            | N::StringLastIndexOf
            | N::StringSubstringFrom
            | N::StringSubstringFromTemporary
            | N::StringSubstringRange
            | N::StringSubstringRangeTemporary
            | N::StringTrim
            | N::StringTrimTemporary
            | N::StringTrimStart
            | N::StringTrimStartTemporary
            | N::StringTrimEnd
            | N::StringTrimEndTemporary
            | N::StringJoinArray
            | N::StringJoinArrayTemporary
            | N::StringConcatArray
            | N::StringConcatArrayTemporary
            | N::StringRepeat
            | N::StringRepeatTemporary
            | N::StringToChars
            | N::StringToCharsTemporary
            | N::StringFromChars
            | N::StringFromCharsTemporary
            | N::StringReplace
            | N::StringReplaceTemporary
            | N::StringSplit
            | N::StringSplitTemporary
            | N::StringFromLong
            | N::StringFromLongTemporary
            | N::StringFromULong
            | N::StringFromULongTemporary
            | N::StringFromDouble
            | N::StringFromDoubleTemporary
            | N::StringFromFloat
            | N::StringFromFloatTemporary
            | N::StringFromBool
            | N::StringFromBoolTemporary
            | N::StringFromChar
            | N::StringFromCharTemporary
            | N::StringJoin
            | N::StringJoinTemporary
            | N::StringBuilderLength
            | N::StringBuilderClear
            | N::StringTryParseBool
            | N::StringTryParseChar
            | N::StringTryParseSByte
            | N::StringTryParseByte
            | N::StringTryParseShort
            | N::StringTryParseUShort
            | N::StringTryParseInt
            | N::StringTryParseUInt
            | N::StringTryParseLong
            | N::StringTryParseULong
            | N::StringTryParseFloat
            | N::StringTryParseDouble
            | N::ListCapacity
            | N::ListEnsureCapacity
            | N::ListAddRange
            | N::ListInsert
            | N::ListReverse
            | N::ListGetRange
            | N::ListGetRangeTemporary
            | N::ListVersionMismatch
            | N::DictionaryCapacity
            | N::DictionaryEnsureCapacity
            | N::DictionaryGetOr
            | N::MathUnaryFloat
            | N::MathUnaryDouble
            | N::MathBinaryFloat
            | N::MathBinaryDouble
            | N::MathPredicateFloat
            | N::MathPredicateDouble
            | N::MathPowFloat
            | N::MathPowDouble
            | N::TimeMonotonicMilliseconds
            | N::TimeUnixMilliseconds
            | N::RandomMix
            | N::RandomEntropySeed
            | N::ConsoleWrite
            | N::ConsoleWriteLine
            | N::ReportRuntimeError(_)
    )
}

/// Instructions that release objects that existed before them.
fn is_releasing_instruction(instruction: &Instruction) -> bool {
    match instruction {
        Instruction::ListSet { .. }
        | Instruction::ListRemoveAt { .. }
        | Instruction::ListClear { .. }
        | Instruction::DictionaryAdd { .. }
        | Instruction::DictionarySet { .. }
        | Instruction::DictionaryRemove { .. }
        | Instruction::DictionaryClear { .. }
        | Instruction::ForeignCall { .. } => true,
        Instruction::CallIntrinsic { intrinsic, .. } => !non_releasing_intrinsic(intrinsic),
        _ => false,
    }
}

/// Every concrete method an interface method can dispatch to.
fn interface_targets(module: &Module) -> HashMap<SymbolId, Vec<SymbolId>> {
    let mut implementations: HashMap<SymbolId, Vec<&mir::InterfaceImplementation>> = HashMap::new();
    for implementation in &module.interface_implementations {
        implementations
            .entry(implementation.interface)
            .or_default()
            .push(implementation);
    }
    let mut targets = HashMap::new();
    for interface in &module.interfaces {
        let found = implementations.get(&interface.symbol);
        for (slot, method) in interface.methods.iter().enumerate() {
            let methods = found
                .into_iter()
                .flatten()
                .filter_map(|implementation| implementation.methods.get(slot).copied())
                .collect();
            targets.insert(method.symbol, methods);
        }
    }
    add_virtual_targets(module, &mut targets);
    targets
}

/// Every function a virtual call can reach, keyed by the method that introduced
/// the slot: the implementations of that slot in every class. A slot that is
/// abstract everywhere has no target, which callers treat as unknown.
pub(crate) fn add_virtual_targets(module: &Module, targets: &mut HashMap<SymbolId, Vec<SymbolId>>) {
    for class in &module.classes {
        for slot in &class.vtable {
            let found = targets.entry(slot.method).or_default();
            if let Some(implementation) = slot.implementation
                && !found.contains(&implementation)
            {
                found.push(implementation);
            }
        }
    }
}

fn is_retain_of(instruction: &Instruction, local: LocalId) -> bool {
    matches!(
        instruction,
        Instruction::Retain {
            value: Operand { kind: OperandKind::Copy(Place::Local(id)), .. }
        } if *id == local
    )
}

/// Whether `instruction` is not a call into Aster code, or every function it
/// can reach is in `non_releasing`. An unknown or unimplemented target is not.
fn calls_only_non_releasing(
    instruction: &Instruction,
    non_releasing: &HashSet<SymbolId>,
    targets: &HashMap<SymbolId, Vec<SymbolId>>,
) -> bool {
    match instruction {
        Instruction::Call { function, .. } => non_releasing.contains(function),
        Instruction::CallInterface { method, .. } | Instruction::CallVirtual { method, .. } => {
            targets.get(method).is_some_and(|found| {
                !found.is_empty() && found.iter().all(|target| non_releasing.contains(target))
            })
        }
        _ => true,
    }
}

/// A place whose root is an object field or array element.
fn heap_place(place: &Place) -> bool {
    match place {
        Place::Field { base, .. } => heap_place(base),
        Place::ObjectField { .. } | Place::Index { .. } => true,
        Place::Local(_) | Place::Symbol(_) | Place::EnumField { .. } => false,
    }
}

/// A local that E4 may leave uncounted, with every instruction E4 removes.
struct Borrow {
    local: LocalId,
    /// Single-use lowering temporaries whose borrowed value is copied into
    /// `local` (design 15, E4 through temporaries). They are borrowed with it.
    sources: Vec<LocalId>,
    /// Position of each borrowed load that writes the local or a source: the
    /// instructions that open the window.
    loads: Vec<At>,
    /// Position of each copy `local = source`.
    links: Vec<At>,
    /// Position of the load retains, captures, capture releases, link
    /// retains and exit releases of the local and its sources.
    removable: Vec<At>,
}

impl Borrow {
    fn members(&self) -> impl Iterator<Item = LocalId> + '_ {
        std::iter::once(self.local).chain(self.sources.iter().copied())
    }
}

/// `(block index, instruction index)`.
type At = (usize, usize);

/// Per-function lookup tables for E4, built once from the post-E3 function.
/// Each list is sorted by local id, so one local's entries are a slice.
struct Facts {
    /// Instructions that read a local.
    reads: Vec<(u32, At)>,
    /// Instructions that write a local (or a field of it).
    writes: Vec<(u32, At)>,
    /// `Release` instructions of a local.
    releases: Vec<(u32, At)>,
    /// Locals read by a terminator.
    terminator_reads: HashSet<LocalId>,
}

/// The entries of `local` in a list sorted by local id.
fn entries_of(list: &[(u32, At)], local: LocalId) -> &[(u32, At)] {
    let start = list.partition_point(|(id, _)| *id < local.0);
    let end = start + list[start..].partition_point(|(id, _)| *id == local.0);
    &list[start..end]
}

impl Facts {
    fn new(function: &Function) -> Self {
        let mut facts = Facts {
            reads: Vec::new(),
            writes: Vec::new(),
            releases: Vec::new(),
            terminator_reads: HashSet::new(),
        };
        let mut reads = HashSet::new();
        for (block_index, block) in function.blocks.iter().enumerate() {
            for (at, instruction) in block.instructions.iter().enumerate() {
                let position = (block_index, at);
                reads.clear();
                owned_instruction_reads(instruction, &mut reads);
                for local in &reads {
                    facts.reads.push((local.0, position));
                }
                for place in destination_places(instruction) {
                    if let Some(local) = root_local(place) {
                        let entry = (local.0, position);
                        if facts.writes.last() != Some(&entry) {
                            facts.writes.push(entry);
                        }
                    }
                }
                if let Instruction::Release {
                    value:
                        Operand {
                            kind: OperandKind::Copy(Place::Local(local)),
                            ..
                        },
                } = instruction
                {
                    facts.releases.push((local.0, position));
                }
            }
            match &block.terminator {
                Terminator::Branch { condition, .. } => {
                    owned_operand_reads(condition, &mut facts.terminator_reads);
                }
                Terminator::Return(Some(operand)) => {
                    owned_operand_reads(operand, &mut facts.terminator_reads);
                }
                _ => {}
            }
        }
        facts.reads.sort_unstable();
        facts.writes.sort_unstable();
        facts.releases.sort_unstable();
        facts
    }
}

/// Per block, whether the window of a borrow is open at the block entry on
/// every path from the function entry. `loads` are the borrow's load
/// positions per block, `invalidations` the invalidating positions per block.
/// The entry block starts closed; blocks unreachable from the entry are
/// closed.
fn entry_windows(
    function: &Function,
    loads: &HashMap<usize, Vec<usize>>,
    invalidations: &[Vec<usize>],
) -> Vec<bool> {
    let count = function.blocks.len();
    let positions = block_positions(function);
    let entry = block_position(&positions, function.entry);
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); count];
    for (block, contents) in function.blocks.iter().enumerate() {
        for target in successor_blocks(&contents.terminator) {
            if let Some(next) = block_position(&positions, target) {
                preds[next].push(block);
            }
        }
    }
    let mut reachable = vec![false; count];
    let mut pending: Vec<usize> = entry.into_iter().collect();
    while let Some(block) = pending.pop() {
        if std::mem::replace(&mut reachable[block], true) {
            continue;
        }
        pending.extend(
            successor_blocks(&function.blocks[block].terminator)
                .into_iter()
                .filter_map(|target| block_position(&positions, target)),
        );
    }
    let mut open: Vec<bool> = (0..count)
        .map(|block| reachable[block] && Some(block) != entry)
        .collect();
    let leaving = |block: usize, entering: bool| match (
        loads.get(&block).and_then(|list| list.last()),
        invalidations[block].last(),
    ) {
        (None, None) => entering,
        (Some(_), None) => true,
        (None, Some(_)) => false,
        (Some(load), Some(invalidation)) => load >= invalidation,
    };
    loop {
        let mut changed = false;
        for block in 0..count {
            if !reachable[block] || Some(block) == entry {
                continue;
            }
            let now = preds[block]
                .iter()
                .filter(|pred| reachable[**pred])
                .all(|pred| leaving(*pred, open[*pred]));
            if now != open[block] {
                open[block] = now;
                changed = true;
            }
        }
        if !changed {
            return open;
        }
    }
}

/// How one instruction mentions a local.
#[derive(Default)]
struct Usage {
    /// The local's value is stored, consumed or otherwise kept.
    kept: bool,
    /// The local's value is a direct argument or receiver of an Aster call.
    call_argument: bool,
}

/// Usage of `local` by an instruction known to read it.
fn usage_of(instruction: &Instruction, local: LocalId) -> Usage {
    let direct = |operand: &Operand| matches!(&operand.kind, OperandKind::Copy(Place::Local(id)) if *id == local);
    let mut usage = Usage::default();
    match instruction {
        Instruction::Assign { value, .. } => match &value.kind {
            RvalueKind::Use(operand)
            | RvalueKind::Cast(operand)
            | RvalueKind::Upcast(operand)
            | RvalueKind::MakeInterface {
                object: operand, ..
            } => usage.kept = direct(operand),
            RvalueKind::Aggregate(fields) | RvalueKind::EnumConstruct { fields, .. } => {
                usage.kept = fields.iter().any(|field| direct(&field.value));
            }
            _ => {}
        },
        Instruction::Call { arguments, .. } => {
            usage.call_argument = arguments.iter().any(direct);
        }
        Instruction::CallInterface {
            receiver,
            arguments,
            ..
        }
        | Instruction::CallVirtual {
            receiver,
            arguments,
            ..
        } => usage.call_argument = direct(receiver) || arguments.iter().any(direct),
        Instruction::ForeignCall { arguments, .. } => {
            usage.kept = arguments.iter().any(direct);
        }
        Instruction::CallIntrinsic {
            intrinsic,
            arguments,
            ..
        } => {
            usage.kept = arguments.iter().enumerate().any(|(position, argument)| {
                stores_argument(intrinsic, position) && direct(argument)
            });
        }
        Instruction::ListAdd { value, .. }
        | Instruction::ListSet { value, .. }
        | Instruction::Retain { value }
        | Instruction::Release { value } => usage.kept = direct(value),
        Instruction::DictionaryAdd { key, value, .. }
        | Instruction::DictionarySet { key, value, .. } => {
            usage.kept = direct(key) || direct(value);
        }
        _ => {}
    }
    usage
}

/// The instructions around a borrowed load of `local` at `at` that E4 removes:
/// the retain of the loaded value, the capture of the old value and its
/// release. `None` when the instruction is not a borrowed load in the shape
/// the C5 pass emits.
fn load_pattern(
    instructions: &[Instruction],
    at: usize,
    local: LocalId,
    captures: &HashSet<LocalId>,
) -> Option<Vec<usize>> {
    // `Some(source)`: a field or element copy retained before the capture;
    // `None`: a runtime read retained after the instruction.
    let source = match &instructions[at] {
        Instruction::Assign {
            target: Place::Local(id),
            value:
                Rvalue {
                    kind:
                        RvalueKind::Use(
                            operand @ Operand {
                                kind: OperandKind::Copy(place),
                                ..
                            },
                        ),
                    ..
                },
        } if *id == local && heap_place(place) => Some(operand),
        Instruction::ListGet {
            destination: Place::Local(id),
            ..
        }
        | Instruction::CallIntrinsic {
            destination: Some(Place::Local(id)),
            intrinsic: Intrinsic::DictionaryGetOr,
            ..
        } if *id == local => None,
        _ => return None,
    };
    let capture = at
        .checked_sub(1)
        .and_then(|index| match &instructions[index] {
            Instruction::Assign {
                target: Place::Local(temporary),
                value:
                    Rvalue {
                        kind:
                            RvalueKind::Use(Operand {
                                kind: OperandKind::Copy(Place::Local(id)),
                                ..
                            }),
                        ..
                    },
            } if *id == local && captures.contains(temporary) => Some((index, *temporary)),
            _ => None,
        });
    let mut removable = Vec::new();
    let tail = if let Some(source) = source {
        let retain = capture.map_or(at, |(index, _)| index).checked_sub(1)?;
        if !matches!(&instructions[retain], Instruction::Retain { value } if value == source) {
            return None;
        }
        removable.push(retain);
        at + 1
    } else {
        if !matches!(
            instructions.get(at + 1),
            Some(Instruction::Retain {
                value: Operand { kind: OperandKind::Copy(Place::Local(id)), .. }
            }) if *id == local
        ) {
            return None;
        }
        removable.push(at + 1);
        at + 2
    };
    if let Some((index, temporary)) = capture {
        if !instructions
            .get(tail)
            .is_some_and(|instruction| is_release_of(instruction, temporary))
        {
            return None;
        }
        removable.extend([index, tail]);
    }
    Some(removable)
}

impl Context<'_> {
    /// Whether `type_` is exactly one counted leaf with an empty path.
    fn single_leaf(&self, type_: &Type) -> bool {
        match type_ {
            Type::Class(symbol) => !self.shape.native.contains(symbol),
            Type::Interface(_)
            | Type::Array(_)
            | Type::String
            | Type::List(_)
            | Type::Dictionary(_, _) => true,
            _ => false,
        }
    }

    /// Locals of the pre-pass function that qualify structurally as borrowed
    /// (E4): one leaf, every write a borrowed load in the C5 shape or a copy
    /// of a single-use temporary that a borrowed load wrote earlier in the
    /// same block. Windows are checked separately against the summary.
    fn find_borrows(
        &self,
        original: &Function,
        facts: &Facts,
        captures: &HashSet<LocalId>,
    ) -> Vec<Borrow> {
        let mut found = Vec::new();
        'locals: for local in &original.locals {
            if !self.single_leaf(&local.type_) {
                continue;
            }
            let writes = entries_of(&facts.writes, local.id);
            if writes.is_empty() {
                continue;
            }
            let mut borrow = Borrow {
                local: local.id,
                sources: Vec::new(),
                loads: Vec::new(),
                links: Vec::new(),
                removable: Vec::new(),
            };
            for &(_, (block, at)) in writes {
                let instructions = &self.function.blocks[block].instructions;
                if let Some(around) = load_pattern(instructions, at, local.id, captures) {
                    borrow.loads.push((block, at));
                    borrow
                        .removable
                        .extend(around.into_iter().map(|index| (block, index)));
                } else if !self.link_pattern(original, facts, captures, (block, at), &mut borrow) {
                    continue 'locals;
                }
            }
            borrow.removable.extend(
                entries_of(&facts.releases, local.id)
                    .iter()
                    .map(|(_, at)| *at),
            );
            if !borrow.loads.is_empty() {
                found.push(borrow);
            }
        }
        found
    }

    /// The write `local = source` at `at`, where `source` is a lowering
    /// temporary of one counted leaf written exactly once, by a borrowed load
    /// earlier in the same block, and read only by this copy and the
    /// instructions around the load and the copy. Records the link, the
    /// source and every instruction E4 then removes; `false` when the write is
    /// not in that shape.
    fn link_pattern(
        &self,
        original: &Function,
        facts: &Facts,
        captures: &HashSet<LocalId>,
        at: At,
        borrow: &mut Borrow,
    ) -> bool {
        let (block, index) = at;
        let instructions = &self.function.blocks[block].instructions;
        let Instruction::Assign {
            target: Place::Local(target),
            value:
                Rvalue {
                    kind:
                        RvalueKind::Use(Operand {
                            kind: OperandKind::Copy(Place::Local(source)),
                            ..
                        }),
                    ..
                },
        } = &instructions[index]
        else {
            return false;
        };
        let source = *source;
        if *target != borrow.local
            || source == borrow.local
            || captures.contains(&source)
            || borrow.sources.contains(&source)
        {
            return false;
        }
        let Some(declared) = original.locals.iter().find(|local| local.id == source) else {
            return false;
        };
        if !declared.temporary || !self.single_leaf(&declared.type_) {
            return false;
        }
        let [(_, (source_block, source_at))] = entries_of(&facts.writes, source) else {
            return false;
        };
        let (source_block, source_at) = (*source_block, *source_at);
        if source_block != block || source_at >= index {
            return false;
        }
        let Some(mut around) = load_pattern(instructions, source_at, source, captures) else {
            return false;
        };
        let mut first = index;
        if let Some(previous) = index.checked_sub(1)
            && let Instruction::Assign {
                target: Place::Local(old),
                value:
                    Rvalue {
                        kind:
                            RvalueKind::Use(Operand {
                                kind: OperandKind::Copy(Place::Local(read)),
                                ..
                            }),
                        ..
                    },
            } = &instructions[previous]
            && *read == borrow.local
            && captures.contains(old)
            && instructions
                .get(index + 1)
                .is_some_and(|next| is_release_of(next, *old))
        {
            around.extend([previous, index + 1]);
            first = previous;
        }
        if let Some(previous) = first.checked_sub(1)
            && is_retain_of(&instructions[previous], source)
        {
            around.push(previous);
        }
        let exits: Vec<At> = entries_of(&facts.releases, source)
            .iter()
            .map(|(_, at)| *at)
            .collect();
        let accounted = |position: &At| {
            exits.contains(position)
                || (position.0 == block && (position.1 == index || around.contains(&position.1)))
        };
        if !entries_of(&facts.reads, source)
            .iter()
            .all(|(_, at)| accounted(at))
        {
            return false;
        }
        borrow.sources.push(source);
        borrow.loads.push((block, source_at));
        borrow.links.push(at);
        borrow
            .removable
            .extend(around.into_iter().map(|position| (block, position)));
        borrow.removable.extend(exits.iter().copied());
        true
    }

    /// Whether a write to `place` by `instruction` may release an object: a
    /// store of reference-bearing data into an element or object field.
    fn heap_store_releases(&self, instruction: &Instruction, place: &Place) -> bool {
        if !heap_place(place) {
            return false;
        }
        let stored = match instruction {
            Instruction::Assign { value, .. } => Some(value.type_.clone()),
            _ => self.place_type(place),
        };
        stored.is_none_or(|type_| has_leaves(self.shape, &type_))
    }

    /// Whether the instruction invalidates borrowed heap reads (design 14).
    fn invalidates(
        &self,
        instruction: &Instruction,
        non_releasing: &HashSet<SymbolId>,
        targets: &HashMap<SymbolId, Vec<SymbolId>>,
    ) -> bool {
        if matches!(instruction, Instruction::Release { .. })
            || is_releasing_instruction(instruction)
            || !calls_only_non_releasing(instruction, non_releasing, targets)
        {
            return true;
        }
        destination_places(instruction).any(|place| {
            root_local(place).is_none()
                && match instruction {
                    Instruction::Assign { value, .. } => has_leaves(self.shape, &value.type_),
                    _ => self
                        .place_type(place)
                        .is_none_or(|type_| has_leaves(self.shape, &type_)),
                }
        })
    }

    /// The invalidating instructions of the function, per block in ascending
    /// order, leaving out the `dropped` ones (design 15).
    fn invalidations(
        &self,
        non_releasing: &HashSet<SymbolId>,
        dropped: &HashSet<At>,
        targets: &HashMap<SymbolId, Vec<SymbolId>>,
    ) -> Vec<Vec<usize>> {
        self.function
            .blocks
            .iter()
            .enumerate()
            .map(|(block, contents)| {
                contents
                    .instructions
                    .iter()
                    .enumerate()
                    .filter(|(at, instruction)| {
                        !dropped.contains(&(block, *at))
                            && self.invalidates(instruction, non_releasing, targets)
                    })
                    .map(|(at, _)| at)
                    .collect()
            })
            .collect()
    }

    /// Whether every read of the borrowed local (and of its sources) happens
    /// with the window open on every path from the function entry (design
    /// 15), and the local is never kept. A load opens the window, an
    /// invalidating instruction closes it, and a write of the local that is
    /// not a load makes the local ineligible before this check.
    fn window_holds(
        &self,
        facts: &Facts,
        invalidations: &[Vec<usize>],
        borrow: &Borrow,
        non_releasing: &HashSet<SymbolId>,
        targets: &HashMap<SymbolId, Vec<SymbolId>>,
    ) -> bool {
        if borrow
            .members()
            .any(|member| facts.terminator_reads.contains(&member))
        {
            return false;
        }
        let own: HashSet<At> = borrow.removable.iter().copied().collect();
        let mut loads: HashMap<usize, Vec<usize>> = HashMap::new();
        for &(block, at) in &borrow.loads {
            loads.entry(block).or_default().push(at);
        }
        for list in loads.values_mut() {
            list.sort_unstable();
        }
        let mut entering: Option<Vec<bool>> = None;
        for member in borrow.members() {
            for &(_, (block, at)) in entries_of(&facts.reads, member) {
                if own.contains(&(block, at)) {
                    continue;
                }
                let instruction = &self.function.blocks[block].instructions[at];
                if member == borrow.local || !borrow.links.contains(&(block, at)) {
                    let usage = usage_of(instruction, member);
                    if usage.kept
                        || (usage.call_argument
                            && !calls_only_non_releasing(instruction, non_releasing, targets))
                    {
                        return false;
                    }
                }
                let last_load = loads
                    .get(&block)
                    .and_then(|list| list[..list.partition_point(|load| *load < at)].last());
                let last_invalidation = invalidations[block]
                    [..invalidations[block].partition_point(|position| *position < at)]
                    .last();
                let open = match (last_load, last_invalidation) {
                    (None, None) => entering
                        .get_or_insert_with(|| entry_windows(self.function, &loads, invalidations))
                        [block],
                    (Some(_), None) => true,
                    (None, Some(_)) => false,
                    (Some(load), Some(invalidation)) => load >= invalidation,
                };
                if !open {
                    return false;
                }
            }
        }
        true
    }

    /// Whether the function, with the `dropped` instructions removed, can
    /// release an object other than through its own parameter exit releases.
    fn releases_locally(&self, dropped: &HashSet<(usize, usize)>) -> bool {
        let parameters: HashSet<LocalId> = self.function.parameters.iter().map(|p| p.id).collect();
        let parameter_root =
            |place: &Place| root_local(place).is_some_and(|l| parameters.contains(&l));
        for (block_index, block) in self.function.blocks.iter().enumerate() {
            for (index, instruction) in block.instructions.iter().enumerate() {
                if dropped.contains(&(block_index, index)) {
                    continue;
                }
                if is_releasing_instruction(instruction) {
                    return true;
                }
                if let Instruction::Release { value } = instruction
                    && let OperandKind::Copy(place) = &value.kind
                    && !parameter_root(place)
                {
                    return true;
                }
                if destination_places(instruction).any(|place| {
                    parameter_root(place)
                        || stores_into_array(place)
                        || self.heap_store_releases(instruction, place)
                }) {
                    return true;
                }
            }
        }
        false
    }
}

/// Greatest fixpoint of the non-releasing summary over the call graph.
fn non_releasing_summary(
    contexts: &[Context<'_>],
    dropped: &[HashSet<(usize, usize)>],
    targets: &HashMap<SymbolId, Vec<SymbolId>>,
) -> HashSet<SymbolId> {
    let mut summary: HashSet<SymbolId> = contexts
        .iter()
        .zip(dropped)
        .filter(|(context, dropped)| !context.releases_locally(dropped))
        .map(|(context, _)| context.function.symbol)
        .collect();
    loop {
        let doomed: Vec<SymbolId> = contexts
            .iter()
            .map(|context| context.function)
            .filter(|function| summary.contains(&function.symbol))
            .filter(|function| {
                !function
                    .blocks
                    .iter()
                    .flat_map(|block| &block.instructions)
                    .all(|instruction| calls_only_non_releasing(instruction, &summary, targets))
            })
            .map(|function| function.symbol)
            .collect();
        if doomed.is_empty() {
            return summary;
        }
        for symbol in doomed {
            summary.remove(&symbol);
        }
    }
}

/// E4 and E5. The non-releasing summary and the set of borrowed locals depend
/// on each other (a recursive function releases nothing only if its borrowed
/// locals are not released, and a local may be borrowed only if the calls in
/// its window release nothing), so both start optimistic and shrink together
/// until stable: every round removes at least one borrowed local, so the loop
/// ends after at most `locals + 1` rounds. Returns the number of rounds.
fn elide_with_summaries(
    module: &Module,
    shape: &Shape<'_>,
    output: &mut Module,
    captures: &[HashSet<LocalId>],
    stats: &mut OwnershipStats,
) -> usize {
    let targets = interface_targets(module);
    let mut scratch = vec![OwnershipStats::default(); output.functions.len()];
    let (mut borrows, summary, rounds) = {
        let contexts: Vec<Context<'_>> = output
            .functions
            .iter()
            .zip(scratch.iter_mut())
            .map(|(function, stats)| Context {
                shape,
                function,
                locals: local_types(function),
                next_local: 0,
                new_locals: Vec::new(),
                stats,
            })
            .collect();
        let facts: Vec<Facts> = output.functions.iter().map(Facts::new).collect();
        let mut borrows: Vec<Vec<Borrow>> = contexts
            .iter()
            .zip(&module.functions)
            .zip(captures)
            .zip(&facts)
            .map(|(((context, original), captures), facts)| {
                context.find_borrows(original, facts, captures)
            })
            .collect();
        let mut rounds = 0;
        loop {
            rounds += 1;
            let dropped: Vec<HashSet<(usize, usize)>> = borrows
                .iter()
                .map(|list| {
                    list.iter()
                        .flat_map(|borrow| borrow.removable.iter().copied())
                        .collect()
                })
                .collect();
            let summary = non_releasing_summary(&contexts, &dropped, &targets);
            let mut shrunk = false;
            for (((context, list), dropped), facts) in
                contexts.iter().zip(&mut borrows).zip(&dropped).zip(&facts)
            {
                if list.is_empty() {
                    continue;
                }
                let invalidations = context.invalidations(&summary, dropped, &targets);
                let before = list.len();
                list.retain(|borrow| {
                    context.window_holds(facts, &invalidations, borrow, &summary, &targets)
                });
                shrunk |= list.len() != before;
            }
            if !shrunk {
                break (borrows, summary, rounds);
            }
        }
    };
    for ((function, borrows), original) in output
        .functions
        .iter_mut()
        .zip(&mut borrows)
        .zip(&module.functions)
    {
        let removed = remove_borrows(function, borrows);
        stats.retains -= removed.retains;
        stats.releases -= removed.releases;
        stats.elided_borrows += removed.retains + removed.releases;
        if summary.contains(&function.symbol) {
            let removed = drop_parameter_counts(function, original.entry);
            stats.retains -= removed.retains;
            stats.releases -= removed.releases;
            stats.elided_summary_parameters += removed.retains + removed.releases;
        }
    }
    rounds
}

/// Remove every instruction of the borrowed locals; returns the count
/// operations removed (a removed capture assignment is not one).
fn remove_borrows(function: &mut Function, borrows: &[Borrow]) -> Removed {
    let mut removed = Removed::default();
    let positions: HashSet<(usize, usize)> = borrows
        .iter()
        .flat_map(|borrow| borrow.removable.iter().copied())
        .collect();
    if positions.is_empty() {
        return removed;
    }
    for (block_index, block) in function.blocks.iter_mut().enumerate() {
        let mut index = 0;
        block.instructions.retain(|instruction| {
            let drop = positions.contains(&(block_index, index));
            index += 1;
            if drop {
                match instruction {
                    Instruction::Retain { .. } => removed.retains += 1,
                    Instruction::Release { .. } => removed.releases += 1,
                    _ => {}
                }
            }
            !drop
        });
    }
    removed
}

/// E5: in a non-releasing function, drop the entry block the pass added (the
/// retains of the parameters) and the parameter exit releases. Nothing is
/// removed unless the entry has exactly the shape the pass emits.
fn drop_parameter_counts(function: &mut Function, original_entry: BasicBlockId) -> Removed {
    let mut removed = Removed::default();
    if function.entry == original_entry {
        return removed;
    }
    let Some(position) = function
        .blocks
        .iter()
        .position(|block| block.id == function.entry)
    else {
        return removed;
    };
    let entry = &function.blocks[position];
    if entry.terminator != Terminator::Goto(original_entry)
        || !entry
            .instructions
            .iter()
            .all(|instruction| matches!(instruction, Instruction::Retain { .. }))
    {
        return removed;
    }
    removed.retains += entry.instructions.len();
    function.entry = original_entry;
    function.blocks.remove(position);
    let parameters: HashSet<LocalId> = function.parameters.iter().map(|p| p.id).collect();
    for block in &mut function.blocks {
        let before = block.instructions.len();
        block.instructions.retain(|instruction| {
            !matches!(
                instruction,
                Instruction::Release { value: Operand { kind: OperandKind::Copy(place), .. } }
                    if root_local(place).is_some_and(|local| parameters.contains(&local))
            )
        });
        removed.releases += before - block.instructions.len();
    }
    removed
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
                base: None,
                is_abstract: false,
                is_sealed: false,
                vtable: Vec::new(),
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
                base: None,
                is_abstract: false,
                is_sealed: false,
                vtable: Vec::new(),
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
                + elided_stats.elided_borrows
                + elided_stats.elided_summary_parameters
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
        // t = this.f; x = t; Unknown(x). The call is to a function the module
        // does not define, so it may release: `x` is not borrowable (E4) and
        // only E2 applies.
        let result = both(
            vec![local(0, class(), false)],
            vec![local(1, class(), true), local(2, class(), false)],
            vec![block(
                0,
                vec![
                    assign(lp(1), &class(), copy(field(0), &class())),
                    assign(lp(2), &class(), copy(lp(1), &class())),
                    Instruction::Call {
                        destination: None,
                        function: SymbolId(2),
                        arguments: vec![copy(lp(2), &class())],
                        return_type: Type::Void,
                    },
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
                ..OwnershipStats::default()
            }
        );
        assert_eq!(
            result.elided.blocks[1].instructions,
            vec![
                retain(field(0), &class()),
                assign(lp(1), &class(), copy(field(0), &class())),
                assign(lp(2), &class(), copy(lp(1), &class())),
                Instruction::Call {
                    destination: None,
                    function: SymbolId(2),
                    arguments: vec![copy(lp(2), &class())],
                    return_type: Type::Void,
                },
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

    // ---- E4 borrow windows and E5 non-releasing summaries (design 14) ----

    const NODE: u32 = 60;
    const VALUE: u32 = 61;
    const CHILDREN: u32 = 62;
    const NEXT: u32 = 63;
    const SINK: u32 = 64;
    const ITEMS: u32 = 65;
    const SUM: u32 = 1;
    const GET_VALUE: u32 = 2;
    const STORE: u32 = 3;
    const RUNNER: u32 = 4;
    const CLEAR_ITEMS: u32 = 6;

    fn node() -> Type {
        Type::Class(SymbolId(NODE))
    }

    fn nodes() -> Type {
        Type::Array(Box::new(node()))
    }

    fn node_list() -> Type {
        Type::List(Box::new(node()))
    }

    fn sink() -> Type {
        Type::Interface(SymbolId(30))
    }

    fn node_field(local: u32, field: u32) -> Place {
        Place::ObjectField {
            object: Box::new(copy(lp(local), &node())),
            field: SymbolId(field),
        }
    }

    fn int_constant(value: &str) -> Operand {
        Operand {
            type_: Type::Int,
            kind: OperandKind::Constant(mir::Constant::Integer(value.into())),
        }
    }

    fn compute(
        target: u32,
        type_: &Type,
        left: Operand,
        operator: mir::BinaryOperator,
        right: Operand,
    ) -> Instruction {
        Instruction::Assign {
            target: lp(target),
            value: Rvalue {
                type_: type_.clone(),
                kind: RvalueKind::Binary {
                    left,
                    operator,
                    right,
                },
            },
        }
    }

    fn add(target: u32, left: u32, right: u32) -> Instruction {
        compute(
            target,
            &Type::Int,
            copy(lp(left), &Type::Int),
            mir::BinaryOperator::Add,
            copy(lp(right), &Type::Int),
        )
    }

    fn call_of(
        function: u32,
        destination: Option<u32>,
        arguments: Vec<Operand>,
        return_type: &Type,
    ) -> Instruction {
        Instruction::Call {
            destination: destination.map(lp),
            function: SymbolId(function),
            arguments,
            return_type: return_type.clone(),
        }
    }

    fn function(
        symbol: u32,
        parameters: Vec<Local>,
        locals: Vec<Local>,
        blocks: Vec<BasicBlock>,
        return_type: Type,
    ) -> Function {
        Function {
            constructor: false,
            symbol: SymbolId(symbol),
            owner: None,
            name: format!("f{symbol}"),
            visibility: Visibility::Public,
            parameters,
            locals,
            return_type,
            entry: BasicBlockId(0),
            blocks,
            temporary_subregion_candidates: Vec::new(),
        }
    }

    /// `int GetValue(Node this) { return this.value; }`
    fn get_value() -> Function {
        function(
            GET_VALUE,
            vec![local(0, node(), false)],
            Vec::new(),
            vec![block(
                0,
                Vec::new(),
                Terminator::Return(Some(copy(node_field(0, VALUE), &Type::Int))),
            )],
            Type::Int,
        )
    }

    /// `void Store(Node this, Node other) { this.next = other; }`: releases the
    /// object `this.next` held.
    fn store() -> Function {
        function(
            STORE,
            vec![local(0, node(), false), local(1, node(), false)],
            Vec::new(),
            vec![block(
                0,
                vec![assign(node_field(0, NEXT), &node(), copy(lp(1), &node()))],
                Terminator::Return(None),
            )],
            Type::Void,
        )
    }

    /// `int ClearItems(Node this) { this.items.Clear(); return 0; }`
    fn clear_items() -> Function {
        function(
            CLEAR_ITEMS,
            vec![local(0, node(), false)],
            Vec::new(),
            vec![block(
                0,
                vec![Instruction::ListClear {
                    list: copy(node_field(0, ITEMS), &node_list()),
                }],
                Terminator::Return(Some(int_constant("0"))),
            )],
            Type::Int,
        )
    }

    fn graph(implementation: u32, functions: Vec<Function>) -> Module {
        Module {
            structs: Vec::new(),
            classes: vec![mir::ClassDefinition {
                base: None,
                is_abstract: false,
                is_sealed: false,
                vtable: Vec::new(),
                symbol: SymbolId(NODE),
                name: "Node".into(),
                fields: vec![
                    field_definition(VALUE, Type::Int),
                    field_definition(CHILDREN, nodes()),
                    field_definition(NEXT, node()),
                    field_definition(SINK, sink()),
                    field_definition(ITEMS, node_list()),
                ],
            }],
            interfaces: vec![mir::InterfaceDefinition {
                symbol: SymbolId(30),
                name: "ISink".into(),
                methods: vec![mir::InterfaceMethodDefinition {
                    symbol: SymbolId(31),
                    name: "Read".into(),
                    parameters: Vec::new(),
                    return_type: Type::Int,
                }],
            }],
            enums: Vec::new(),
            interface_implementations: vec![mir::InterfaceImplementation {
                class: SymbolId(NODE),
                interface: SymbolId(30),
                methods: vec![SymbolId(implementation)],
            }],
            foreign_functions: Vec::new(),
            functions,
        }
    }

    struct Graph {
        c5: Module,
        c6: Module,
        c5_stats: OwnershipStats,
        c6_stats: OwnershipStats,
    }

    /// C5 and C6 output of a whole module; both must validate and the C6
    /// accounting must add up.
    fn elide_graph(module: &Module) -> Graph {
        let (c5, c5_stats) = insert_ownership(module);
        assert_eq!(validate_ownership(&c5), Ok(()), "C5: {c5:#?}");
        let (c6, c6_stats) = insert_ownership_elided(module);
        assert_eq!(validate_ownership(&c6), Ok(()), "C6: {c6:#?}");
        for (module, stats) in [(&c5, c5_stats), (&c6, c6_stats)] {
            let total = module
                .functions
                .iter()
                .map(counts)
                .fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1));
            assert_eq!(total, (stats.retains, stats.releases));
        }
        assert_eq!(
            c5_stats.retains + c5_stats.releases,
            c6_stats.retains
                + c6_stats.releases
                + c6_stats.elided_null_captures
                + c6_stats.elided_moves
                + c6_stats.elided_parameters
                + c6_stats.elided_borrows
                + c6_stats.elided_summary_parameters
        );
        Graph {
            c5,
            c6,
            c5_stats,
            c6_stats,
        }
    }

    fn function_counts(module: &Module, symbol: u32) -> (usize, usize) {
        counts(
            module
                .functions
                .iter()
                .find(|function| function.symbol == SymbolId(symbol))
                .unwrap(),
        )
    }

    fn cond_branch(local_id: u32, then_block: u32, else_block: u32) -> Terminator {
        Terminator::Branch {
            condition: copy(lp(local_id), &Type::Bool),
            then_block: BasicBlockId(then_block),
            else_block: BasicBlockId(else_block),
        }
    }

    /// `int Sum(Node this)`: `total = this.value; foreach child in
    /// this.children { total += Sum(child); }`, with the array and the element
    /// loaded inside one block (a loop body that reloads the field).
    fn tree_sum() -> Function {
        function(
            SUM,
            vec![local(0, node(), false)],
            vec![
                local(1, Type::Int, false),
                local(2, Type::Int, false),
                local(3, nodes(), true),
                local(4, Type::Int, true),
                local(5, Type::Bool, true),
                local(6, nodes(), true),
                local(7, node(), false),
                local(8, Type::Int, true),
            ],
            vec![
                block(
                    0,
                    vec![
                        assign(lp(1), &Type::Int, copy(node_field(0, VALUE), &Type::Int)),
                        assign(lp(2), &Type::Int, int_constant("0")),
                    ],
                    Terminator::Goto(BasicBlockId(1)),
                ),
                block(
                    1,
                    vec![
                        assign(lp(3), &nodes(), copy(node_field(0, CHILDREN), &nodes())),
                        Instruction::Assign {
                            target: lp(4),
                            value: Rvalue {
                                type_: Type::Int,
                                kind: RvalueKind::ArrayLength(copy(lp(3), &nodes())),
                            },
                        },
                        compute(
                            5,
                            &Type::Bool,
                            copy(lp(2), &Type::Int),
                            mir::BinaryOperator::Less,
                            copy(lp(4), &Type::Int),
                        ),
                    ],
                    cond_branch(5, 2, 3),
                ),
                block(
                    2,
                    vec![
                        assign(lp(6), &nodes(), copy(node_field(0, CHILDREN), &nodes())),
                        assign(lp(7), &node(), copy(element(6, &node()), &node())),
                        call_of(SUM, Some(8), vec![copy(lp(7), &node())], &Type::Int),
                        add(1, 1, 8),
                        compute(
                            2,
                            &Type::Int,
                            copy(lp(2), &Type::Int),
                            mir::BinaryOperator::Add,
                            int_constant("1"),
                        ),
                    ],
                    Terminator::Goto(BasicBlockId(1)),
                ),
                block(
                    3,
                    Vec::new(),
                    Terminator::Return(Some(copy(lp(1), &Type::Int))),
                ),
            ],
            Type::Int,
        )
    }

    #[test]
    fn recursive_tree_sum_has_no_count_operations_in_c6() {
        let result = elide_graph(&graph(GET_VALUE, vec![tree_sum(), get_value()]));
        assert_ne!(function_counts(&result.c5, SUM), (0, 0));
        assert_eq!(function_counts(&result.c6, SUM), (0, 0));
        assert!(result.c6_stats.elided_borrows > 0, "{:?}", result.c6_stats);
        assert!(
            result.c6_stats.elided_summary_parameters > 0,
            "{:?}",
            result.c6_stats
        );
        assert_eq!(result.c5_stats.elided_borrows, 0);
    }

    /// `int Run(Node a)`: `x = a.next; v = GetValue(x); y = x.next; w =
    /// GetValue(y); return v + w;` with `x` read after a call.
    fn getter_chain() -> Function {
        function(
            RUNNER,
            vec![local(0, node(), false)],
            vec![
                local(1, node(), false),
                local(2, Type::Int, false),
                local(3, node(), false),
                local(4, Type::Int, false),
                local(5, Type::Int, true),
            ],
            vec![block(
                0,
                vec![
                    assign(lp(1), &node(), copy(node_field(0, NEXT), &node())),
                    call_of(GET_VALUE, Some(2), vec![copy(lp(1), &node())], &Type::Int),
                    assign(lp(3), &node(), copy(node_field(1, NEXT), &node())),
                    call_of(GET_VALUE, Some(4), vec![copy(lp(3), &node())], &Type::Int),
                    add(5, 2, 4),
                ],
                Terminator::Return(Some(copy(lp(5), &Type::Int))),
            )],
            Type::Int,
        )
    }

    #[test]
    fn getter_chain_through_non_releasing_getters_has_no_count_operations() {
        let result = elide_graph(&graph(GET_VALUE, vec![getter_chain(), get_value()]));
        assert_ne!(function_counts(&result.c5, RUNNER), (0, 0));
        assert_eq!(function_counts(&result.c6, RUNNER), (0, 0));
        assert_eq!(function_counts(&result.c6, GET_VALUE), (0, 0));
        assert!(result.c6_stats.elided_borrows >= 4);
    }

    /// `int Run(Node a)`: `s = a.sink; v = s.Read(); w = s.Read();
    /// return v + w;`
    fn interface_reader() -> Function {
        let read = |destination: u32| Instruction::CallInterface {
            destination: Some(lp(destination)),
            receiver: copy(lp(1), &sink()),
            method: SymbolId(31),
            arguments: Vec::new(),
            return_type: Type::Int,
        };
        function(
            RUNNER,
            vec![local(0, node(), false)],
            vec![
                local(1, sink(), false),
                local(2, Type::Int, false),
                local(3, Type::Int, false),
                local(4, Type::Int, true),
            ],
            vec![block(
                0,
                vec![
                    assign(lp(1), &sink(), copy(node_field(0, SINK), &sink())),
                    read(2),
                    read(3),
                    add(4, 2, 3),
                ],
                Terminator::Return(Some(copy(lp(4), &Type::Int))),
            )],
            Type::Int,
        )
    }

    #[test]
    fn interface_call_with_one_non_releasing_implementation_keeps_the_receiver_borrowed() {
        let result = elide_graph(&graph(GET_VALUE, vec![interface_reader(), get_value()]));
        assert_ne!(function_counts(&result.c5, RUNNER), (0, 0));
        assert_eq!(function_counts(&result.c6, RUNNER), (0, 0));
        assert!(result.c6_stats.elided_borrows > 0);
    }

    #[test]
    fn interface_call_with_a_releasing_implementation_counts_the_receiver() {
        let result = elide_graph(&graph(CLEAR_ITEMS, vec![interface_reader(), clear_items()]));
        assert_eq!(result.c6_stats.elided_borrows, 0);
        assert_eq!(result.c6_stats.elided_summary_parameters, 0);
        assert_eq!(function_counts(&result.c6, RUNNER), (2, 2));
    }

    #[test]
    fn borrowed_local_passed_to_a_callee_that_stores_into_its_source_field_stays_counted() {
        // x = a.next; Store(a, x); v = GetValue(x): Store releases a.next.
        let caller = function(
            RUNNER,
            vec![local(0, node(), false)],
            vec![local(1, node(), false), local(2, Type::Int, false)],
            vec![block(
                0,
                vec![
                    assign(lp(1), &node(), copy(node_field(0, NEXT), &node())),
                    call_of(
                        STORE,
                        None,
                        vec![copy(lp(0), &node()), copy(lp(1), &node())],
                        &Type::Void,
                    ),
                    call_of(GET_VALUE, Some(2), vec![copy(lp(1), &node())], &Type::Int),
                ],
                Terminator::Return(Some(copy(lp(2), &Type::Int))),
            )],
            Type::Int,
        );
        let result = elide_graph(&graph(GET_VALUE, vec![caller, store(), get_value()]));
        assert_eq!(result.c6_stats.elided_borrows, 0, "{:?}", result.c6_stats);
        // The parameter and the loaded local are both still counted (the
        // only difference to C5 is the E1 null capture).
        assert_eq!(function_counts(&result.c6, RUNNER), (2, 2));
    }

    fn list_reader(with_set: bool) -> Function {
        let mut instructions = vec![Instruction::ListGet {
            destination: lp(2),
            list: copy(lp(0), &node_list()),
            index: zero(),
            element_type: node(),
        }];
        if with_set {
            instructions.push(Instruction::ListSet {
                list: copy(lp(0), &node_list()),
                index: zero(),
                value: copy(lp(1), &node()),
            });
        }
        instructions.push(call_of(
            GET_VALUE,
            Some(3),
            vec![copy(lp(2), &node())],
            &Type::Int,
        ));
        function(
            RUNNER,
            vec![local(0, node_list(), false), local(1, node(), false)],
            vec![local(2, node(), false), local(3, Type::Int, false)],
            vec![block(
                0,
                instructions,
                Terminator::Return(Some(copy(lp(3), &Type::Int))),
            )],
            Type::Int,
        )
    }

    #[test]
    fn borrowed_local_used_after_a_list_set_stays_counted() {
        let result = elide_graph(&graph(GET_VALUE, vec![list_reader(true), get_value()]));
        assert_eq!(result.c6_stats.elided_borrows, 0, "{:?}", result.c6_stats);
    }

    #[test]
    fn list_element_read_used_before_any_invalidation_is_borrowed() {
        let result = elide_graph(&graph(GET_VALUE, vec![list_reader(false), get_value()]));
        assert!(result.c6_stats.elided_borrows > 0);
        assert!(function_counts(&result.c6, RUNNER) < function_counts(&result.c5, RUNNER));
    }

    #[test]
    fn a_function_that_calls_a_releasing_function_keeps_its_parameter_counts() {
        // void Run(Node this) { Store(this, this); }
        let caller = function(
            RUNNER,
            vec![local(0, node(), false)],
            Vec::new(),
            vec![block(
                0,
                vec![call_of(
                    STORE,
                    None,
                    vec![copy(lp(0), &node()), copy(lp(0), &node())],
                    &Type::Void,
                )],
                Terminator::Return(None),
            )],
            Type::Void,
        );
        let result = elide_graph(&graph(GET_VALUE, vec![caller, store()]));
        assert_eq!(result.c6_stats.elided_summary_parameters, 0);
        assert_eq!(
            function_counts(&result.c6, RUNNER),
            function_counts(&result.c5, RUNNER)
        );
        assert_ne!(function_counts(&result.c6, RUNNER), (0, 0));
    }

    #[test]
    fn a_borrowed_local_read_in_another_block_is_borrowed_while_the_window_stays_open() {
        // x = a.next in block 0, v = GetValue(x) in block 1: block 0 dominates
        // block 1 and nothing between them invalidates (design 15).
        let caller = function(
            RUNNER,
            vec![local(0, node(), false)],
            vec![local(1, node(), false), local(2, Type::Int, false)],
            vec![
                block(
                    0,
                    vec![assign(lp(1), &node(), copy(node_field(0, NEXT), &node()))],
                    Terminator::Goto(BasicBlockId(1)),
                ),
                block(
                    1,
                    vec![call_of(
                        GET_VALUE,
                        Some(2),
                        vec![copy(lp(1), &node())],
                        &Type::Int,
                    )],
                    Terminator::Return(Some(copy(lp(2), &Type::Int))),
                ),
            ],
            Type::Int,
        );
        let result = elide_graph(&graph(GET_VALUE, vec![caller, get_value()]));
        assert!(result.c6_stats.elided_borrows > 0, "{:?}", result.c6_stats);
        assert_eq!(function_counts(&result.c6, RUNNER), (0, 0));
    }

    // ---- E4 across blocks and through temporaries (design 15) ----

    fn bool_constant(value: bool) -> Operand {
        Operand {
            type_: Type::Bool,
            kind: OperandKind::Constant(mir::Constant::Boolean(value)),
        }
    }

    /// `int Sum(Node this)` with the array loaded once before the loop:
    /// `total = this.value; i = 0; children = this.children; while (i <
    /// children.Length) { child = children[0]; total += Sum(child); [Store];
    /// i += 1; } return total;`
    fn tree_sum_hoisted(releasing_in_loop: bool) -> Function {
        let mut body = vec![
            assign(lp(7), &node(), copy(element(3, &node()), &node())),
            call_of(SUM, Some(8), vec![copy(lp(7), &node())], &Type::Int),
            add(1, 1, 8),
        ];
        if releasing_in_loop {
            body.push(call_of(
                STORE,
                None,
                vec![copy(lp(0), &node()), copy(lp(0), &node())],
                &Type::Void,
            ));
        }
        body.push(compute(
            2,
            &Type::Int,
            copy(lp(2), &Type::Int),
            mir::BinaryOperator::Add,
            int_constant("1"),
        ));
        function(
            SUM,
            vec![local(0, node(), false)],
            vec![
                local(1, Type::Int, false),
                local(2, Type::Int, false),
                local(3, nodes(), false),
                local(4, Type::Int, true),
                local(5, Type::Bool, true),
                local(7, node(), false),
                local(8, Type::Int, true),
            ],
            vec![
                block(
                    0,
                    vec![
                        assign(lp(1), &Type::Int, copy(node_field(0, VALUE), &Type::Int)),
                        assign(lp(2), &Type::Int, int_constant("0")),
                        assign(lp(3), &nodes(), copy(node_field(0, CHILDREN), &nodes())),
                    ],
                    Terminator::Goto(BasicBlockId(1)),
                ),
                block(
                    1,
                    vec![
                        Instruction::Assign {
                            target: lp(4),
                            value: Rvalue {
                                type_: Type::Int,
                                kind: RvalueKind::ArrayLength(copy(lp(3), &nodes())),
                            },
                        },
                        compute(
                            5,
                            &Type::Bool,
                            copy(lp(2), &Type::Int),
                            mir::BinaryOperator::Less,
                            copy(lp(4), &Type::Int),
                        ),
                    ],
                    cond_branch(5, 2, 3),
                ),
                block(2, body, Terminator::Goto(BasicBlockId(1))),
                block(
                    3,
                    Vec::new(),
                    Terminator::Return(Some(copy(lp(1), &Type::Int))),
                ),
            ],
            Type::Int,
        )
    }

    #[test]
    fn array_loaded_before_a_loop_and_read_inside_has_no_count_operations() {
        let result = elide_graph(&graph(
            GET_VALUE,
            vec![tree_sum_hoisted(false), get_value()],
        ));
        assert_ne!(function_counts(&result.c5, SUM), (0, 0));
        assert_eq!(function_counts(&result.c6, SUM), (0, 0));
        assert!(result.c6_stats.elided_borrows > 0, "{:?}", result.c6_stats);
    }

    #[test]
    fn array_loaded_before_a_loop_stays_counted_when_the_loop_releases() {
        // `Store` releases `this.next` on every iteration: the window of
        // `children` is closed on the back edge, so the header read is not
        // covered.
        let result = elide_graph(&graph(
            GET_VALUE,
            vec![tree_sum_hoisted(true), store(), get_value()],
        ));
        assert_eq!(result.c6_stats.elided_borrows, 0, "{:?}", result.c6_stats);
        assert_ne!(function_counts(&result.c6, SUM), (0, 0));
    }

    /// `int Run(Node a, Node b)`: `x = a.next; if (true) { <branch> }
    /// return GetValue(x);` where the branch is empty on the other path.
    fn diamond(branch: Vec<Instruction>) -> Function {
        function(
            RUNNER,
            vec![local(0, node(), false), local(4, node(), false)],
            vec![
                local(1, node(), false),
                local(2, Type::Int, false),
                local(3, Type::Bool, true),
            ],
            vec![
                block(
                    0,
                    vec![
                        assign(lp(1), &node(), copy(node_field(0, NEXT), &node())),
                        assign(lp(3), &Type::Bool, bool_constant(true)),
                    ],
                    cond_branch(3, 1, 2),
                ),
                block(1, branch, Terminator::Goto(BasicBlockId(3))),
                block(2, Vec::new(), Terminator::Goto(BasicBlockId(3))),
                block(
                    3,
                    vec![call_of(
                        GET_VALUE,
                        Some(2),
                        vec![copy(lp(1), &node())],
                        &Type::Int,
                    )],
                    Terminator::Return(Some(copy(lp(2), &Type::Int))),
                ),
            ],
            Type::Int,
        )
    }

    #[test]
    fn borrowed_local_read_after_a_join_is_borrowed_when_both_paths_stay_open() {
        let result = elide_graph(&graph(
            GET_VALUE,
            vec![
                diamond(vec![call_of(
                    GET_VALUE,
                    None,
                    vec![copy(lp(0), &node())],
                    &Type::Int,
                )]),
                get_value(),
            ],
        ));
        assert!(result.c6_stats.elided_borrows > 0, "{:?}", result.c6_stats);
        assert_eq!(function_counts(&result.c6, RUNNER), (0, 0));
    }

    #[test]
    fn cross_block_window_with_a_releasing_call_on_one_path_stays_counted() {
        let result = elide_graph(&graph(
            GET_VALUE,
            vec![
                diamond(vec![call_of(
                    STORE,
                    None,
                    vec![copy(lp(0), &node()), copy(lp(4), &node())],
                    &Type::Void,
                )]),
                store(),
                get_value(),
            ],
        ));
        assert_eq!(result.c6_stats.elided_borrows, 0, "{:?}", result.c6_stats);
        assert_ne!(function_counts(&result.c6, RUNNER), (0, 0));
    }

    #[test]
    fn cross_block_window_with_a_list_set_on_one_path_stays_counted() {
        let result = elide_graph(&graph(
            GET_VALUE,
            vec![
                diamond(vec![Instruction::ListSet {
                    list: copy(node_field(0, ITEMS), &node_list()),
                    index: zero(),
                    value: copy(lp(4), &node()),
                }]),
                get_value(),
            ],
        ));
        assert_eq!(result.c6_stats.elided_borrows, 0, "{:?}", result.c6_stats);
        assert_ne!(function_counts(&result.c6, RUNNER), (0, 0));
    }

    #[test]
    fn cross_block_window_broken_by_a_write_of_the_source_field_on_one_path_stays_counted() {
        // `a.next = b` on one branch releases the object `x` borrowed.
        let result = elide_graph(&graph(
            GET_VALUE,
            vec![
                diamond(vec![assign(
                    node_field(0, NEXT),
                    &node(),
                    copy(lp(4), &node()),
                )]),
                get_value(),
            ],
        ));
        assert_eq!(result.c6_stats.elided_borrows, 0, "{:?}", result.c6_stats);
        assert_ne!(function_counts(&result.c6, RUNNER), (0, 0));
    }

    /// `int Run(List<Node> list)`: `Node entity = list[0]` lowered through
    /// the temporary, then `GetValue(entity)`; with `looped`, inside a loop
    /// as in `longrun_frame`; with `releasing`, `Store(entity, entity)` is
    /// called afterwards.
    fn entity_from_list(looped: bool, releasing: bool) -> Function {
        let mut body = vec![
            Instruction::ListGet {
                destination: lp(2),
                list: copy(lp(0), &node_list()),
                index: zero(),
                element_type: node(),
            },
            assign(lp(1), &node(), copy(lp(2), &node())),
            call_of(GET_VALUE, Some(3), vec![copy(lp(1), &node())], &Type::Int),
        ];
        if releasing {
            body.push(call_of(
                STORE,
                None,
                vec![copy(lp(1), &node()), copy(lp(1), &node())],
                &Type::Void,
            ));
        }
        let locals = vec![
            local(1, node(), false),
            local(2, node(), true),
            local(3, Type::Int, false),
            local(5, Type::Int, false),
            local(6, Type::Bool, true),
        ];
        let blocks = if looped {
            body.push(compute(
                5,
                &Type::Int,
                copy(lp(5), &Type::Int),
                mir::BinaryOperator::Add,
                int_constant("1"),
            ));
            vec![
                block(
                    0,
                    vec![assign(lp(5), &Type::Int, int_constant("0"))],
                    Terminator::Goto(BasicBlockId(1)),
                ),
                block(
                    1,
                    vec![compute(
                        6,
                        &Type::Bool,
                        copy(lp(5), &Type::Int),
                        mir::BinaryOperator::Less,
                        int_constant("3"),
                    )],
                    cond_branch(6, 2, 3),
                ),
                block(2, body, Terminator::Goto(BasicBlockId(1))),
                block(
                    3,
                    Vec::new(),
                    Terminator::Return(Some(copy(lp(3), &Type::Int))),
                ),
            ]
        } else {
            vec![block(
                0,
                body,
                Terminator::Return(Some(copy(lp(3), &Type::Int))),
            )]
        };
        function(
            RUNNER,
            vec![local(0, node_list(), false)],
            locals,
            blocks,
            Type::Int,
        )
    }

    #[test]
    fn list_element_copied_through_a_temporary_is_borrowed() {
        for looped in [false, true] {
            let result = elide_graph(&graph(
                GET_VALUE,
                vec![entity_from_list(looped, false), get_value()],
            ));
            assert_ne!(function_counts(&result.c5, RUNNER), (0, 0));
            assert_eq!(
                function_counts(&result.c6, RUNNER),
                (0, 0),
                "looped {looped}"
            );
            assert!(result.c6_stats.elided_borrows > 0, "{:?}", result.c6_stats);
        }
    }

    #[test]
    fn list_element_copied_through_a_temporary_stays_counted_across_a_releasing_call() {
        for looped in [false, true] {
            let result = elide_graph(&graph(
                GET_VALUE,
                vec![entity_from_list(looped, true), store(), get_value()],
            ));
            assert_eq!(result.c6_stats.elided_borrows, 0, "{:?}", result.c6_stats);
            assert_ne!(function_counts(&result.c6, RUNNER), (0, 0));
        }
    }
}
