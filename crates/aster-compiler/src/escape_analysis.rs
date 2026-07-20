//! Conservative, intraprocedural escape classification for MIR object allocations.
//!
//! This pass deliberately does not change [`mir::AllocationRegion`]. It records
//! only what can be learned inside one function. Calls other than the mandatory
//! constructor receiver remain conservative until function summaries exist.

use std::collections::HashSet;

use aster_mir as mir;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EscapeClassification {
    /// No use visible in this function forces the object to outlive the call.
    LocalCandidate,
    /// The allocation must remain persistent under the current local analysis.
    Persistent(EscapeReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EscapeReason {
    NonLocalDestination,
    Returned,
    Stored,
    Contained,
    InterfaceConversion,
    PassedToCall,
    PassedToInterfaceCall,
    PassedToIntrinsic,
    UnsupportedUse,
}

/// Run the dormant local analysis for every MIR function.
///
/// Results are intentionally not applied to MIR yet. The pass still runs in
/// normal compilation so later stages can consume the same implementation
/// rather than introducing a test-only prototype.
pub(super) fn analyze(module: &mir::Module) {
    let constructors = constructor_symbols(module);
    let mut local_candidates = 0_usize;
    let mut persistent = 0_usize;

    for function in &module.functions {
        for classification in classify_function(function, &constructors) {
            match classification {
                EscapeClassification::LocalCandidate => local_candidates += 1,
                EscapeClassification::Persistent(_) => persistent += 1,
            }
        }
    }

    debug_assert_eq!(
        local_candidates + persistent,
        object_allocation_count(module),
        "every MIR object allocation must receive one local escape classification"
    );
}

fn constructor_symbols(module: &mir::Module) -> HashSet<mir::SymbolId> {
    module
        .functions
        .iter()
        .filter(|function| function.constructor)
        .map(|function| function.symbol)
        .collect()
}

fn object_allocation_count(module: &mir::Module) -> usize {
    module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .filter(|instruction| matches!(instruction, mir::Instruction::AllocateObject { .. }))
        .count()
}

fn classify_function(
    function: &mir::Function,
    constructors: &HashSet<mir::SymbolId>,
) -> Vec<EscapeClassification> {
    let mut classifications = Vec::new();

    for block in &function.blocks {
        for instruction in &block.instructions {
            let mir::Instruction::AllocateObject { destination, .. } = instruction else {
                continue;
            };
            let mir::Place::Local(origin) = destination else {
                classifications.push(EscapeClassification::Persistent(
                    EscapeReason::NonLocalDestination,
                ));
                continue;
            };
            classifications.push(classify_allocation(function, *origin, constructors));
        }
    }

    classifications
}

fn classify_allocation(
    function: &mir::Function,
    origin: mir::LocalId,
    constructors: &HashSet<mir::SymbolId>,
) -> EscapeClassification {
    let aliases = collect_aliases(function, origin);

    for block in &function.blocks {
        for instruction in &block.instructions {
            if let Some(reason) = instruction_escape(instruction, &aliases, constructors) {
                return EscapeClassification::Persistent(reason);
            }
        }
        if let Some(reason) = terminator_escape(&block.terminator, &aliases) {
            return EscapeClassification::Persistent(reason);
        }
    }

    EscapeClassification::LocalCandidate
}

fn collect_aliases(function: &mir::Function, origin: mir::LocalId) -> HashSet<mir::LocalId> {
    let mut aliases = HashSet::from([origin]);

    loop {
        let mut changed = false;
        for instruction in function.blocks.iter().flat_map(|block| &block.instructions) {
            let mir::Instruction::Assign {
                target: mir::Place::Local(destination),
                value,
            } = instruction
            else {
                continue;
            };
            if alias_source(value, &aliases).is_some() {
                changed |= aliases.insert(*destination);
            }
        }
        if !changed {
            return aliases;
        }
    }
}

fn alias_source(value: &mir::Rvalue, aliases: &HashSet<mir::LocalId>) -> Option<mir::LocalId> {
    match &value.kind {
        mir::RvalueKind::Use(operand) | mir::RvalueKind::Cast(operand)
            if matches!(&value.type_, mir::Type::Class(_)) =>
        {
            direct_alias(operand, aliases)
        }
        _ => None,
    }
}

fn instruction_escape(
    instruction: &mir::Instruction,
    aliases: &HashSet<mir::LocalId>,
    constructors: &HashSet<mir::SymbolId>,
) -> Option<EscapeReason> {
    match instruction {
        mir::Instruction::Assign { target, value } => assignment_escape(target, value, aliases),
        mir::Instruction::Call {
            function,
            arguments,
            ..
        } => {
            for (index, argument) in arguments.iter().enumerate() {
                if direct_alias(argument, aliases).is_some()
                    && !(index == 0 && constructors.contains(function))
                {
                    return Some(EscapeReason::PassedToCall);
                }
            }
            None
        }
        mir::Instruction::CallInterface {
            receiver,
            arguments,
            ..
        } => {
            if direct_alias(receiver, aliases).is_some()
                || arguments
                    .iter()
                    .any(|argument| direct_alias(argument, aliases).is_some())
            {
                Some(EscapeReason::PassedToInterfaceCall)
            } else {
                None
            }
        }
        mir::Instruction::CallIntrinsic { arguments, .. } => arguments
            .iter()
            .any(|argument| direct_alias(argument, aliases).is_some())
            .then_some(EscapeReason::PassedToIntrinsic),
        mir::Instruction::AllocateArray { .. } | mir::Instruction::AllocateObject { .. } => None,
    }
}

fn assignment_escape(
    target: &mir::Place,
    value: &mir::Rvalue,
    aliases: &HashSet<mir::LocalId>,
) -> Option<EscapeReason> {
    match &value.kind {
        mir::RvalueKind::Use(operand) if direct_alias(operand, aliases).is_some() => {
            if matches!(target, mir::Place::Local(_)) {
                None
            } else {
                Some(EscapeReason::Stored)
            }
        }
        mir::RvalueKind::Cast(operand) if direct_alias(operand, aliases).is_some() => {
            if matches!(target, mir::Place::Local(_)) && matches!(&value.type_, mir::Type::Class(_))
            {
                None
            } else {
                Some(EscapeReason::UnsupportedUse)
            }
        }
        mir::RvalueKind::MakeInterface { object, .. }
            if direct_alias(object, aliases).is_some() =>
        {
            Some(EscapeReason::InterfaceConversion)
        }
        mir::RvalueKind::Aggregate(fields) | mir::RvalueKind::EnumConstruct { fields, .. }
            if fields
                .iter()
                .any(|field| direct_alias(&field.value, aliases).is_some()) =>
        {
            Some(EscapeReason::Contained)
        }
        mir::RvalueKind::Equality { .. } => None,
        _ if rvalue_uses_alias(value, aliases) => Some(EscapeReason::UnsupportedUse),
        _ => None,
    }
}

fn rvalue_uses_alias(value: &mir::Rvalue, aliases: &HashSet<mir::LocalId>) -> bool {
    match &value.kind {
        mir::RvalueKind::Use(operand)
        | mir::RvalueKind::Discriminant(operand)
        | mir::RvalueKind::ArrayLength(operand)
        | mir::RvalueKind::Cast(operand)
        | mir::RvalueKind::Unary { operand, .. } => direct_alias(operand, aliases).is_some(),
        mir::RvalueKind::Aggregate(fields) | mir::RvalueKind::EnumConstruct { fields, .. } => {
            fields
                .iter()
                .any(|field| direct_alias(&field.value, aliases).is_some())
        }
        mir::RvalueKind::MakeInterface { object, .. } => direct_alias(object, aliases).is_some(),
        mir::RvalueKind::Binary { left, right, .. }
        | mir::RvalueKind::Equality { left, right, .. } => {
            direct_alias(left, aliases).is_some() || direct_alias(right, aliases).is_some()
        }
    }
}

fn terminator_escape(
    terminator: &mir::Terminator,
    aliases: &HashSet<mir::LocalId>,
) -> Option<EscapeReason> {
    match terminator {
        mir::Terminator::Return(Some(value)) if direct_alias(value, aliases).is_some() => {
            Some(EscapeReason::Returned)
        }
        mir::Terminator::Goto(_)
        | mir::Terminator::Branch { .. }
        | mir::Terminator::Return(_)
        | mir::Terminator::End
        | mir::Terminator::Unreachable => None,
    }
}

fn direct_alias(operand: &mir::Operand, aliases: &HashSet<mir::LocalId>) -> Option<mir::LocalId> {
    let mir::OperandKind::Copy(mir::Place::Local(local)) = &operand.kind else {
        return None;
    };
    aliases.contains(local).then_some(*local)
}

#[cfg(test)]
mod tests {
    use super::{EscapeClassification, EscapeReason, classify_function, constructor_symbols};
    use crate::{compile, mir};

    fn classifications(source: &str, function_name: &str) -> Vec<EscapeClassification> {
        let compilation = compile(source)
            .unwrap_or_else(|diagnostics| panic!("invalid test source: {diagnostics:#?}"));
        let constructors = constructor_symbols(&compilation.mir);
        let function = compilation
            .mir
            .functions
            .iter()
            .find(|function| function.name == function_name && function.owner.is_none())
            .unwrap_or_else(|| panic!("missing MIR function `{function_name}`"));
        classify_function(function, &constructors)
    }

    #[test]
    fn constructor_receiver_and_local_aliases_remain_a_local_candidate() {
        let source = "public class Box { public Box() {} } public int Run() { Box first = new Box(); Box second = first; return 1; }";
        assert_eq!(
            classifications(source, "Run"),
            vec![EscapeClassification::LocalCandidate]
        );
    }

    #[test]
    fn returning_an_alias_requires_persistent_storage() {
        let source = "public class Box { public Box() {} } public Box Create() { Box first = new Box(); Box second = first; return second; }";
        assert_eq!(
            classifications(source, "Create"),
            vec![EscapeClassification::Persistent(EscapeReason::Returned)]
        );
    }

    #[test]
    fn storing_an_object_reference_requires_persistent_storage() {
        let source = "public class Box { public Box() {} } public class Holder { public Box value; public Holder(Box initial) { value = initial; } } public int Run() { Holder holder = new Holder(new Box()); Box box = new Box(); holder.value = box; return 0; }";
        let classifications = classifications(source, "Run");
        assert!(
            classifications.contains(&EscapeClassification::Persistent(EscapeReason::Stored)),
            "{classifications:#?}"
        );
    }

    #[test]
    fn passing_an_object_to_a_free_function_is_conservative() {
        let source = "public class Box { public Box() {} } public void Consume(Box value) {} public int Run() { Box box = new Box(); Consume(box); return 0; }";
        assert_eq!(
            classifications(source, "Run"),
            vec![EscapeClassification::Persistent(EscapeReason::PassedToCall)]
        );
    }

    #[test]
    fn ordinary_method_receivers_wait_for_function_summaries() {
        let source = "public class Box { public Box() {} public int Get() { return 1; } } public int Run() { Box box = new Box(); return box.Get(); }";
        assert_eq!(
            classifications(source, "Run"),
            vec![EscapeClassification::Persistent(EscapeReason::PassedToCall)]
        );
    }

    #[test]
    fn arrays_are_outside_the_first_local_escape_analysis() {
        let source = "public int Run() { int[] values = [1, 2, 3]; return values.Length; }";
        assert!(classifications(source, "Run").is_empty());
    }

    #[test]
    fn actual_mir_regions_remain_persistent() {
        let source = "public class Box { public Box() {} } public int Run() { Box box = new Box(); return 1; }";
        let compilation = compile(source).expect("valid source");
        assert!(compilation.mir.functions.iter().all(|function| {
            function.blocks.iter().all(|block| {
                block.instructions.iter().all(|instruction| {
                    !matches!(
                        instruction,
                        mir::Instruction::AllocateObject {
                            region: mir::AllocationRegion::Temporary,
                            ..
                        }
                    )
                })
            })
        }));
    }
}
