//! Research-only planning for backend-neutral AARM Temporary subregion candidates.
//!
//! Candidate metadata is deliberately non-executable. The normal compiler
//! pipeline never invokes this module, and the execution backend rejects every
//! non-empty candidate list. Only the explicit research lowering below can
//! replace validated candidates with executable checkpoint instructions.

#![cfg_attr(not(test), allow(dead_code))]

use std::{
    collections::{HashMap, HashSet},
    error::Error,
    fmt,
};

use aster_mir as mir;

use crate::{escape_analysis, lifetime_analysis::LifetimeAnalysisReport};

mod validation;

use validation::TemporarySubregionValidationReport;

/// Deterministic evidence from the experimental AARM-5D MIR transformation.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AarmTemporarySubregionLoweringReport {
    pub validated_subregions_received: usize,
    pub subregions_lowered: usize,
    pub enter_instructions_inserted: usize,
    pub exit_instructions_inserted: usize,
}

/// Controlled failure from the experimental AARM-5D MIR transformation.
#[doc(hidden)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AarmTemporarySubregionLoweringError {
    message: &'static str,
}

impl AarmTemporarySubregionLoweringError {
    const fn new(message: &'static str) -> Self {
        Self { message }
    }

    #[must_use]
    pub const fn message(&self) -> &'static str {
        self.message
    }
}

impl fmt::Display for AarmTemporarySubregionLoweringError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl Error for AarmTemporarySubregionLoweringError {}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TemporarySubregionResearchAnalysis {
    lifetime: LifetimeAnalysisReport,
    validation: TemporarySubregionValidationReport,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FunctionCandidatePlan {
    function: mir::SymbolId,
    candidates: Vec<mir::TemporarySubregionCandidate>,
}

/// Opt in to the exact AARM-5A -> 5B -> 5C -> 5D research pipeline.
///
/// Ordinary [`crate::compile`] never calls this function. The transformation
/// operates on a clone, derives validation from that exact immutable MIR
/// snapshot, builds every transformed instruction stream before publication,
/// and replaces `module` only after the complete transformation succeeds.
/// Candidate metadata is cleared from the executable result so explicit MIR
/// instructions are its only subregion execution authority.
#[doc(hidden)]
pub fn lower_aarm_temporary_subregions_for_research(
    module: &mut mir::Module,
) -> Result<AarmTemporarySubregionLoweringReport, AarmTemporarySubregionLoweringError> {
    let mut analyzed = module.clone();
    if analyzed
        .functions
        .iter()
        .any(function_contains_executable_subregion_instruction)
    {
        return Err(AarmTemporarySubregionLoweringError::new(
            "AARM executable subregion lowering requires untransformed MIR",
        ));
    }
    escape_analysis::assign_allocation_regions(&mut analyzed);
    let analysis = analyze_plan_and_validate_candidate_subregions(&mut analyzed);
    let (lowered, report) = lower_validated_exact_snapshot(&analyzed, &analysis.validation)?;
    *module = lowered;
    Ok(report)
}

#[allow(clippy::too_many_lines)]
fn lower_validated_exact_snapshot(
    module: &mir::Module,
    validation: &TemporarySubregionValidationReport,
) -> Result<(mir::Module, AarmTemporarySubregionLoweringReport), AarmTemporarySubregionLoweringError>
{
    let mut lowered = module.clone();
    let mut report = AarmTemporarySubregionLoweringReport {
        validated_subregions_received: validation.validated.len(),
        ..AarmTemporarySubregionLoweringReport::default()
    };

    for function in &mut lowered.functions {
        let subregions = validation
            .validated
            .iter()
            .filter(|subregion| subregion.function == function.symbol)
            .collect::<Vec<_>>();

        if !subregions.is_empty() {
            let mut blocks = HashMap::new();
            for (index, block) in function.blocks.iter().enumerate() {
                if blocks.insert(block.id, index).is_some() {
                    return Err(AarmTemporarySubregionLoweringError::new(
                        "validated AARM subregion no longer has unique basic blocks",
                    ));
                }
            }
            let mut ids = HashSet::new();
            for subregion in &subregions {
                let Some(&checkpoint_index) = blocks.get(&subregion.checkpoint.block) else {
                    return Err(AarmTemporarySubregionLoweringError::new(
                        "validated AARM subregion no longer matches its MIR snapshot",
                    ));
                };
                if subregion.checkpoint.instruction_boundary
                    > function.blocks[checkpoint_index].instructions.len()
                    || subregion.rewinds.is_empty()
                    || subregion.rewinds.first() != Some(&subregion.rewind)
                    || subregion.rewinds.iter().any(|rewind| {
                        blocks.get(&rewind.block).is_none_or(|index| {
                            let block = &function.blocks[*index];
                            rewind.instruction_boundary > block.instructions.len()
                        })
                    })
                    || !ids.insert(subregion.id)
                    || !function
                        .temporary_subregion_candidates
                        .iter()
                        .any(|candidate| {
                            candidate.id == subregion.id
                                && candidate.checkpoint == subregion.checkpoint
                                && candidate.rewinds == subregion.rewinds
                                && candidate.allocations == subregion.allocations
                        })
                    || subregion.allocations.iter().any(|site| {
                        site.function != function.symbol
                            || blocks.get(&site.block).is_none_or(|index| {
                                let block = &function.blocks[*index];
                                site.instruction_index >= block.instructions.len()
                            })
                            || !function.blocks[blocks[&site.block]]
                                .instructions
                                .get(site.instruction_index)
                                .is_some_and(validated_temporary_allocation_form)
                    })
                {
                    return Err(AarmTemporarySubregionLoweringError::new(
                        "validated AARM subregion no longer matches its MIR snapshot",
                    ));
                }
            }

            let exits = subregions
                .iter()
                .try_fold(0_usize, |count, subregion| {
                    count.checked_add(subregion.rewinds.len())
                })
                .ok_or_else(|| {
                    AarmTemporarySubregionLoweringError::new(
                        "AARM subregion instruction count exceeds the addressable range",
                    )
                })?;
            let _inserted = subregions.len().checked_add(exits).ok_or_else(|| {
                AarmTemporarySubregionLoweringError::new(
                    "AARM subregion instruction count exceeds the addressable range",
                )
            })?;
            for block in &mut function.blocks {
                let original = &block.instructions;
                let additional = subregions
                    .iter()
                    .filter(|subregion| subregion.checkpoint.block == block.id)
                    .count()
                    + subregions
                        .iter()
                        .map(|subregion| {
                            subregion
                                .rewinds
                                .iter()
                                .filter(|rewind| rewind.block == block.id)
                                .count()
                        })
                        .sum::<usize>();
                let capacity = original.len().checked_add(additional).ok_or_else(|| {
                    AarmTemporarySubregionLoweringError::new(
                        "AARM subregion instruction count exceeds the addressable range",
                    )
                })?;
                let mut instructions = Vec::with_capacity(capacity);
                for boundary in 0..=original.len() {
                    for subregion in &subregions {
                        if subregion.rewinds.iter().any(|rewind| {
                            rewind.block == block.id && rewind.instruction_boundary == boundary
                        }) {
                            instructions.push(mir::Instruction::TemporarySubregionExit {
                                id: subregion.id,
                            });
                            report.exit_instructions_inserted += 1;
                        }
                    }
                    for subregion in &subregions {
                        if subregion.checkpoint.block == block.id
                            && subregion.checkpoint.instruction_boundary == boundary
                        {
                            instructions.push(mir::Instruction::TemporarySubregionEnter {
                                id: subregion.id,
                            });
                            report.enter_instructions_inserted += 1;
                        }
                    }
                    if let Some(instruction) = original.get(boundary) {
                        instructions.push(instruction.clone());
                    }
                }
                block.instructions = instructions;
            }
            report.subregions_lowered += subregions.len();
        }
        function.temporary_subregion_candidates.clear();
    }

    if report.subregions_lowered != report.validated_subregions_received
        || report.enter_instructions_inserted != report.subregions_lowered
        || report.exit_instructions_inserted < report.subregions_lowered
    {
        return Err(AarmTemporarySubregionLoweringError::new(
            "validated AARM subregion references an unknown function",
        ));
    }

    Ok((lowered, report))
}

fn validated_temporary_allocation_form(instruction: &mir::Instruction) -> bool {
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

/// Run the explicit research-only AARM-5A -> AARM-5B -> AARM-5C orchestration.
///
/// The lifetime report, candidate plans, and validation all observe the same
/// immutable executable MIR snapshot. Candidate metadata is attached only
/// after validation, so no independently pairable stale-report API exists.
fn analyze_plan_and_validate_candidate_subregions(
    module: &mut mir::Module,
) -> TemporarySubregionResearchAnalysis {
    for function in &mut module.functions {
        function.temporary_subregion_candidates.clear();
    }
    let analysis = validation::analyze_plan_validate_exact_snapshot(module);

    for (function, plan) in module.functions.iter_mut().zip(analysis.plans) {
        debug_assert_eq!(function.symbol, plan.function);
        function.temporary_subregion_candidates = plan.candidates;
    }

    TemporarySubregionResearchAnalysis {
        lifetime: analysis.lifetime,
        validation: analysis.validation,
    }
}

fn report_matches_module(module: &mir::Module, lifetime: &LifetimeAnalysisReport) -> bool {
    let mut functions = HashMap::new();
    let mut actual = Vec::new();
    let mut actual_sites = HashSet::new();

    for function in &module.functions {
        if functions.insert(function.symbol, function).is_some() {
            return false;
        }
        let mut blocks = HashSet::new();
        for block in &function.blocks {
            if !blocks.insert(block.id) {
                return false;
            }
            for (instruction_index, instruction) in block.instructions.iter().enumerate() {
                let Some(region) = escape_analysis::dynamic_allocation_region(instruction) else {
                    continue;
                };
                let site = mir::MirAllocationSite {
                    function: function.symbol,
                    block: block.id,
                    instruction_index,
                };
                if !actual_sites.insert(site) {
                    return false;
                }
                actual.push((site, region));
            }
        }
    }

    let mut reported = Vec::with_capacity(lifetime.proofs.len());
    let mut reported_sites = HashSet::new();
    for proof in &lifetime.proofs {
        if !reported_sites.insert(proof.site) {
            return false;
        }
        let Some(function) = functions.get(&proof.site.function) else {
            return false;
        };
        let Some(block) = function
            .blocks
            .iter()
            .find(|block| block.id == proof.site.block)
        else {
            return false;
        };
        let Some(instruction) = block.instructions.get(proof.site.instruction_index) else {
            return false;
        };
        if escape_analysis::dynamic_allocation_region(instruction) != Some(proof.region)
            || (proof.region == mir::AllocationRegion::Persistent && !proof.dead_after.is_empty())
            || proof.dead_after.iter().any(|point| {
                function
                    .blocks
                    .iter()
                    .find(|block| block.id == point.block)
                    .is_none_or(|block| {
                        point.instruction_boundary == 0
                            || point.instruction_boundary > block.instructions.len()
                    })
            })
        {
            return false;
        }
        reported.push((proof.site, proof.region));
    }

    let persistent_sites = lifetime
        .proofs
        .iter()
        .filter(|proof| proof.region == mir::AllocationRegion::Persistent)
        .count();
    let temporary_sites = lifetime.proofs.len() - persistent_sites;
    let temporary_sites_with_reference_death = lifetime
        .proofs
        .iter()
        .filter(|proof| {
            proof.region == mir::AllocationRegion::Temporary && !proof.dead_after.is_empty()
        })
        .count();
    if lifetime.summary
        != (crate::lifetime_analysis::LifetimeProofSummary {
            dynamic_allocation_sites: lifetime.proofs.len(),
            persistent_sites,
            temporary_sites,
            temporary_sites_with_reference_death,
            temporary_sites_unresolved: temporary_sites - temporary_sites_with_reference_death,
        })
    {
        return false;
    }

    actual.sort_unstable_by_key(site_region_key);
    reported.sort_unstable_by_key(site_region_key);
    actual == reported
}

fn site_region_key(
    (site, region): &(mir::MirAllocationSite, mir::AllocationRegion),
) -> (u32, u32, usize, u8) {
    (
        site.function.0,
        site.block.0,
        site.instruction_index,
        match region {
            mir::AllocationRegion::Temporary => 0,
            mir::AllocationRegion::Persistent => 1,
        },
    )
}

fn plan_function(
    function: &mir::Function,
    lifetime: &LifetimeAnalysisReport,
) -> Vec<mir::TemporarySubregionCandidate> {
    if function.blocks.len() != 1 {
        return if acyclic_cfg(function).is_some() {
            plan_acyclic_cfg_function(function, lifetime)
        } else {
            plan_simple_natural_loop_function(function, lifetime)
        };
    }
    let [block] = function.blocks.as_slice() else {
        return Vec::new();
    };
    if function.entry != block.id
        || !matches!(
            block.terminator,
            mir::Terminator::Return(_) | mir::Terminator::End
        )
        || function_contains_concurrency_boundary(function)
        || function_contains_executable_subregion_instruction(function)
    {
        return Vec::new();
    }

    let temporary_allocations = block
        .instructions
        .iter()
        .enumerate()
        .filter_map(|(index, instruction)| {
            (escape_analysis::dynamic_allocation_region(instruction)
                == Some(mir::AllocationRegion::Temporary))
            .then_some(index)
        })
        .collect::<Vec<_>>();
    let mut candidates = lifetime
        .proofs
        .iter()
        .filter(|proof| {
            proof.site.function == function.symbol
                && proof.site.block == block.id
                && proof.region == mir::AllocationRegion::Temporary
                && !proof.dead_after.is_empty()
        })
        .filter_map(|proof| {
            let allocation_index = proof.site.instruction_index;
            let rewind_boundary = proof
                .dead_after
                .iter()
                .filter(|point| {
                    point.block == block.id
                        && point.instruction_boundary > allocation_index
                        && point.instruction_boundary <= block.instructions.len()
                })
                .map(|point| point.instruction_boundary)
                .min()?;
            let younger_inside_span = temporary_allocations
                .iter()
                .any(|younger| allocation_index < *younger && *younger < rewind_boundary);
            (!younger_inside_span).then_some(mir::TemporarySubregionCandidate {
                id: mir::TemporarySubregionId(0),
                checkpoint: mir::MirPoint {
                    block: block.id,
                    instruction_boundary: allocation_index,
                },
                rewinds: vec![mir::MirPoint {
                    block: block.id,
                    instruction_boundary: rewind_boundary,
                }],
                allocations: vec![proof.site],
            })
        })
        .collect::<Vec<_>>();

    candidates.sort_unstable_by_key(|candidate| {
        (
            candidate.checkpoint.block.0,
            candidate.checkpoint.instruction_boundary,
            candidate.rewinds[0].instruction_boundary,
            candidate.allocations[0].instruction_index,
        )
    });
    for (index, candidate) in candidates.iter_mut().enumerate() {
        let Ok(id) = u32::try_from(index) else {
            return Vec::new();
        };
        candidate.id = mir::TemporarySubregionId(id);
    }

    if validate_simple_candidate_plan(function, &candidates) {
        candidates
    } else {
        Vec::new()
    }
}

/// A deliberately narrow AARM-5E2B1 natural loop. The header selects the
/// single body entry, while one or more latch and loop-exit blocks end an
/// iteration after the fine mark has been rewound.
#[derive(Clone, Debug)]
pub(super) struct SimpleNaturalLoop {
    pub(super) blocks: HashMap<mir::BasicBlockId, usize>,
    pub(super) successors: HashMap<mir::BasicBlockId, Vec<mir::BasicBlockId>>,
    pub(super) header: mir::BasicBlockId,
    pub(super) latches: Vec<mir::BasicBlockId>,
    pub(super) exits: Vec<mir::BasicBlockId>,
    pub(super) body_entry: mir::BasicBlockId,
    pub(super) body: HashSet<mir::BasicBlockId>,
    dominators: HashMap<mir::BasicBlockId, HashSet<mir::BasicBlockId>>,
}

impl SimpleNaturalLoop {
    pub(super) fn block<'a>(
        &self,
        function: &'a mir::Function,
        id: mir::BasicBlockId,
    ) -> &'a mir::BasicBlock {
        &function.blocks[self.blocks[&id]]
    }

    pub(super) fn dominates(&self, dominator: mir::BasicBlockId, block: mir::BasicBlockId) -> bool {
        self.dominators
            .get(&block)
            .is_some_and(|set| set.contains(&dominator))
    }
}

