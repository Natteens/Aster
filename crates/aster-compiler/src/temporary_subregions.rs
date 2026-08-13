//! Research-only planning for backend-neutral AARM Temporary subregion candidates.
//!
//! Candidate metadata is deliberately non-executable. The normal compiler
//! pipeline never invokes this module, and the execution backend rejects every
//! non-empty candidate list until later AARM validation and runtime work exist.

#![cfg_attr(not(test), allow(dead_code))]

use std::collections::{HashMap, HashSet};

use aster_mir as mir;

use crate::{escape_analysis, lifetime_analysis::LifetimeAnalysisReport};

/// Run the explicit research-only AARM-5A -> AARM-5B orchestration and replace
/// every function's candidate metadata. Keeping the immutable analysis and
/// report-consuming planner inside one operation prevents stale MIR pairing.
pub(super) fn analyze_and_populate_candidate_subregions(module: &mut mir::Module) {
    for function in &mut module.functions {
        function.temporary_subregion_candidates.clear();
    }
    let lifetime = crate::lifetime_analysis::analyze(module);
    if !report_matches_module(module, &lifetime) {
        return;
    }

    let plans = module
        .functions
        .iter()
        .map(|function| (function.symbol, plan_function(function, &lifetime)))
        .collect::<HashMap<_, _>>();
    for function in &mut module.functions {
        function.temporary_subregion_candidates =
            plans.get(&function.symbol).cloned().unwrap_or_default();
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
    let [block] = function.blocks.as_slice() else {
        return Vec::new();
    };
    if function.entry != block.id
        || !matches!(
            block.terminator,
            mir::Terminator::Return(_) | mir::Terminator::End
        )
        || function_contains_concurrency_boundary(function)
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

fn function_contains_concurrency_boundary(function: &mir::Function) -> bool {
    function
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .any(|instruction| {
            matches!(
                instruction,
                mir::Instruction::CallIntrinsic {
                    intrinsic: mir::Intrinsic::TaskRun
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
                        | mir::Intrinsic::ParallelReduce,
                    ..
                }
            )
        })
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

    fn prepare(mut module: mir::Module) -> (mir::Module, LifetimeAnalysisReport) {
        escape_analysis::assign_allocation_regions(&mut module);
        let lifetime = crate::lifetime_analysis::analyze(&module);
        analyze_and_populate_candidate_subregions(&mut module);
        (module, lifetime)
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
        analyze_and_populate_candidate_subregions(&mut module);
        let expected = module.functions[0].temporary_subregion_candidates.clone();

        analyze_and_populate_candidate_subregions(&mut module);
        assert_eq!(module.functions[0].temporary_subregion_candidates, expected);

        module.functions[0].temporary_subregion_candidates.clear();
        analyze_and_populate_candidate_subregions(&mut module);
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
        analyze_and_populate_candidate_subregions(&mut module);
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
}
