//! Conservative long-lived ownership over the existing Persistent arena.
//!
//! The pass recognizes a fresh, return-only reference produced by a direct
//! ASTER call, reuses escape analysis for its complete local alias closure,
//! and reuses MIR liveness for the first same-block point where every alias is
//! dead. Only repeated CFG blocks are selected. Ambiguous, shared, contained,
//! interface, recursive-allocation, and cross-worker shapes remain Persistent.

use std::collections::{HashMap, HashSet, VecDeque};

use aster_mir as mir;

use crate::{escape_analysis, lifetime_analysis};

#[derive(Clone, Debug)]
struct OwnedRegionPlan {
    function: mir::SymbolId,
    block: mir::BasicBlockId,
    id: mir::OwnedRegionId,
    checkpoint: usize,
    rewind: usize,
    invalidated: Vec<mir::LocalId>,
}

pub(super) fn lower(module: &mut mir::Module) {
    let plans = plan(module);
    if plans.is_empty() {
        return;
    }

    for function in &mut module.functions {
        let function_plans = plans
            .iter()
            .filter(|plan| plan.function == function.symbol)
            .collect::<Vec<_>>();
        if function_plans.is_empty() {
            continue;
        }
        for block in &mut function.blocks {
            let block_plans = function_plans
                .iter()
                .copied()
                .filter(|plan| plan.block == block.id)
                .collect::<Vec<_>>();
            if block_plans.is_empty() {
                continue;
            }
            let original = std::mem::take(&mut block.instructions);
            let mut rewritten = Vec::with_capacity(original.len() + block_plans.len() * 2);
            for boundary in 0..=original.len() {
                for plan in block_plans.iter().filter(|plan| plan.rewind == boundary) {
                    rewritten.push(mir::Instruction::OwnedRegionExit {
                        id: plan.id,
                        invalidated: plan.invalidated.clone(),
                    });
                }
                for plan in block_plans
                    .iter()
                    .filter(|plan| plan.checkpoint == boundary)
                {
                    rewritten.push(mir::Instruction::OwnedRegionEnter { id: plan.id });
                }
                if let Some(instruction) = original.get(boundary) {
                    rewritten.push(instruction.clone());
                }
            }
            block.instructions = rewritten;
        }
    }
}

fn plan(module: &mir::Module) -> Vec<OwnedRegionPlan> {
    if module.functions.iter().any(|function| {
        function
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .any(|instruction| {
                matches!(
                    instruction,
                    mir::Instruction::OwnedRegionEnter { .. }
                        | mir::Instruction::OwnedRegionExit { .. }
                )
            })
    }) {
        return Vec::new();
    }

    let effects = persistent_effects(module);
    let producers = owned_return_producers(module, &effects);
    let reference_facts = escape_analysis::reference_facts(module);
    let mut plans = Vec::new();

    for function in &module.functions {
        let cyclic = cyclic_blocks(function);
        let liveness = lifetime_analysis::reference_liveness(function);
        let mut next_id = 0_u32;
        for block in &function.blocks {
            if !cyclic.contains(&block.id) {
                continue;
            }
            let mut occupied_until = 0_usize;
            for (instruction_index, instruction) in block.instructions.iter().enumerate() {
                if instruction_index < occupied_until {
                    continue;
                }
                let mir::Instruction::Call {
                    destination: Some(mir::Place::Local(destination)),
                    function: callee,
                    return_type,
                    ..
                } = instruction
                else {
                    continue;
                };
                if !producers.contains(callee) || !supported_owned_type(return_type) {
                    continue;
                }
                let Some(reference) = reference_facts.local(function.symbol, *destination) else {
                    continue;
                };
                if reference.escape_reason.is_some() || reference.aliases.is_empty() {
                    continue;
                }
                let Some(rewind) = liveness
                    .dead_after(block.id, instruction_index, &reference.aliases)
                    .into_iter()
                    .filter(|point| {
                        point.block == block.id
                            && point.instruction_boundary > instruction_index
                            && point.instruction_boundary <= block.instructions.len()
                    })
                    .map(|point| point.instruction_boundary)
                    .min()
                else {
                    continue;
                };
                if !supported_span(
                    &block.instructions[instruction_index..rewind],
                    *callee,
                    &effects,
                ) {
                    continue;
                }
                plans.push(OwnedRegionPlan {
                    function: function.symbol,
                    block: block.id,
                    id: mir::OwnedRegionId(next_id),
                    checkpoint: instruction_index,
                    rewind,
                    invalidated: reference.aliases,
                });
                next_id = next_id.saturating_add(1);
                occupied_until = rewind;
            }
        }
    }

    plans
}