type CfgParts = (
    HashMap<mir::BasicBlockId, usize>,
    HashMap<mir::BasicBlockId, Vec<mir::BasicBlockId>>,
    HashMap<mir::BasicBlockId, Vec<mir::BasicBlockId>>,
    HashSet<mir::BasicBlockId>,
);

fn cfg_parts(function: &mir::Function) -> Option<CfgParts> {
    let mut blocks = HashMap::new();
    for (index, block) in function.blocks.iter().enumerate() {
        blocks.insert(block.id, index).is_none().then_some(())?;
    }
    blocks.contains_key(&function.entry).then_some(())?;
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
            mir::Terminator::Unreachable => return None,
        };
        targets
            .iter()
            .all(|target| blocks.contains_key(target))
            .then_some(())?;
        successors.insert(block.id, targets);
    }
    let mut reachable = HashSet::new();
    let mut pending = vec![function.entry];
    while let Some(block) = pending.pop() {
        if !reachable.insert(block) {
            continue;
        }
        pending.extend(successors[&block].iter().copied());
    }
    (reachable.len() == blocks.len()).then_some(())?;
    let mut predecessors = blocks
        .keys()
        .copied()
        .map(|block| (block, Vec::new()))
        .collect::<HashMap<_, _>>();
    for (block, targets) in &successors {
        for target in targets {
            predecessors.get_mut(target)?.push(*block);
        }
    }
    for targets in predecessors.values_mut() {
        targets.sort_unstable_by_key(|block| block.0);
    }
    Some((blocks, successors, predecessors, reachable))
}

