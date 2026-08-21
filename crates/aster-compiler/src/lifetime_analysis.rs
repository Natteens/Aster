//! Conservative AARM reference-lifetime proofs over finalized MIR.
//!
//! The general MIR optimizer and owned-region planner reuse this one CFG
//! liveness solution. Its reference-death results alone do not authorize an
//! arena rewind, change executable MIR, or reach the runtime.

#![cfg_attr(not(test), allow(dead_code))]

use std::collections::{HashMap, HashSet, VecDeque};

use aster_mir as mir;

use crate::escape_analysis;

/// A conservative reference-death result for one static allocation site.
///
/// Empty `dead_after` means no early reference-death point was proven. Even a
/// non-empty result is not evidence that the shared LIFO arena can rewind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AllocationLifetimeProof {
    pub site: mir::MirAllocationSite,
    pub region: mir::AllocationRegion,
    pub aliases: Vec<mir::LocalId>,
    pub dead_after: Vec<mir::MirPoint>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct LifetimeProofSummary {
    pub dynamic_allocation_sites: usize,
    pub persistent_sites: usize,
    pub temporary_sites: usize,
    pub temporary_sites_with_reference_death: usize,
    pub temporary_sites_unresolved: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct LifetimeAnalysisReport {
    pub proofs: Vec<AllocationLifetimeProof>,
    pub summary: LifetimeProofSummary,
}

/// Analyze finalized MIR without mutating it or changing the normal compiler
/// pipeline. Escape regions and alias closures come exclusively from the
/// existing escape-analysis authority.
pub(super) fn analyze(module: &mir::Module) -> LifetimeAnalysisReport {
    let duplicate_functions = duplicate_function_symbols(module);
    if !duplicate_functions.is_empty() {
        // AllocationSite uses the function symbol as its module-local identity.
        // Duplicate symbols make every such identity ambiguous, and the
        // existing interprocedural summary map also assumes unique symbols.
        return LifetimeAnalysisReport::default();
    }
    let facts = escape_analysis::allocation_escape_facts(module);
    let mut analyses = HashMap::new();

    for function in &module.functions {
        analyses.insert(function.symbol, FunctionLiveness::build(function));
    }

    let ambiguous_sites = ambiguous_alias_sites(&facts);
    let mut proofs = facts
        .into_iter()
        .map(|fact| {
            let site = fact.site;
            let dead_after = if fact.region == mir::AllocationRegion::Temporary
                && !ambiguous_sites.contains(&site)
            {
                analyses
                    .get(&site.function)
                    .and_then(Option::as_ref)
                    .and_then(|analysis| analysis.reference_dead_after(site, &fact.aliases))
                    .unwrap_or_default()
            } else {
                Vec::new()
            };

            AllocationLifetimeProof {
                site,
                region: fact.region,
                aliases: fact.aliases,
                dead_after,
            }
        })
        .collect::<Vec<_>>();

    proofs.sort_by_key(|proof| {
        (
            proof.site.function.0,
            proof.site.block.0,
            proof.site.instruction_index,
        )
    });
    let persistent_sites = proofs
        .iter()
        .filter(|proof| proof.region == mir::AllocationRegion::Persistent)
        .count();
    let temporary_sites = proofs.len() - persistent_sites;
    let temporary_sites_with_reference_death = proofs
        .iter()
        .filter(|proof| {
            proof.region == mir::AllocationRegion::Temporary && !proof.dead_after.is_empty()
        })
        .count();

    LifetimeAnalysisReport {
        summary: LifetimeProofSummary {
            dynamic_allocation_sites: proofs.len(),
            persistent_sites,
            temporary_sites,
            temporary_sites_with_reference_death,
            temporary_sites_unresolved: temporary_sites - temporary_sites_with_reference_death,
        },
        proofs,
    }
}

/// One immutable view of the existing MIR liveness solution for a function.
/// The owned-region planner reuses it for every candidate call result.
pub(super) struct ReferenceLiveness {
    function: mir::SymbolId,
    analysis: Option<FunctionLiveness>,
}

pub(super) fn reference_liveness(function: &mir::Function) -> ReferenceLiveness {
    ReferenceLiveness {
        function: function.symbol,
        analysis: FunctionLiveness::build(function),
    }
}

impl ReferenceLiveness {
    /// Whether `local` is live immediately after one MIR instruction.
    ///
    /// General MIR dead-assignment elimination reuses this exact CFG solution
    /// instead of maintaining a second local-liveness implementation.
    pub(super) fn local_is_live_after(
        &self,
        block: mir::BasicBlockId,
        instruction_index: usize,
        local: mir::LocalId,
    ) -> Option<bool> {
        let analysis = self.analysis.as_ref()?;
        let block = *analysis.block_indices.get(&block)?;
        let local = *analysis.local_indices.get(&local)?;
        analysis
            .instruction_live_after
            .get(block)?
            .get(instruction_index)
            .map(|live| live.contains(local))
    }

    /// Apply the existing MIR liveness authority to a call result. The call
    /// site acts as the definition point; no region classification is inferred.
    pub(super) fn dead_after(
        &self,
        block: mir::BasicBlockId,
        instruction_index: usize,
        aliases: &[mir::LocalId],
    ) -> Vec<mir::MirPoint> {
        self.analysis
            .as_ref()
            .and_then(|analysis| {
                analysis.reference_dead_after(
                    mir::MirAllocationSite {
                        function: self.function,
                        block,
                        instruction_index,
                    },
                    aliases,
                )
            })
            .unwrap_or_default()
    }
}

fn duplicate_function_symbols(module: &mir::Module) -> HashSet<mir::SymbolId> {
    let mut seen = HashSet::new();
    let mut duplicates = HashSet::new();
    for function in &module.functions {
        if !seen.insert(function.symbol) {
            duplicates.insert(function.symbol);
        }
    }
    duplicates
}

/// Flow-insensitive aliases cannot safely distinguish two allocation sites
/// whose possible alias sets overlap. Keep their region decisions, but
/// withhold reference-death locations until a future reaching-definition
/// model can separate the sites.
fn ambiguous_alias_sites(
    facts: &[escape_analysis::AllocationEscapeFact],
) -> HashSet<mir::MirAllocationSite> {
    let mut ambiguous = HashSet::new();
    for (index, left) in facts.iter().enumerate() {
        if left.region != mir::AllocationRegion::Temporary {
            continue;
        }
        for right in &facts[index + 1..] {
            if right.region != mir::AllocationRegion::Temporary
                || left.site.function != right.site.function
            {
                continue;
            }
            if left.origin == right.origin
                || sorted_aliases_intersect(&left.aliases, &right.aliases)
            {
                ambiguous.insert(fact_site(left));
                ambiguous.insert(fact_site(right));
            }
        }
    }
    ambiguous
}

fn fact_site(fact: &escape_analysis::AllocationEscapeFact) -> mir::MirAllocationSite {
    fact.site
}

fn sorted_aliases_intersect(left: &[mir::LocalId], right: &[mir::LocalId]) -> bool {
    let (mut left_index, mut right_index) = (0, 0);
    while left_index < left.len() && right_index < right.len() {
        match left[left_index].0.cmp(&right[right_index].0) {
            std::cmp::Ordering::Less => left_index += 1,
            std::cmp::Ordering::Greater => right_index += 1,
            std::cmp::Ordering::Equal => return true,
        }
    }
    false
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DenseSet(Vec<bool>);

impl DenseSet {
    fn empty(len: usize) -> Self {
        Self(vec![false; len])
    }

    fn insert(&mut self, index: usize) {
        self.0[index] = true;
    }

    fn contains(&self, index: usize) -> bool {
        self.0[index]
    }

    fn union_with(&mut self, other: &Self) {
        for (value, incoming) in self.0.iter_mut().zip(&other.0) {
            *value |= incoming;
        }
    }

    fn without(&self, removed: &Self) -> Self {
        Self(
            self.0
                .iter()
                .zip(&removed.0)
                .map(|(value, removed)| *value && !*removed)
                .collect(),
        )
    }
}

#[derive(Clone)]
struct Access {
    uses: DenseSet,
    writes: DenseSet,
    must_defs: DenseSet,
}

impl Access {
    fn empty(local_count: usize) -> Self {
        Self {
            uses: DenseSet::empty(local_count),
            writes: DenseSet::empty(local_count),
            must_defs: DenseSet::empty(local_count),
        }
    }
}

struct FunctionLiveness {
    block_indices: HashMap<mir::BasicBlockId, usize>,
    local_indices: HashMap<mir::LocalId, usize>,
    successors: Vec<Vec<usize>>,
    instruction_live_after: Vec<Vec<DenseSet>>,
}

impl FunctionLiveness {
    fn build(function: &mir::Function) -> Option<Self> {
        let local_indices = local_indices(function)?;
        let block_indices = block_indices(function)?;
        if !block_indices.contains_key(&function.entry) {
            return None;
        }

        let mut successors = Vec::with_capacity(function.blocks.len());
        let mut instruction_accesses = Vec::with_capacity(function.blocks.len());
        let mut terminator_accesses = Vec::with_capacity(function.blocks.len());

        for block in &function.blocks {
            successors.push(successor_indices(&block.terminator, &block_indices)?);
            instruction_accesses.push(
                block
                    .instructions
                    .iter()
                    .map(|instruction| instruction_access(instruction, &local_indices))
                    .collect::<Option<Vec<_>>>()?,
            );
            terminator_accesses.push(terminator_access(&block.terminator, &local_indices)?);
        }

        let local_count = local_indices.len();
        let mut block_uses = Vec::with_capacity(function.blocks.len());
        let mut block_defs = Vec::with_capacity(function.blocks.len());
        for (accesses, terminator) in instruction_accesses.iter().zip(&terminator_accesses) {
            let mut uses = DenseSet::empty(local_count);
            let mut defs = DenseSet::empty(local_count);
            for access in accesses.iter().chain(std::iter::once(terminator)) {
                for index in 0..local_count {
                    if access.uses.contains(index) && !defs.contains(index) {
                        uses.insert(index);
                    }
                    if access.must_defs.contains(index) {
                        defs.insert(index);
                    }
                }
            }
            block_uses.push(uses);
            block_defs.push(defs);
        }

        let mut live_in = vec![DenseSet::empty(local_count); function.blocks.len()];
        let mut live_out = live_in.clone();
        loop {
            let mut changed = false;
            for block_index in (0..function.blocks.len()).rev() {
                let mut next_out = DenseSet::empty(local_count);
                for successor in &successors[block_index] {
                    next_out.union_with(&live_in[*successor]);
                }
                let mut next_in = next_out.without(&block_defs[block_index]);
                next_in.union_with(&block_uses[block_index]);
                if next_out != live_out[block_index] || next_in != live_in[block_index] {
                    live_out[block_index] = next_out;
                    live_in[block_index] = next_in;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }

        let mut instruction_live_after = Vec::with_capacity(function.blocks.len());
        for block_index in 0..function.blocks.len() {
            let accesses = &instruction_accesses[block_index];
            let mut after = vec![DenseSet::empty(local_count); accesses.len()];
            let mut live = live_out[block_index].clone();
            live.union_with(&terminator_accesses[block_index].uses);
            for instruction_index in (0..accesses.len()).rev() {
                after[instruction_index] = live.clone();
                live = live.without(&accesses[instruction_index].must_defs);
                live.union_with(&accesses[instruction_index].uses);
            }
            instruction_live_after.push(after);
        }

        Some(Self {
            block_indices,
            local_indices,
            successors,
            instruction_live_after,
        })
    }

    fn reference_dead_after(
        &self,
        site: mir::MirAllocationSite,
        aliases: &[mir::LocalId],
    ) -> Option<Vec<mir::MirPoint>> {
        if aliases.is_empty() {
            return None;
        }
        let alias_indices = aliases
            .iter()
            .map(|local| self.local_indices.get(local).copied())
            .collect::<Option<Vec<_>>>()?;
        let site_block = *self.block_indices.get(&site.block)?;
        if site.instruction_index >= self.instruction_live_after[site_block].len() {
            return None;
        }

        let reachable = self.reachable_blocks(site_block);
        let mut locations = Vec::new();
        for (block_id, block_index) in &self.block_indices {
            if !reachable[*block_index] {
                continue;
            }
            for (instruction_index, live_after) in
                self.instruction_live_after[*block_index].iter().enumerate()
            {
                if *block_index == site_block && instruction_index < site.instruction_index {
                    continue;
                }
                if alias_indices
                    .iter()
                    .all(|alias| !live_after.contains(*alias))
                {
                    locations.push(mir::MirPoint {
                        block: *block_id,
                        instruction_boundary: instruction_index.checked_add(1)?,
                    });
                }
            }
        }
        locations
            .sort_unstable_by_key(|location| (location.block.0, location.instruction_boundary));
        Some(locations)
    }

    fn reachable_blocks(&self, start: usize) -> Vec<bool> {
        let mut reachable = vec![false; self.successors.len()];
        let mut pending = VecDeque::from([start]);
        while let Some(block) = pending.pop_front() {
            if std::mem::replace(&mut reachable[block], true) {
                continue;
            }
            pending.extend(self.successors[block].iter().copied());
        }
        reachable
    }
}

fn local_indices(function: &mir::Function) -> Option<HashMap<mir::LocalId, usize>> {
    let mut locals = function
        .parameters
        .iter()
        .chain(&function.locals)
        .map(|local| local.id)
        .collect::<Vec<_>>();
    locals.sort_unstable_by_key(|local| local.0);
    if locals.windows(2).any(|pair| pair[0] == pair[1]) {
        return None;
    }
    Some(
        locals
            .into_iter()
            .enumerate()
            .map(|(index, local)| (local, index))
            .collect(),
    )
}

fn block_indices(function: &mir::Function) -> Option<HashMap<mir::BasicBlockId, usize>> {
    let mut indices = HashMap::with_capacity(function.blocks.len());
    for (index, block) in function.blocks.iter().enumerate() {
        if indices.insert(block.id, index).is_some() {
            return None;
        }
    }
    Some(indices)
}

fn successor_indices(
    terminator: &mir::Terminator,
    blocks: &HashMap<mir::BasicBlockId, usize>,
) -> Option<Vec<usize>> {
    match terminator {
        mir::Terminator::Goto(target) => Some(vec![*blocks.get(target)?]),
        mir::Terminator::Branch {
            then_block,
            else_block,
            ..
        } => Some(vec![*blocks.get(then_block)?, *blocks.get(else_block)?]),
        mir::Terminator::Return(_) | mir::Terminator::End | mir::Terminator::Unreachable => {
            Some(Vec::new())
        }
    }
}

#[allow(clippy::too_many_lines)]
fn instruction_access(
    instruction: &mir::Instruction,
    locals: &HashMap<mir::LocalId, usize>,
) -> Option<Access> {
    let mut access = Access::empty(locals.len());
    match instruction {
        mir::Instruction::OwnedRegionEnter { .. }
        | mir::Instruction::OwnedRegionExit { .. }
        | mir::Instruction::TemporarySubregionEnter { .. }
        | mir::Instruction::TemporarySubregionExit { .. } => {}
        mir::Instruction::Assign { target, value } => {
            read_rvalue(value, locals, &mut access.uses)?;
            write_place(target, true, locals, &mut access)?;
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
            read_operands(arguments, locals, &mut access.uses)?;
            write_optional_place(destination.as_ref(), true, locals, &mut access)?;
        }
        mir::Instruction::CallInterface {
            destination,
            receiver,
            arguments,
            ..
        } => {
            read_operand(receiver, locals, &mut access.uses)?;
            read_operands(arguments, locals, &mut access.uses)?;
            write_optional_place(destination.as_ref(), true, locals, &mut access)?;
        }
        mir::Instruction::AllocateArray {
            destination,
            length,
            ..
        } => {
            read_operand(length, locals, &mut access.uses)?;
            write_place(destination, true, locals, &mut access)?;
        }
        mir::Instruction::AllocateObject { destination, .. }
        | mir::Instruction::AllocateList { destination, .. }
        | mir::Instruction::AllocateDictionary { destination, .. }
        | mir::Instruction::AllocateStringBuilder { destination, .. } => {
            write_place(destination, true, locals, &mut access)?;
        }
        mir::Instruction::StringBuilderAppend { builder, value, .. } => {
            read_operand(builder, locals, &mut access.uses)?;
            read_operand(value, locals, &mut access.uses)?;
        }
        mir::Instruction::StringBuilderToString {
            destination,
            builder,
            ..
        } => {
            read_operand(builder, locals, &mut access.uses)?;
            write_place(destination, true, locals, &mut access)?;
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
            read_operand(dictionary, locals, &mut access.uses)?;
            read_operand(key, locals, &mut access.uses)?;
            read_operand(value, locals, &mut access.uses)?;
            write_place(destination, true, locals, &mut access)?;
        }
        mir::Instruction::DictionaryTryGet {
            destination,
            dictionary,
            key,
            ..
        } => {
            read_operand(dictionary, locals, &mut access.uses)?;
            read_operand(key, locals, &mut access.uses)?;
            write_place(destination, false, locals, &mut access)?;
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
            read_operand(dictionary, locals, &mut access.uses)?;
            read_operand(key, locals, &mut access.uses)?;
            write_place(destination, true, locals, &mut access)?;
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
            read_operand(dictionary, locals, &mut access.uses)?;
            write_place(destination, true, locals, &mut access)?;
        }
        mir::Instruction::DictionaryClear { dictionary } => {
            read_operand(dictionary, locals, &mut access.uses)?;
        }
        mir::Instruction::ListAdd { list, value } => {
            read_operand(list, locals, &mut access.uses)?;
            read_operand(value, locals, &mut access.uses)?;
        }
        mir::Instruction::ListGet {
            destination,
            list,
            index,
            ..
        } => {
            read_operand(list, locals, &mut access.uses)?;
            read_operand(index, locals, &mut access.uses)?;
            write_place(destination, false, locals, &mut access)?;
        }
        mir::Instruction::ListRemoveAt { list, index } => {
            read_operand(list, locals, &mut access.uses)?;
            read_operand(index, locals, &mut access.uses)?;
        }
        mir::Instruction::ListSet { list, index, value } => {
            read_operand(list, locals, &mut access.uses)?;
            read_operand(index, locals, &mut access.uses)?;
            read_operand(value, locals, &mut access.uses)?;
        }
        mir::Instruction::ListClear { list } => {
            read_operand(list, locals, &mut access.uses)?;
        }
        mir::Instruction::ListToArray {
            destination, list, ..
        } => {
            read_operand(list, locals, &mut access.uses)?;
            write_place(destination, true, locals, &mut access)?;
        }
        mir::Instruction::StringDecodeNext {
            string,
            cursor,
            char_destination,
            next_cursor_destination,
            ok_destination,
        } => {
            read_operand(string, locals, &mut access.uses)?;
            read_operand(cursor, locals, &mut access.uses)?;
            write_place(char_destination, false, locals, &mut access)?;
            write_place(next_cursor_destination, false, locals, &mut access)?;
            write_place(ok_destination, true, locals, &mut access)?;
        }
    }
    Some(access)
}

/// Whether `instruction` definitely replaces the complete storage of
/// `local`. This is the same fail-closed write authority used by MIR
/// liveness; loop proofs must not maintain a second destination classifier.
pub(super) fn instruction_defines_direct_local(
    function: &mir::Function,
    instruction: &mir::Instruction,
    local: mir::LocalId,
) -> bool {
    let Some(locals) = local_indices(function) else {
        return false;
    };
    let Some(index) = locals.get(&local).copied() else {
        return false;
    };
    instruction_access(instruction, &locals).is_some_and(|access| access.writes.contains(index))
}

fn terminator_access(
    terminator: &mir::Terminator,
    locals: &HashMap<mir::LocalId, usize>,
) -> Option<Access> {
    let mut access = Access::empty(locals.len());
    match terminator {
        mir::Terminator::Goto(_) | mir::Terminator::End | mir::Terminator::Unreachable => {}
        mir::Terminator::Branch { condition, .. } => {
            read_operand(condition, locals, &mut access.uses)?;
        }
        mir::Terminator::Return(value) => {
            if let Some(value) = value {
                read_operand(value, locals, &mut access.uses)?;
            }
        }
    }
    Some(access)
}

fn write_optional_place(
    place: Option<&mir::Place>,
    must_define: bool,
    locals: &HashMap<mir::LocalId, usize>,
    access: &mut Access,
) -> Option<()> {
    if let Some(place) = place {
        write_place(place, must_define, locals, access)?;
    }
    Some(())
}

fn write_place(
    place: &mir::Place,
    must_define: bool,
    locals: &HashMap<mir::LocalId, usize>,
    access: &mut Access,
) -> Option<()> {
    match place {
        mir::Place::Local(local) => {
            let index = *locals.get(local)?;
            access.writes.insert(index);
            if must_define {
                access.must_defs.insert(index);
            }
        }
        mir::Place::Symbol(_) => {}
        mir::Place::Field { base, .. } | mir::Place::EnumField { base, .. } => {
            read_place(base, locals, &mut access.uses)?;
        }
        mir::Place::Index { array, index, .. } => {
            read_operand(array, locals, &mut access.uses)?;
            read_operand(index, locals, &mut access.uses)?;
        }
        mir::Place::ObjectField { object, .. } => {
            read_operand(object, locals, &mut access.uses)?;
        }
    }
    Some(())
}

fn read_operands(
    operands: &[mir::Operand],
    locals: &HashMap<mir::LocalId, usize>,
    uses: &mut DenseSet,
) -> Option<()> {
    for operand in operands {
        read_operand(operand, locals, uses)?;
    }
    Some(())
}

fn read_operand(
    operand: &mir::Operand,
    locals: &HashMap<mir::LocalId, usize>,
    uses: &mut DenseSet,
) -> Option<()> {
    match &operand.kind {
        mir::OperandKind::Copy(place) => read_place(place, locals, uses),
        mir::OperandKind::Constant(_) | mir::OperandKind::Function(_) => Some(()),
    }
}

fn read_place(
    place: &mir::Place,
    locals: &HashMap<mir::LocalId, usize>,
    uses: &mut DenseSet,
) -> Option<()> {
    match place {
        mir::Place::Local(local) => uses.insert(*locals.get(local)?),
        mir::Place::Symbol(_) => {}
        mir::Place::Field { base, .. } | mir::Place::EnumField { base, .. } => {
            read_place(base, locals, uses)?;
        }
        mir::Place::Index { array, index, .. } => {
            read_operand(array, locals, uses)?;
            read_operand(index, locals, uses)?;
        }
        mir::Place::ObjectField { object, .. } => read_operand(object, locals, uses)?,
    }
    Some(())
}

fn read_rvalue(
    value: &mir::Rvalue,
    locals: &HashMap<mir::LocalId, usize>,
    uses: &mut DenseSet,
) -> Option<()> {
    match &value.kind {
        mir::RvalueKind::Use(operand)
        | mir::RvalueKind::Discriminant(operand)
        | mir::RvalueKind::ArrayLength(operand)
        | mir::RvalueKind::ListLength(operand)
        | mir::RvalueKind::DictionaryLength(operand)
        | mir::RvalueKind::ListVersion(operand)
        | mir::RvalueKind::StringByteLength(operand)
        | mir::RvalueKind::Cast(operand)
        | mir::RvalueKind::Unary { operand, .. } => read_operand(operand, locals, uses)?,
        mir::RvalueKind::Aggregate(fields) | mir::RvalueKind::EnumConstruct { fields, .. } => {
            for field in fields {
                read_operand(&field.value, locals, uses)?;
            }
        }
        mir::RvalueKind::MakeInterface { object, .. } => read_operand(object, locals, uses)?,
        mir::RvalueKind::Binary { left, right, .. }
        | mir::RvalueKind::Equality { left, right, .. } => {
            read_operand(left, locals, uses)?;
            read_operand(right, locals, uses)?;
        }
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLASS: mir::SymbolId = mir::SymbolId(900);
    const FUNCTION: mir::SymbolId = mir::SymbolId(100);
    const SINK: mir::LocalId = mir::LocalId(99);
    const CONDITION: mir::LocalId = mir::LocalId(98);

    fn local(id: u32, type_: mir::Type) -> mir::Local {
        mir::Local {
            id: mir::LocalId(id),
            symbol: None,
            name: format!("l{id}"),
            type_,
            mutable: true,
            temporary: true,
        }
    }

    fn block(
        id: u32,
        instructions: Vec<mir::Instruction>,
        terminator: mir::Terminator,
    ) -> mir::BasicBlock {
        mir::BasicBlock {
            id: mir::BasicBlockId(id),
            instructions,
            terminator,
        }
    }

    fn function(
        symbol: mir::SymbolId,
        entry: u32,
        blocks: Vec<mir::BasicBlock>,
        mut locals: Vec<mir::Local>,
    ) -> mir::Function {
        locals.push(local(SINK.0, mir::Type::Bool));
        locals.push(local(CONDITION.0, mir::Type::Bool));
        mir::Function {
            constructor: false,
            symbol,
            owner: None,
            name: format!("f{}", symbol.0),
            visibility: mir::Visibility::Public,
            parameters: Vec::new(),
            locals,
            return_type: mir::Type::Void,
            entry: mir::BasicBlockId(entry),
            blocks,
            temporary_subregion_candidates: Vec::new(),
        }
    }

    fn module(functions: Vec<mir::Function>) -> mir::Module {
        mir::Module {
            structs: Vec::new(),
            classes: Vec::new(),
            interfaces: Vec::new(),
            enums: Vec::new(),
            interface_implementations: Vec::new(),
            foreign_functions: Vec::new(),
            functions,
        }
    }

    fn object_local(id: u32) -> mir::Local {
        local(id, mir::Type::Class(CLASS))
    }

    fn copy(local: mir::LocalId, type_: mir::Type) -> mir::Operand {
        mir::Operand {
            type_,
            kind: mir::OperandKind::Copy(mir::Place::Local(local)),
        }
    }

    fn constant_bool(value: bool) -> mir::Operand {
        mir::Operand {
            type_: mir::Type::Bool,
            kind: mir::OperandKind::Constant(mir::Constant::Boolean(value)),
        }
    }

    fn allocate(destination: u32) -> mir::Instruction {
        mir::Instruction::AllocateObject {
            destination: mir::Place::Local(mir::LocalId(destination)),
            class: CLASS,
            region: mir::AllocationRegion::Persistent,
        }
    }

    fn observe(local: u32) -> mir::Instruction {
        let value = copy(mir::LocalId(local), mir::Type::Class(CLASS));
        mir::Instruction::Assign {
            target: mir::Place::Local(SINK),
            value: mir::Rvalue {
                type_: mir::Type::Bool,
                kind: mir::RvalueKind::Equality {
                    left: value.clone(),
                    right: value,
                    negated: false,
                },
            },
        }
    }

    fn assign_alias(destination: u32, source: u32) -> mir::Instruction {
        mir::Instruction::Assign {
            target: mir::Place::Local(mir::LocalId(destination)),
            value: mir::Rvalue {
                type_: mir::Type::Class(CLASS),
                kind: mir::RvalueKind::Use(copy(mir::LocalId(source), mir::Type::Class(CLASS))),
            },
        }
    }

    fn condition() -> mir::Operand {
        copy(CONDITION, mir::Type::Bool)
    }

    fn proof(
        report: &LifetimeAnalysisReport,
        function: mir::SymbolId,
        block: u32,
        instruction_index: usize,
    ) -> &AllocationLifetimeProof {
        report
            .proofs
            .iter()
            .find(|proof| {
                proof.site
                    == mir::MirAllocationSite {
                        function,
                        block: mir::BasicBlockId(block),
                        instruction_index,
                    }
            })
            .expect("allocation proof")
    }

    fn after(block: u32, instruction_index: usize) -> mir::MirPoint {
        mir::MirPoint {
            block: mir::BasicBlockId(block),
            instruction_boundary: instruction_index + 1,
        }
    }

    fn emitted_region(instruction: &mir::Instruction) -> Option<mir::AllocationRegion> {
        match instruction {
            mir::Instruction::AllocateArray { region, .. }
            | mir::Instruction::AllocateObject { region, .. }
            | mir::Instruction::AllocateList { region, .. }
            | mir::Instruction::AllocateDictionary { region, .. }
            | mir::Instruction::AllocateStringBuilder { region, .. }
            | mir::Instruction::StringBuilderToString { region, .. }
            | mir::Instruction::DictionaryEntries { region, .. }
            | mir::Instruction::DictionaryKeys { region, .. }
            | mir::Instruction::DictionaryValues { region, .. }
            | mir::Instruction::ListToArray { region, .. } => Some(*region),
            mir::Instruction::CallIntrinsic { intrinsic, .. } => intrinsic.allocation_region(),
            _ => None,
        }
    }

    #[test]
    fn straight_line_last_use_is_reference_dead_only_after_the_use() {
        let function = function(
            FUNCTION,
            10,
            vec![
                block(10, vec![allocate(1), observe(1)], mir::Terminator::End),
                block(999, vec![observe(1)], mir::Terminator::End),
            ],
            vec![object_local(1)],
        );
        let report = analyze(&module(vec![function]));
        let proof = proof(&report, FUNCTION, 10, 0);

        assert_eq!(proof.region, mir::AllocationRegion::Temporary);
        assert!(!proof.dead_after.contains(&after(10, 0)));
        assert!(proof.dead_after.contains(&after(10, 1)));
        assert!(
            proof
                .dead_after
                .iter()
                .all(|location| location.block != mir::BasicBlockId(999))
        );
    }

    #[test]
    fn alias_chain_stays_live_until_the_final_alias_use() {
        let function = function(
            FUNCTION,
            10,
            vec![block(
                10,
                vec![
                    allocate(7),
                    assign_alias(3, 7),
                    assign_alias(11, 3),
                    observe(11),
                ],
                mir::Terminator::End,
            )],
            vec![object_local(11), object_local(7), object_local(3)],
        );
        let report = analyze(&module(vec![function]));
        let proof = proof(&report, FUNCTION, 10, 0);

        assert_eq!(
            proof.aliases,
            vec![mir::LocalId(3), mir::LocalId(7), mir::LocalId(11)]
        );
        assert_eq!(proof.dead_after, vec![after(10, 3)]);
    }

    #[test]
    fn branch_successor_union_keeps_predecessor_live() {
        let function = function(
            FUNCTION,
            10,
            vec![
                block(
                    10,
                    vec![allocate(1)],
                    mir::Terminator::Branch {
                        condition: condition(),
                        then_block: mir::BasicBlockId(20),
                        else_block: mir::BasicBlockId(30),
                    },
                ),
                block(
                    20,
                    vec![observe(1)],
                    mir::Terminator::Goto(mir::BasicBlockId(40)),
                ),
                block(
                    30,
                    vec![mir::Instruction::Assign {
                        target: mir::Place::Local(SINK),
                        value: mir::Rvalue {
                            type_: mir::Type::Bool,
                            kind: mir::RvalueKind::Use(constant_bool(false)),
                        },
                    }],
                    mir::Terminator::Goto(mir::BasicBlockId(40)),
                ),
                block(40, Vec::new(), mir::Terminator::End),
            ],
            vec![object_local(1)],
        );
        let report = analyze(&module(vec![function]));
        let proof = proof(&report, FUNCTION, 10, 0);

        assert!(!proof.dead_after.contains(&after(10, 0)));
        assert!(proof.dead_after.contains(&after(20, 0)));
    }

    #[test]
    fn loop_backedge_reaches_a_fixed_point() {
        let function = function(
            FUNCTION,
            10,
            vec![
                block(
                    10,
                    vec![allocate(1)],
                    mir::Terminator::Goto(mir::BasicBlockId(20)),
                ),
                block(
                    20,
                    vec![observe(1)],
                    mir::Terminator::Branch {
                        condition: condition(),
                        then_block: mir::BasicBlockId(20),
                        else_block: mir::BasicBlockId(30),
                    },
                ),
                block(30, Vec::new(), mir::Terminator::End),
            ],
            vec![object_local(1)],
        );
        let report = analyze(&module(vec![function]));
        let proof = proof(&report, FUNCTION, 10, 0);

        assert!(proof.dead_after.is_empty());
        assert_eq!(report.summary.temporary_sites_unresolved, 1);
    }

    #[test]
    fn loop_local_site_can_be_reference_dead_before_the_backedge() {
        let function = function(
            FUNCTION,
            20,
            vec![
                block(
                    20,
                    vec![allocate(1), observe(1)],
                    mir::Terminator::Branch {
                        condition: condition(),
                        then_block: mir::BasicBlockId(20),
                        else_block: mir::BasicBlockId(30),
                    },
                ),
                block(30, Vec::new(), mir::Terminator::End),
            ],
            vec![object_local(1)],
        );
        let report = analyze(&module(vec![function]));

        assert!(
            proof(&report, FUNCTION, 20, 0)
                .dead_after
                .contains(&after(20, 1))
        );
    }

    #[test]
    fn early_return_does_not_hide_a_use_on_the_continuing_path() {
        let function = function(
            FUNCTION,
            10,
            vec![
                block(
                    10,
                    vec![allocate(1)],
                    mir::Terminator::Branch {
                        condition: condition(),
                        then_block: mir::BasicBlockId(20),
                        else_block: mir::BasicBlockId(30),
                    },
                ),
                block(20, Vec::new(), mir::Terminator::Return(None)),
                block(30, vec![observe(1)], mir::Terminator::Return(None)),
            ],
            vec![object_local(1)],
        );
        let report = analyze(&module(vec![function]));
        let proof = proof(&report, FUNCTION, 10, 0);

        assert!(!proof.dead_after.contains(&after(10, 0)));
        assert!(proof.dead_after.contains(&after(30, 0)));
    }

    #[test]
    fn nested_branches_union_every_continuing_successor() {
        let function = function(
            FUNCTION,
            10,
            vec![
                block(
                    10,
                    vec![allocate(1)],
                    mir::Terminator::Branch {
                        condition: condition(),
                        then_block: mir::BasicBlockId(20),
                        else_block: mir::BasicBlockId(30),
                    },
                ),
                block(
                    20,
                    Vec::new(),
                    mir::Terminator::Branch {
                        condition: condition(),
                        then_block: mir::BasicBlockId(40),
                        else_block: mir::BasicBlockId(50),
                    },
                ),
                block(30, Vec::new(), mir::Terminator::Return(None)),
                block(40, vec![observe(1)], mir::Terminator::End),
                block(50, Vec::new(), mir::Terminator::End),
            ],
            vec![object_local(1)],
        );
        let report = analyze(&module(vec![function]));
        let proof = proof(&report, FUNCTION, 10, 0);

        assert!(!proof.dead_after.contains(&after(10, 0)));
        assert!(proof.dead_after.contains(&after(40, 0)));
    }

    #[test]
    fn same_origin_allocation_sites_are_distinct_but_unresolved() {
        let function = function(
            FUNCTION,
            10,
            vec![block(
                10,
                vec![allocate(1), observe(1), allocate(1), observe(1)],
                mir::Terminator::End,
            )],
            vec![object_local(1)],
        );
        let report = analyze(&module(vec![function]));

        assert_eq!(report.proofs.len(), 2);
        assert_ne!(report.proofs[0].site, report.proofs[1].site);
        assert!(
            report
                .proofs
                .iter()
                .all(|proof| proof.dead_after.is_empty())
        );
        assert_eq!(report.summary.temporary_sites_unresolved, 2);
    }

    #[test]
    fn intersecting_flow_insensitive_alias_sets_withhold_both_site_proofs() {
        let function = function(
            FUNCTION,
            10,
            vec![block(
                10,
                vec![allocate(1), assign_alias(2, 1), allocate(2), observe(2)],
                mir::Terminator::End,
            )],
            vec![object_local(1), object_local(2)],
        );
        let report = analyze(&module(vec![function]));

        assert_eq!(report.proofs.len(), 2);
        assert!(
            report
                .proofs
                .iter()
                .all(|proof| proof.dead_after.is_empty())
        );
    }

    #[test]
    fn direct_local_overwrite_kills_the_previous_value_without_site_conflation() {
        let overwrite = mir::Instruction::Assign {
            target: mir::Place::Local(mir::LocalId(1)),
            value: mir::Rvalue {
                type_: mir::Type::Class(CLASS),
                kind: mir::RvalueKind::Use(mir::Operand {
                    type_: mir::Type::Class(CLASS),
                    kind: mir::OperandKind::Function(mir::SymbolId(777)),
                }),
            },
        };
        let function = function(
            FUNCTION,
            10,
            vec![block(
                10,
                vec![allocate(1), observe(1), overwrite, observe(1)],
                mir::Terminator::End,
            )],
            vec![object_local(1)],
        );
        let report = analyze(&module(vec![function]));
        let proof = proof(&report, FUNCTION, 10, 0);

        assert!(proof.dead_after.contains(&after(10, 1)));
        assert!(!proof.dead_after.contains(&after(10, 2)));
    }

    #[test]
    fn overlapping_allocations_report_reference_death_not_lifo_safety() {
        let function = function(
            FUNCTION,
            10,
            vec![block(
                10,
                vec![allocate(1), allocate(2), observe(1), observe(2)],
                mir::Terminator::End,
            )],
            vec![object_local(1), object_local(2)],
        );
        let report = analyze(&module(vec![function]));
        let older = proof(&report, FUNCTION, 10, 0);

        assert!(older.dead_after.contains(&after(10, 2)));
        assert!(
            older
                .dead_after
                .iter()
                .any(|location| *location == after(10, 2))
        );
        // The younger allocation is still live here, so this proof cannot
        // authorize rewinding the shared LIFO arena to the older allocation.
        assert!(
            !proof(&report, FUNCTION, 10, 1)
                .dead_after
                .contains(&after(10, 2))
        );
    }

    #[test]
    fn returned_persistent_allocation_never_receives_a_death_proof() {
        let function = function(
            FUNCTION,
            10,
            vec![block(
                10,
                vec![allocate(1)],
                mir::Terminator::Return(Some(copy(mir::LocalId(1), mir::Type::Class(CLASS)))),
            )],
            vec![object_local(1)],
        );
        let report = analyze(&module(vec![function]));
        let proof = proof(&report, FUNCTION, 10, 0);

        assert_eq!(proof.region, mir::AllocationRegion::Persistent);
        assert!(proof.dead_after.is_empty());
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn storage_containment_interface_and_unknown_calls_remain_persistent_barriers() {
        let value = copy(mir::LocalId(1), mir::Type::Class(CLASS));
        let functions = vec![
            function(
                mir::SymbolId(201),
                10,
                vec![block(
                    10,
                    vec![
                        allocate(1),
                        mir::Instruction::Assign {
                            target: mir::Place::Field {
                                base: Box::new(mir::Place::Local(mir::LocalId(2))),
                                field: mir::SymbolId(1),
                            },
                            value: mir::Rvalue {
                                type_: mir::Type::Class(CLASS),
                                kind: mir::RvalueKind::Use(value.clone()),
                            },
                        },
                    ],
                    mir::Terminator::End,
                )],
                vec![object_local(1), object_local(2)],
            ),
            function(
                mir::SymbolId(202),
                10,
                vec![block(
                    10,
                    vec![
                        allocate(1),
                        mir::Instruction::Assign {
                            target: mir::Place::Local(mir::LocalId(2)),
                            value: mir::Rvalue {
                                type_: mir::Type::User(mir::SymbolId(2)),
                                kind: mir::RvalueKind::Aggregate(vec![mir::FieldOperand {
                                    field: mir::SymbolId(3),
                                    value: value.clone(),
                                }]),
                            },
                        },
                    ],
                    mir::Terminator::End,
                )],
                vec![object_local(1), local(2, mir::Type::User(mir::SymbolId(2)))],
            ),
            function(
                mir::SymbolId(203),
                10,
                vec![block(
                    10,
                    vec![
                        allocate(1),
                        mir::Instruction::Assign {
                            target: mir::Place::Local(mir::LocalId(2)),
                            value: mir::Rvalue {
                                type_: mir::Type::Interface(mir::SymbolId(4)),
                                kind: mir::RvalueKind::MakeInterface {
                                    object: value.clone(),
                                    class: CLASS,
                                    interface: mir::SymbolId(4),
                                },
                            },
                        },
                    ],
                    mir::Terminator::End,
                )],
                vec![
                    object_local(1),
                    local(2, mir::Type::Interface(mir::SymbolId(4))),
                ],
            ),
            function(
                mir::SymbolId(204),
                10,
                vec![block(
                    10,
                    vec![
                        allocate(1),
                        mir::Instruction::Call {
                            destination: None,
                            function: mir::SymbolId(9999),
                            arguments: vec![value.clone()],
                            return_type: mir::Type::Void,
                        },
                    ],
                    mir::Terminator::End,
                )],
                vec![object_local(1)],
            ),
            function(
                mir::SymbolId(205),
                10,
                vec![block(
                    10,
                    vec![
                        mir::Instruction::AllocateArray {
                            destination: mir::Place::Local(mir::LocalId(1)),
                            element_type: mir::Type::Int,
                            length: constant_bool(false),
                            initialization: mir::ArrayInitialization::Default,
                            region: mir::AllocationRegion::Persistent,
                        },
                        mir::Instruction::CallIntrinsic {
                            destination: None,
                            intrinsic: mir::Intrinsic::ParallelForEach,
                            arguments: vec![copy(
                                mir::LocalId(1),
                                mir::Type::Array(Box::new(mir::Type::Int)),
                            )],
                            return_type: mir::Type::Void,
                        },
                    ],
                    mir::Terminator::End,
                )],
                vec![local(1, mir::Type::Array(Box::new(mir::Type::Int)))],
            ),
        ];
        let report = analyze(&module(functions));

        assert_eq!(report.summary.persistent_sites, 5);
        assert!(report.proofs.iter().all(|proof| {
            proof.region == mir::AllocationRegion::Persistent && proof.dead_after.is_empty()
        }));
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn collection_and_index_containment_remain_persistent_barriers() {
        let functions = vec![
            function(
                mir::SymbolId(301),
                10,
                vec![block(
                    10,
                    vec![
                        allocate(1),
                        mir::Instruction::Assign {
                            target: mir::Place::Index {
                                array: Box::new(copy(
                                    mir::LocalId(2),
                                    mir::Type::Array(Box::new(mir::Type::Class(CLASS))),
                                )),
                                index: Box::new(constant_bool(false)),
                                element_type: mir::Type::Class(CLASS),
                                bounds: mir::ArrayBounds::Checked,
                            },
                            value: mir::Rvalue {
                                type_: mir::Type::Class(CLASS),
                                kind: mir::RvalueKind::Use(copy(
                                    mir::LocalId(1),
                                    mir::Type::Class(CLASS),
                                )),
                            },
                        },
                    ],
                    mir::Terminator::End,
                )],
                vec![
                    object_local(1),
                    local(2, mir::Type::Array(Box::new(mir::Type::Class(CLASS)))),
                ],
            ),
            function(
                mir::SymbolId(302),
                10,
                vec![block(
                    10,
                    vec![
                        allocate(1),
                        mir::Instruction::ListAdd {
                            list: copy(
                                mir::LocalId(2),
                                mir::Type::List(Box::new(mir::Type::Class(CLASS))),
                            ),
                            value: copy(mir::LocalId(1), mir::Type::Class(CLASS)),
                        },
                    ],
                    mir::Terminator::End,
                )],
                vec![
                    object_local(1),
                    local(2, mir::Type::List(Box::new(mir::Type::Class(CLASS)))),
                ],
            ),
            function(
                mir::SymbolId(303),
                10,
                vec![block(
                    10,
                    vec![
                        allocate(1),
                        mir::Instruction::DictionaryAdd {
                            destination: mir::Place::Local(SINK),
                            dictionary: copy(
                                mir::LocalId(2),
                                mir::Type::Dictionary(
                                    Box::new(mir::Type::Class(CLASS)),
                                    Box::new(mir::Type::Class(CLASS)),
                                ),
                            ),
                            key: copy(mir::LocalId(1), mir::Type::Class(CLASS)),
                            value: copy(mir::LocalId(1), mir::Type::Class(CLASS)),
                        },
                    ],
                    mir::Terminator::End,
                )],
                vec![
                    object_local(1),
                    local(
                        2,
                        mir::Type::Dictionary(
                            Box::new(mir::Type::Class(CLASS)),
                            Box::new(mir::Type::Class(CLASS)),
                        ),
                    ),
                ],
            ),
            function(
                mir::SymbolId(304),
                10,
                vec![block(
                    10,
                    vec![
                        allocate(1),
                        mir::Instruction::DictionarySet {
                            destination: mir::Place::Local(SINK),
                            dictionary: copy(
                                mir::LocalId(2),
                                mir::Type::Dictionary(
                                    Box::new(mir::Type::Class(CLASS)),
                                    Box::new(mir::Type::Class(CLASS)),
                                ),
                            ),
                            key: copy(mir::LocalId(1), mir::Type::Class(CLASS)),
                            value: copy(mir::LocalId(1), mir::Type::Class(CLASS)),
                        },
                    ],
                    mir::Terminator::End,
                )],
                vec![
                    object_local(1),
                    local(
                        2,
                        mir::Type::Dictionary(
                            Box::new(mir::Type::Class(CLASS)),
                            Box::new(mir::Type::Class(CLASS)),
                        ),
                    ),
                ],
            ),
            function(
                mir::SymbolId(305),
                10,
                vec![block(
                    10,
                    vec![
                        allocate(1),
                        mir::Instruction::Assign {
                            target: mir::Place::Local(mir::LocalId(2)),
                            value: mir::Rvalue {
                                type_: mir::Type::User(mir::SymbolId(306)),
                                kind: mir::RvalueKind::EnumConstruct {
                                    case: mir::SymbolId(307),
                                    tag: 1,
                                    fields: vec![mir::FieldOperand {
                                        field: mir::SymbolId(308),
                                        value: copy(mir::LocalId(1), mir::Type::Class(CLASS)),
                                    }],
                                },
                            },
                        },
                    ],
                    mir::Terminator::End,
                )],
                vec![
                    object_local(1),
                    local(2, mir::Type::User(mir::SymbolId(306))),
                ],
            ),
        ];
        let report = analyze(&module(functions));

        assert_eq!(report.summary.persistent_sites, 5);
        assert!(report.proofs.iter().all(|proof| {
            proof.region == mir::AllocationRegion::Persistent && proof.dead_after.is_empty()
        }));
    }

    #[test]
    fn concurrency_intrinsic_boundaries_remain_escape_analysis_barriers() {
        let intrinsics = [
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
        ];
        let functions = intrinsics
            .into_iter()
            .enumerate()
            .map(|(index, intrinsic)| {
                function(
                    mir::SymbolId(400 + u32::try_from(index).expect("small index")),
                    10,
                    vec![block(
                        10,
                        vec![
                            allocate(1),
                            mir::Instruction::CallIntrinsic {
                                destination: None,
                                intrinsic,
                                arguments: vec![copy(mir::LocalId(1), mir::Type::Class(CLASS))],
                                return_type: mir::Type::Void,
                            },
                        ],
                        mir::Terminator::End,
                    )],
                    vec![object_local(1)],
                )
            })
            .collect();
        let report = analyze(&module(functions));

        // Every case contains the explicit object allocation. WaitAll also
        // owns a caller-context result-array allocation, even in this
        // intentionally shape-minimal barrier fixture.
        assert_eq!(report.summary.persistent_sites, intrinsics.len() + 1);
        assert!(report.proofs.iter().all(|proof| {
            proof.region == mir::AllocationRegion::Persistent && proof.dead_after.is_empty()
        }));
    }

    #[test]
    fn malformed_cfg_or_local_universe_withholds_proof() {
        let missing_successor = function(
            FUNCTION,
            10,
            vec![block(
                10,
                vec![allocate(1)],
                mir::Terminator::Goto(mir::BasicBlockId(999)),
            )],
            vec![object_local(1)],
        );
        let duplicate_local = function(
            mir::SymbolId(101),
            10,
            vec![block(10, vec![allocate(1)], mir::Terminator::End)],
            vec![object_local(1), object_local(1)],
        );
        let duplicate_block = function(
            mir::SymbolId(102),
            10,
            vec![
                block(10, vec![allocate(1)], mir::Terminator::End),
                block(10, Vec::new(), mir::Terminator::End),
            ],
            vec![object_local(1)],
        );
        let unknown_local = function(
            mir::SymbolId(103),
            10,
            vec![block(
                10,
                vec![allocate(1), observe(777)],
                mir::Terminator::End,
            )],
            vec![object_local(1)],
        );
        let missing_entry = function(
            mir::SymbolId(104),
            999,
            vec![block(10, vec![allocate(1)], mir::Terminator::End)],
            vec![object_local(1)],
        );
        let duplicate_symbol_left = function(
            mir::SymbolId(105),
            10,
            vec![block(10, vec![allocate(1)], mir::Terminator::End)],
            vec![object_local(1)],
        );
        let duplicate_symbol_right = function(
            mir::SymbolId(105),
            20,
            vec![block(20, vec![allocate(2)], mir::Terminator::End)],
            vec![object_local(2)],
        );
        let mut duplicate_symbol_right = duplicate_symbol_right;
        duplicate_symbol_right
            .parameters
            .push(local(77, mir::Type::Int));
        let malformed_report = analyze(&module(vec![
            missing_successor,
            duplicate_local,
            duplicate_block,
            unknown_local,
            missing_entry,
        ]));

        assert!(
            malformed_report
                .proofs
                .iter()
                .all(|proof| proof.dead_after.is_empty())
        );
        assert_eq!(
            analyze(&module(vec![duplicate_symbol_left, duplicate_symbol_right])),
            LifetimeAnalysisReport::default()
        );
    }

    #[test]
    fn sparse_ids_and_block_vector_order_produce_identical_proofs() {
        let blocks = vec![
            block(
                70,
                vec![allocate(42)],
                mir::Terminator::Goto(mir::BasicBlockId(900)),
            ),
            block(900, vec![observe(42)], mir::Terminator::End),
        ];
        let forward = function(FUNCTION, 70, blocks.clone(), vec![object_local(42)]);
        let reversed = function(
            FUNCTION,
            70,
            blocks.into_iter().rev().collect(),
            vec![object_local(42)],
        );

        assert_eq!(
            analyze(&module(vec![forward])),
            analyze(&module(vec![reversed]))
        );
    }

    #[test]
    fn nested_destination_is_a_use_and_only_an_exact_local_is_a_def() {
        let locals = HashMap::from([
            (mir::LocalId(1), 0),
            (mir::LocalId(2), 1),
            (mir::LocalId(3), 2),
        ]);
        let instruction = mir::Instruction::Assign {
            target: mir::Place::Field {
                base: Box::new(mir::Place::Local(mir::LocalId(1))),
                field: mir::SymbolId(88),
            },
            value: mir::Rvalue {
                type_: mir::Type::Bool,
                kind: mir::RvalueKind::Equality {
                    left: copy(mir::LocalId(2), mir::Type::Class(CLASS)),
                    right: copy(mir::LocalId(3), mir::Type::Class(CLASS)),
                    negated: false,
                },
            },
        };
        let access = instruction_access(&instruction, &locals).expect("supported access");

        assert!(access.uses.contains(0));
        assert!(access.uses.contains(1));
        assert!(access.uses.contains(2));
        assert!(!access.must_defs.0.into_iter().any(|defined| defined));
    }

    #[test]
    fn controlled_failure_destinations_are_not_liveness_kills() {
        let locals = HashMap::from([
            (mir::LocalId(1), 0),
            (mir::LocalId(2), 1),
            (mir::LocalId(3), 2),
        ]);
        let list_get = mir::Instruction::ListGet {
            destination: mir::Place::Local(mir::LocalId(1)),
            list: copy(
                mir::LocalId(2),
                mir::Type::List(Box::new(mir::Type::Class(CLASS))),
            ),
            index: constant_bool(false),
            element_type: mir::Type::Class(CLASS),
        };
        let decode = mir::Instruction::StringDecodeNext {
            string: copy(mir::LocalId(1), mir::Type::String),
            cursor: copy(mir::LocalId(2), mir::Type::Int),
            char_destination: mir::Place::Local(mir::LocalId(1)),
            next_cursor_destination: mir::Place::Local(mir::LocalId(2)),
            ok_destination: mir::Place::Local(mir::LocalId(3)),
        };

        let get_access = instruction_access(&list_get, &locals).expect("list access");
        assert!(!get_access.must_defs.contains(0));
        let decode_access = instruction_access(&decode, &locals).expect("decode access");
        assert!(!decode_access.must_defs.contains(0));
        assert!(!decode_access.must_defs.contains(1));
        assert!(decode_access.must_defs.contains(2));
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn every_current_dynamic_allocation_form_has_one_shared_escape_fact() {
        let instructions = vec![
            allocate(1),
            mir::Instruction::AllocateArray {
                destination: mir::Place::Local(mir::LocalId(2)),
                element_type: mir::Type::Int,
                length: constant_bool(false),
                initialization: mir::ArrayInitialization::Default,
                region: mir::AllocationRegion::Persistent,
            },
            mir::Instruction::AllocateList {
                destination: mir::Place::Local(mir::LocalId(3)),
                element_type: mir::Type::Int,
                region: mir::AllocationRegion::Persistent,
            },
            mir::Instruction::AllocateDictionary {
                destination: mir::Place::Local(mir::LocalId(4)),
                key_type: mir::Type::Int,
                value_type: mir::Type::Int,
                region: mir::AllocationRegion::Persistent,
            },
            mir::Instruction::AllocateStringBuilder {
                destination: mir::Place::Local(mir::LocalId(5)),
                class: mir::SymbolId(5),
                region: mir::AllocationRegion::Persistent,
            },
            mir::Instruction::StringBuilderToString {
                destination: mir::Place::Local(mir::LocalId(6)),
                builder: copy(mir::LocalId(5), mir::Type::Class(mir::SymbolId(5))),
                class: mir::SymbolId(5),
                region: mir::AllocationRegion::Persistent,
            },
            mir::Instruction::DictionaryEntries {
                destination: mir::Place::Local(mir::LocalId(7)),
                dictionary: copy(
                    mir::LocalId(4),
                    mir::Type::Dictionary(Box::new(mir::Type::Int), Box::new(mir::Type::Int)),
                ),
                key_type: mir::Type::Int,
                value_type: mir::Type::Int,
                entry_type: mir::Type::User(mir::SymbolId(7)),
                entry_layout: mir::DictionaryEntryLayout {
                    key_field: mir::SymbolId(70),
                    value_field: mir::SymbolId(71),
                },
                region: mir::AllocationRegion::Persistent,
            },
            mir::Instruction::CallIntrinsic {
                destination: Some(mir::Place::Local(mir::LocalId(8))),
                intrinsic: mir::Intrinsic::StringConcatTemporary,
                arguments: Vec::new(),
                return_type: mir::Type::String,
            },
        ];
        let function = function(
            FUNCTION,
            10,
            vec![block(10, instructions, mir::Terminator::End)],
            vec![
                object_local(1),
                local(2, mir::Type::Array(Box::new(mir::Type::Int))),
                local(3, mir::Type::List(Box::new(mir::Type::Int))),
                local(
                    4,
                    mir::Type::Dictionary(Box::new(mir::Type::Int), Box::new(mir::Type::Int)),
                ),
                local(5, mir::Type::Class(mir::SymbolId(5))),
                local(6, mir::Type::String),
                local(
                    7,
                    mir::Type::Array(Box::new(mir::Type::User(mir::SymbolId(7)))),
                ),
                local(8, mir::Type::String),
            ],
        );
        let mut module = module(vec![function]);
        escape_analysis::assign_allocation_regions(&mut module);
        let facts = escape_analysis::allocation_escape_facts(&module);
        let report = analyze(&module);

        assert_eq!(facts.len(), 8);
        for fact in &facts {
            let function = module
                .functions
                .iter()
                .find(|function| function.symbol == fact.site.function)
                .expect("fact function");
            let block = function
                .blocks
                .iter()
                .find(|block| block.id == fact.site.block)
                .expect("fact block");
            assert_eq!(
                emitted_region(&block.instructions[fact.site.instruction_index]),
                Some(fact.region),
                "shared fact must match finalized MIR at {:?}/{:?}/{}",
                fact.site.function,
                fact.site.block,
                fact.site.instruction_index
            );
        }
        assert_eq!(report.summary.dynamic_allocation_sites, 8);
        assert_eq!(
            report.summary.persistent_sites + report.summary.temporary_sites,
            8
        );
        assert_eq!(
            report.summary.temporary_sites_with_reference_death
                + report.summary.temporary_sites_unresolved,
            report.summary.temporary_sites
        );
    }

    #[test]
    fn borrowed_calls_are_uses_and_returned_aliases_extend_the_alias_closure() {
        let borrowed = crate::compile(
            "public class Box { public Box() {} public int Get() { return 1; } } \
             public int Run() { Box box = new Box(); return box.Get(); }",
        )
        .expect("borrowed call source");
        let borrowed_report = analyze(&borrowed.mir);
        let borrowed_run = borrowed
            .mir
            .functions
            .iter()
            .find(|function| function.name == "Run")
            .expect("Run")
            .symbol;
        let borrowed_proof = borrowed_report
            .proofs
            .iter()
            .find(|proof| proof.site.function == borrowed_run)
            .expect("Run allocation");
        assert_eq!(borrowed_proof.region, mir::AllocationRegion::Temporary);
        assert!(!borrowed_proof.dead_after.is_empty());

        let returned_alias = crate::compile(
            "public class Box { public Box() {} public int Get() { return 1; } } \
             public Box Identity(Box value) { return value; } \
             public int Run() { Box first = new Box(); Box second = Identity(first); return second.Get(); }",
        )
        .expect("alias-returning call source");
        let alias_report = analyze(&returned_alias.mir);
        let alias_run = returned_alias
            .mir
            .functions
            .iter()
            .find(|function| function.name == "Run")
            .expect("Run")
            .symbol;
        let alias_proof = alias_report
            .proofs
            .iter()
            .find(|proof| proof.site.function == alias_run)
            .expect("Run allocation");
        assert_eq!(alias_proof.region, mir::AllocationRegion::Temporary);
        assert!(alias_proof.aliases.len() >= 2);
        assert!(!alias_proof.dead_after.is_empty());
    }

    #[test]
    fn analysis_is_read_only_and_preserves_escape_regions() {
        let sources = [
            "public class Box { public Box() {} } public int Run() { Box box = new Box(); return 1; }",
            "public int Run() { int[] values = [1, 2, 3]; return values.Length; }",
            "public int Run() { List<int> values = new List<int>(); return values.Length; }",
            "public int Run() { Dictionary<string, int> values = new Dictionary<string, int>(); return values.Length; }",
            "public class Box { public Box() {} } public Box Make() { return new Box(); }",
            "public int Run() { string left = \"As\"; string value = left + \"ter\"; return value.Length; }",
            "public class Box { public Box() {} } public class Holder { public Box Value; public Holder(Box value) { Value = value; } } public int Run() { Box first = new Box(); Holder holder = new Holder(first); Box box = new Box(); holder.Value = box; return 0; }",
            "public class Box { public Box() {} } public void Visit(Box value, int depth) { if (depth > 0) { Visit(value, depth - 1); } } public int Run() { Box box = new Box(); Visit(box, 2); return 0; }",
            "public interface IBox { int Get(); } public class Box : IBox { public Box() {} public int Get() { return 1; } } public int Run() { Box box = new Box(); IBox view = box; return view.Get(); }",
        ];

        for source in sources {
            let compilation = crate::compile(source).expect("representative source compiles");
            let before = compilation.mir.clone();
            for fact in escape_analysis::allocation_escape_facts(&compilation.mir) {
                let function = compilation
                    .mir
                    .functions
                    .iter()
                    .find(|function| function.symbol == fact.site.function)
                    .expect("fact function");
                let block = function
                    .blocks
                    .iter()
                    .find(|block| block.id == fact.site.block)
                    .expect("fact block");
                assert_eq!(
                    emitted_region(&block.instructions[fact.site.instruction_index]),
                    Some(fact.region),
                    "shared fact must match the finalized MIR region at {:?}/{:?}/{}",
                    fact.site.function,
                    fact.site.block,
                    fact.site.instruction_index
                );
            }
            let first = analyze(&compilation.mir);
            let second = analyze(&compilation.mir);
            assert_eq!(first, second);
            assert_eq!(compilation.mir, before);
        }
    }
}