fn owned_return_producers(
    module: &mir::Module,
    effects: &HashMap<mir::SymbolId, bool>,
) -> HashSet<mir::SymbolId> {
    let facts = escape_analysis::allocation_escape_facts(module);
    let mut facts_by_function = HashMap::<_, Vec<_>>::new();
    for fact in facts {
        if fact.region == mir::AllocationRegion::Persistent {
            facts_by_function
                .entry(fact.site.function)
                .or_default()
                .push(fact);
        }
    }

    module
        .functions
        .iter()
        .filter(|function| {
            supported_owned_type(&function.return_type)
                && function
                    .parameters
                    .iter()
                    .all(|parameter| !may_carry_reference(&parameter.type_))
                && facts_by_function
                    .get(&function.symbol)
                    .is_some_and(|facts| {
                        matches!(facts.as_slice(), [fact]
                            if fact.escape_reason == Some(escape_analysis::EscapeReason::Returned)
                                && !fact.aliases.is_empty()
                                && every_return_uses_owned_alias(function, &fact.aliases))
                    })
                && function.blocks.iter().all(|block| {
                    block
                        .instructions
                        .iter()
                        .all(|instruction| match instruction {
                            mir::Instruction::Call {
                                function: callee, ..
                            } => !effects.get(callee).copied().unwrap_or(true),
                            mir::Instruction::CallInterface { .. }
                            | mir::Instruction::OwnedRegionEnter { .. }
                            | mir::Instruction::OwnedRegionExit { .. } => false,
                            mir::Instruction::CallIntrinsic { intrinsic, .. } => {
                                !concurrency_intrinsic(*intrinsic)
                            }
                            _ => true,
                        })
                })
        })
        .map(|function| function.symbol)
        .collect()
}