fn cfg_dominators(
    entry: mir::BasicBlockId,
    reachable: &HashSet<mir::BasicBlockId>,
    predecessors: &HashMap<mir::BasicBlockId, Vec<mir::BasicBlockId>>,
) -> HashMap<mir::BasicBlockId, HashSet<mir::BasicBlockId>> {
    let mut order = reachable.iter().copied().collect::<Vec<_>>();
    order.sort_unstable_by_key(|block| block.0);
    let mut dominators = order
        .iter()
        .map(|block| {
            (
                *block,
                if *block == entry {
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
            if *block == entry {
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
            return dominators;
        }
    }
}

/// Build one natural-loop node set from a common-header backedge set.
#[allow(clippy::too_many_lines)]
fn natural_loop_from_parts(
    function: &mir::Function,
    blocks: HashMap<mir::BasicBlockId, usize>,
    successors: HashMap<mir::BasicBlockId, Vec<mir::BasicBlockId>>,
    predecessors: &HashMap<mir::BasicBlockId, Vec<mir::BasicBlockId>>,
    dominators: HashMap<mir::BasicBlockId, HashSet<mir::BasicBlockId>>,
    header: mir::BasicBlockId,
    mut latches: Vec<mir::BasicBlockId>,
) -> Option<SimpleNaturalLoop> {
    latches.sort_unstable_by_key(|block| block.0);
    latches.dedup();
    if latches.is_empty() || latches.contains(&header) {
        return None;
    }
    let mir::Terminator::Branch {
        then_block,
        else_block,
        ..
    } = function.blocks[blocks[&header]].terminator
    else {
        return None;
    };

    let mut body = HashSet::from([header]);
    let mut pending = latches.clone();
    while let Some(block) = pending.pop() {
        body.insert(block);
        for predecessor in &predecessors[&block] {
            if *predecessor != header && body.insert(*predecessor) {
                pending.push(*predecessor);
            }
        }
    }
    body.iter()
        .all(|block| dominators[block].contains(&header))
        .then_some(())?;
    let (body_entry, loop_exit) = match (body.contains(&then_block), body.contains(&else_block)) {
        (true, false) => (then_block, else_block),
        (false, true) => (else_block, then_block),
        _ => return None,
    };
    (body_entry != header && loop_exit != header).then_some(())?;
    (predecessors[&header]
        .iter()
        .filter(|predecessor| !body.contains(predecessor))
        .count()
        <= 1)
        .then_some(())?;
    for block in &body {
        if *block != header
            && predecessors[block]
                .iter()
                .any(|predecessor| !body.contains(predecessor))
        {
            return None;
        }
    }
    successors[&header]
        .iter()
        .filter(|successor| !body.contains(successor))
        .all(|successor| *successor == loop_exit)
        .then_some(())?;

    // The reverse natural-loop set contains paths which reach a latch.  A
    // source-level `break` or `return` has an explicit terminal block which
    // does not reach a latch, so include that one terminal edge block as an
    // iteration exit.  Direct mixed branch edges remain withheld until a
    // later slice grows deterministic edge splitting.
    let natural_body = body.clone();
    let mut exits = Vec::new();
    for block in natural_body
        .iter()
        .copied()
        .filter(|block| *block != header)
    {
        for successor in &successors[&block] {
            if natural_body.contains(successor) || *successor == header {
                continue;
            }
            if *successor == loop_exit {
                matches!(
                    function.blocks[blocks[&block]].terminator,
                    mir::Terminator::Goto(target) if target == loop_exit
                )
                .then_some(())?;
                exits.push(block);
                continue;
            }
            let exit_block = &function.blocks[blocks[successor]];
            if !predecessors[successor]
                .iter()
                .all(|predecessor| natural_body.contains(predecessor))
                || !matches!(
                    exit_block.terminator,
                    mir::Terminator::Goto(_) | mir::Terminator::Return(_) | mir::Terminator::End
                )
            {
                return None;
            }
            if body.insert(*successor) {
                exits.push(*successor);
            }
        }
    }
    exits.sort_unstable_by_key(|block| block.0);
    exits.dedup();

    let mut reverse = HashSet::new();
    let mut pending = latches.clone();
    while let Some(block) = pending.pop() {
        if !body.contains(&block) || !reverse.insert(block) {
            continue;
        }
        pending.extend(predecessors[&block].iter().copied());
    }
    natural_body
        .iter()
        .all(|block| *block == header || exits.contains(block) || reverse.contains(block))
        .then_some(())?;
    Some(SimpleNaturalLoop {
        blocks,
        successors,
        header,
        latches,
        exits,
        body_entry,
        body,
        dominators,
    })
}

/// Discover reducible natural loops in deterministic header order. Ancestor
/// loops may contain a child cycle; leaf selection below is responsible for
/// withholding those ancestors from executable fine lowering.
pub(super) fn natural_loops(function: &mir::Function) -> Vec<SimpleNaturalLoop> {
    let Some((blocks, successors, predecessors, reachable)) = cfg_parts(function) else {
        return Vec::new();
    };
    let dominators = cfg_dominators(function.entry, &reachable, &predecessors);
    let backedges = successors
        .iter()
        .flat_map(|(source, targets)| targets.iter().map(move |target| (*source, *target)))
        .filter(|(source, target)| dominators[source].contains(target))
        .collect::<HashSet<_>>();
    let mut indegree = reachable
        .iter()
        .copied()
        .map(|block| (block, 0_usize))
        .collect::<HashMap<_, _>>();
    for block in &reachable {
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
    let mut seen = 0;
    while let Some(block) = ready.pop() {
        seen += 1;
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
    if seen != reachable.len() {
        return Vec::new();
    }
    let mut grouped = HashMap::<mir::BasicBlockId, Vec<mir::BasicBlockId>>::new();
    for (source, target) in backedges {
        grouped.entry(target).or_default().push(source);
    }
    let mut headers = grouped.keys().copied().collect::<Vec<_>>();
    headers.sort_unstable_by_key(|header| header.0);
    headers
        .into_iter()
        .filter_map(|header| {
            natural_loop_from_parts(
                function,
                blocks.clone(),
                successors.clone(),
                &predecessors,
                dominators.clone(),
                header,
                grouped.remove(&header).unwrap_or_default(),
            )
        })
        .collect()
}

/// Return each natural loop's smallest strict containing loop, if any. The
/// `(body size, header)` ordering makes the parent choice canonical even when
/// the MIR block vector is permuted.
fn natural_loop_parents(loops: &[SimpleNaturalLoop]) -> Vec<Option<usize>> {
    loops
        .iter()
        .enumerate()
        .map(|(child_index, child)| {
            loops
                .iter()
                .enumerate()
                .filter(|(parent_index, parent)| {
                    *parent_index != child_index
                        && child.body.len() < parent.body.len()
                        && child.body.is_subset(&parent.body)
                })
                .min_by_key(|(_, parent)| (parent.body.len(), parent.header.0))
                .map(|(parent_index, _)| parent_index)
        })
        .collect()
}

/// Select only innermost natural loops. Their bodies contain no strict child
/// loop, so a B2A checkpoint can never dynamically enclose another checkpoint.
fn leaf_natural_loops(function: &mir::Function) -> Vec<SimpleNaturalLoop> {
    let loops = natural_loops(function);
    let parents = natural_loop_parents(&loops);
    loops
        .iter()
        .enumerate()
        .filter(|(index, _)| !parents.iter().flatten().any(|parent| parent == index))
        .map(|(_, loop_cfg)| loop_cfg.clone())
        .collect()
}

/// The deliberately small AARM-5E1 CFG model.  It only represents the
/// entry-reachable, acyclic portion of a function; malformed or cyclic CFGs
/// return `None` and are withheld from research planning.
#[derive(Clone, Debug)]
pub(super) struct AcyclicCfg {
    pub(super) blocks: HashMap<mir::BasicBlockId, usize>,
    pub(super) successors: HashMap<mir::BasicBlockId, Vec<mir::BasicBlockId>>,
    pub(super) reachable: HashSet<mir::BasicBlockId>,
    pub(super) topological: Vec<mir::BasicBlockId>,
    dominators: HashMap<mir::BasicBlockId, HashSet<mir::BasicBlockId>>,
    postdominators: HashMap<mir::BasicBlockId, HashSet<Option<mir::BasicBlockId>>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ImmediatePostdominator {
    Block(mir::BasicBlockId),
    FunctionExit,
}

impl AcyclicCfg {
    fn block<'a>(&self, function: &'a mir::Function, id: mir::BasicBlockId) -> &'a mir::BasicBlock {
        &function.blocks[self.blocks[&id]]
    }

    pub(super) fn dominates(&self, dominator: mir::BasicBlockId, block: mir::BasicBlockId) -> bool {
        self.dominators
            .get(&block)
            .is_some_and(|set| set.contains(&dominator))
    }

    pub(super) fn reachable_from(&self, start: mir::BasicBlockId) -> HashSet<mir::BasicBlockId> {
        let mut reached = HashSet::new();
        let mut pending = vec![start];
        while let Some(block) = pending.pop() {
            if !self.reachable.contains(&block) || !reached.insert(block) {
                continue;
            }
            pending.extend(self.successors[&block].iter().copied());
        }
        reached
    }

    pub(super) fn reachable_until(
        &self,
        start: mir::BasicBlockId,
        stop: Option<mir::BasicBlockId>,
    ) -> HashSet<mir::BasicBlockId> {
        let mut reached = HashSet::new();
        let mut pending = vec![start];
        while let Some(block) = pending.pop() {
            if Some(block) == stop || !self.reachable.contains(&block) || !reached.insert(block) {
                continue;
            }
            pending.extend(self.successors[&block].iter().copied());
        }
        reached
    }

    fn immediate_postdominator(&self, block: mir::BasicBlockId) -> Option<ImmediatePostdominator> {
        let postdominators = self.postdominators.get(&block)?;
        let mut candidates = postdominators
            .iter()
            .copied()
            .filter(|candidate| *candidate != Some(block))
            .collect::<Vec<_>>();
        candidates.sort_unstable_by_key(|candidate| candidate.map_or(u32::MAX, |id| id.0));
        candidates
            .into_iter()
            .find_map(|candidate| match candidate {
                None if postdominators.len() == 2 => Some(ImmediatePostdominator::FunctionExit),
                Some(candidate) => postdominators
                    .iter()
                    .copied()
                    .filter(|other| *other != Some(candidate) && *other != Some(block))
                    .all(|other| self.postdominators[&candidate].contains(&other))
                    .then_some(ImmediatePostdominator::Block(candidate)),
                None => None,
            })
    }
}

#[allow(clippy::too_many_lines)]
pub(super) fn acyclic_cfg(function: &mir::Function) -> Option<AcyclicCfg> {
    let mut blocks = HashMap::new();
    for (index, block) in function.blocks.iter().enumerate() {
        if blocks.insert(block.id, index).is_some() {
            return None;
        }
    }
    if !blocks.contains_key(&function.entry) {
        return None;
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
            mir::Terminator::Unreachable => return None,
        };
        if targets.iter().any(|target| !blocks.contains_key(target)) {
            return None;
        }
        successors.insert(block.id, targets);
    }
    let mut reachable = HashSet::new();
    let mut pending = vec![function.entry];
    while let Some(block) = pending.pop() {
        if !reachable.insert(block) {
            continue;
        }
        pending.extend(successors[&block].iter().copied());
    }
    let mut incoming = HashMap::<mir::BasicBlockId, usize>::new();
    for block in &reachable {
        incoming.insert(*block, 0);
    }
    for block in &reachable {
        for successor in &successors[block] {
            if reachable.contains(successor) {
                *incoming.get_mut(successor).expect("reachable successor") += 1;
            }
        }
    }
    let mut ready = incoming
        .iter()
        .filter_map(|(block, count)| (*count == 0).then_some(*block))
        .collect::<Vec<_>>();
    ready.sort_unstable_by_key(|block| block.0);
    let mut topological = Vec::with_capacity(reachable.len());
    while let Some(block) = ready.pop() {
        topological.push(block);
        for successor in &successors[&block] {
            if !reachable.contains(successor) {
                continue;
            }
            let count = incoming.get_mut(successor).expect("reachable successor");
            *count -= 1;
            if *count == 0 {
                ready.push(*successor);
            }
        }
        ready.sort_unstable_by_key(|block| std::cmp::Reverse(block.0));
    }
    if topological.len() != reachable.len() {
        return None;
    }

    let all = reachable.clone();
    let mut predecessors = HashMap::<mir::BasicBlockId, Vec<mir::BasicBlockId>>::new();
    for block in &reachable {
        predecessors.insert(*block, Vec::new());
    }
    for block in &reachable {
        for successor in &successors[block] {
            if reachable.contains(successor) {
                predecessors
                    .get_mut(successor)
                    .expect("reachable successor")
                    .push(*block);
            }
        }
    }
    let mut dominators = reachable
        .iter()
        .map(|block| {
            (
                *block,
                if *block == function.entry {
                    HashSet::from([*block])
                } else {
                    all.clone()
                },
            )
        })
        .collect::<HashMap<_, _>>();
    loop {
        let mut changed = false;
        for block in &topological {
            if *block == function.entry {
                continue;
            }
            let preds = &predecessors[block];
            let mut next = preds
                .iter()
                .map(|pred| dominators[pred].clone())
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
    let virtual_exit = None;
    let mut postdominators = reachable
        .iter()
        .map(|block| (*block, HashSet::from([Some(*block), virtual_exit])))
        .collect::<HashMap<_, _>>();
    loop {
        let mut changed = false;
        for block in topological.iter().rev() {
            let next_nodes = if successors[block].is_empty() {
                vec![virtual_exit]
            } else {
                successors[block].iter().copied().map(Some).collect()
            };
            let mut next = next_nodes
                .iter()
                .map(|successor| {
                    successor
                        .and_then(|id| postdominators.get(&id).cloned())
                        .unwrap_or_else(|| HashSet::from([virtual_exit]))
                })
                .reduce(|left, right| left.intersection(&right).copied().collect())
                .unwrap_or_else(|| HashSet::from([virtual_exit]));
            next.insert(Some(*block));
            if next != postdominators[block] {
                postdominators.insert(*block, next);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    Some(AcyclicCfg {
        blocks,
        successors,
        reachable,
        topological,
        dominators,
        postdominators,
    })
}

#[allow(clippy::too_many_lines)]
fn plan_acyclic_cfg_function(
    function: &mir::Function,
    lifetime: &LifetimeAnalysisReport,
) -> Vec<mir::TemporarySubregionCandidate> {
    if function_contains_concurrency_boundary(function)
        || function_contains_executable_subregion_instruction(function)
    {
        return Vec::new();
    }
    let Some(cfg) = acyclic_cfg(function) else {
        return Vec::new();
    };
    let proofs = lifetime
        .proofs
        .iter()
        .filter(|proof| {
            proof.site.function == function.symbol
                && proof.region == mir::AllocationRegion::Temporary
        })
        .map(|proof| (proof.site, proof))
        .collect::<HashMap<_, _>>();
    let mut candidates = Vec::new();
    for checkpoint_block in &cfg.topological {
        let Some(join) = cfg.immediate_postdominator(*checkpoint_block) else {
            continue;
        };
        let stop = match join {
            ImmediatePostdominator::Block(block) => Some(block),
            ImmediatePostdominator::FunctionExit => None,
        };
        let region = cfg.reachable_until(*checkpoint_block, stop);
        let rewinds = match join {
            ImmediatePostdominator::Block(join) => region
                .iter()
                .filter(|block| cfg.successors[block].contains(&join))
                .map(|block| mir::MirPoint {
                    block: *block,
                    instruction_boundary: cfg.block(function, *block).instructions.len(),
                })
                .collect::<Vec<_>>(),
            ImmediatePostdominator::FunctionExit => region
                .iter()
                .filter(|block| {
                    matches!(
                        cfg.block(function, **block).terminator,
                        mir::Terminator::Return(_) | mir::Terminator::End
                    )
                })
                .map(|block| mir::MirPoint {
                    block: *block,
                    instruction_boundary: cfg.block(function, *block).instructions.len(),
                })
                .collect::<Vec<_>>(),
        };
        if rewinds.is_empty() || rewinds.iter().any(|point| point.instruction_boundary == 0) {
            continue;
        }
        let mut rewinds = rewinds;
        rewinds.sort_unstable_by_key(|point| (point.block.0, point.instruction_boundary));
        let mut allocations = region
            .iter()
            .flat_map(|block| {
                cfg.block(function, *block)
                    .instructions
                    .iter()
                    .enumerate()
                    .filter_map(move |(instruction_index, instruction)| {
                        (escape_analysis::dynamic_allocation_region(instruction)
                            == Some(mir::AllocationRegion::Temporary))
                        .then_some(mir::MirAllocationSite {
                            function: function.symbol,
                            block: *block,
                            instruction_index,
                        })
                    })
            })
            .collect::<Vec<_>>();
        allocations.sort_unstable_by_key(|site| (site.block.0, site.instruction_index));
        if allocations.is_empty()
            || allocations
                .iter()
                .any(|site| !cfg.dominates(*checkpoint_block, site.block))
        {
            continue;
        }
        let proof_ok = allocations.iter().all(|site| {
            let Some(proof) = proofs.get(site) else {
                return false;
            };
            let reachable_from_site = cfg.reachable_from(site.block);
            let exits = rewinds
                .iter()
                .filter(|point| reachable_from_site.contains(&point.block))
                .collect::<Vec<_>>();
            !exits.is_empty()
                && exits
                    .into_iter()
                    .all(|point| proof.dead_after.contains(point))
        });
        if !proof_ok {
            continue;
        }
        candidates.push(mir::TemporarySubregionCandidate {
            id: mir::TemporarySubregionId(0),
            checkpoint: mir::MirPoint {
                block: *checkpoint_block,
                instruction_boundary: 0,
            },
            rewinds,
            allocations,
        });
    }
    candidates.sort_unstable_by_key(|candidate| {
        (
            candidate.checkpoint.block.0,
            candidate
                .rewinds
                .first()
                .map(|point| (point.block.0, point.instruction_boundary)),
            candidate.allocations.len(),
        )
    });
    let mut used_blocks = HashSet::new();
    let mut selected = Vec::new();
    for candidate in candidates {
        let join = cfg
            .immediate_postdominator(candidate.checkpoint.block)
            .and_then(|join| match join {
                ImmediatePostdominator::Block(block) => Some(block),
                ImmediatePostdominator::FunctionExit => None,
            });
        let region = cfg.reachable_until(candidate.checkpoint.block, join);
        // The rewinds leave the region before their successor/join.  This is
        // a conservative block-level disjointness test; it intentionally
        // withholds ambiguous/nested candidates rather than nesting marks.
        if region.iter().any(|block| used_blocks.contains(block)) {
            continue;
        }
        used_blocks.extend(region);
        selected.push(candidate);
    }
    for (index, candidate) in selected.iter_mut().enumerate() {
        let Ok(id) = u32::try_from(index) else {
            return Vec::new();
        };
        candidate.id = mir::TemporarySubregionId(id);
    }
    selected
}

fn plan_simple_natural_loop_function(
    function: &mir::Function,
    lifetime: &LifetimeAnalysisReport,
) -> Vec<mir::TemporarySubregionCandidate> {
    if function_contains_concurrency_boundary(function)
        || function_contains_executable_subregion_instruction(function)
    {
        return Vec::new();
    }
    let mut candidates = leaf_natural_loops(function)
        .into_iter()
        .filter_map(|loop_cfg| plan_one_natural_loop(function, lifetime, &loop_cfg))
        .collect::<Vec<_>>();
    candidates.sort_unstable_by_key(|candidate| {
        (
            candidate.checkpoint.block.0,
            candidate.checkpoint.instruction_boundary,
        )
    });
    for (index, candidate) in candidates.iter_mut().enumerate() {
        let Ok(id) = u32::try_from(index) else {
            return Vec::new();
        };
        candidate.id = mir::TemporarySubregionId(id);
    }
    candidates
}

fn plan_one_natural_loop(
    function: &mir::Function,
    lifetime: &LifetimeAnalysisReport,
    loop_cfg: &SimpleNaturalLoop,
) -> Option<mir::TemporarySubregionCandidate> {
    if function.blocks[loop_cfg.blocks[&loop_cfg.header]]
        .instructions
        .iter()
        .any(|instruction| {
            escape_analysis::dynamic_allocation_region(instruction)
                == Some(mir::AllocationRegion::Temporary)
        })
    {
        return None;
    }
    let rewinds = simple_loop_rewinds(function, loop_cfg)?;
    let mut body_blocks = loop_cfg
        .body
        .iter()
        .copied()
        .filter(|block| *block != loop_cfg.header)
        .collect::<Vec<_>>();
    body_blocks.sort_unstable_by_key(|block| block.0);
    let mut allocations = body_blocks
        .iter()
        .flat_map(|block| {
            loop_cfg
                .block(function, *block)
                .instructions
                .iter()
                .enumerate()
                .filter_map(move |(instruction_index, instruction)| {
                    (escape_analysis::dynamic_allocation_region(instruction)
                        == Some(mir::AllocationRegion::Temporary))
                    .then_some(mir::MirAllocationSite {
                        function: function.symbol,
                        block: *block,
                        instruction_index,
                    })
                })
        })
        .collect::<Vec<_>>();
    allocations.sort_unstable_by_key(|site| (site.block.0, site.instruction_index));
    if allocations.is_empty()
        || allocations
            .iter()
            .any(|site| !loop_cfg.dominates(loop_cfg.body_entry, site.block))
    {
        return None;
    }
    let proofs = lifetime
        .proofs
        .iter()
        .map(|proof| (proof.site, proof))
        .collect::<HashMap<_, _>>();
    if allocations.iter().any(|site| {
        proofs.get(site).is_none_or(|proof| {
            proof.region != mir::AllocationRegion::Temporary
                || simple_loop_reachable_rewinds(loop_cfg, site.block, &rewinds)
                    .iter()
                    .any(|rewind| !proof.dead_after.contains(rewind))
        })
    }) {
        return None;
    }
    let body_instructions = body_blocks
        .iter()
        .flat_map(|block| loop_cfg.block(function, *block).instructions.iter())
        .collect::<Vec<_>>();
    if validation::span_barrier_for_research(&body_instructions).is_some() {
        return None;
    }
    Some(mir::TemporarySubregionCandidate {
        id: mir::TemporarySubregionId(0),
        checkpoint: mir::MirPoint {
            block: loop_cfg.body_entry,
            instruction_boundary: 0,
        },
        rewinds,
        allocations,
    })
}

fn simple_loop_rewinds(
    function: &mir::Function,
    loop_cfg: &SimpleNaturalLoop,
) -> Option<Vec<mir::MirPoint>> {
    let mut blocks = loop_cfg.latches.clone();
    blocks.extend(loop_cfg.exits.iter().copied());
    blocks.sort_unstable_by_key(|block| block.0);
    blocks.dedup();
    blocks
        .iter()
        .all(|block| {
            let terminator = &loop_cfg.block(function, *block).terminator;
            (loop_cfg.latches.contains(block)
                && matches!(terminator, mir::Terminator::Goto(target) if *target == loop_cfg.header))
                || (loop_cfg.exits.contains(block)
                    && matches!(
                        terminator,
                        mir::Terminator::Goto(_)
                            | mir::Terminator::Return(_)
                            | mir::Terminator::End
                    ))
        })
        .then_some(())?;
    (!blocks.is_empty()).then_some(())?;
    Some(
        blocks
            .into_iter()
            .map(|block| mir::MirPoint {
                block,
                instruction_boundary: loop_cfg.block(function, block).instructions.len(),
            })
            .collect(),
    )
}

fn simple_loop_reachable_rewinds(
    loop_cfg: &SimpleNaturalLoop,
    start: mir::BasicBlockId,
    rewinds: &[mir::MirPoint],
) -> Vec<mir::MirPoint> {
    let exits = rewinds
        .iter()
        .map(|rewind| (rewind.block, *rewind))
        .collect::<HashMap<_, _>>();
    let mut seen = HashSet::new();
    let mut pending = vec![start];
    let mut reachable = Vec::new();
    while let Some(block) = pending.pop() {
        if !seen.insert(block) {
            continue;
        }
        if let Some(rewind) = exits.get(&block) {
            reachable.push(*rewind);
            continue;
        }
        pending.extend(
            loop_cfg.successors[&block]
                .iter()
                .copied()
                .filter(|successor| {
                    loop_cfg.body.contains(successor) && *successor != loop_cfg.header
                }),
        );
    }
    reachable.sort_unstable_by_key(|rewind| (rewind.block.0, rewind.instruction_boundary));
    reachable
}

fn function_contains_executable_subregion_instruction(function: &mir::Function) -> bool {
    function
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .any(|instruction| {
            matches!(
                instruction,
                mir::Instruction::TemporarySubregionEnter { .. }
                    | mir::Instruction::TemporarySubregionExit { .. }
            )
        })
}

fn function_contains_concurrency_boundary(function: &mir::Function) -> bool {
    function
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .any(|instruction| match instruction {
            mir::Instruction::CallIntrinsic { intrinsic, .. } => {
                is_concurrency_intrinsic(*intrinsic)
            }
            _ => false,
        })
}

fn is_concurrency_intrinsic(intrinsic: mir::Intrinsic) -> bool {
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

fn validate_simple_candidate_plan(
    function: &mir::Function,
    candidates: &[mir::TemporarySubregionCandidate],
) -> bool {
    let mut block_indices = HashMap::new();
    for block in &function.blocks {
        if block_indices.insert(block.id, block).is_some() {
            return false;
        }
    }

    let mut owned_allocations = HashSet::new();
    let mut previous_rewind = None;
    for (index, candidate) in candidates.iter().enumerate() {
        let Ok(expected_id) = u32::try_from(index) else {
            return false;
        };
        let [rewind] = candidate.rewinds.as_slice() else {
            return false;
        };
        let [allocation] = candidate.allocations.as_slice() else {
            return false;
        };
        let Some(checkpoint_block) = block_indices.get(&candidate.checkpoint.block) else {
            return false;
        };
        let Some(rewind_block) = block_indices.get(&rewind.block) else {
            return false;
        };
        let Some(allocation_block) = block_indices.get(&allocation.block) else {
            return false;
        };
        let Some(allocation_instruction) = allocation_block
            .instructions
            .get(allocation.instruction_index)
        else {
            return false;
        };
        if candidate.id != mir::TemporarySubregionId(expected_id)
            || allocation.function != function.symbol
            || !owned_allocations.insert(*allocation)
            || escape_analysis::dynamic_allocation_region(allocation_instruction)
                != Some(mir::AllocationRegion::Temporary)
            || candidate.checkpoint.block != allocation.block
            || rewind.block != allocation.block
            || candidate.checkpoint.instruction_boundary > checkpoint_block.instructions.len()
            || rewind.instruction_boundary > rewind_block.instructions.len()
            || candidate.checkpoint.instruction_boundary != allocation.instruction_index
            || rewind.instruction_boundary <= allocation.instruction_index
            || previous_rewind
                .is_some_and(|previous| previous > candidate.checkpoint.instruction_boundary)
        {
            return false;
        }
        previous_rewind = Some(rewind.instruction_boundary);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const FUNCTION: mir::SymbolId = mir::SymbolId(100);
    const OTHER_FUNCTION: mir::SymbolId = mir::SymbolId(101);
    const CLASS: mir::SymbolId = mir::SymbolId(900);
    const BLOCK: mir::BasicBlockId = mir::BasicBlockId(10);
    const SINK: mir::LocalId = mir::LocalId(99);

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
            functions,
        }
    }

    fn object_local(id: u32) -> mir::Local {
        local(id, mir::Type::Class(CLASS))
    }

    fn copy(id: u32, type_: mir::Type) -> mir::Operand {
        mir::Operand {
            type_,
            kind: mir::OperandKind::Copy(mir::Place::Local(mir::LocalId(id))),
        }
    }

    fn allocate_object(id: u32) -> mir::Instruction {
        mir::Instruction::AllocateObject {
            destination: mir::Place::Local(mir::LocalId(id)),
            class: CLASS,
            region: mir::AllocationRegion::Persistent,
        }
    }

    fn temporary_object(id: u32) -> mir::Instruction {
        mir::Instruction::AllocateObject {
            destination: mir::Place::Local(mir::LocalId(id)),
            class: CLASS,
            region: mir::AllocationRegion::Temporary,
        }
    }

    fn observe_object(id: u32) -> mir::Instruction {
        let operand = copy(id, mir::Type::Class(CLASS));
        mir::Instruction::Assign {
            target: mir::Place::Local(SINK),
            value: mir::Rvalue {
                type_: mir::Type::Bool,
                kind: mir::RvalueKind::Equality {
                    left: operand.clone(),
                    right: operand,
                    negated: false,
                },
            },
        }
    }

    fn unrelated() -> mir::Instruction {
        mir::Instruction::Assign {
            target: mir::Place::Local(SINK),
            value: mir::Rvalue {
                type_: mir::Type::Bool,
                kind: mir::RvalueKind::Use(mir::Operand {
                    type_: mir::Type::Bool,
                    kind: mir::OperandKind::Constant(mir::Constant::Boolean(true)),
                }),
            },
        }
    }

    fn prepare_research(
        mut module: mir::Module,
    ) -> (mir::Module, TemporarySubregionResearchAnalysis) {
        escape_analysis::assign_allocation_regions(&mut module);
        let analysis = analyze_plan_and_validate_candidate_subregions(&mut module);
        (module, analysis)
    }

    fn prepare(module: mir::Module) -> (mir::Module, LifetimeAnalysisReport) {
        let (module, analysis) = prepare_research(module);
        (module, analysis.lifetime)
    }

    fn single_block_plan(
        instructions: Vec<mir::Instruction>,
        locals: Vec<mir::Local>,
    ) -> (mir::Module, LifetimeAnalysisReport) {
        prepare(module(vec![function(
            FUNCTION,
            BLOCK.0,
            vec![block(BLOCK.0, instructions, mir::Terminator::End)],
            locals,
        )]))
    }

    fn site(instruction_index: usize) -> mir::MirAllocationSite {
        mir::MirAllocationSite {
            function: FUNCTION,
            block: BLOCK,
            instruction_index,
        }
    }

    fn point(instruction_boundary: usize) -> mir::MirPoint {
        mir::MirPoint {
            block: BLOCK,
            instruction_boundary,
        }
    }

    #[test]
    fn empty_and_persistent_functions_have_empty_candidate_plans() {
        let (empty, _) = single_block_plan(Vec::new(), Vec::new());
        assert!(empty.functions[0].temporary_subregion_candidates.is_empty());

        let returned = function(
            FUNCTION,
            BLOCK.0,
            vec![block(
                BLOCK.0,
                vec![allocate_object(1)],
                mir::Terminator::Return(Some(copy(1, mir::Type::Class(CLASS)))),
            )],
            vec![object_local(1)],
        );
        let (returned, report) = prepare(module(vec![returned]));
        assert_eq!(report.proofs[0].region, mir::AllocationRegion::Persistent);
        assert!(
            returned.functions[0]
                .temporary_subregion_candidates
                .is_empty()
        );
    }

    #[test]
    fn simple_last_use_and_immediate_death_use_precise_boundaries() {
        let (simple, _) = single_block_plan(
            vec![allocate_object(1), observe_object(1), unrelated()],
            vec![object_local(1)],
        );
        assert_eq!(
            simple.functions[0].temporary_subregion_candidates,
            vec![mir::TemporarySubregionCandidate {
                id: mir::TemporarySubregionId(0),
                checkpoint: point(0),
                rewinds: vec![point(2)],
                allocations: vec![site(0)],
            }]
        );

        let (immediate, _) = single_block_plan(vec![allocate_object(1)], vec![object_local(1)]);
        assert_eq!(
            immediate.functions[0].temporary_subregion_candidates[0].rewinds,
            vec![point(1)]
        );
    }

    #[test]
    fn sequential_lifetimes_emit_ordered_disjoint_candidates() {
        let (module, _) = single_block_plan(
            vec![
                allocate_object(1),
                observe_object(1),
                allocate_object(2),
                observe_object(2),
            ],
            vec![object_local(1), object_local(2)],
        );
        let candidates = &module.functions[0].temporary_subregion_candidates;
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].id, mir::TemporarySubregionId(0));
        assert_eq!(candidates[0].checkpoint, point(0));
        assert_eq!(candidates[0].rewinds, vec![point(2)]);
        assert_eq!(candidates[0].allocations, vec![site(0)]);
        assert_eq!(candidates[1].id, mir::TemporarySubregionId(1));
        assert_eq!(candidates[1].checkpoint, point(2));
        assert_eq!(candidates[1].rewinds, vec![point(4)]);
        assert_eq!(candidates[1].allocations, vec![site(2)]);
    }

    #[test]
    fn crossing_and_nested_lifetimes_withhold_the_older_candidate() {
        let (crossing, _) = single_block_plan(
            vec![
                allocate_object(1),
                allocate_object(2),
                observe_object(1),
                observe_object(2),
            ],
            vec![object_local(1), object_local(2)],
        );
        let crossing = &crossing.functions[0].temporary_subregion_candidates;
        assert_eq!(crossing.len(), 1);
        assert_eq!(crossing[0].allocations, vec![site(1)]);

        let (nested, _) = single_block_plan(
            vec![
                allocate_object(1),
                allocate_object(2),
                observe_object(2),
                observe_object(1),
            ],
            vec![object_local(1), object_local(2)],
        );
        let nested = &nested.functions[0].temporary_subregion_candidates;
        assert_eq!(nested.len(), 1);
        assert_eq!(nested[0].allocations, vec![site(1)]);
        assert_eq!(nested[0].rewinds, vec![point(3)]);
    }

    #[test]
    fn allocation_at_the_older_final_use_blocks_the_older_rewind() {
        let string = |id| local(id, mir::Type::String);
        let allocate_string = mir::Instruction::CallIntrinsic {
            destination: Some(mir::Place::Local(mir::LocalId(1))),
            intrinsic: mir::Intrinsic::StringFromLongTemporary,
            arguments: vec![mir::Operand {
                type_: mir::Type::Long,
                kind: mir::OperandKind::Constant(mir::Constant::Integer("1".to_owned())),
            }],
            return_type: mir::Type::String,
        };
        let allocate_younger_while_using_older = mir::Instruction::CallIntrinsic {
            destination: Some(mir::Place::Local(mir::LocalId(2))),
            intrinsic: mir::Intrinsic::StringConcatTemporary,
            arguments: vec![
                copy(1, mir::Type::String),
                mir::Operand {
                    type_: mir::Type::String,
                    kind: mir::OperandKind::Constant(mir::Constant::String("x".to_owned())),
                },
            ],
            return_type: mir::Type::String,
        };
        let (module, _) = single_block_plan(
            vec![allocate_string, allocate_younger_while_using_older],
            vec![string(1), string(2)],
        );
        let candidates = &module.functions[0].temporary_subregion_candidates;
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].allocations, vec![site(1)]);
    }

    #[test]
    fn same_local_ambiguity_and_unsupported_cfg_are_withheld() {
        let (same_local, report) = single_block_plan(
            vec![
                allocate_object(1),
                observe_object(1),
                allocate_object(1),
                observe_object(1),
            ],
            vec![object_local(1)],
        );
        assert!(
            report
                .proofs
                .iter()
                .all(|proof| proof.dead_after.is_empty())
        );
        assert!(
            same_local.functions[0]
                .temporary_subregion_candidates
                .is_empty()
        );

        let multi_block = function(
            FUNCTION,
            10,
            vec![
                block(
                    10,
                    vec![allocate_object(1)],
                    mir::Terminator::Goto(mir::BasicBlockId(20)),
                ),
                block(20, vec![observe_object(1)], mir::Terminator::End),
            ],
            vec![object_local(1)],
        );
        let (multi_block, _) = prepare(module(vec![multi_block]));
        assert!(
            multi_block.functions[0]
                .temporary_subregion_candidates
                .is_empty()
        );
    }

    #[test]
    fn collection_owner_may_be_a_candidate_but_is_not_marked_validated() {
        let list_type = mir::Type::List(Box::new(mir::Type::Int));
        let allocate = mir::Instruction::AllocateList {
            destination: mir::Place::Local(mir::LocalId(1)),
            element_type: mir::Type::Int,
            region: mir::AllocationRegion::Persistent,
        };
        let use_list = mir::Instruction::Assign {
            target: mir::Place::Local(mir::LocalId(2)),
            value: mir::Rvalue {
                type_: mir::Type::Int,
                kind: mir::RvalueKind::ListLength(copy(1, list_type.clone())),
            },
        };
        let (module, _) = single_block_plan(
            vec![allocate, use_list],
            vec![local(1, list_type), local(2, mir::Type::Int)],
        );
        let candidate = &module.functions[0].temporary_subregion_candidates[0];
        assert_eq!(candidate.allocations, vec![site(0)]);
        assert_eq!(candidate.rewinds, vec![point(2)]);
    }

    #[test]
    fn concurrency_boundary_withholds_otherwise_local_candidate() {
        let concurrency = mir::Instruction::CallIntrinsic {
            destination: None,
            intrinsic: mir::Intrinsic::TaskRun,
            arguments: Vec::new(),
            return_type: mir::Type::Void,
        };
        let (module, report) = single_block_plan(
            vec![allocate_object(1), observe_object(1), concurrency],
            vec![object_local(1)],
        );
        assert_eq!(report.proofs[0].region, mir::AllocationRegion::Temporary);
        assert!(
            module.functions[0]
                .temporary_subregion_candidates
                .is_empty()
        );
    }

    #[test]
    fn planning_is_deterministic_idempotent_and_replaces_prior_metadata() {
        let mut module = module(vec![function(
            FUNCTION,
            BLOCK.0,
            vec![block(
                BLOCK.0,
                vec![
                    allocate_object(1),
                    observe_object(1),
                    allocate_object(2),
                    observe_object(2),
                ],
                mir::Terminator::End,
            )],
            vec![object_local(1), object_local(2)],
        )]);
        escape_analysis::assign_allocation_regions(&mut module);
        analyze_plan_and_validate_candidate_subregions(&mut module);
        let expected = module.functions[0].temporary_subregion_candidates.clone();

        analyze_plan_and_validate_candidate_subregions(&mut module);
        assert_eq!(module.functions[0].temporary_subregion_candidates, expected);

        module.functions[0].temporary_subregion_candidates.clear();
        analyze_plan_and_validate_candidate_subregions(&mut module);
        assert_eq!(module.functions[0].temporary_subregion_candidates, expected);
    }

    #[test]
    fn structural_validator_accepts_boundaries_zero_and_n() {
        let function = function(
            FUNCTION,
            BLOCK.0,
            vec![block(
                BLOCK.0,
                vec![temporary_object(1), unrelated()],
                mir::Terminator::End,
            )],
            vec![object_local(1)],
        );
        let candidate = mir::TemporarySubregionCandidate {
            id: mir::TemporarySubregionId(0),
            checkpoint: point(0),
            rewinds: vec![point(2)],
            allocations: vec![site(0)],
        };
        assert!(validate_simple_candidate_plan(&function, &[candidate]));
    }

    #[test]
    fn structural_validator_rejects_invalid_points_and_allocation_sites() {
        let make_function = || {
            function(
                FUNCTION,
                BLOCK.0,
                vec![block(
                    BLOCK.0,
                    vec![temporary_object(1)],
                    mir::Terminator::End,
                )],
                vec![object_local(1)],
            )
        };
        let valid = || mir::TemporarySubregionCandidate {
            id: mir::TemporarySubregionId(0),
            checkpoint: point(0),
            rewinds: vec![point(1)],
            allocations: vec![site(0)],
        };

        let mut function = make_function();
        let mut candidate = valid();
        candidate.rewinds[0].instruction_boundary = 2;
        assert!(!validate_simple_candidate_plan(&function, &[candidate]));

        let mut candidate = valid();
        candidate.checkpoint.block = mir::BasicBlockId(999);
        assert!(!validate_simple_candidate_plan(&function, &[candidate]));

        let mut candidate = valid();
        candidate.allocations[0].function = OTHER_FUNCTION;
        assert!(!validate_simple_candidate_plan(&function, &[candidate]));

        let mut candidate = valid();
        candidate.allocations[0].instruction_index = 1;
        assert!(!validate_simple_candidate_plan(&function, &[candidate]));

        function.blocks[0].instructions[0] = unrelated();
        assert!(!validate_simple_candidate_plan(&function, &[valid()]));

        let mut function = make_function();
        if let mir::Instruction::AllocateObject { region, .. } =
            &mut function.blocks[0].instructions[0]
        {
            *region = mir::AllocationRegion::Persistent;
        }
        assert!(!validate_simple_candidate_plan(&function, &[valid()]));
    }

    #[test]
    fn structural_validator_rejects_duplicate_ids_ownership_and_crossing_order() {
        let function = function(
            FUNCTION,
            BLOCK.0,
            vec![block(
                BLOCK.0,
                vec![temporary_object(1), temporary_object(2)],
                mir::Terminator::End,
            )],
            vec![object_local(1), object_local(2)],
        );
        let first = mir::TemporarySubregionCandidate {
            id: mir::TemporarySubregionId(0),
            checkpoint: point(0),
            rewinds: vec![point(1)],
            allocations: vec![site(0)],
        };
        let second = mir::TemporarySubregionCandidate {
            id: mir::TemporarySubregionId(1),
            checkpoint: point(1),
            rewinds: vec![point(2)],
            allocations: vec![site(1)],
        };
        assert!(validate_simple_candidate_plan(
            &function,
            &[first.clone(), second.clone()]
        ));

        let mut duplicate_id = second.clone();
        duplicate_id.id = mir::TemporarySubregionId(0);
        assert!(!validate_simple_candidate_plan(
            &function,
            &[first.clone(), duplicate_id]
        ));

        let mut duplicate_site = second.clone();
        duplicate_site.allocations = first.allocations.clone();
        duplicate_site.checkpoint = first.checkpoint;
        assert!(!validate_simple_candidate_plan(
            &function,
            &[first.clone(), duplicate_site]
        ));

        assert!(!validate_simple_candidate_plan(&function, &[second, first]));
    }

    #[test]
    fn replanning_after_same_sized_mir_change_uses_fresh_liveness() {
        let (mut module, _) = single_block_plan(
            vec![allocate_object(1), observe_object(1), unrelated()],
            vec![object_local(1)],
        );
        assert_eq!(
            module.functions[0].temporary_subregion_candidates[0].rewinds,
            vec![point(2)]
        );
        module.functions[0].blocks[0].instructions.swap(1, 2);
        analyze_plan_and_validate_candidate_subregions(&mut module);
        assert_eq!(
            module.functions[0].temporary_subregion_candidates,
            vec![mir::TemporarySubregionCandidate {
                id: mir::TemporarySubregionId(0),
                checkpoint: point(0),
                rewinds: vec![point(3)],
                allocations: vec![site(0)],
            }]
        );
    }

    #[test]
    fn normal_source_lowering_keeps_research_candidates_empty() {
        let compilation = crate::compile(
            r"
class Box {
    public int Value;
}

public int Main() {
    Box value = new Box();
    value.Value = 7;
    return value.Value;
}
",
        )
        .expect("representative source compiles");
        assert!(
            compilation
                .mir
                .functions
                .iter()
                .all(|function| function.temporary_subregion_candidates.is_empty())
        );

        let async_compilation = crate::compile(
            "public int Compute() { return 42; } \
             public async Task<int> Calculate() { int value = await Task.Run(Compute); return value + 1; }",
        )
        .expect("representative async source compiles");
        assert!(
            async_compilation
                .mir
                .functions
                .iter()
                .all(|function| function.temporary_subregion_candidates.is_empty())
        );
    }

    mod temporary_subregion_validation {
        use super::*;
        use crate::temporary_subregions::validation::{
            self, TemporarySubregionRejectionReason as Reason, TemporarySubregionValidationReport,
        };

        fn candidate(
            id: u32,
            checkpoint: usize,
            rewind: usize,
            allocations: Vec<usize>,
        ) -> mir::TemporarySubregionCandidate {
            mir::TemporarySubregionCandidate {
                id: mir::TemporarySubregionId(id),
                checkpoint: point(checkpoint),
                rewinds: vec![point(rewind)],
                allocations: allocations.into_iter().map(site).collect(),
            }
        }

        fn validate_raw(
            mut module: mir::Module,
            candidates: impl IntoIterator<Item = mir::TemporarySubregionCandidate>,
        ) -> TemporarySubregionValidationReport {
            escape_analysis::assign_allocation_regions(&mut module);
            let lifetime = crate::lifetime_analysis::analyze(&module);
            let candidates = candidates.into_iter().collect::<Vec<_>>();
            validate_with_report(&module, &lifetime, &candidates)
        }

        fn validate_with_report(
            module: &mir::Module,
            lifetime: &LifetimeAnalysisReport,
            candidates: &[mir::TemporarySubregionCandidate],
        ) -> TemporarySubregionValidationReport {
            let plans = module
                .functions
                .iter()
                .map(|function| FunctionCandidatePlan {
                    function: function.symbol,
                    candidates: if function.symbol == FUNCTION {
                        candidates.to_vec()
                    } else {
                        Vec::new()
                    },
                })
                .collect::<Vec<_>>();
            validation::validate_for_test(module, lifetime, &plans)
        }

        fn reason(report: &TemporarySubregionValidationReport) -> Reason {
            assert!(report.validated.is_empty());
            assert_eq!(report.rejected.len(), 1);
            report.rejected[0].reason
        }

        fn end_function(
            instructions: Vec<mir::Instruction>,
            locals: Vec<mir::Local>,
        ) -> mir::Module {
            module(vec![function(
                FUNCTION,
                BLOCK.0,
                vec![block(BLOCK.0, instructions, mir::Terminator::End)],
                locals,
            )])
        }

        fn temporary_array(id: u32) -> mir::Instruction {
            mir::Instruction::AllocateArray {
                destination: mir::Place::Local(mir::LocalId(id)),
                element_type: mir::Type::Int,
                length: mir::Operand {
                    type_: mir::Type::Int,
                    kind: mir::OperandKind::Constant(mir::Constant::Integer("4".to_owned())),
                },
                requires_default: true,
                region: mir::AllocationRegion::Temporary,
            }
        }

        fn direct_call() -> mir::Instruction {
            mir::Instruction::Call {
                destination: None,
                function: OTHER_FUNCTION,
                arguments: Vec::new(),
                return_type: mir::Type::Void,
            }
        }

        fn interface_call() -> mir::Instruction {
            mir::Instruction::CallInterface {
                destination: None,
                receiver: copy(2, mir::Type::Class(CLASS)),
                method: mir::SymbolId(777),
                arguments: Vec::new(),
                return_type: mir::Type::Void,
            }
        }

        fn builder_allocation(id: u32, region: mir::AllocationRegion) -> mir::Instruction {
            mir::Instruction::AllocateStringBuilder {
                destination: mir::Place::Local(mir::LocalId(id)),
                class: CLASS,
                region,
            }
        }

        fn builder_append(id: u32) -> mir::Instruction {
            mir::Instruction::StringBuilderAppend {
                builder: copy(id, mir::Type::Class(CLASS)),
                value: mir::Operand {
                    type_: mir::Type::String,
                    kind: mir::OperandKind::Constant(mir::Constant::String("x".to_owned())),
                },
                class: CLASS,
            }
        }

        fn builder_to_string(id: u32, destination: u32) -> mir::Instruction {
            mir::Instruction::StringBuilderToString {
                destination: mir::Place::Local(mir::LocalId(destination)),
                builder: copy(id, mir::Type::Class(CLASS)),
                class: CLASS,
                region: mir::AllocationRegion::Temporary,
            }
        }

        fn builder_provenance(
            entry: u32,
            blocks: &[(u32, Vec<mir::Instruction>)],
            successors: &[(u32, Vec<u32>)],
            rewinds: &[(u32, usize)],
        ) -> Result<(), Reason> {
            let block_refs = blocks
                .iter()
                .map(|(block, instructions)| (mir::BasicBlockId(*block), instructions.as_slice()))
                .collect::<Vec<_>>();
            let successors = successors
                .iter()
                .map(|(block, targets)| {
                    (
                        mir::BasicBlockId(*block),
                        targets.iter().copied().map(mir::BasicBlockId).collect(),
                    )
                })
                .collect::<HashMap<_, _>>();
            let rewinds = rewinds
                .iter()
                .map(|(block, boundary)| mir::MirPoint {
                    block: mir::BasicBlockId(*block),
                    instruction_boundary: *boundary,
                })
                .collect::<Vec<_>>();
            validation::validate_builder_provenance(
                mir::BasicBlockId(entry),
                &block_refs,
                &successors,
                &rewinds,
            )
        }

        #[test]
        fn builder_provenance_straight_line_is_candidate_local_and_direct_local_only() {
            assert_eq!(
                builder_provenance(
                    10,
                    &[(
                        10,
                        vec![
                            builder_allocation(1, mir::AllocationRegion::Temporary),
                            builder_append(1),
                            builder_to_string(1, 2),
                        ],
                    )],
                    &[(10, Vec::new())],
                    &[(10, 3)],
                ),
                Ok(())
            );

            for instructions in [
                vec![builder_append(1)],
                vec![
                    builder_allocation(1, mir::AllocationRegion::Persistent),
                    builder_append(1),
                ],
                vec![
                    builder_allocation(1, mir::AllocationRegion::Temporary),
                    mir::Instruction::Assign {
                        target: mir::Place::Local(mir::LocalId(2)),
                        value: mir::Rvalue {
                            type_: mir::Type::Class(CLASS),
                            kind: mir::RvalueKind::Use(copy(1, mir::Type::Class(CLASS))),
                        },
                    },
                ],
                vec![
                    builder_allocation(1, mir::AllocationRegion::Temporary),
                    builder_allocation(1, mir::AllocationRegion::Temporary),
                ],
            ] {
                assert_eq!(
                    builder_provenance(10, &[(10, instructions)], &[(10, Vec::new())], &[(10, 2)],),
                    Err(Reason::BuilderOwnershipBarrier)
                );
            }

            let non_local_append = mir::Instruction::StringBuilderAppend {
                builder: mir::Operand {
                    type_: mir::Type::Class(CLASS),
                    kind: mir::OperandKind::Copy(mir::Place::ObjectField {
                        object: Box::new(copy(9, mir::Type::Class(CLASS))),
                        field: mir::SymbolId(901),
                    }),
                },
                value: mir::Operand {
                    type_: mir::Type::String,
                    kind: mir::OperandKind::Constant(mir::Constant::String("x".to_owned())),
                },
                class: CLASS,
            };
            assert_eq!(
                builder_provenance(
                    10,
                    &[(
                        10,
                        vec![
                            builder_allocation(1, mir::AllocationRegion::Temporary),
                            non_local_append,
                        ],
                    )],
                    &[(10, Vec::new())],
                    &[(10, 2)],
                ),
                Err(Reason::BuilderOwnershipBarrier)
            );
        }

        #[test]
        fn builder_provenance_acyclic_joins_require_identical_owned_sets() {
            let valid = builder_provenance(
                10,
                &[
                    (
                        10,
                        vec![builder_allocation(1, mir::AllocationRegion::Temporary)],
                    ),
                    (11, vec![builder_append(1)]),
                    (12, vec![unrelated()]),
                    (13, vec![builder_append(1), builder_to_string(1, 2)]),
                ],
                &[
                    (10, vec![11, 12]),
                    (11, vec![13]),
                    (12, vec![13]),
                    (13, vec![]),
                ],
                &[(13, 2)],
            );
            assert_eq!(valid, Ok(()));

            let same_builder_on_both_arms = builder_provenance(
                10,
                &[
                    (
                        10,
                        vec![builder_allocation(1, mir::AllocationRegion::Temporary)],
                    ),
                    (11, vec![builder_append(1)]),
                    (12, vec![builder_append(1)]),
                    (13, vec![builder_append(1)]),
                ],
                &[
                    (10, vec![11, 12]),
                    (11, vec![13]),
                    (12, vec![13]),
                    (13, vec![]),
                ],
                &[(13, 1)],
            );
            assert_eq!(same_builder_on_both_arms, Ok(()));

            let mismatch = builder_provenance(
                10,
                &[
                    (10, vec![]),
                    (
                        11,
                        vec![builder_allocation(1, mir::AllocationRegion::Temporary)],
                    ),
                    (12, vec![unrelated()]),
                    (13, vec![builder_append(1)]),
                ],
                &[
                    (10, vec![11, 12]),
                    (11, vec![13]),
                    (12, vec![13]),
                    (13, vec![]),
                ],
                &[(13, 1)],
            );
            assert_eq!(mismatch, Err(Reason::BuilderOwnershipBarrier));
        }

        #[test]
        fn builder_provenance_treats_each_loop_path_as_one_iteration_domain() {
            let iteration = builder_provenance(
                20,
                &[
                    (
                        20,
                        vec![builder_allocation(1, mir::AllocationRegion::Temporary)],
                    ),
                    (21, vec![builder_append(1)]),
                    (22, vec![builder_append(1)]),
                    (23, vec![builder_append(1)]),
                ],
                &[
                    (20, vec![21, 22]),
                    (21, vec![23]),
                    (22, vec![]),
                    (23, vec![]),
                ],
                &[(22, 1), (23, 1)],
            );
            assert_eq!(
                iteration,
                Ok(()),
                "continue/break/latch rewinds are terminal"
            );

            assert_eq!(
                builder_provenance(
                    20,
                    &[(20, vec![builder_append(1)])],
                    &[(20, Vec::new())],
                    &[(20, 1)],
                ),
                Err(Reason::BuilderOwnershipBarrier),
                "loop re-entry never inherits a previous iteration's ownership"
            );

            let multiple_latches = builder_provenance(
                20,
                &[
                    (
                        20,
                        vec![builder_allocation(1, mir::AllocationRegion::Temporary)],
                    ),
                    (21, vec![builder_append(1)]),
                    (22, vec![builder_append(1)]),
                ],
                &[(20, vec![21, 22]), (21, vec![]), (22, vec![])],
                &[(21, 1), (22, 1)],
            );
            assert_eq!(multiple_latches, Ok(()));

            let nested_leaf = builder_provenance(
                30,
                &[(
                    30,
                    vec![
                        builder_allocation(1, mir::AllocationRegion::Temporary),
                        builder_append(1),
                    ],
                )],
                &[(30, Vec::new())],
                &[(30, 2)],
            );
            assert_eq!(nested_leaf, Ok(()));
        }

        #[test]
        fn straight_line_object_and_array_candidates_validate() {
            let (object_module, object_analysis) = prepare_research(end_function(
                vec![allocate_object(1), observe_object(1), unrelated()],
                vec![object_local(1)],
            ));
            assert_eq!(
                object_analysis.validation.validated,
                vec![validation::ValidatedTemporarySubregion {
                    function: FUNCTION,
                    id: mir::TemporarySubregionId(0),
                    checkpoint: point(0),
                    rewind: point(2),
                    rewinds: vec![point(2)],
                    allocations: vec![site(0)],
                }]
            );
            assert!(object_analysis.validation.rejected.is_empty());
            assert_eq!(object_analysis.validation.validated_allocation_count(), 1);
            assert_eq!(
                object_module.functions[0]
                    .temporary_subregion_candidates
                    .len(),
                1
            );

            let (_, immediate_object) = prepare_research(end_function(
                vec![allocate_object(1)],
                vec![object_local(1)],
            ));
            assert_eq!(immediate_object.validation.validated.len(), 1);
            assert_eq!(
                immediate_object.validation.validated[0].checkpoint,
                point(0)
            );
            assert_eq!(immediate_object.validation.validated[0].rewind, point(1));

            let (array_module, array_analysis) = prepare_research(end_function(
                vec![temporary_array(1)],
                vec![local(1, mir::Type::Array(Box::new(mir::Type::Int)))],
            ));
            assert_eq!(array_analysis.validation.validated.len(), 1);
            assert_eq!(array_analysis.validation.validated[0].rewind, point(1));
            assert_eq!(
                array_analysis.validation.validated[0].allocations,
                vec![site(0)]
            );
            assert_eq!(
                array_module.functions[0].temporary_subregion_candidates[0].rewinds,
                vec![point(1)]
            );
        }

        #[test]
        fn older_live_and_persistent_allocations_do_not_block_inner_rewind() {
            let (older_live, analysis) = prepare_research(end_function(
                vec![
                    allocate_object(1),
                    allocate_object(2),
                    observe_object(2),
                    observe_object(1),
                ],
                vec![object_local(1), object_local(2)],
            ));
            assert_eq!(analysis.validation.validated.len(), 1);
            assert_eq!(analysis.validation.validated[0].checkpoint, point(1));
            assert_eq!(analysis.validation.validated[0].rewind, point(3));
            assert_eq!(analysis.validation.validated[0].allocations, vec![site(1)]);
            assert_eq!(
                older_live.functions[0].temporary_subregion_candidates[0].allocations,
                vec![site(1)]
            );

            let persistent_between = function(
                FUNCTION,
                BLOCK.0,
                vec![block(
                    BLOCK.0,
                    vec![allocate_object(1), allocate_object(2), observe_object(1)],
                    mir::Terminator::Return(Some(copy(2, mir::Type::Class(CLASS)))),
                )],
                vec![object_local(1), object_local(2)],
            );
            let (_, analysis) = prepare_research(module(vec![persistent_between]));
            assert_eq!(analysis.validation.validated.len(), 1);
            assert_eq!(analysis.validation.validated[0].allocations, vec![site(0)]);
            assert_eq!(analysis.validation.validated[0].rewind, point(3));
        }

        #[test]
        fn later_allocations_are_outside_rewind_and_complete_multi_site_spans_validate() {
            let module = end_function(
                vec![
                    temporary_object(1),
                    observe_object(1),
                    temporary_object(2),
                    observe_object(2),
                ],
                vec![object_local(1), object_local(2)],
            );
            let earlier = validate_raw(module.clone(), vec![candidate(0, 0, 2, vec![0])]);
            assert_eq!(earlier.validated.len(), 1);
            assert_eq!(earlier.validated[0].allocations, vec![site(0)]);
            assert_eq!(earlier.validated[0].rewind, point(2));

            let combined = validate_raw(module, vec![candidate(0, 0, 4, vec![0, 2])]);
            assert_eq!(combined.validated.len(), 1);
            assert_eq!(combined.validated_allocation_count(), 2);
            assert_eq!(combined.validated[0].allocations, vec![site(0), site(2)]);
        }

        #[test]
        fn sequential_candidates_validate_deterministically_and_idempotently() {
            let mut module = end_function(
                vec![
                    allocate_object(1),
                    observe_object(1),
                    allocate_object(2),
                    observe_object(2),
                ],
                vec![object_local(1), object_local(2)],
            );
            escape_analysis::assign_allocation_regions(&mut module);
            let first = analyze_plan_and_validate_candidate_subregions(&mut module);
            let candidates = module.functions[0].temporary_subregion_candidates.clone();
            let second = analyze_plan_and_validate_candidate_subregions(&mut module);

            assert_eq!(first, second);
            assert_eq!(
                module.functions[0].temporary_subregion_candidates,
                candidates
            );
            assert_eq!(first.validation.validated.len(), 2);
            assert_eq!(first.validation.validated_allocation_count(), 2);
            assert_eq!(first.validation.validated[0].checkpoint, point(0));
            assert_eq!(first.validation.validated[1].checkpoint, point(2));
            assert!(first.validation.rejected.is_empty());
        }

        #[test]
        fn unaccounted_younger_temporary_and_non_exact_death_are_rejected() {
            let module = end_function(
                vec![
                    temporary_object(1),
                    temporary_object(2),
                    observe_object(1),
                    observe_object(2),
                ],
                vec![object_local(1), object_local(2)],
            );
            let report = validate_raw(module, vec![candidate(0, 0, 3, vec![0])]);
            assert_eq!(reason(&report), Reason::UnaccountedTemporaryAllocation);

            let mut module = end_function(
                vec![temporary_object(1), observe_object(1), unrelated()],
                vec![object_local(1)],
            );
            escape_analysis::assign_allocation_regions(&mut module);
            let mut lifetime = crate::lifetime_analysis::analyze(&module);
            lifetime.proofs[0]
                .dead_after
                .retain(|location| *location == point(2));
            let report = validate_with_report(&module, &lifetime, &[candidate(0, 0, 3, vec![0])]);
            assert_eq!(reason(&report), Reason::MissingReferenceDeathProof);
        }

        #[test]
        fn rejected_younger_and_live_alias_artifacts_never_reach_5d_lowering() {
            let assert_not_lowered = |mut module: mir::Module,
                                      candidate: mir::TemporarySubregionCandidate,
                                      expected: Reason| {
                escape_analysis::assign_allocation_regions(&mut module);
                let lifetime = crate::lifetime_analysis::analyze(&module);
                let validation =
                    validate_with_report(&module, &lifetime, std::slice::from_ref(&candidate));
                assert_eq!(reason(&validation), expected);

                module.functions[0].temporary_subregion_candidates = vec![candidate];
                let (lowered, report) = lower_validated_exact_snapshot(&module, &validation)
                    .expect("an empty validated set lowers without executable markers");
                assert_eq!(report, AarmTemporarySubregionLoweringReport::default());
                assert!(
                    lowered.functions[0]
                        .temporary_subregion_candidates
                        .is_empty(),
                    "rejected candidate metadata is cleared"
                );
                assert!(lowered.functions[0].blocks[0].instructions.iter().all(
                    |instruction| !matches!(
                        instruction,
                        mir::Instruction::TemporarySubregionEnter { .. }
                            | mir::Instruction::TemporarySubregionExit { .. }
                    )
                ));
            };

            assert_not_lowered(
                end_function(
                    vec![
                        temporary_object(1),
                        temporary_object(2),
                        observe_object(1),
                        observe_object(2),
                    ],
                    vec![object_local(1), object_local(2)],
                ),
                candidate(0, 0, 3, vec![0]),
                Reason::UnaccountedTemporaryAllocation,
            );

            assert_not_lowered(
                end_function(
                    vec![
                        temporary_object(1),
                        mir::Instruction::Assign {
                            target: mir::Place::Local(mir::LocalId(2)),
                            value: mir::Rvalue {
                                type_: mir::Type::Class(CLASS),
                                kind: mir::RvalueKind::Use(copy(1, mir::Type::Class(CLASS))),
                            },
                        },
                        observe_object(2),
                    ],
                    vec![object_local(1), object_local(2)],
                ),
                candidate(0, 0, 2, vec![0]),
                Reason::MissingReferenceDeathProof,
            );
        }

        #[test]
        fn allocation_on_the_older_final_use_instruction_cannot_validate_older_only() {
            let allocate_older = mir::Instruction::CallIntrinsic {
                destination: Some(mir::Place::Local(mir::LocalId(1))),
                intrinsic: mir::Intrinsic::StringFromLongTemporary,
                arguments: vec![mir::Operand {
                    type_: mir::Type::Long,
                    kind: mir::OperandKind::Constant(mir::Constant::Integer("1".to_owned())),
                }],
                return_type: mir::Type::String,
            };
            let allocate_younger = mir::Instruction::CallIntrinsic {
                destination: Some(mir::Place::Local(mir::LocalId(2))),
                intrinsic: mir::Intrinsic::StringConcatTemporary,
                arguments: vec![
                    copy(1, mir::Type::String),
                    mir::Operand {
                        type_: mir::Type::String,
                        kind: mir::OperandKind::Constant(mir::Constant::String("x".to_owned())),
                    },
                ],
                return_type: mir::Type::String,
            };
            let report = validate_raw(
                end_function(
                    vec![allocate_older, allocate_younger],
                    vec![local(1, mir::Type::String), local(2, mir::Type::String)],
                ),
                vec![candidate(0, 0, 2, vec![0])],
            );
            assert!(report.validated.is_empty());
            assert_eq!(
                report.rejected[0].reason,
                Reason::UnaccountedTemporaryAllocation
            );
        }

        #[test]
        fn crossing_nested_and_duplicate_intervals_reject_every_participant() {
            let module = end_function(
                vec![
                    temporary_object(1),
                    observe_object(1),
                    unrelated(),
                    unrelated(),
                    temporary_object(2),
                    observe_object(2),
                ],
                vec![object_local(1), object_local(2)],
            );
            let crossing = vec![candidate(0, 0, 4, vec![0]), candidate(1, 2, 6, vec![4])];
            let report = validate_raw(module.clone(), crossing.clone());
            assert!(report.validated.is_empty());
            assert_eq!(report.rejected.len(), 2);
            assert!(
                report
                    .rejected
                    .iter()
                    .all(|rejected| rejected.reason == Reason::OverlappingSubregion)
            );
            let mut reversed = crossing;
            reversed.reverse();
            assert_eq!(report, validate_raw(module.clone(), reversed));

            let nested = vec![candidate(0, 0, 6, vec![0, 4]), candidate(1, 2, 6, vec![4])];
            let report = validate_raw(module.clone(), nested);
            assert!(report.validated.is_empty());
            assert_eq!(report.rejected.len(), 2);
            assert!(
                report
                    .rejected
                    .iter()
                    .all(|rejected| rejected.reason == Reason::OverlappingSubregion)
            );

            let duplicate = vec![candidate(0, 0, 4, vec![0]), candidate(1, 0, 4, vec![0])];
            let report = validate_raw(module, duplicate);
            assert!(report.validated.is_empty());
            assert_eq!(report.rejected.len(), 2);
            assert!(
                report
                    .rejected
                    .iter()
                    .all(|rejected| rejected.reason == Reason::OverlappingSubregion)
            );
        }

        #[test]
        fn malformed_points_sites_regions_and_ids_are_rejected_without_repair() {
            let base_module = end_function(
                vec![temporary_object(1), observe_object(1)],
                vec![object_local(1)],
            );

            assert_eq!(
                reason(&validate_raw(
                    base_module.clone(),
                    vec![candidate(0, 0, 3, vec![0])]
                )),
                Reason::MalformedPoint
            );

            let mut unknown_block = candidate(0, 0, 2, vec![0]);
            unknown_block.checkpoint.block = mir::BasicBlockId(999);
            assert_eq!(
                reason(&validate_raw(base_module.clone(), vec![unknown_block])),
                Reason::MalformedPoint
            );

            assert_eq!(
                reason(&validate_raw(
                    base_module.clone(),
                    vec![candidate(0, 0, 2, vec![1])]
                )),
                Reason::MalformedAllocationSite
            );

            let mut wrong_function = candidate(0, 0, 2, vec![0]);
            wrong_function.allocations[0].function = OTHER_FUNCTION;
            assert_eq!(
                reason(&validate_raw(base_module.clone(), vec![wrong_function])),
                Reason::WrongFunction
            );

            let mut duplicate_site = candidate(0, 0, 2, vec![0]);
            duplicate_site.allocations.push(site(0));
            assert_eq!(
                reason(&validate_raw(base_module.clone(), vec![duplicate_site])),
                Reason::DuplicateAllocationSite
            );

            let duplicate_ids = vec![candidate(0, 0, 2, vec![0]), candidate(0, 0, 2, vec![0])];
            let report = validate_raw(base_module, duplicate_ids);
            assert_eq!(report.rejected.len(), 2);
            assert!(
                report
                    .rejected
                    .iter()
                    .all(|rejected| rejected.reason == Reason::DuplicateId)
            );

            let returned = function(
                FUNCTION,
                BLOCK.0,
                vec![block(
                    BLOCK.0,
                    vec![allocate_object(1)],
                    mir::Terminator::Return(Some(copy(1, mir::Type::Class(CLASS)))),
                )],
                vec![object_local(1)],
            );
            assert_eq!(
                reason(&validate_raw(
                    module(vec![returned]),
                    vec![candidate(0, 0, 1, vec![0])]
                )),
                Reason::PersistentAllocation
            );
        }

        #[test]
        fn mismatched_allocation_snapshot_is_rejected_as_stale_and_candidates_are_not_mutated() {
            let mut module = end_function(
                vec![temporary_object(1), observe_object(1)],
                vec![object_local(1)],
            );
            escape_analysis::assign_allocation_regions(&mut module);
            let lifetime = crate::lifetime_analysis::analyze(&module);
            let candidates = vec![candidate(0, 0, 2, vec![0])];
            let expected_candidates = candidates.clone();

            if let mir::Instruction::AllocateObject { region, .. } =
                &mut module.functions[0].blocks[0].instructions[0]
            {
                *region = mir::AllocationRegion::Persistent;
            }
            let report = validate_with_report(&module, &lifetime, &candidates);
            assert_eq!(reason(&report), Reason::StaleAnalysis);
            assert_eq!(candidates, expected_candidates);
        }

        #[test]
        fn cycles_and_malformed_multiple_rewinds_are_unsupported() {
            let branch = function(
                FUNCTION,
                BLOCK.0,
                vec![block(
                    BLOCK.0,
                    vec![temporary_object(1)],
                    mir::Terminator::Branch {
                        condition: mir::Operand {
                            type_: mir::Type::Bool,
                            kind: mir::OperandKind::Constant(mir::Constant::Boolean(true)),
                        },
                        then_block: BLOCK,
                        else_block: BLOCK,
                    },
                )],
                vec![object_local(1)],
            );
            assert_eq!(
                reason(&validate_raw(
                    module(vec![branch]),
                    vec![candidate(0, 0, 1, vec![0])]
                )),
                Reason::UnsupportedControlFlow
            );

            let loop_function = function(
                FUNCTION,
                BLOCK.0,
                vec![block(
                    BLOCK.0,
                    vec![temporary_object(1)],
                    mir::Terminator::Goto(BLOCK),
                )],
                vec![object_local(1)],
            );
            assert_eq!(
                reason(&validate_raw(
                    module(vec![loop_function]),
                    vec![candidate(0, 0, 1, vec![0])]
                )),
                Reason::UnsupportedControlFlow
            );

            let early_return = function(
                FUNCTION,
                BLOCK.0,
                vec![
                    block(
                        BLOCK.0,
                        vec![temporary_object(1)],
                        mir::Terminator::Goto(mir::BasicBlockId(20)),
                    ),
                    block(20, Vec::new(), mir::Terminator::Return(None)),
                ],
                vec![object_local(1)],
            );
            // AARM-5E1 now accepts the acyclic fallthrough-to-Return shape
            // when the rewind is the proven predecessor boundary.
            let report = validate_raw(
                module(vec![early_return]),
                vec![candidate(0, 0, 1, vec![0])],
            );
            assert_eq!(report.validated.len(), 1);
            assert!(report.rejected.is_empty());

            let mut multiple_rewinds = candidate(0, 0, 2, vec![0]);
            multiple_rewinds.rewinds.push(point(2));
            assert_eq!(
                reason(&validate_raw(
                    end_function(
                        vec![temporary_object(1), observe_object(1)],
                        vec![object_local(1)]
                    ),
                    vec![multiple_rewinds]
                )),
                Reason::UnsupportedControlFlow
            );
        }

        #[test]
        fn direct_interface_and_intrinsic_calls_inside_span_are_barriers() {
            for call in [direct_call(), interface_call()] {
                let report = validate_raw(
                    end_function(
                        vec![temporary_object(1), call, observe_object(1)],
                        vec![object_local(1), object_local(2)],
                    ),
                    vec![candidate(0, 0, 3, vec![0])],
                );
                assert_eq!(reason(&report), Reason::CallBarrier);
            }

            let intrinsic = mir::Instruction::CallIntrinsic {
                destination: None,
                intrinsic: mir::Intrinsic::Log,
                arguments: Vec::new(),
                return_type: mir::Type::Void,
            };
            let report = validate_raw(
                end_function(
                    vec![temporary_object(1), intrinsic, observe_object(1)],
                    vec![object_local(1)],
                ),
                vec![candidate(0, 0, 3, vec![0])],
            );
            assert_eq!(reason(&report), Reason::CallBarrier);

            let outside = validate_raw(
                end_function(
                    vec![direct_call(), temporary_object(1), observe_object(1)],
                    vec![object_local(1)],
                ),
                vec![candidate(0, 1, 3, vec![1])],
            );
            assert_eq!(outside.validated.len(), 1);
            assert!(outside.rejected.is_empty());
        }

        #[test]
        fn fine_owned_collections_builders_and_immutable_strings_validate() {
            let list_type = mir::Type::List(Box::new(mir::Type::Int));
            let list = mir::Instruction::AllocateList {
                destination: mir::Place::Local(mir::LocalId(1)),
                element_type: mir::Type::Int,
                region: mir::AllocationRegion::Temporary,
            };
            let list_report = validate_raw(
                end_function(vec![list], vec![local(1, list_type)]),
                vec![candidate(0, 0, 1, vec![0])],
            );
            assert_eq!(list_report.validated.len(), 1, "{list_report:#?}");

            let dictionary_type =
                mir::Type::Dictionary(Box::new(mir::Type::Int), Box::new(mir::Type::Int));
            let dictionary = mir::Instruction::AllocateDictionary {
                destination: mir::Place::Local(mir::LocalId(1)),
                key_type: mir::Type::Int,
                value_type: mir::Type::Int,
                region: mir::AllocationRegion::Temporary,
            };
            let dictionary_report = validate_raw(
                end_function(vec![dictionary], vec![local(1, dictionary_type)]),
                vec![candidate(0, 0, 1, vec![0])],
            );
            assert_eq!(
                dictionary_report.validated.len(),
                1,
                "{dictionary_report:#?}"
            );

            let builder = mir::Instruction::AllocateStringBuilder {
                destination: mir::Place::Local(mir::LocalId(1)),
                class: CLASS,
                region: mir::AllocationRegion::Temporary,
            };
            let builder_report = validate_raw(
                end_function(vec![builder], vec![object_local(1)]),
                vec![candidate(0, 0, 1, vec![0])],
            );
            assert_eq!(builder_report.validated.len(), 1, "{builder_report:#?}");
            assert!(builder_report.rejected.is_empty());

            let string = mir::Instruction::CallIntrinsic {
                destination: Some(mir::Place::Local(mir::LocalId(1))),
                intrinsic: mir::Intrinsic::StringFromLongTemporary,
                arguments: vec![mir::Operand {
                    type_: mir::Type::Long,
                    kind: mir::OperandKind::Constant(mir::Constant::Integer("7".to_owned())),
                }],
                return_type: mir::Type::String,
            };
            let string_report = validate_raw(
                end_function(vec![string], vec![local(1, mir::Type::String)]),
                vec![candidate(0, 0, 1, vec![0])],
            );
            assert_eq!(string_report.validated.len(), 1, "{string_report:#?}");
            assert!(string_report.rejected.is_empty());

            let join = mir::Instruction::CallIntrinsic {
                destination: Some(mir::Place::Local(mir::LocalId(1))),
                intrinsic: mir::Intrinsic::StringJoinTemporary,
                arguments: vec![copy(2, mir::Type::Array(Box::new(mir::Type::String)))],
                return_type: mir::Type::String,
            };
            let join_report = validate_raw(
                end_function(
                    vec![join],
                    vec![
                        local(1, mir::Type::String),
                        local(2, mir::Type::Array(Box::new(mir::Type::String))),
                    ],
                ),
                vec![candidate(0, 0, 1, vec![0])],
            );
            assert_eq!(join_report.validated.len(), 1, "{join_report:#?}");

            let to_string = mir::Instruction::StringBuilderToString {
                destination: mir::Place::Local(mir::LocalId(1)),
                builder: copy(2, mir::Type::Class(CLASS)),
                class: CLASS,
                region: mir::AllocationRegion::Temporary,
            };
            assert_eq!(
                reason(&validate_raw(
                    end_function(
                        vec![to_string],
                        vec![local(1, mir::Type::String), object_local(2)]
                    ),
                    vec![candidate(0, 0, 1, vec![0])]
                )),
                Reason::BuilderOwnershipBarrier
            );
        }

        #[test]
        fn every_supported_immutable_temporary_string_producer_validates() {
            let scalar = |type_, constant| mir::Operand {
                type_,
                kind: mir::OperandKind::Constant(constant),
            };
            let producers = vec![
                (
                    mir::Intrinsic::StringFromLongTemporary,
                    scalar(mir::Type::Long, mir::Constant::Integer("7".to_owned())),
                ),
                (
                    mir::Intrinsic::StringFromULongTemporary,
                    scalar(mir::Type::ULong, mir::Constant::Integer("7".to_owned())),
                ),
                (
                    mir::Intrinsic::StringFromDoubleTemporary,
                    scalar(mir::Type::Double, mir::Constant::Float("7.5".to_owned())),
                ),
                (
                    mir::Intrinsic::StringFromFloatTemporary,
                    scalar(mir::Type::Float, mir::Constant::Float("7.5".to_owned())),
                ),
                (
                    mir::Intrinsic::StringFromBoolTemporary,
                    scalar(mir::Type::Bool, mir::Constant::Boolean(true)),
                ),
                (
                    mir::Intrinsic::StringFromCharTemporary,
                    scalar(mir::Type::Char, mir::Constant::Character('x')),
                ),
            ];
            for (intrinsic, argument) in producers {
                let report = validate_raw(
                    end_function(
                        vec![mir::Instruction::CallIntrinsic {
                            destination: Some(mir::Place::Local(mir::LocalId(1))),
                            intrinsic,
                            arguments: vec![argument],
                            return_type: mir::Type::String,
                        }],
                        vec![local(1, mir::Type::String)],
                    ),
                    vec![candidate(0, 0, 1, vec![0])],
                );
                assert_eq!(report.validated.len(), 1, "{intrinsic:?}: {report:#?}");
                assert!(report.rejected.is_empty());
            }
        }

        #[test]
        fn hidden_collection_and_builder_backing_growth_inside_span_is_rejected() {
            let list_type = mir::Type::List(Box::new(mir::Type::Int));
            let list_add = mir::Instruction::ListAdd {
                list: copy(1, list_type.clone()),
                value: mir::Operand {
                    type_: mir::Type::Int,
                    kind: mir::OperandKind::Constant(mir::Constant::Integer("1".to_owned())),
                },
            };
            let report = validate_raw(
                end_function(
                    vec![
                        mir::Instruction::AllocateList {
                            destination: mir::Place::Local(mir::LocalId(1)),
                            element_type: mir::Type::Int,
                            region: mir::AllocationRegion::Temporary,
                        },
                        temporary_object(2),
                        list_add,
                        observe_object(2),
                    ],
                    vec![local(1, list_type), object_local(2)],
                ),
                vec![candidate(0, 1, 4, vec![1])],
            );
            assert_eq!(reason(&report), Reason::BuilderOwnershipBarrier);

            let dictionary_type =
                mir::Type::Dictionary(Box::new(mir::Type::Int), Box::new(mir::Type::Int));
            for mutation in [
                mir::Instruction::DictionaryAdd {
                    destination: mir::Place::Local(SINK),
                    dictionary: copy(1, dictionary_type.clone()),
                    key: mir::Operand {
                        type_: mir::Type::Int,
                        kind: mir::OperandKind::Constant(mir::Constant::Integer("1".to_owned())),
                    },
                    value: mir::Operand {
                        type_: mir::Type::Int,
                        kind: mir::OperandKind::Constant(mir::Constant::Integer("2".to_owned())),
                    },
                },
                mir::Instruction::DictionarySet {
                    destination: mir::Place::Local(SINK),
                    dictionary: copy(1, dictionary_type.clone()),
                    key: mir::Operand {
                        type_: mir::Type::Int,
                        kind: mir::OperandKind::Constant(mir::Constant::Integer("1".to_owned())),
                    },
                    value: mir::Operand {
                        type_: mir::Type::Int,
                        kind: mir::OperandKind::Constant(mir::Constant::Integer("3".to_owned())),
                    },
                },
            ] {
                let report = validate_raw(
                    end_function(
                        vec![
                            mir::Instruction::AllocateDictionary {
                                destination: mir::Place::Local(mir::LocalId(1)),
                                key_type: mir::Type::Int,
                                value_type: mir::Type::Int,
                                region: mir::AllocationRegion::Temporary,
                            },
                            temporary_object(2),
                            mutation,
                            observe_object(2),
                        ],
                        vec![local(1, dictionary_type.clone()), object_local(2)],
                    ),
                    vec![candidate(0, 1, 4, vec![1])],
                );
                assert_eq!(reason(&report), Reason::BuilderOwnershipBarrier);
            }

            let append = mir::Instruction::StringBuilderAppend {
                builder: copy(1, mir::Type::Class(CLASS)),
                value: mir::Operand {
                    type_: mir::Type::String,
                    kind: mir::OperandKind::Constant(mir::Constant::String("x".to_owned())),
                },
                class: CLASS,
            };
            let report = validate_raw(
                end_function(
                    vec![
                        mir::Instruction::AllocateStringBuilder {
                            destination: mir::Place::Local(mir::LocalId(1)),
                            class: CLASS,
                            region: mir::AllocationRegion::Temporary,
                        },
                        temporary_object(2),
                        append,
                        observe_object(2),
                    ],
                    vec![object_local(1), object_local(2)],
                ),
                vec![candidate(0, 1, 4, vec![1])],
            );
            assert_eq!(reason(&report), Reason::BuilderOwnershipBarrier);
        }

        #[test]
        fn precheckpoint_collection_reads_and_string_rvalues_inside_span_are_rejected() {
            let list_type = mir::Type::List(Box::new(mir::Type::Int));
            let dictionary_type =
                mir::Type::Dictionary(Box::new(mir::Type::Int), Box::new(mir::Type::Int));
            for value in [
                mir::Rvalue {
                    type_: mir::Type::Int,
                    kind: mir::RvalueKind::ListLength(copy(1, list_type.clone())),
                },
                mir::Rvalue {
                    type_: mir::Type::Int,
                    kind: mir::RvalueKind::ListVersion(copy(1, list_type.clone())),
                },
                mir::Rvalue {
                    type_: mir::Type::Int,
                    kind: mir::RvalueKind::DictionaryLength(copy(1, dictionary_type.clone())),
                },
            ] {
                let report = validate_raw(
                    end_function(
                        vec![
                            temporary_object(2),
                            mir::Instruction::Assign {
                                target: mir::Place::Local(SINK),
                                value,
                            },
                            observe_object(2),
                        ],
                        vec![local(1, list_type.clone()), object_local(2)],
                    ),
                    vec![candidate(0, 0, 3, vec![0])],
                );
                assert_eq!(reason(&report), Reason::CollectionBarrier);
            }

            for value in [
                mir::Rvalue {
                    type_: mir::Type::Int,
                    kind: mir::RvalueKind::StringByteLength(copy(1, mir::Type::String)),
                },
                mir::Rvalue {
                    type_: mir::Type::Bool,
                    kind: mir::RvalueKind::Equality {
                        left: copy(1, mir::Type::String),
                        right: copy(1, mir::Type::String),
                        negated: false,
                    },
                },
            ] {
                let report = validate_raw(
                    end_function(
                        vec![
                            temporary_object(2),
                            mir::Instruction::Assign {
                                target: mir::Place::Local(SINK),
                                value,
                            },
                            observe_object(2),
                        ],
                        vec![local(1, mir::Type::String), object_local(2)],
                    ),
                    vec![candidate(0, 0, 3, vec![0])],
                );
                assert_eq!(reason(&report), Reason::StringBarrier);
            }
        }

        #[test]
        fn object_fields_array_indexes_and_array_length_are_execution_safe() {
            let object = copy(1, mir::Type::Class(CLASS));
            let object_field = mir::Place::ObjectField {
                object: Box::new(object.clone()),
                field: mir::SymbolId(777),
            };
            let object_report = validate_raw(
                end_function(
                    vec![
                        temporary_object(1),
                        mir::Instruction::Assign {
                            target: object_field.clone(),
                            value: mir::Rvalue {
                                type_: mir::Type::Int,
                                kind: mir::RvalueKind::Use(mir::Operand {
                                    type_: mir::Type::Int,
                                    kind: mir::OperandKind::Constant(mir::Constant::Integer(
                                        "7".into(),
                                    )),
                                }),
                            },
                        },
                        mir::Instruction::Assign {
                            target: mir::Place::Local(mir::LocalId(3)),
                            value: mir::Rvalue {
                                type_: mir::Type::Int,
                                kind: mir::RvalueKind::Use(mir::Operand {
                                    type_: mir::Type::Int,
                                    kind: mir::OperandKind::Copy(object_field),
                                }),
                            },
                        },
                        observe_object(1),
                    ],
                    vec![object_local(1), local(3, mir::Type::Int)],
                ),
                vec![candidate(0, 0, 4, vec![0])],
            );
            assert_eq!(object_report.validated.len(), 1);
            assert!(object_report.rejected.is_empty());

            let array_type = mir::Type::Array(Box::new(mir::Type::Int));
            let array = copy(1, array_type.clone());
            let index = mir::Operand {
                type_: mir::Type::Int,
                kind: mir::OperandKind::Constant(mir::Constant::Integer("0".into())),
            };
            let array_index = mir::Place::Index {
                array: Box::new(array.clone()),
                index: Box::new(index),
                element_type: mir::Type::Int,
            };
            let array_report = validate_raw(
                end_function(
                    vec![
                        temporary_array(1),
                        mir::Instruction::Assign {
                            target: array_index.clone(),
                            value: mir::Rvalue {
                                type_: mir::Type::Int,
                                kind: mir::RvalueKind::Use(mir::Operand {
                                    type_: mir::Type::Int,
                                    kind: mir::OperandKind::Constant(mir::Constant::Integer(
                                        "11".into(),
                                    )),
                                }),
                            },
                        },
                        mir::Instruction::Assign {
                            target: mir::Place::Local(mir::LocalId(3)),
                            value: mir::Rvalue {
                                type_: mir::Type::Int,
                                kind: mir::RvalueKind::Use(mir::Operand {
                                    type_: mir::Type::Int,
                                    kind: mir::OperandKind::Copy(array_index),
                                }),
                            },
                        },
                        mir::Instruction::Assign {
                            target: mir::Place::Local(mir::LocalId(4)),
                            value: mir::Rvalue {
                                type_: mir::Type::Int,
                                kind: mir::RvalueKind::ArrayLength(array),
                            },
                        },
                    ],
                    vec![
                        local(1, array_type),
                        local(3, mir::Type::Int),
                        local(4, mir::Type::Int),
                    ],
                ),
                vec![candidate(0, 0, 4, vec![0])],
            );
            assert_eq!(array_report.validated.len(), 1);
            assert!(array_report.rejected.is_empty());
        }

        #[test]
        fn execution_unsafe_assign_forms_are_rejected_before_5d() {
            let divide = mir::Instruction::Assign {
                target: mir::Place::Local(SINK),
                value: mir::Rvalue {
                    type_: mir::Type::Int,
                    kind: mir::RvalueKind::Binary {
                        left: mir::Operand {
                            type_: mir::Type::Int,
                            kind: mir::OperandKind::Constant(mir::Constant::Integer("8".into())),
                        },
                        operator: mir::BinaryOperator::Divide,
                        right: mir::Operand {
                            type_: mir::Type::Int,
                            kind: mir::OperandKind::Constant(mir::Constant::Integer("2".into())),
                        },
                    },
                },
            };
            let nonlocal_target = mir::Instruction::Assign {
                target: mir::Place::Field {
                    base: Box::new(mir::Place::Local(SINK)),
                    field: mir::SymbolId(777),
                },
                value: mir::Rvalue {
                    type_: mir::Type::Bool,
                    kind: mir::RvalueKind::Use(mir::Operand {
                        type_: mir::Type::Bool,
                        kind: mir::OperandKind::Constant(mir::Constant::Boolean(true)),
                    }),
                },
            };

            for instruction in [divide, nonlocal_target] {
                let report = validate_raw(
                    end_function(
                        vec![temporary_object(2), instruction, observe_object(2)],
                        vec![object_local(2)],
                    ),
                    vec![candidate(0, 0, 3, vec![0])],
                );
                assert_eq!(reason(&report), Reason::UnsupportedInstruction);
            }
        }

        #[test]
        fn every_task_async_and_parallel_intrinsic_is_a_function_barrier() {
            let intrinsics = [
                mir::Intrinsic::TaskRun,
                mir::Intrinsic::TaskWait,
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
            for intrinsic in intrinsics {
                let boundary = mir::Instruction::CallIntrinsic {
                    destination: None,
                    intrinsic,
                    arguments: Vec::new(),
                    return_type: mir::Type::Void,
                };
                let report = validate_raw(
                    end_function(
                        vec![temporary_object(1), observe_object(1), boundary],
                        vec![object_local(1)],
                    ),
                    vec![candidate(0, 0, 2, vec![0])],
                );
                assert_eq!(reason(&report), Reason::ConcurrencyBarrier, "{intrinsic:?}");
            }
        }

        #[test]
        fn exact_orchestration_refreshes_liveness_and_same_local_is_never_resurrected() {
            let mut module = end_function(
                vec![allocate_object(1), observe_object(1), unrelated()],
                vec![object_local(1)],
            );
            escape_analysis::assign_allocation_regions(&mut module);
            let first = analyze_plan_and_validate_candidate_subregions(&mut module);
            assert_eq!(first.validation.validated[0].rewind, point(2));

            module.functions[0].blocks[0].instructions.swap(1, 2);
            let second = analyze_plan_and_validate_candidate_subregions(&mut module);
            assert_eq!(second.validation.validated[0].rewind, point(3));

            let (_, ambiguous) = prepare_research(end_function(
                vec![
                    allocate_object(1),
                    observe_object(1),
                    allocate_object(1),
                    observe_object(1),
                ],
                vec![object_local(1)],
            ));
            assert!(ambiguous.validation.validated.is_empty());
            assert!(ambiguous.validation.rejected.is_empty());
        }
    }

    #[test]
    fn research_lowering_inserts_sequential_boundaries_atomically_and_clears_candidates() {
        let mut module = module(vec![function(
            FUNCTION,
            BLOCK.0,
            vec![block(
                BLOCK.0,
                vec![
                    allocate_object(1),
                    observe_object(1),
                    allocate_object(2),
                    observe_object(2),
                ],
                mir::Terminator::End,
            )],
            vec![object_local(1), object_local(2)],
        )]);

        let report = lower_aarm_temporary_subregions_for_research(&mut module)
            .expect("validated sequential subregions lower");

        assert_eq!(
            report,
            AarmTemporarySubregionLoweringReport {
                validated_subregions_received: 2,
                subregions_lowered: 2,
                enter_instructions_inserted: 2,
                exit_instructions_inserted: 2,
            }
        );
        let instructions = &module.functions[0].blocks[0].instructions;
        assert!(matches!(
            instructions[0],
            mir::Instruction::TemporarySubregionEnter {
                id: mir::TemporarySubregionId(0)
            }
        ));
        assert!(matches!(
            instructions[1],
            mir::Instruction::AllocateObject { .. }
        ));
        assert!(matches!(instructions[2], mir::Instruction::Assign { .. }));
        assert!(matches!(
            instructions[3],
            mir::Instruction::TemporarySubregionExit {
                id: mir::TemporarySubregionId(0)
            }
        ));
        assert!(matches!(
            instructions[4],
            mir::Instruction::TemporarySubregionEnter {
                id: mir::TemporarySubregionId(1)
            }
        ));
        assert!(matches!(
            instructions[5],
            mir::Instruction::AllocateObject { .. }
        ));
        assert!(matches!(instructions[6], mir::Instruction::Assign { .. }));
        assert!(matches!(
            instructions[7],
            mir::Instruction::TemporarySubregionExit {
                id: mir::TemporarySubregionId(1)
            }
        ));
        assert!(
            module.functions[0]
                .temporary_subregion_candidates
                .is_empty()
        );
    }

    #[test]
    fn rejected_candidate_never_becomes_executable() {
        let intrinsic = mir::Instruction::CallIntrinsic {
            destination: None,
            intrinsic: mir::Intrinsic::Log,
            arguments: Vec::new(),
            return_type: mir::Type::Void,
        };
        let mut module = module(vec![function(
            FUNCTION,
            BLOCK.0,
            vec![block(
                BLOCK.0,
                vec![allocate_object(1), intrinsic, observe_object(1)],
                mir::Terminator::End,
            )],
            vec![object_local(1)],
        )]);

        let report = lower_aarm_temporary_subregions_for_research(&mut module)
            .expect("rejected research candidates are safely omitted");
        assert_eq!(report.subregions_lowered, 0);
        assert!(
            module.functions[0]
                .temporary_subregion_candidates
                .is_empty()
        );
        assert!(
            module.functions[0].blocks[0]
                .instructions
                .iter()
                .all(|instruction| {
                    !matches!(
                        instruction,
                        mir::Instruction::TemporarySubregionEnter { .. }
                            | mir::Instruction::TemporarySubregionExit { .. }
                    )
                })
        );
    }

    #[test]
    fn preexisting_executable_marker_is_rejected_atomically() {
        let mut module = module(vec![function(
            FUNCTION,
            BLOCK.0,
            vec![block(
                BLOCK.0,
                vec![mir::Instruction::TemporarySubregionEnter {
                    id: mir::TemporarySubregionId(7),
                }],
                mir::Terminator::End,
            )],
            Vec::new(),
        )]);
        let before = module.clone();

        let error = lower_aarm_temporary_subregions_for_research(&mut module)
            .expect_err("preexisting executable authority must be rejected");

        assert_eq!(
            error.message(),
            "AARM executable subregion lowering requires untransformed MIR"
        );
        assert_eq!(module, before);
    }

    #[test]
    fn stale_validated_artifact_leaves_the_complete_module_unchanged() {
        let mut module = module(vec![function(
            FUNCTION,
            BLOCK.0,
            vec![block(
                BLOCK.0,
                vec![allocate_object(1), observe_object(1)],
                mir::Terminator::End,
            )],
            vec![object_local(1)],
        )]);
        escape_analysis::assign_allocation_regions(&mut module);
        let mut analysis = analyze_plan_and_validate_candidate_subregions(&mut module);
        analysis.validation.validated[0].rewind.instruction_boundary = usize::MAX;
        let before = module.clone();

        let result = lower_validated_exact_snapshot(&module, &analysis.validation);

        assert!(result.is_err());
        assert_eq!(module, before);
    }

    #[test]
    fn ordinary_compile_emits_no_executable_subregion_instructions() {
        let source = "public class Box { public int value; } public int Run() { Box box = new Box(); box.value = 7; return box.value; }";
        let compilation = crate::compile(source).expect("ordinary compilation succeeds");
        assert!(compilation.mir.functions.iter().all(|function| {
            function.blocks.iter().all(|block| {
                block.instructions.iter().all(|instruction| {
                    !matches!(
                        instruction,
                        mir::Instruction::TemporarySubregionEnter { .. }
                            | mir::Instruction::TemporarySubregionExit { .. }
                    )
                })
            })
        }));
    }

    #[test]
    fn acyclic_diamond_plans_one_mark_with_mutually_exclusive_exits() {
        let mut module = module(vec![function(
            FUNCTION,
            BLOCK.0,
            vec![
                block(
                    BLOCK.0,
                    Vec::new(),
                    mir::Terminator::Branch {
                        condition: mir::Operand {
                            type_: mir::Type::Bool,
                            kind: mir::OperandKind::Constant(mir::Constant::Boolean(true)),
                        },
                        then_block: mir::BasicBlockId(20),
                        else_block: mir::BasicBlockId(30),
                    },
                ),
                block(
                    20,
                    vec![allocate_object(1), observe_object(1)],
                    mir::Terminator::Goto(mir::BasicBlockId(40)),
                ),
                block(
                    30,
                    vec![allocate_object(2), observe_object(2)],
                    mir::Terminator::Goto(mir::BasicBlockId(40)),
                ),
                block(40, vec![unrelated()], mir::Terminator::End),
            ],
            vec![object_local(1), object_local(2)],
        )]);

        let report = lower_aarm_temporary_subregions_for_research(&mut module)
            .expect("acyclic diamond lowering succeeds");
        assert_eq!(report.subregions_lowered, 1);
        assert_eq!(report.enter_instructions_inserted, 1);
        assert_eq!(report.exit_instructions_inserted, 2);
        let blocks = &module.functions[0].blocks;
        assert!(matches!(
            blocks[0].instructions.first(),
            Some(mir::Instruction::TemporarySubregionEnter {
                id: mir::TemporarySubregionId(0)
            })
        ));
        for block in [&blocks[1], &blocks[2]] {
            assert!(matches!(
                block.instructions.last(),
                Some(mir::Instruction::TemporarySubregionExit {
                    id: mir::TemporarySubregionId(0)
                })
            ));
        }
    }

    #[test]
    fn simple_natural_loop_plans_one_iteration_local_candidate() {
        let loop_function = function(
            FUNCTION,
            BLOCK.0,
            vec![
                block(
                    BLOCK.0,
                    Vec::new(),
                    mir::Terminator::Branch {
                        condition: mir::Operand {
                            type_: mir::Type::Bool,
                            kind: mir::OperandKind::Constant(mir::Constant::Boolean(true)),
                        },
                        then_block: mir::BasicBlockId(20),
                        else_block: mir::BasicBlockId(40),
                    },
                ),
                block(
                    20,
                    vec![temporary_object(1), observe_object(1)],
                    mir::Terminator::Goto(mir::BasicBlockId(30)),
                ),
                block(30, vec![unrelated()], mir::Terminator::Goto(BLOCK)),
                block(40, Vec::new(), mir::Terminator::End),
            ],
            vec![object_local(1)],
        );
        let (module, analysis) = prepare_research(module(vec![loop_function]));
        assert_eq!(analysis.validation.rejected, Vec::new());
        assert_eq!(
            module.functions[0].temporary_subregion_candidates,
            vec![mir::TemporarySubregionCandidate {
                id: mir::TemporarySubregionId(0),
                checkpoint: mir::MirPoint {
                    block: mir::BasicBlockId(20),
                    instruction_boundary: 0,
                },
                rewinds: vec![mir::MirPoint {
                    block: mir::BasicBlockId(30),
                    instruction_boundary: 1,
                }],
                allocations: vec![mir::MirAllocationSite {
                    function: FUNCTION,
                    block: mir::BasicBlockId(20),
                    instruction_index: 0,
                }],
            }]
        );
    }

    #[test]
    fn loop_break_and_continue_paths_receive_distinct_iteration_exits() {
        let loop_function = function(
            FUNCTION,
            BLOCK.0,
            vec![
                block(
                    BLOCK.0,
                    Vec::new(),
                    mir::Terminator::Goto(mir::BasicBlockId(20)),
                ),
                block(
                    20,
                    Vec::new(),
                    mir::Terminator::Branch {
                        condition: mir::Operand {
                            type_: mir::Type::Bool,
                            kind: mir::OperandKind::Constant(mir::Constant::Boolean(true)),
                        },
                        then_block: mir::BasicBlockId(30),
                        else_block: mir::BasicBlockId(60),
                    },
                ),
                block(
                    30,
                    vec![temporary_object(1), observe_object(1)],
                    mir::Terminator::Branch {
                        condition: mir::Operand {
                            type_: mir::Type::Bool,
                            kind: mir::OperandKind::Constant(mir::Constant::Boolean(true)),
                        },
                        then_block: mir::BasicBlockId(40),
                        else_block: mir::BasicBlockId(50),
                    },
                ),
                block(
                    40,
                    vec![unrelated()],
                    mir::Terminator::Goto(mir::BasicBlockId(20)),
                ),
                block(50, vec![unrelated()], mir::Terminator::Return(None)),
                block(60, Vec::new(), mir::Terminator::End),
            ],
            vec![object_local(1)],
        );
        let (module, _) = prepare(module(vec![loop_function]));
        let candidates = &module.functions[0].temporary_subregion_candidates;
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].checkpoint.block, mir::BasicBlockId(30));
        assert_eq!(
            candidates[0].rewinds,
            vec![
                mir::MirPoint {
                    block: mir::BasicBlockId(40),
                    instruction_boundary: 1,
                },
                mir::MirPoint {
                    block: mir::BasicBlockId(50),
                    instruction_boundary: 1,
                },
            ]
        );
    }

    #[test]
    fn multiple_latches_receive_one_iteration_candidate_with_two_exits() {
        let loop_function = function(
            FUNCTION,
            BLOCK.0,
            vec![
                block(
                    BLOCK.0,
                    Vec::new(),
                    mir::Terminator::Goto(mir::BasicBlockId(20)),
                ),
                block(
                    20,
                    Vec::new(),
                    mir::Terminator::Branch {
                        condition: mir::Operand {
                            type_: mir::Type::Bool,
                            kind: mir::OperandKind::Constant(mir::Constant::Boolean(true)),
                        },
                        then_block: mir::BasicBlockId(30),
                        else_block: mir::BasicBlockId(60),
                    },
                ),
                block(
                    30,
                    vec![temporary_object(1), observe_object(1)],
                    mir::Terminator::Branch {
                        condition: mir::Operand {
                            type_: mir::Type::Bool,
                            kind: mir::OperandKind::Constant(mir::Constant::Boolean(true)),
                        },
                        then_block: mir::BasicBlockId(40),
                        else_block: mir::BasicBlockId(50),
                    },
                ),
                block(
                    40,
                    vec![unrelated()],
                    mir::Terminator::Goto(mir::BasicBlockId(20)),
                ),
                block(
                    50,
                    vec![unrelated()],
                    mir::Terminator::Goto(mir::BasicBlockId(20)),
                ),
                block(60, Vec::new(), mir::Terminator::End),
            ],
            vec![object_local(1)],
        );
        let (module, _) = prepare(module(vec![loop_function]));
        assert_eq!(
            module.functions[0].temporary_subregion_candidates[0]
                .rewinds
                .len(),
            2
        );
    }

    #[test]
    fn break_surviving_alias_withholds_iteration_reclaim() {
        let loop_function = function(
            FUNCTION,
            BLOCK.0,
            vec![
                block(
                    BLOCK.0,
                    Vec::new(),
                    mir::Terminator::Goto(mir::BasicBlockId(20)),
                ),
                block(
                    20,
                    Vec::new(),
                    mir::Terminator::Branch {
                        condition: mir::Operand {
                            type_: mir::Type::Bool,
                            kind: mir::OperandKind::Constant(mir::Constant::Boolean(true)),
                        },
                        then_block: mir::BasicBlockId(30),
                        else_block: mir::BasicBlockId(50),
                    },
                ),
                block(
                    30,
                    vec![
                        temporary_object(1),
                        mir::Instruction::Assign {
                            target: mir::Place::Local(mir::LocalId(2)),
                            value: mir::Rvalue {
                                type_: mir::Type::Class(CLASS),
                                kind: mir::RvalueKind::Use(copy(1, mir::Type::Class(CLASS))),
                            },
                        },
                    ],
                    mir::Terminator::Branch {
                        condition: mir::Operand {
                            type_: mir::Type::Bool,
                            kind: mir::OperandKind::Constant(mir::Constant::Boolean(true)),
                        },
                        then_block: mir::BasicBlockId(40),
                        else_block: mir::BasicBlockId(45),
                    },
                ),
                block(
                    40,
                    vec![unrelated()],
                    mir::Terminator::Goto(mir::BasicBlockId(50)),
                ),
                block(
                    45,
                    vec![unrelated()],
                    mir::Terminator::Goto(mir::BasicBlockId(20)),
                ),
                block(50, vec![observe_object(2)], mir::Terminator::End),
            ],
            vec![object_local(1), object_local(2)],
        );
        let (module, _) = prepare(module(vec![loop_function]));
        assert!(
            module.functions[0]
                .temporary_subregion_candidates
                .is_empty()
        );
    }

    #[test]
    fn nested_function_selects_only_the_innermost_leaf_loop() {
        let function = function(
            FUNCTION,
            BLOCK.0,
            vec![
                block(
                    BLOCK.0,
                    Vec::new(),
                    mir::Terminator::Goto(mir::BasicBlockId(20)),
                ),
                block(
                    20,
                    Vec::new(),
                    mir::Terminator::Branch {
                        condition: mir::Operand {
                            type_: mir::Type::Bool,
                            kind: mir::OperandKind::Constant(mir::Constant::Boolean(true)),
                        },
                        then_block: mir::BasicBlockId(30),
                        else_block: mir::BasicBlockId(90),
                    },
                ),
                block(
                    30,
                    vec![temporary_object(1)],
                    mir::Terminator::Goto(mir::BasicBlockId(40)),
                ),
                block(
                    40,
                    Vec::new(),
                    mir::Terminator::Branch {
                        condition: mir::Operand {
                            type_: mir::Type::Bool,
                            kind: mir::OperandKind::Constant(mir::Constant::Boolean(true)),
                        },
                        then_block: mir::BasicBlockId(50),
                        else_block: mir::BasicBlockId(70),
                    },
                ),
                block(
                    50,
                    vec![temporary_object(2), observe_object(2)],
                    mir::Terminator::Goto(mir::BasicBlockId(60)),
                ),
                block(
                    60,
                    vec![unrelated()],
                    mir::Terminator::Goto(mir::BasicBlockId(40)),
                ),
                block(
                    70,
                    vec![observe_object(1)],
                    mir::Terminator::Goto(mir::BasicBlockId(80)),
                ),
                block(
                    80,
                    vec![unrelated()],
                    mir::Terminator::Goto(mir::BasicBlockId(20)),
                ),
                block(90, Vec::new(), mir::Terminator::End),
            ],
            vec![object_local(1), object_local(2)],
        );
        let (module, _) = prepare(module(vec![function]));
        let candidates = &module.functions[0].temporary_subregion_candidates;
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].checkpoint.block, mir::BasicBlockId(50));
        assert_eq!(
            candidates[0].allocations,
            vec![mir::MirAllocationSite {
                function: FUNCTION,
                block: mir::BasicBlockId(50),
                instruction_index: 0,
            }]
        );
    }

    #[test]
    fn sibling_inner_loops_receive_disjoint_leaf_candidates() {
        let function = function(
            FUNCTION,
            BLOCK.0,
            vec![
                block(
                    BLOCK.0,
                    Vec::new(),
                    mir::Terminator::Goto(mir::BasicBlockId(20)),
                ),
                block(
                    20,
                    Vec::new(),
                    mir::Terminator::Branch {
                        condition: mir::Operand {
                            type_: mir::Type::Bool,
                            kind: mir::OperandKind::Constant(mir::Constant::Boolean(true)),
                        },
                        then_block: mir::BasicBlockId(30),
                        else_block: mir::BasicBlockId(100),
                    },
                ),
                block(30, Vec::new(), mir::Terminator::Goto(mir::BasicBlockId(40))),
                block(
                    40,
                    Vec::new(),
                    mir::Terminator::Branch {
                        condition: mir::Operand {
                            type_: mir::Type::Bool,
                            kind: mir::OperandKind::Constant(mir::Constant::Boolean(true)),
                        },
                        then_block: mir::BasicBlockId(50),
                        else_block: mir::BasicBlockId(60),
                    },
                ),
                block(
                    50,
                    vec![temporary_object(1), observe_object(1)],
                    mir::Terminator::Goto(mir::BasicBlockId(40)),
                ),
                block(60, Vec::new(), mir::Terminator::Goto(mir::BasicBlockId(70))),
                block(
                    70,
                    Vec::new(),
                    mir::Terminator::Branch {
                        condition: mir::Operand {
                            type_: mir::Type::Bool,
                            kind: mir::OperandKind::Constant(mir::Constant::Boolean(true)),
                        },
                        then_block: mir::BasicBlockId(80),
                        else_block: mir::BasicBlockId(90),
                    },
                ),
                block(
                    80,
                    vec![temporary_object(2), observe_object(2)],
                    mir::Terminator::Goto(mir::BasicBlockId(70)),
                ),
                block(
                    90,
                    vec![unrelated()],
                    mir::Terminator::Goto(mir::BasicBlockId(95)),
                ),
                block(
                    95,
                    vec![unrelated()],
                    mir::Terminator::Goto(mir::BasicBlockId(20)),
                ),
                block(100, Vec::new(), mir::Terminator::End),
            ],
            vec![object_local(1), object_local(2)],
        );
        let (module, _) = prepare(module(vec![function]));
        let candidates = &module.functions[0].temporary_subregion_candidates;
        assert_eq!(candidates.len(), 2);
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.checkpoint.block)
                .collect::<Vec<_>>(),
            vec![mir::BasicBlockId(50), mir::BasicBlockId(80)]
        );
    }

    #[test]
    fn three_level_nesting_selects_only_the_deepest_leaf() {
        let function = function(
            FUNCTION,
            BLOCK.0,
            vec![
                block(
                    BLOCK.0,
                    Vec::new(),
                    mir::Terminator::Goto(mir::BasicBlockId(20)),
                ),
                block(
                    20,
                    Vec::new(),
                    mir::Terminator::Branch {
                        condition: mir::Operand {
                            type_: mir::Type::Bool,
                            kind: mir::OperandKind::Constant(mir::Constant::Boolean(true)),
                        },
                        then_block: mir::BasicBlockId(30),
                        else_block: mir::BasicBlockId(110),
                    },
                ),
                block(30, Vec::new(), mir::Terminator::Goto(mir::BasicBlockId(40))),
                block(
                    40,
                    Vec::new(),
                    mir::Terminator::Branch {
                        condition: mir::Operand {
                            type_: mir::Type::Bool,
                            kind: mir::OperandKind::Constant(mir::Constant::Boolean(true)),
                        },
                        then_block: mir::BasicBlockId(50),
                        else_block: mir::BasicBlockId(90),
                    },
                ),
                block(50, Vec::new(), mir::Terminator::Goto(mir::BasicBlockId(60))),
                block(
                    60,
                    Vec::new(),
                    mir::Terminator::Branch {
                        condition: mir::Operand {
                            type_: mir::Type::Bool,
                            kind: mir::OperandKind::Constant(mir::Constant::Boolean(true)),
                        },
                        then_block: mir::BasicBlockId(70),
                        else_block: mir::BasicBlockId(80),
                    },
                ),
                block(
                    70,
                    vec![temporary_object(1), observe_object(1)],
                    mir::Terminator::Goto(mir::BasicBlockId(60)),
                ),
                block(
                    80,
                    vec![unrelated()],
                    mir::Terminator::Goto(mir::BasicBlockId(90)),
                ),
                block(
                    90,
                    vec![unrelated()],
                    mir::Terminator::Goto(mir::BasicBlockId(100)),
                ),
                block(
                    100,
                    vec![unrelated()],
                    mir::Terminator::Goto(mir::BasicBlockId(20)),
                ),
                block(110, Vec::new(), mir::Terminator::End),
            ],
            vec![object_local(1)],
        );
        let (module, _) = prepare(module(vec![function]));
        let candidates = &module.functions[0].temporary_subregion_candidates;
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].checkpoint.block, mir::BasicBlockId(70));
    }

    #[test]
    fn irreducible_nested_cycle_is_withheld() {
        let function = function(
            FUNCTION,
            BLOCK.0,
            vec![
                block(
                    BLOCK.0,
                    Vec::new(),
                    mir::Terminator::Branch {
                        condition: mir::Operand {
                            type_: mir::Type::Bool,
                            kind: mir::OperandKind::Constant(mir::Constant::Boolean(true)),
                        },
                        then_block: mir::BasicBlockId(20),
                        else_block: mir::BasicBlockId(30),
                    },
                ),
                block(20, Vec::new(), mir::Terminator::Goto(mir::BasicBlockId(40))),
                block(30, Vec::new(), mir::Terminator::Goto(mir::BasicBlockId(50))),
                block(
                    40,
                    vec![temporary_object(1)],
                    mir::Terminator::Goto(mir::BasicBlockId(50)),
                ),
                block(
                    50,
                    vec![observe_object(1)],
                    mir::Terminator::Goto(mir::BasicBlockId(40)),
                ),
            ],
            vec![object_local(1)],
        );
        let (module, _) = prepare(module(vec![function]));
        assert!(
            module.functions[0]
                .temporary_subregion_candidates
                .is_empty()
        );
    }

    #[test]
    fn loop_carried_alias_withholds_iteration_reclaim() {
        let loop_function = function(
            FUNCTION,
            BLOCK.0,
            vec![
                block(
                    BLOCK.0,
                    vec![observe_object(2)],
                    mir::Terminator::Branch {
                        condition: mir::Operand {
                            type_: mir::Type::Bool,
                            kind: mir::OperandKind::Constant(mir::Constant::Boolean(true)),
                        },
                        then_block: mir::BasicBlockId(20),
                        else_block: mir::BasicBlockId(40),
                    },
                ),
                block(
                    20,
                    vec![
                        temporary_object(1),
                        mir::Instruction::Assign {
                            target: mir::Place::Local(mir::LocalId(2)),
                            value: mir::Rvalue {
                                type_: mir::Type::Class(CLASS),
                                kind: mir::RvalueKind::Use(copy(1, mir::Type::Class(CLASS))),
                            },
                        },
                    ],
                    mir::Terminator::Goto(mir::BasicBlockId(30)),
                ),
                block(30, vec![unrelated()], mir::Terminator::Goto(BLOCK)),
                block(40, Vec::new(), mir::Terminator::End),
            ],
            vec![object_local(1), object_local(2)],
        );
        let (module, analysis) = prepare_research(module(vec![loop_function]));
        assert!(analysis.validation.validated.is_empty());
        assert!(
            module.functions[0]
                .temporary_subregion_candidates
                .is_empty()
        );
    }
}
