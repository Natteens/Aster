//! Conservative AARM-5C proof barriers for Temporary subregion candidates.
//!
//! This module validates research metadata only. It does not mutate MIR,
//! create executable checkpoint operations, or reach the runtime/backend.

use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashMap, HashSet},
};

use aster_mir as mir;

use crate::{escape_analysis, lifetime_analysis::LifetimeAnalysisReport};

use super::FunctionCandidatePlan;

pub(super) struct ExactSnapshotValidation {
    pub lifetime: LifetimeAnalysisReport,
    pub plans: Vec<FunctionCandidatePlan>,
    pub validation: TemporarySubregionValidationReport,
}

/// Compiler-owned proof artifact for the deliberately narrow AARM-5C subset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ValidatedTemporarySubregion {
    pub function: mir::SymbolId,
    pub id: mir::TemporarySubregionId,
    pub checkpoint: mir::MirPoint,
    /// First rewind retained for AARM-5D test compatibility. AARM-5E1 uses
    /// the complete `rewinds` set below.
    pub rewind: mir::MirPoint,
    pub rewinds: Vec<mir::MirPoint>,
    pub allocations: Vec<mir::MirAllocationSite>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum TemporarySubregionRejectionReason {
    StaleAnalysis,
    UnsupportedControlFlow,
    DuplicateId,
    MalformedPoint,
    WrongFunction,
    MalformedAllocationSite,
    PersistentAllocation,
    DuplicateAllocationSite,
    MissingReferenceDeathProof,
    UnaccountedTemporaryAllocation,
    OverlappingSubregion,
    CallBarrier,
    ConcurrencyBarrier,
    CollectionBarrier,
    StringBarrier,
    BuilderOwnershipBarrier,
    UnsupportedInstruction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FineOwnedKind {
    StringBuilder,
    List,
    Dictionary,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RejectedTemporarySubregion {
    pub function: mir::SymbolId,
    pub candidate: mir::TemporarySubregionCandidate,
    pub reason: TemporarySubregionRejectionReason,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct TemporarySubregionValidationReport {
    pub validated: Vec<ValidatedTemporarySubregion>,
    pub rejected: Vec<RejectedTemporarySubregion>,
}

impl TemporarySubregionValidationReport {
    pub fn validated_allocation_count(&self) -> usize {
        self.validated
            .iter()
            .map(|subregion| subregion.allocations.len())
            .sum()
    }
}

struct ProvisionalSubregion {
    validated: ValidatedTemporarySubregion,
    candidate: mir::TemporarySubregionCandidate,
}

/// Analyze, plan, and validate one immutable executable MIR snapshot.
///
/// The raw validator is private so production research code cannot pair an
/// independently retained report with later MIR.
pub(super) fn analyze_plan_validate_exact_snapshot(
    module: &mir::Module,
) -> ExactSnapshotValidation {
    let lifetime = crate::lifetime_analysis::analyze(module);
    let plans = if super::report_matches_module(module, &lifetime) {
        module
            .functions
            .iter()
            .map(|function| FunctionCandidatePlan {
                function: function.symbol,
                candidates: super::plan_function(function, &lifetime),
            })
            .collect::<Vec<_>>()
    } else {
        module
            .functions
            .iter()
            .map(|function| FunctionCandidatePlan {
                function: function.symbol,
                candidates: Vec::new(),
            })
            .collect()
    };
    let validation = validate_exact_snapshot(module, &lifetime, &plans);

    ExactSnapshotValidation {
        lifetime,
        plans,
        validation,
    }
}

fn validate_exact_snapshot(
    module: &mir::Module,
    lifetime: &LifetimeAnalysisReport,
    plans: &[FunctionCandidatePlan],
) -> TemporarySubregionValidationReport {
    if !inputs_match(module, lifetime, plans) {
        return reject_all(plans, TemporarySubregionRejectionReason::StaleAnalysis);
    }

    let proofs = lifetime
        .proofs
        .iter()
        .map(|proof| (proof.site, proof))
        .collect::<HashMap<_, _>>();
    if proofs.len() != lifetime.proofs.len() {
        return reject_all(plans, TemporarySubregionRejectionReason::StaleAnalysis);
    }

    let mut report = TemporarySubregionValidationReport::default();
    let mut provisional = Vec::new();

    for (function, plan) in module.functions.iter().zip(plans) {
        let duplicate_ids = duplicate_candidate_ids(&plan.candidates);
        let supported_function = supported_function(function);
        let has_concurrency = super::function_contains_concurrency_boundary(function);

        for candidate in &plan.candidates {
            let rejected = |reason| RejectedTemporarySubregion {
                function: function.symbol,
                candidate: candidate.clone(),
                reason,
            };

            if supported_function.is_none()
                || candidate.rewinds.is_empty()
                || matches!(&supported_function, Some(SupportedFunction::Straight(_)))
                    && candidate.rewinds.len() != 1
            {
                report.rejected.push(rejected(
                    TemporarySubregionRejectionReason::UnsupportedControlFlow,
                ));
                continue;
            }
            if duplicate_ids.contains(&candidate.id) {
                report
                    .rejected
                    .push(rejected(TemporarySubregionRejectionReason::DuplicateId));
                continue;
            }

            match validate_candidate(
                function,
                supported_function.as_ref().expect("checked above"),
                candidate,
                &proofs,
                has_concurrency,
            ) {
                Ok(validated) => provisional.push(ProvisionalSubregion {
                    validated,
                    candidate: candidate.clone(),
                }),
                Err(reason) => report.rejected.push(rejected(reason)),
            }
        }
    }

    let mut overlaps = vec![false; provisional.len()];
    for left in 0..provisional.len() {
        for right in (left + 1)..provisional.len() {
            if intervals_overlap(&provisional[left].validated, &provisional[right].validated) {
                overlaps[left] = true;
                overlaps[right] = true;
            }
        }
    }
    for (subregion, overlaps) in provisional.into_iter().zip(overlaps) {
        if overlaps {
            report.rejected.push(RejectedTemporarySubregion {
                function: subregion.validated.function,
                candidate: subregion.candidate,
                reason: TemporarySubregionRejectionReason::OverlappingSubregion,
            });
        } else {
            report.validated.push(subregion.validated);
        }
    }

    report.validated.sort_by(compare_validated);
    report.rejected.sort_by(compare_rejected);
    report
}

#[cfg(test)]
pub(super) fn validate_for_test(
    module: &mir::Module,
    lifetime: &LifetimeAnalysisReport,
    plans: &[FunctionCandidatePlan],
) -> TemporarySubregionValidationReport {
    validate_exact_snapshot(module, lifetime, plans)
}

fn inputs_match(
    module: &mir::Module,
    lifetime: &LifetimeAnalysisReport,
    plans: &[FunctionCandidatePlan],
) -> bool {
    if !super::report_matches_module(module, lifetime) || plans.len() != module.functions.len() {
        return false;
    }

    let mut symbols = HashSet::new();
    module
        .functions
        .iter()
        .zip(plans)
        .all(|(function, plan)| symbols.insert(function.symbol) && plan.function == function.symbol)
}

fn reject_all(
    plans: &[FunctionCandidatePlan],
    reason: TemporarySubregionRejectionReason,
) -> TemporarySubregionValidationReport {
    let mut rejected = plans
        .iter()
        .flat_map(|plan| {
            plan.candidates
                .iter()
                .cloned()
                .map(|candidate| RejectedTemporarySubregion {
                    function: plan.function,
                    candidate,
                    reason,
                })
        })
        .collect::<Vec<_>>();
    rejected.sort_by(compare_rejected);
    TemporarySubregionValidationReport {
        validated: Vec::new(),
        rejected,
    }
}

fn duplicate_candidate_ids(
    candidates: &[mir::TemporarySubregionCandidate],
) -> HashSet<mir::TemporarySubregionId> {
    let mut seen = HashSet::new();
    let mut duplicates = HashSet::new();
    for candidate in candidates {
        if !seen.insert(candidate.id) {
            duplicates.insert(candidate.id);
        }
    }
    duplicates
}

enum SupportedFunction<'a> {
    Straight(&'a mir::BasicBlock),
    Acyclic(Box<super::AcyclicCfg>),
    NaturalLoops(Vec<super::SimpleNaturalLoop>),
}

fn supported_function(function: &mir::Function) -> Option<SupportedFunction<'_>> {
    if let [block] = function.blocks.as_slice() {
        return (function.entry == block.id
            && matches!(
                block.terminator,
                mir::Terminator::Return(_) | mir::Terminator::End
            ))
        .then_some(SupportedFunction::Straight(block));
    }
    super::acyclic_cfg(function)
        .map(|cfg| SupportedFunction::Acyclic(Box::new(cfg)))
        .or_else(|| {
            let loops = super::natural_loops(function);
            (!loops.is_empty()).then_some(SupportedFunction::NaturalLoops(loops))
        })
}

#[allow(clippy::too_many_lines)]
fn validate_candidate(
    function: &mir::Function,
    supported: &SupportedFunction<'_>,
    candidate: &mir::TemporarySubregionCandidate,
    proofs: &HashMap<mir::MirAllocationSite, &crate::lifetime_analysis::AllocationLifetimeProof>,
    has_concurrency: bool,
) -> Result<ValidatedTemporarySubregion, TemporarySubregionRejectionReason> {
    if let SupportedFunction::Acyclic(cfg) = supported {
        return validate_cfg_candidate(function, cfg, candidate, proofs, has_concurrency);
    }
    if let SupportedFunction::NaturalLoops(loops) = supported {
        let Some((loop_index, loop_cfg)) = loops.iter().enumerate().find(|(_, loop_cfg)| {
            candidate.checkpoint
                == (mir::MirPoint {
                    block: loop_cfg.body_entry,
                    instruction_boundary: 0,
                })
        }) else {
            return Err(TemporarySubregionRejectionReason::UnsupportedControlFlow);
        };
        if loops.iter().enumerate().any(|(child_index, child)| {
            child_index != loop_index
                && child.body.len() < loop_cfg.body.len()
                && child.body.is_subset(&loop_cfg.body)
        }) {
            return Err(TemporarySubregionRejectionReason::UnsupportedControlFlow);
        }
        return validate_loop_candidate(function, loop_cfg, candidate, proofs, has_concurrency);
    }
    let SupportedFunction::Straight(block) = supported else {
        unreachable!()
    };
    let rewind = candidate.rewinds[0];
    if candidate.checkpoint.block != block.id
        || rewind.block != block.id
        || candidate.checkpoint.instruction_boundary > block.instructions.len()
        || rewind.instruction_boundary > block.instructions.len()
        || candidate.checkpoint.instruction_boundary >= rewind.instruction_boundary
    {
        return Err(TemporarySubregionRejectionReason::MalformedPoint);
    }
    if candidate.allocations.is_empty() {
        return Err(TemporarySubregionRejectionReason::MalformedAllocationSite);
    }
    let straight_successors = HashMap::from([(block.id, Vec::new())]);
    validate_builder_provenance(
        block.id,
        &[(
            block.id,
            &block.instructions
                [candidate.checkpoint.instruction_boundary..rewind.instruction_boundary],
        )],
        &straight_successors,
        &[rewind],
    )?;

    let mut previous_site = None;
    for site in &candidate.allocations {
        if site.function != function.symbol {
            return Err(TemporarySubregionRejectionReason::WrongFunction);
        }
        if site.block != block.id
            || site.instruction_index < candidate.checkpoint.instruction_boundary
            || site.instruction_index >= rewind.instruction_boundary
        {
            return Err(TemporarySubregionRejectionReason::MalformedAllocationSite);
        }
        if previous_site == Some(*site) {
            return Err(TemporarySubregionRejectionReason::DuplicateAllocationSite);
        }
        if previous_site.is_some_and(|previous| compare_site(&previous, site).is_gt()) {
            return Err(TemporarySubregionRejectionReason::MalformedAllocationSite);
        }
        previous_site = Some(*site);

        let Some(instruction) = block.instructions.get(site.instruction_index) else {
            return Err(TemporarySubregionRejectionReason::MalformedAllocationSite);
        };
        let Some(region) = escape_analysis::dynamic_allocation_region(instruction) else {
            return Err(TemporarySubregionRejectionReason::MalformedAllocationSite);
        };
        if region != mir::AllocationRegion::Temporary {
            return Err(TemporarySubregionRejectionReason::PersistentAllocation);
        }
        match allocation_form_barrier(instruction) {
            Some(reason) => return Err(reason),
            None if !supported_allocation_form(instruction) => {
                return Err(TemporarySubregionRejectionReason::MalformedAllocationSite);
            }
            None => {}
        }
    }

    if has_concurrency {
        return Err(TemporarySubregionRejectionReason::ConcurrencyBarrier);
    }

    if let Some(reason) = span_barrier(
        &block.instructions[candidate.checkpoint.instruction_boundary..rewind.instruction_boundary],
    ) {
        return Err(reason);
    }

    let temporary_sites = block.instructions
        [candidate.checkpoint.instruction_boundary..rewind.instruction_boundary]
        .iter()
        .enumerate()
        .filter_map(|(offset, instruction)| {
            (escape_analysis::dynamic_allocation_region(instruction)
                == Some(mir::AllocationRegion::Temporary))
            .then_some(mir::MirAllocationSite {
                function: function.symbol,
                block: block.id,
                instruction_index: candidate.checkpoint.instruction_boundary + offset,
            })
        })
        .collect::<Vec<_>>();
    if temporary_sites != candidate.allocations {
        return Err(TemporarySubregionRejectionReason::UnaccountedTemporaryAllocation);
    }

    for site in &candidate.allocations {
        let Some(proof) = proofs.get(site) else {
            return Err(TemporarySubregionRejectionReason::MissingReferenceDeathProof);
        };
        if proof.region != mir::AllocationRegion::Temporary || !proof.dead_after.contains(&rewind) {
            return Err(TemporarySubregionRejectionReason::MissingReferenceDeathProof);
        }
    }

    Ok(ValidatedTemporarySubregion {
        function: function.symbol,
        id: candidate.id,
        checkpoint: candidate.checkpoint,
        rewind,
        rewinds: vec![rewind],
        allocations: candidate.allocations.clone(),
    })
}

#[allow(clippy::too_many_lines)]
fn validate_loop_candidate(
    function: &mir::Function,
    loop_cfg: &super::SimpleNaturalLoop,
    candidate: &mir::TemporarySubregionCandidate,
    proofs: &HashMap<mir::MirAllocationSite, &crate::lifetime_analysis::AllocationLifetimeProof>,
    has_concurrency: bool,
) -> Result<ValidatedTemporarySubregion, TemporarySubregionRejectionReason> {
    let Some(rewinds) = super::simple_loop_rewinds(function, loop_cfg) else {
        return Err(TemporarySubregionRejectionReason::UnsupportedControlFlow);
    };
    let rewind = rewinds[0];
    if candidate.checkpoint
        != (mir::MirPoint {
            block: loop_cfg.body_entry,
            instruction_boundary: 0,
        })
        || candidate.rewinds != rewinds
    {
        return Err(TemporarySubregionRejectionReason::MalformedPoint);
    }
    if has_concurrency
        || function_contains_temporary_allocation(
            loop_cfg
                .block(function, loop_cfg.header)
                .instructions
                .as_slice(),
        )
    {
        return Err(if has_concurrency {
            TemporarySubregionRejectionReason::ConcurrencyBarrier
        } else {
            TemporarySubregionRejectionReason::UnaccountedTemporaryAllocation
        });
    }
    let mut body_blocks = loop_cfg
        .body
        .iter()
        .copied()
        .filter(|block| *block != loop_cfg.header)
        .collect::<Vec<_>>();
    body_blocks.sort_unstable_by_key(|block| block.0);
    let provenance_blocks = body_blocks
        .iter()
        .map(|block| {
            let instructions = &loop_cfg.block(function, *block).instructions;
            let start = if *block == loop_cfg.body_entry {
                candidate.checkpoint.instruction_boundary
            } else {
                0
            };
            let end = rewinds
                .iter()
                .filter(|rewind| rewind.block == *block)
                .map(|rewind| rewind.instruction_boundary)
                .min()
                .unwrap_or(instructions.len());
            (*block, &instructions[start..end])
        })
        .collect::<Vec<_>>();
    let provenance_successors = loop_cfg
        .successors
        .iter()
        .map(|(block, successors)| {
            (
                *block,
                successors
                    .iter()
                    .copied()
                    .filter(|successor| {
                        provenance_blocks
                            .iter()
                            .any(|(block, _)| block == successor)
                    })
                    .collect(),
            )
        })
        .collect::<HashMap<_, _>>();
    validate_builder_provenance(
        loop_cfg.body_entry,
        &provenance_blocks,
        &provenance_successors,
        &rewinds,
    )?;
    let mut expected_sites = Vec::new();
    for block in &body_blocks {
        let instructions = &loop_cfg.block(function, *block).instructions;
        if let Some(reason) = span_barrier(instructions) {
            return Err(reason);
        }
        for (instruction_index, instruction) in instructions.iter().enumerate() {
            if escape_analysis::dynamic_allocation_region(instruction)
                == Some(mir::AllocationRegion::Temporary)
            {
                expected_sites.push(mir::MirAllocationSite {
                    function: function.symbol,
                    block: *block,
                    instruction_index,
                });
            }
        }
    }
    expected_sites.sort_unstable_by_key(|site| (site.block.0, site.instruction_index));
    if expected_sites.is_empty() || candidate.allocations != expected_sites {
        return Err(TemporarySubregionRejectionReason::UnaccountedTemporaryAllocation);
    }
    for site in &candidate.allocations {
        if site.function != function.symbol || !loop_cfg.dominates(loop_cfg.body_entry, site.block)
        {
            return Err(TemporarySubregionRejectionReason::WrongFunction);
        }
        let Some(instruction) = loop_cfg
            .block(function, site.block)
            .instructions
            .get(site.instruction_index)
        else {
            return Err(TemporarySubregionRejectionReason::MalformedAllocationSite);
        };
        if escape_analysis::dynamic_allocation_region(instruction)
            != Some(mir::AllocationRegion::Temporary)
        {
            return Err(TemporarySubregionRejectionReason::PersistentAllocation);
        }
        if allocation_form_barrier(instruction).is_some() || !supported_allocation_form(instruction)
        {
            return Err(allocation_form_barrier(instruction)
                .unwrap_or(TemporarySubregionRejectionReason::MalformedAllocationSite));
        }
        let Some(proof) = proofs.get(site) else {
            return Err(TemporarySubregionRejectionReason::MissingReferenceDeathProof);
        };
        if proof.region != mir::AllocationRegion::Temporary
            || super::simple_loop_reachable_rewinds(loop_cfg, site.block, &rewinds)
                .iter()
                .any(|rewind| !proof.dead_after.contains(rewind))
        {
            return Err(TemporarySubregionRejectionReason::MissingReferenceDeathProof);
        }
    }
    Ok(ValidatedTemporarySubregion {
        function: function.symbol,
        id: candidate.id,
        checkpoint: candidate.checkpoint,
        rewind,
        rewinds,
        allocations: candidate.allocations.clone(),
    })
}

fn function_contains_temporary_allocation(instructions: &[mir::Instruction]) -> bool {
    instructions.iter().any(|instruction| {
        escape_analysis::dynamic_allocation_region(instruction)
            == Some(mir::AllocationRegion::Temporary)
    })
}

#[allow(clippy::too_many_lines)]
fn validate_cfg_candidate(
    function: &mir::Function,
    cfg: &super::AcyclicCfg,
    candidate: &mir::TemporarySubregionCandidate,
    proofs: &HashMap<mir::MirAllocationSite, &crate::lifetime_analysis::AllocationLifetimeProof>,
    has_concurrency: bool,
) -> Result<ValidatedTemporarySubregion, TemporarySubregionRejectionReason> {
    if candidate.checkpoint.instruction_boundary != 0
        || !cfg.reachable.contains(&candidate.checkpoint.block)
        || candidate.rewinds.iter().any(|rewind| {
            !cfg.reachable.contains(&rewind.block)
                || cfg.block(function, rewind.block).instructions.len()
                    != rewind.instruction_boundary
                || rewind.instruction_boundary == 0
        })
    {
        return Err(TemporarySubregionRejectionReason::MalformedPoint);
    }
    let Some(join) = cfg.immediate_postdominator(candidate.checkpoint.block) else {
        return Err(TemporarySubregionRejectionReason::UnsupportedControlFlow);
    };
    let stop = match join {
        super::ImmediatePostdominator::Block(block) => Some(block),
        super::ImmediatePostdominator::FunctionExit => None,
    };
    let region = cfg.reachable_until(candidate.checkpoint.block, stop);
    let mut expected_rewinds = match join {
        super::ImmediatePostdominator::Block(join) => region
            .iter()
            .filter(|block| cfg.successors[block].contains(&join))
            .map(|block| mir::MirPoint {
                block: *block,
                instruction_boundary: cfg.block(function, *block).instructions.len(),
            })
            .collect::<Vec<_>>(),
        super::ImmediatePostdominator::FunctionExit => region
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
    expected_rewinds.sort_unstable_by_key(|point| (point.block.0, point.instruction_boundary));
    if candidate.rewinds != expected_rewinds || candidate.allocations.is_empty() {
        return Err(TemporarySubregionRejectionReason::MalformedAllocationSite);
    }
    if has_concurrency {
        return Err(TemporarySubregionRejectionReason::ConcurrencyBarrier);
    }

    let provenance_blocks = cfg
        .topological
        .iter()
        .filter(|block| region.contains(block))
        .map(|block| {
            let instructions = &cfg.block(function, *block).instructions;
            let start = if *block == candidate.checkpoint.block {
                candidate.checkpoint.instruction_boundary
            } else {
                0
            };
            let end = candidate
                .rewinds
                .iter()
                .filter(|rewind| rewind.block == *block)
                .map(|rewind| rewind.instruction_boundary)
                .min()
                .unwrap_or(instructions.len());
            (*block, &instructions[start..end])
        })
        .collect::<Vec<_>>();
    let provenance_successors = cfg
        .successors
        .iter()
        .map(|(block, successors)| {
            (
                *block,
                successors
                    .iter()
                    .copied()
                    .filter(|successor| {
                        provenance_blocks
                            .iter()
                            .any(|(block, _)| block == successor)
                    })
                    .collect(),
            )
        })
        .collect::<HashMap<_, _>>();
    validate_builder_provenance(
        candidate.checkpoint.block,
        &provenance_blocks,
        &provenance_successors,
        &candidate.rewinds,
    )?;

    let mut expected_sites = Vec::new();
    for block in &cfg.topological {
        if !region.contains(block) {
            continue;
        }
        let block_ref = cfg.block(function, *block);
        if let Some(reason) = span_barrier(&block_ref.instructions) {
            return Err(reason);
        }
        for (instruction_index, instruction) in block_ref.instructions.iter().enumerate() {
            if escape_analysis::dynamic_allocation_region(instruction)
                == Some(mir::AllocationRegion::Temporary)
            {
                expected_sites.push(mir::MirAllocationSite {
                    function: function.symbol,
                    block: *block,
                    instruction_index,
                });
            }
        }
    }
    expected_sites.sort_unstable_by_key(|site| (site.block.0, site.instruction_index));
    if candidate.allocations != expected_sites {
        return Err(TemporarySubregionRejectionReason::UnaccountedTemporaryAllocation);
    }
    for site in &candidate.allocations {
        if site.function != function.symbol
            || !cfg.dominates(candidate.checkpoint.block, site.block)
        {
            return Err(TemporarySubregionRejectionReason::WrongFunction);
        }
        let Some(instruction) = cfg
            .block(function, site.block)
            .instructions
            .get(site.instruction_index)
        else {
            return Err(TemporarySubregionRejectionReason::MalformedAllocationSite);
        };
        if escape_analysis::dynamic_allocation_region(instruction)
            != Some(mir::AllocationRegion::Temporary)
        {
            return Err(TemporarySubregionRejectionReason::PersistentAllocation);
        }
        if allocation_form_barrier(instruction).is_some() || !supported_allocation_form(instruction)
        {
            return Err(allocation_form_barrier(instruction)
                .unwrap_or(TemporarySubregionRejectionReason::MalformedAllocationSite));
        }
        let Some(proof) = proofs.get(site) else {
            return Err(TemporarySubregionRejectionReason::MissingReferenceDeathProof);
        };
        let reachable_from_site = cfg.reachable_from(site.block);
        let exits = candidate
            .rewinds
            .iter()
            .filter(|rewind| reachable_from_site.contains(&rewind.block))
            .collect::<Vec<_>>();
        if exits.is_empty()
            || proof.region != mir::AllocationRegion::Temporary
            || exits
                .into_iter()
                .any(|rewind| !proof.dead_after.contains(rewind))
        {
            return Err(TemporarySubregionRejectionReason::MissingReferenceDeathProof);
        }
    }
    Ok(ValidatedTemporarySubregion {
        function: function.symbol,
        id: candidate.id,
        checkpoint: candidate.checkpoint,
        rewind: candidate.rewinds[0],
        rewinds: candidate.rewinds.clone(),
        allocations: candidate.allocations.clone(),
    })
}

#[allow(clippy::match_same_arms)]
fn supported_allocation_form(instruction: &mir::Instruction) -> bool {
    match instruction {
        mir::Instruction::AllocateObject { .. } | mir::Instruction::AllocateArray { .. } => true,
        mir::Instruction::AllocateStringBuilder {
            destination: mir::Place::Local(_),
            region: mir::AllocationRegion::Temporary,
            ..
        }
        | mir::Instruction::StringBuilderToString {
            destination: mir::Place::Local(_),
            builder:
                mir::Operand {
                    kind: mir::OperandKind::Copy(mir::Place::Local(_)),
                    ..
                },
            region: mir::AllocationRegion::Temporary,
            ..
        } => true,
        mir::Instruction::AllocateList {
            destination: mir::Place::Local(_),
            element_type,
            region: mir::AllocationRegion::Temporary,
        } => is_execution_safe_type(element_type),
        mir::Instruction::AllocateDictionary {
            destination: mir::Place::Local(_),
            key_type,
            value_type,
            region: mir::AllocationRegion::Temporary,
        } => is_execution_safe_type(key_type) && is_execution_safe_type(value_type),
        mir::Instruction::CallIntrinsic {
            destination,
            intrinsic,
            arguments,
            return_type,
        } => immutable_temporary_string_intrinsic_is_executable(
            destination.as_ref(),
            *intrinsic,
            arguments,
            return_type,
        ),
        _ => false,
    }
}

#[allow(clippy::match_same_arms)]
fn allocation_form_barrier(
    instruction: &mir::Instruction,
) -> Option<TemporarySubregionRejectionReason> {
    match instruction {
        mir::Instruction::DictionaryEntries { .. }
        | mir::Instruction::DictionaryKeys { .. }
        | mir::Instruction::DictionaryValues { .. }
        | mir::Instruction::ListToArray { .. } => {
            Some(TemporarySubregionRejectionReason::CollectionBarrier)
        }
        mir::Instruction::AllocateList {
            destination: mir::Place::Local(_),
            element_type,
            region: mir::AllocationRegion::Temporary,
        } if is_execution_safe_type(element_type) => None,
        mir::Instruction::AllocateDictionary {
            destination: mir::Place::Local(_),
            key_type,
            value_type,
            region: mir::AllocationRegion::Temporary,
        } if is_execution_safe_type(key_type) && is_execution_safe_type(value_type) => None,
        mir::Instruction::AllocateList { .. } | mir::Instruction::AllocateDictionary { .. } => {
            Some(TemporarySubregionRejectionReason::CollectionBarrier)
        }
        mir::Instruction::AllocateStringBuilder {
            destination: mir::Place::Local(_),
            region: mir::AllocationRegion::Temporary,
            ..
        }
        | mir::Instruction::StringBuilderToString {
            destination: mir::Place::Local(_),
            builder:
                mir::Operand {
                    kind: mir::OperandKind::Copy(mir::Place::Local(_)),
                    ..
                },
            region: mir::AllocationRegion::Temporary,
            ..
        } => None,
        mir::Instruction::AllocateStringBuilder { .. } => {
            Some(TemporarySubregionRejectionReason::BuilderOwnershipBarrier)
        }
        mir::Instruction::CallIntrinsic {
            destination,
            intrinsic,
            arguments,
            return_type,
        } if immutable_temporary_string_intrinsic_is_executable(
            destination.as_ref(),
            *intrinsic,
            arguments,
            return_type,
        ) =>
        {
            None
        }
        mir::Instruction::StringBuilderToString { .. } | mir::Instruction::CallIntrinsic { .. } => {
            Some(TemporarySubregionRejectionReason::StringBarrier)
        }
        _ => None,
    }
}

pub(super) fn span_barrier_for_research(
    instructions: &[&mir::Instruction],
) -> Option<TemporarySubregionRejectionReason> {
    for instruction in instructions {
        let single = std::slice::from_ref(*instruction);
        if let Some(reason) = span_barrier(single) {
            return Some(reason);
        }
    }
    None
}

#[allow(clippy::match_same_arms, clippy::too_many_lines)]
fn span_barrier(instructions: &[mir::Instruction]) -> Option<TemporarySubregionRejectionReason> {
    for instruction in instructions {
        let reason = match instruction {
            mir::Instruction::TemporarySubregionEnter { .. }
            | mir::Instruction::TemporarySubregionExit { .. } => {
                Some(TemporarySubregionRejectionReason::UnsupportedControlFlow)
            }
            mir::Instruction::Call { .. } | mir::Instruction::CallInterface { .. } => {
                Some(TemporarySubregionRejectionReason::CallBarrier)
            }
            mir::Instruction::ForeignCall { .. } => {
                Some(TemporarySubregionRejectionReason::CallBarrier)
            }
            mir::Instruction::CallIntrinsic {
                destination,
                intrinsic,
                arguments,
                return_type,
            } => {
                if super::is_concurrency_intrinsic(*intrinsic) {
                    Some(TemporarySubregionRejectionReason::ConcurrencyBarrier)
                } else if immutable_temporary_string_intrinsic_is_executable(
                    destination.as_ref(),
                    *intrinsic,
                    arguments,
                    return_type,
                ) {
                    None
                } else if intrinsic.allocation_region().is_some() {
                    Some(TemporarySubregionRejectionReason::StringBarrier)
                } else {
                    Some(TemporarySubregionRejectionReason::CallBarrier)
                }
            }
            mir::Instruction::DictionaryEntries { .. }
            | mir::Instruction::DictionaryKeys { .. }
            | mir::Instruction::DictionaryValues { .. }
            | mir::Instruction::ListToArray { .. } => {
                Some(TemporarySubregionRejectionReason::CollectionBarrier)
            }
            mir::Instruction::DictionaryClear { dictionary }
                if direct_local_operand(dictionary) =>
            {
                None
            }
            mir::Instruction::AllocateList {
                destination: mir::Place::Local(_),
                element_type,
                region: mir::AllocationRegion::Temporary,
            } if is_execution_safe_type(element_type) => None,
            mir::Instruction::AllocateDictionary {
                destination: mir::Place::Local(_),
                key_type,
                value_type,
                region: mir::AllocationRegion::Temporary,
            } if is_execution_safe_type(key_type) && is_execution_safe_type(value_type) => None,
            mir::Instruction::ListAdd { list, value }
                if direct_local_operand(list) && is_execution_safe_operand(value) =>
            {
                None
            }
            mir::Instruction::ListGet {
                destination: mir::Place::Local(_),
                list,
                index,
                element_type,
            } if direct_local_operand(list)
                && is_execution_safe_operand(index)
                && is_execution_safe_type(element_type) =>
            {
                None
            }
            mir::Instruction::ListRemoveAt { list, index }
                if direct_local_operand(list) && is_execution_safe_operand(index) =>
            {
                None
            }
            mir::Instruction::ListSet { list, index, value }
                if direct_local_operand(list)
                    && is_execution_safe_operand(index)
                    && is_execution_safe_operand(value) =>
            {
                None
            }
            mir::Instruction::ListClear { list } if direct_local_operand(list) => None,
            mir::Instruction::DictionaryAdd {
                destination: mir::Place::Local(_),
                dictionary,
                key,
                value,
            }
            | mir::Instruction::DictionarySet {
                destination: mir::Place::Local(_),
                dictionary,
                key,
                value,
            } if direct_local_operand(dictionary)
                && is_execution_safe_operand(key)
                && is_execution_safe_operand(value) =>
            {
                None
            }
            mir::Instruction::DictionaryTryGet {
                destination: mir::Place::Local(_),
                dictionary,
                key,
                value_type,
                ..
            } if direct_local_operand(dictionary)
                && is_execution_safe_operand(key)
                && is_execution_safe_type(value_type) =>
            {
                None
            }
            mir::Instruction::DictionaryContainsKey {
                destination: mir::Place::Local(_),
                dictionary,
                key,
            }
            | mir::Instruction::DictionaryRemove {
                destination: mir::Place::Local(_),
                dictionary,
                key,
            } if direct_local_operand(dictionary) && is_execution_safe_operand(key) => None,
            mir::Instruction::AllocateList { .. }
            | mir::Instruction::AllocateDictionary { .. }
            | mir::Instruction::DictionaryAdd { .. }
            | mir::Instruction::DictionarySet { .. }
            | mir::Instruction::DictionaryTryGet { .. }
            | mir::Instruction::DictionaryContainsKey { .. }
            | mir::Instruction::DictionaryRemove { .. }
            | mir::Instruction::DictionaryClear { .. }
            | mir::Instruction::ListAdd { .. }
            | mir::Instruction::ListGet { .. }
            | mir::Instruction::ListSet { .. }
            | mir::Instruction::ListRemoveAt { .. }
            | mir::Instruction::ListClear { .. } => {
                Some(TemporarySubregionRejectionReason::CollectionBarrier)
            }
            mir::Instruction::AllocateStringBuilder {
                destination: mir::Place::Local(_),
                region: mir::AllocationRegion::Temporary,
                ..
            }
            | mir::Instruction::StringBuilderAppend {
                builder:
                    mir::Operand {
                        kind: mir::OperandKind::Copy(mir::Place::Local(_)),
                        ..
                    },
                ..
            }
            | mir::Instruction::StringBuilderToString {
                destination: mir::Place::Local(_),
                builder:
                    mir::Operand {
                        kind: mir::OperandKind::Copy(mir::Place::Local(_)),
                        ..
                    },
                ..
            } => None,
            mir::Instruction::AllocateStringBuilder { .. } => {
                Some(TemporarySubregionRejectionReason::BuilderOwnershipBarrier)
            }
            mir::Instruction::StringBuilderAppend { .. }
            | mir::Instruction::StringBuilderToString { .. }
            | mir::Instruction::StringDecodeNext { .. } => {
                Some(TemporarySubregionRejectionReason::StringBarrier)
            }
            mir::Instruction::Assign { target, value } => assign_barrier(target, value),
            mir::Instruction::AllocateArray {
                destination,
                element_type,
                length,
                ..
            } if matches!(destination, mir::Place::Local(_))
                && is_execution_safe_type(element_type)
                && is_execution_safe_operand(length) =>
            {
                None
            }
            mir::Instruction::AllocateObject {
                destination: mir::Place::Local(_),
                ..
            } => None,
            mir::Instruction::AllocateArray { .. } | mir::Instruction::AllocateObject { .. } => {
                Some(TemporarySubregionRejectionReason::UnsupportedInstruction)
            }
            mir::Instruction::OwnedRegionEnter { .. }
            | mir::Instruction::OwnedRegionExit { .. } => {
                Some(TemporarySubregionRejectionReason::UnsupportedInstruction)
            }
        };
        if reason.is_some() {
            return reason;
        }
    }
    None
}

/// Structural, candidate-local provenance for direct-local hidden-backing
/// owners. Semantic lifetime and escape authority remains with AARM-5A; this
/// pass only proves the executable representation accepted by the compiler
/// and independently rechecked by the backend.
#[cfg_attr(test, allow(dead_code))]
#[allow(clippy::too_many_lines)]
pub(super) fn validate_builder_provenance(
    entry: mir::BasicBlockId,
    blocks: &[(mir::BasicBlockId, &[mir::Instruction])],
    successors: &HashMap<mir::BasicBlockId, Vec<mir::BasicBlockId>>,
    rewinds: &[mir::MirPoint],
) -> Result<(), TemporarySubregionRejectionReason> {
    let terminal = rewinds
        .iter()
        .map(|point| point.block)
        .collect::<HashSet<_>>();
    let block_instructions = blocks
        .iter()
        .map(|(block, instructions)| (*block, *instructions))
        .collect::<HashMap<_, _>>();
    let mut states = HashMap::from([(entry, BTreeMap::<u32, FineOwnedKind>::new())]);
    let mut pending = vec![entry];
    while let Some(block) = pending.pop() {
        let mut owned = states[&block].clone();
        let Some(instructions) = block_instructions.get(&block) else {
            return Err(TemporarySubregionRejectionReason::BuilderOwnershipBarrier);
        };
        for instruction in *instructions {
            match instruction {
                mir::Instruction::AllocateStringBuilder {
                    destination: mir::Place::Local(local),
                    region: mir::AllocationRegion::Temporary,
                    ..
                } => {
                    if owned
                        .insert(local.0, FineOwnedKind::StringBuilder)
                        .is_some()
                    {
                        return Err(TemporarySubregionRejectionReason::BuilderOwnershipBarrier);
                    }
                }
                mir::Instruction::AllocateList {
                    destination: mir::Place::Local(local),
                    region: mir::AllocationRegion::Temporary,
                    ..
                } => {
                    if owned.insert(local.0, FineOwnedKind::List).is_some() {
                        return Err(TemporarySubregionRejectionReason::BuilderOwnershipBarrier);
                    }
                }
                mir::Instruction::AllocateDictionary {
                    destination: mir::Place::Local(local),
                    region: mir::AllocationRegion::Temporary,
                    ..
                } => {
                    if owned.insert(local.0, FineOwnedKind::Dictionary).is_some() {
                        return Err(TemporarySubregionRejectionReason::BuilderOwnershipBarrier);
                    }
                }
                mir::Instruction::AllocateStringBuilder { .. }
                | mir::Instruction::AllocateList { .. }
                | mir::Instruction::AllocateDictionary { .. } => {
                    return Err(TemporarySubregionRejectionReason::BuilderOwnershipBarrier);
                }
                mir::Instruction::StringBuilderAppend { builder, .. }
                | mir::Instruction::StringBuilderToString { builder, .. } => {
                    if !receiver_is_fine_owned(builder, FineOwnedKind::StringBuilder, &owned) {
                        return Err(TemporarySubregionRejectionReason::BuilderOwnershipBarrier);
                    }
                }
                mir::Instruction::ListAdd { list, .. }
                | mir::Instruction::ListGet { list, .. }
                | mir::Instruction::ListRemoveAt { list, .. } => {
                    if !receiver_is_fine_owned(list, FineOwnedKind::List, &owned) {
                        return Err(TemporarySubregionRejectionReason::BuilderOwnershipBarrier);
                    }
                }
                mir::Instruction::DictionaryAdd { dictionary, .. }
                | mir::Instruction::DictionarySet { dictionary, .. }
                | mir::Instruction::DictionaryTryGet { dictionary, .. }
                | mir::Instruction::DictionaryContainsKey { dictionary, .. }
                | mir::Instruction::DictionaryRemove { dictionary, .. } => {
                    if !receiver_is_fine_owned(dictionary, FineOwnedKind::Dictionary, &owned) {
                        return Err(TemporarySubregionRejectionReason::BuilderOwnershipBarrier);
                    }
                }
                mir::Instruction::Assign { target, value }
                    if !owned_collection_read(value, &owned)
                        && (rvalue_mentions_fine_owned(value, &owned)
                            || matches!(target, mir::Place::Local(local) if owned.contains_key(&local.0))) =>
                {
                    return Err(TemporarySubregionRejectionReason::BuilderOwnershipBarrier);
                }
                mir::Instruction::Assign {
                    target: mir::Place::Local(local),
                    ..
                } if owned.contains_key(&local.0) => {
                    return Err(TemporarySubregionRejectionReason::BuilderOwnershipBarrier);
                }
                _ => {}
            }
        }
        if terminal.contains(&block) {
            continue;
        }
        for successor in successors.get(&block).into_iter().flatten() {
            let Some(_) = block_instructions.get(successor) else {
                return Err(TemporarySubregionRejectionReason::BuilderOwnershipBarrier);
            };
            if let Some(existing) = states.get(successor) {
                if existing != &owned {
                    return Err(TemporarySubregionRejectionReason::BuilderOwnershipBarrier);
                }
            } else {
                states.insert(*successor, owned.clone());
                pending.push(*successor);
            }
        }
    }
    Ok(())
}

fn receiver_is_fine_owned(
    operand: &mir::Operand,
    expected: FineOwnedKind,
    owned: &BTreeMap<u32, FineOwnedKind>,
) -> bool {
    matches!(operand.kind, mir::OperandKind::Copy(mir::Place::Local(local))
        if owned.get(&local.0) == Some(&expected))
}

fn owned_collection_read(value: &mir::Rvalue, owned: &BTreeMap<u32, FineOwnedKind>) -> bool {
    match &value.kind {
        mir::RvalueKind::ListLength(operand) | mir::RvalueKind::ListVersion(operand) => {
            receiver_is_fine_owned(operand, FineOwnedKind::List, owned)
        }
        mir::RvalueKind::DictionaryLength(operand) => {
            receiver_is_fine_owned(operand, FineOwnedKind::Dictionary, owned)
        }
        _ => false,
    }
}

fn rvalue_mentions_fine_owned(value: &mir::Rvalue, owned: &BTreeMap<u32, FineOwnedKind>) -> bool {
    match &value.kind {
        mir::RvalueKind::Use(operand)
        | mir::RvalueKind::Discriminant(operand)
        | mir::RvalueKind::ArrayLength(operand)
        | mir::RvalueKind::ListLength(operand)
        | mir::RvalueKind::DictionaryLength(operand)
        | mir::RvalueKind::ListVersion(operand)
        | mir::RvalueKind::StringByteLength(operand)
        | mir::RvalueKind::Cast(operand)
        | mir::RvalueKind::Unary { operand, .. } => operand_mentions_owned_builder(operand, owned),
        mir::RvalueKind::Aggregate(fields) | mir::RvalueKind::EnumConstruct { fields, .. } => {
            fields
                .iter()
                .any(|field| operand_mentions_owned_builder(&field.value, owned))
        }
        mir::RvalueKind::MakeInterface { object, .. } => {
            operand_mentions_owned_builder(object, owned)
        }
        mir::RvalueKind::Binary { left, right, .. }
        | mir::RvalueKind::Equality { left, right, .. } => {
            operand_mentions_owned_builder(left, owned)
                || operand_mentions_owned_builder(right, owned)
        }
    }
}

fn operand_mentions_owned_builder(
    operand: &mir::Operand,
    owned: &BTreeMap<u32, FineOwnedKind>,
) -> bool {
    matches!(&operand.kind, mir::OperandKind::Copy(place) if place_mentions_owned_builder(place, owned))
}

fn place_mentions_owned_builder(place: &mir::Place, owned: &BTreeMap<u32, FineOwnedKind>) -> bool {
    match place {
        mir::Place::Local(local) => owned.contains_key(&local.0),
        mir::Place::Symbol(_) => false,
        mir::Place::Field { base, .. } | mir::Place::EnumField { base, .. } => {
            place_mentions_owned_builder(base, owned)
        }
        mir::Place::Index { array, index, .. } => {
            operand_mentions_owned_builder(array, owned)
                || operand_mentions_owned_builder(index, owned)
        }
        mir::Place::ObjectField { object, .. } => operand_mentions_owned_builder(object, owned),
    }
}

fn immutable_temporary_string_intrinsic_is_executable(
    destination: Option<&mir::Place>,
    intrinsic: mir::Intrinsic,
    arguments: &[mir::Operand],
    return_type: &mir::Type,
) -> bool {
    let _ = arguments;
    matches!(destination, Some(mir::Place::Local(_)))
        && return_type == &mir::Type::String
        && matches!(
            intrinsic,
            mir::Intrinsic::StringConcatTemporary
                | mir::Intrinsic::StringJoinTemporary
                | mir::Intrinsic::StringSubstringFromTemporary
                | mir::Intrinsic::StringSubstringRangeTemporary
                | mir::Intrinsic::StringFromLongTemporary
                | mir::Intrinsic::StringFromULongTemporary
                | mir::Intrinsic::StringFromDoubleTemporary
                | mir::Intrinsic::StringFromFloatTemporary
                | mir::Intrinsic::StringFromBoolTemporary
                | mir::Intrinsic::StringFromCharTemporary
        )
}

fn assign_barrier(
    target: &mir::Place,
    value: &mir::Rvalue,
) -> Option<TemporarySubregionRejectionReason> {
    if rvalue_contains_type(value, is_collection_type) {
        return Some(TemporarySubregionRejectionReason::CollectionBarrier);
    }
    if rvalue_contains_type(value, is_string_type) {
        return Some(TemporarySubregionRejectionReason::StringBarrier);
    }
    if !is_execution_safe_place(target) || !is_execution_safe_rvalue(value) {
        return Some(TemporarySubregionRejectionReason::UnsupportedInstruction);
    }
    None
}

fn direct_local_operand(operand: &mir::Operand) -> bool {
    matches!(operand.kind, mir::OperandKind::Copy(mir::Place::Local(_)))
}

fn is_execution_safe_rvalue(value: &mir::Rvalue) -> bool {
    if !is_execution_safe_type(&value.type_) {
        return false;
    }
    match &value.kind {
        mir::RvalueKind::Use(operand)
        | mir::RvalueKind::Cast(operand)
        | mir::RvalueKind::Unary { operand, .. } => is_execution_safe_operand(operand),
        mir::RvalueKind::Binary {
            left,
            operator,
            right,
        } => {
            !matches!(
                operator,
                mir::BinaryOperator::Divide | mir::BinaryOperator::Remainder
            ) && is_execution_safe_operand(left)
                && is_execution_safe_operand(right)
        }
        mir::RvalueKind::Equality { left, right, .. } => {
            is_execution_safe_equality_type(&left.type_)
                && is_execution_safe_operand(left)
                && is_execution_safe_operand(right)
        }
        mir::RvalueKind::ArrayLength(array) => {
            matches!(array.type_, mir::Type::Array(_)) && is_execution_safe_operand(array)
        }
        mir::RvalueKind::Aggregate(_)
        | mir::RvalueKind::EnumConstruct { .. }
        | mir::RvalueKind::Discriminant(_)
        | mir::RvalueKind::ListLength(_)
        | mir::RvalueKind::DictionaryLength(_)
        | mir::RvalueKind::ListVersion(_)
        | mir::RvalueKind::StringByteLength(_)
        | mir::RvalueKind::MakeInterface { .. } => false,
    }
}

fn is_execution_safe_operand(operand: &mir::Operand) -> bool {
    if !is_execution_safe_type(&operand.type_) {
        return false;
    }
    match &operand.kind {
        mir::OperandKind::Constant(mir::Constant::String(_)) => false,
        mir::OperandKind::Constant(_) | mir::OperandKind::Function(_) => true,
        mir::OperandKind::Copy(place) => is_execution_safe_place(place),
    }
}

fn is_execution_safe_place(place: &mir::Place) -> bool {
    match place {
        mir::Place::Local(_) => true,
        mir::Place::ObjectField { object, .. } => {
            matches!(object.type_, mir::Type::Class(_)) && is_execution_safe_operand(object)
        }
        mir::Place::Index {
            array,
            index,
            element_type,
            ..
        } => {
            matches!(
                &array.type_,
                mir::Type::Array(array_element) if array_element.as_ref() == element_type
            ) && is_execution_safe_type(element_type)
                && is_execution_safe_operand(array)
                && is_execution_safe_operand(index)
        }
        mir::Place::Symbol(_) | mir::Place::Field { .. } | mir::Place::EnumField { .. } => false,
    }
}

fn is_execution_safe_type(type_: &mir::Type) -> bool {
    match type_ {
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
        mir::Type::Array(element) => is_execution_safe_type(element),
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
    }
}

fn is_execution_safe_equality_type(type_: &mir::Type) -> bool {
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

fn rvalue_contains_type(value: &mir::Rvalue, predicate: fn(&mir::Type) -> bool) -> bool {
    predicate(&value.type_)
        || match &value.kind {
            mir::RvalueKind::Use(operand)
            | mir::RvalueKind::Discriminant(operand)
            | mir::RvalueKind::ArrayLength(operand)
            | mir::RvalueKind::ListLength(operand)
            | mir::RvalueKind::DictionaryLength(operand)
            | mir::RvalueKind::ListVersion(operand)
            | mir::RvalueKind::StringByteLength(operand)
            | mir::RvalueKind::Cast(operand)
            | mir::RvalueKind::Unary { operand, .. } => predicate(&operand.type_),
            mir::RvalueKind::Aggregate(fields) | mir::RvalueKind::EnumConstruct { fields, .. } => {
                fields.iter().any(|field| predicate(&field.value.type_))
            }
            mir::RvalueKind::MakeInterface { object, .. } => predicate(&object.type_),
            mir::RvalueKind::Binary { left, right, .. }
            | mir::RvalueKind::Equality { left, right, .. } => {
                predicate(&left.type_) || predicate(&right.type_)
            }
        }
}

fn is_collection_type(type_: &mir::Type) -> bool {
    match type_ {
        mir::Type::List(_) | mir::Type::Dictionary(_, _) => true,
        mir::Type::Array(element) => is_collection_type(element),
        _ => false,
    }
}

fn is_string_type(type_: &mir::Type) -> bool {
    match type_ {
        mir::Type::String => true,
        mir::Type::Array(element) => is_string_type(element),
        _ => false,
    }
}

fn intervals_overlap(
    left: &ValidatedTemporarySubregion,
    right: &ValidatedTemporarySubregion,
) -> bool {
    left.function == right.function
        && if left.rewinds.len() == 1
            && right.rewinds.len() == 1
            && left.checkpoint.block == right.checkpoint.block
            && left.rewinds[0].block == right.rewinds[0].block
        {
            left.checkpoint.instruction_boundary < right.rewinds[0].instruction_boundary
                && right.checkpoint.instruction_boundary < left.rewinds[0].instruction_boundary
        } else {
            left.checkpoint == right.checkpoint
                || left
                    .allocations
                    .iter()
                    .any(|site| right.allocations.contains(site))
        }
}

fn compare_site(left: &mir::MirAllocationSite, right: &mir::MirAllocationSite) -> Ordering {
    (left.function.0, left.block.0, left.instruction_index).cmp(&(
        right.function.0,
        right.block.0,
        right.instruction_index,
    ))
}

fn compare_point(left: &mir::MirPoint, right: &mir::MirPoint) -> Ordering {
    (left.block.0, left.instruction_boundary).cmp(&(right.block.0, right.instruction_boundary))
}

fn compare_points(left: &[mir::MirPoint], right: &[mir::MirPoint]) -> Ordering {
    left.iter()
        .zip(right)
        .find_map(|(left, right)| {
            let ordering = compare_point(left, right);
            (!ordering.is_eq()).then_some(ordering)
        })
        .unwrap_or_else(|| left.len().cmp(&right.len()))
}

fn compare_sites(left: &[mir::MirAllocationSite], right: &[mir::MirAllocationSite]) -> Ordering {
    left.iter()
        .zip(right)
        .find_map(|(left, right)| {
            let ordering = compare_site(left, right);
            (!ordering.is_eq()).then_some(ordering)
        })
        .unwrap_or_else(|| left.len().cmp(&right.len()))
}

fn compare_candidates(
    left: &mir::TemporarySubregionCandidate,
    right: &mir::TemporarySubregionCandidate,
) -> Ordering {
    compare_point(&left.checkpoint, &right.checkpoint)
        .then_with(|| compare_points(&left.rewinds, &right.rewinds))
        .then_with(|| compare_sites(&left.allocations, &right.allocations))
        .then_with(|| left.id.0.cmp(&right.id.0))
}

fn compare_validated(
    left: &ValidatedTemporarySubregion,
    right: &ValidatedTemporarySubregion,
) -> Ordering {
    left.function
        .0
        .cmp(&right.function.0)
        .then_with(|| compare_point(&left.checkpoint, &right.checkpoint))
        .then_with(|| compare_points(&left.rewinds, &right.rewinds))
        .then_with(|| compare_sites(&left.allocations, &right.allocations))
        .then_with(|| left.id.0.cmp(&right.id.0))
}

fn compare_rejected(
    left: &RejectedTemporarySubregion,
    right: &RejectedTemporarySubregion,
) -> Ordering {
    left.function
        .0
        .cmp(&right.function.0)
        .then_with(|| compare_candidates(&left.candidate, &right.candidate))
        .then_with(|| reason_order(left.reason).cmp(&reason_order(right.reason)))
}

const fn reason_order(reason: TemporarySubregionRejectionReason) -> u8 {
    match reason {
        TemporarySubregionRejectionReason::StaleAnalysis => 0,
        TemporarySubregionRejectionReason::UnsupportedControlFlow => 1,
        TemporarySubregionRejectionReason::DuplicateId => 2,
        TemporarySubregionRejectionReason::MalformedPoint => 3,
        TemporarySubregionRejectionReason::WrongFunction => 4,
        TemporarySubregionRejectionReason::MalformedAllocationSite => 5,
        TemporarySubregionRejectionReason::PersistentAllocation => 6,
        TemporarySubregionRejectionReason::DuplicateAllocationSite => 7,
        TemporarySubregionRejectionReason::MissingReferenceDeathProof => 8,
        TemporarySubregionRejectionReason::UnaccountedTemporaryAllocation => 9,
        TemporarySubregionRejectionReason::OverlappingSubregion => 10,
        TemporarySubregionRejectionReason::CallBarrier => 11,
        TemporarySubregionRejectionReason::ConcurrencyBarrier => 12,
        TemporarySubregionRejectionReason::CollectionBarrier => 13,
        TemporarySubregionRejectionReason::StringBarrier => 14,
        TemporarySubregionRejectionReason::BuilderOwnershipBarrier => 15,
        TemporarySubregionRejectionReason::UnsupportedInstruction => 16,
    }
}
