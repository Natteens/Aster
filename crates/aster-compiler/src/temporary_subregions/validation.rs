//! Conservative AARM-5C proof barriers for Temporary subregion candidates.
//!
//! This module validates research metadata only. It does not mutate MIR,
//! create executable checkpoint operations, or reach the runtime/backend.

use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
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
    pub rewind: mir::MirPoint,
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
    UnsupportedInstruction,
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
        let supported_function = supported_function_block(function);
        let has_concurrency = super::function_contains_concurrency_boundary(function);

        for candidate in &plan.candidates {
            let rejected = |reason| RejectedTemporarySubregion {
                function: function.symbol,
                candidate: candidate.clone(),
                reason,
            };

            if supported_function.is_none() || candidate.rewinds.len() != 1 {
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

            let block = supported_function.expect("checked above");
            match validate_candidate(function, block, candidate, &proofs, has_concurrency) {
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

fn supported_function_block(function: &mir::Function) -> Option<&mir::BasicBlock> {
    let [block] = function.blocks.as_slice() else {
        return None;
    };
    (function.entry == block.id
        && matches!(
            block.terminator,
            mir::Terminator::Return(_) | mir::Terminator::End
        ))
    .then_some(block)
}

fn validate_candidate(
    function: &mir::Function,
    block: &mir::BasicBlock,
    candidate: &mir::TemporarySubregionCandidate,
    proofs: &HashMap<mir::MirAllocationSite, &crate::lifetime_analysis::AllocationLifetimeProof>,
    has_concurrency: bool,
) -> Result<ValidatedTemporarySubregion, TemporarySubregionRejectionReason> {
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
            None if !matches!(
                instruction,
                mir::Instruction::AllocateObject { .. } | mir::Instruction::AllocateArray { .. }
            ) =>
            {
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
        allocations: candidate.allocations.clone(),
    })
}

fn allocation_form_barrier(
    instruction: &mir::Instruction,
) -> Option<TemporarySubregionRejectionReason> {
    match instruction {
        mir::Instruction::AllocateList { .. }
        | mir::Instruction::AllocateDictionary { .. }
        | mir::Instruction::AllocateStringBuilder { .. }
        | mir::Instruction::DictionaryEntries { .. } => {
            Some(TemporarySubregionRejectionReason::CollectionBarrier)
        }
        mir::Instruction::StringBuilderToString { .. } | mir::Instruction::CallIntrinsic { .. } => {
            Some(TemporarySubregionRejectionReason::StringBarrier)
        }
        _ => None,
    }
}

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
            mir::Instruction::CallIntrinsic { intrinsic, .. } => {
                if super::is_concurrency_intrinsic(*intrinsic) {
                    Some(TemporarySubregionRejectionReason::ConcurrencyBarrier)
                } else if intrinsic.allocation_region().is_some() {
                    Some(TemporarySubregionRejectionReason::StringBarrier)
                } else {
                    Some(TemporarySubregionRejectionReason::CallBarrier)
                }
            }
            mir::Instruction::AllocateList { .. }
            | mir::Instruction::AllocateDictionary { .. }
            | mir::Instruction::AllocateStringBuilder { .. }
            | mir::Instruction::DictionaryAdd { .. }
            | mir::Instruction::DictionarySet { .. }
            | mir::Instruction::DictionaryTryGet { .. }
            | mir::Instruction::DictionaryContainsKey { .. }
            | mir::Instruction::DictionaryRemove { .. }
            | mir::Instruction::DictionaryEntries { .. }
            | mir::Instruction::ListAdd { .. }
            | mir::Instruction::ListGet { .. }
            | mir::Instruction::ListRemoveAt { .. } => {
                Some(TemporarySubregionRejectionReason::CollectionBarrier)
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
        };
        if reason.is_some() {
            return reason;
        }
    }
    None
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
        && left.checkpoint.block == right.checkpoint.block
        && left.checkpoint.instruction_boundary < right.rewind.instruction_boundary
        && right.checkpoint.instruction_boundary < left.rewind.instruction_boundary
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
        .then_with(|| compare_point(&left.rewind, &right.rewind))
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
        TemporarySubregionRejectionReason::UnsupportedInstruction => 15,
    }
}