fn persistent_effects(module: &mir::Module) -> HashMap<mir::SymbolId, bool> {
    let symbols = module
        .functions
        .iter()
        .map(|function| function.symbol)
        .collect::<HashSet<_>>();
    let mut effects = module
        .functions
        .iter()
        .map(|function| {
            let direct = function
                .blocks
                .iter()
                .flat_map(|block| &block.instructions)
                .any(|instruction| {
                    escape_analysis::dynamic_allocation_region(instruction)
                        == Some(mir::AllocationRegion::Persistent)
                        || matches!(instruction, mir::Instruction::CallInterface { .. })
                        || matches!(instruction,
                            mir::Instruction::CallIntrinsic { intrinsic, .. }
                                if concurrency_intrinsic(*intrinsic))
                        || runtime_growth_may_allocate(instruction)
                        || matches!(instruction,
                            mir::Instruction::Call { function, .. } if !symbols.contains(function))
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

fn every_return_uses_owned_alias(function: &mir::Function, aliases: &[mir::LocalId]) -> bool {
    let aliases = aliases.iter().copied().collect::<HashSet<_>>();
    let mut saw_return = false;
    for block in &function.blocks {
        if let mir::Terminator::Return(value) = &block.terminator {
            saw_return = true;
            let Some(mir::Operand {
                kind: mir::OperandKind::Copy(mir::Place::Local(local)),
                ..
            }) = value
            else {
                return false;
            };
            if !aliases.contains(local) {
                return false;
            }
        }
    }
    saw_return
}

fn supported_span(
    instructions: &[mir::Instruction],
    producer: mir::SymbolId,
    effects: &HashMap<mir::SymbolId, bool>,
) -> bool {
    instructions
        .iter()
        .enumerate()
        .all(|(index, instruction)| match instruction {
            mir::Instruction::Call { function, .. } if index == 0 => *function == producer,
            mir::Instruction::Call { function, .. } => {
                !effects.get(function).copied().unwrap_or(true)
            }
            mir::Instruction::CallInterface { .. }
            | mir::Instruction::OwnedRegionEnter { .. }
            | mir::Instruction::OwnedRegionExit { .. }
            | mir::Instruction::TemporarySubregionEnter { .. }
            | mir::Instruction::TemporarySubregionExit { .. } => false,
            mir::Instruction::CallIntrinsic { intrinsic, .. }
                if concurrency_intrinsic(*intrinsic) =>
            {
                false
            }
            instruction => {
                !runtime_growth_may_allocate(instruction)
                    && escape_analysis::dynamic_allocation_region(instruction)
                        != Some(mir::AllocationRegion::Persistent)
            }
        })
}

fn runtime_growth_may_allocate(instruction: &mir::Instruction) -> bool {
    matches!(
        instruction,
        mir::Instruction::ListAdd { .. }
            | mir::Instruction::DictionaryAdd { .. }
            | mir::Instruction::DictionarySet { .. }
            | mir::Instruction::StringBuilderAppend { .. }
    )
}

fn cyclic_blocks(function: &mir::Function) -> HashSet<mir::BasicBlockId> {
    let successors = function
        .blocks
        .iter()
        .map(|block| (block.id, block_successors(&block.terminator)))
        .collect::<HashMap<_, _>>();
    function
        .blocks
        .iter()
        .filter(|block| {
            successors[&block.id]
                .iter()
                .any(|successor| reaches(*successor, block.id, &successors))
        })
        .map(|block| block.id)
        .collect()
}

fn reaches(
    start: mir::BasicBlockId,
    target: mir::BasicBlockId,
    successors: &HashMap<mir::BasicBlockId, Vec<mir::BasicBlockId>>,
) -> bool {
    let mut pending = VecDeque::from([start]);
    let mut seen = HashSet::new();
    while let Some(block) = pending.pop_front() {
        if block == target {
            return true;
        }
        if seen.insert(block) {
            pending.extend(successors.get(&block).into_iter().flatten().copied());
        }
    }
    false
}

fn block_successors(terminator: &mir::Terminator) -> Vec<mir::BasicBlockId> {
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

fn supported_owned_type(type_: &mir::Type) -> bool {
    matches!(
        type_,
        mir::Type::String
            | mir::Type::Array(_)
            | mir::Type::Class(_)
            | mir::Type::List(_)
            | mir::Type::Dictionary(_, _)
    )
}

fn may_carry_reference(type_: &mir::Type) -> bool {
    matches!(
        type_,
        mir::Type::String
            | mir::Type::Array(_)
            | mir::Type::Class(_)
            | mir::Type::Interface(_)
            | mir::Type::List(_)
            | mir::Type::Dictionary(_, _)
            | mir::Type::Task(_)
            | mir::Type::User(_)
            | mir::Type::Enum(_)
    )
}

fn concurrency_intrinsic(intrinsic: mir::Intrinsic) -> bool {
    matches!(
        intrinsic,
        mir::Intrinsic::TaskRun
            | mir::Intrinsic::TaskWait
            | mir::Intrinsic::TaskWaitAll
            | mir::Intrinsic::TaskCancel
            | mir::Intrinsic::TaskCancellationRequested
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

#[cfg(test)]
mod tests {
    use super::*;

    fn compile(source: &str) -> mir::Module {
        crate::compile(source)
            .expect("owned-region test source compiles")
            .mir
    }

    fn marker_count(module: &mir::Module) -> (usize, usize) {
        module
            .functions
            .iter()
            .flat_map(|function| &function.blocks)
            .flat_map(|block| &block.instructions)
            .fold((0, 0), |(enters, exits), instruction| match instruction {
                mir::Instruction::OwnedRegionEnter { .. } => (enters + 1, exits),
                mir::Instruction::OwnedRegionExit { .. } => (enters, exits + 1),
                _ => (enters, exits),
            })
    }

    #[test]
    fn repeated_fresh_return_gets_one_balanced_owned_region() {
        let module = compile(
            r"
                internal int[] Make(int value) { return [value]; }
                public int Run() {
                    int total = 0;
                    for (int i = 0; i < 10; i++) {
                        int[] value = Make(i);
                        total += value[0];
                    }
                    return total;
                }
            ",
        );

        assert_eq!(marker_count(&module), (1, 1));
        let exit = module
            .functions
            .iter()
            .flat_map(|function| &function.blocks)
            .flat_map(|block| &block.instructions)
            .find_map(|instruction| match instruction {
                mir::Instruction::OwnedRegionExit { invalidated, .. } => Some(invalidated),
                _ => None,
            })
            .expect("owned exit exists");
        assert!(!exit.is_empty());
        assert!(exit.windows(2).all(|pair| pair[0].0 < pair[1].0));
    }

    #[test]
    fn live_alias_and_overlapping_fresh_returns_remain_persistent() {
        let module = compile(
            r"
                internal int[] Make(int value) { return [value]; }
                public int Run() {
                    int total = 0;
                    for (int i = 0; i < 10; i++) {
                        int[] first = Make(i);
                        int[] alias = first;
                        int[] second = Make(i + 1);
                        total += alias[0] + second[0];
                    }
                    return total;
                }
            ",
        );

        // The first family overlaps the second and therefore cannot own one
        // coarse Persistent slice. The later independent result is eligible.
        assert_eq!(marker_count(&module), (1, 1));
    }

    #[test]
    fn reference_parameters_and_reference_bearing_graphs_are_not_producers() {
        let module = compile(
            r"
                public class Node {
                    public int[] values;
                    public Node(int[] values) { this.values = values; }
                }
                internal Node Pass(Node value) { return value; }
                public int Run() {
                    Node root = new Node([42]);
                    for (int i = 0; i < 10; i++) { root = Pass(root); }
                    return 42;
                }
            ",
        );

        assert_eq!(marker_count(&module), (0, 0));
    }

    #[test]
    fn worker_intrinsics_never_receive_owned_region_markers() {
        let module = compile(
            r"
                public int Work() { int[] values = [42]; return values[0]; }
                public int Run() {
                    int total = 0;
                    for (int i = 0; i < 10; i++) { total += Task.Run(Work).Wait(); }
                    return total;
                }
            ",
        );

        assert_eq!(marker_count(&module), (0, 0));
    }

    #[test]
    fn transitive_worker_effect_prevents_owned_producer_selection() {
        let module = compile(
            r"
                internal int Work() { return 42; }
                internal int RunWorker() { return Task.Run(Work).Wait(); }
                internal int[] Make() {
                    int value = RunWorker();
                    return [value];
                }
                public int Run() {
                    int total = 0;
                    for (int i = 0; i < 10; i++) {
                        int[] value = Make();
                        total += value[0];
                    }
                    return total;
                }
            ",
        );

        assert_eq!(marker_count(&module), (0, 0));
    }

    #[test]
    fn producer_with_a_non_owned_return_path_falls_back() {
        let module = compile(
            r#"
                internal string Make(int value) {
                    if (value < 0) { return "static"; }
                    return $"value{value}";
                }
                public int Run() {
                    int total = 0;
                    for (int i = 0; i < 10; i++) {
                        string value = Make(i);
                        total += value.Length;
                    }
                    return total;
                }
            "#,
        );

        assert_eq!(marker_count(&module), (0, 0));
    }

    #[test]
    fn generic_owner_method_receiver_remains_a_conservative_reference_parameter() {
        let module = compile(
            r"
                public class Factory<T> {
                    public Factory() {}
                    public U[] Make<U>(U value) { return [value]; }
                }
                public int Run() {
                    Factory<int> factory = new Factory<int>();
                    int total = 0;
                    for (int i = 0; i < 10; i++) {
                        int[] value = factory.Make<int>(i);
                        total += value[0];
                    }
                    return total;
                }
            ",
        );

        assert_eq!(marker_count(&module), (0, 0));
    }

    #[test]
    fn persistent_effects_prevent_dynamic_region_nesting() {
        let module = compile(
            r"
                internal int[] Make(int value) { return [value]; }
                internal int Nested() {
                    int total = 0;
                    for (int i = 0; i < 2; i++) {
                        int[] value = Make(i);
                        total += value[0];
                    }
                    return total;
                }
                public int Run() {
                    int total = 0;
                    for (int i = 0; i < 10; i++) {
                        int[] outer = Make(i);
                        total += Nested();
                        total += outer[0];
                    }
                    return total;
                }
            ",
        );

        let marked = module
            .functions
            .iter()
            .filter(|function| {
                function
                    .blocks
                    .iter()
                    .flat_map(|block| &block.instructions)
                    .any(|instruction| {
                        matches!(instruction, mir::Instruction::OwnedRegionEnter { .. })
                    })
            })
            .map(|function| function.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(marked, ["Nested"]);
    }

    #[test]
    fn recursive_call_while_a_root_is_live_prevents_selection() {
        let module = compile(
            r"
                internal int[] Make(int value) { return [value]; }
                internal int Recurse(int depth) {
                    int total = 0;
                    for (int i = 0; i < 2; i++) {
                        int[] value = Make(i);
                        if (depth > 0) { total += Recurse(depth - 1); }
                        total += value[0];
                    }
                    return total;
                }
                public int Run() { return Recurse(2); }
            ",
        );

        assert_eq!(marker_count(&module), (0, 0));
    }

    #[test]
    fn unrelated_persistent_allocation_after_checkpoint_falls_back() {
        let module = compile(
            r"
                internal int[] Make(int value) { return [value]; }
                public int Run() {
                    int total = 0;
                    int[] survivor = [0];
                    for (int i = 0; i < 10; i++) {
                        int[] owned = Make(i);
                        survivor = Make(i + 1);
                        total += owned[0];
                    }
                    return total + survivor[0];
                }
            ",
        );

        assert_eq!(marker_count(&module), (0, 0));
    }
}
