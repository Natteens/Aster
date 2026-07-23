//! Conservative escape classification and region selection for MIR allocations.
//!
//! The pass first computes interprocedural summaries for direct Aster calls.
//! Summaries are solved over the direct-call graph by strongly connected
//! components and a monotone fixpoint, so direct and mutual recursion do not
//! depend on declaration order.
//!
//! Objects, arrays, and dynamic strings proven local to their containing
//! function are rewritten to [`mir::AllocationRegion::Temporary`]. Every
//! uncertain or escaping allocation remains persistent.

use std::collections::{HashMap, HashSet};

use aster_mir as mir;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ParameterSummary {
    /// The callee can store this parameter or pass it to an unknown/escaping use.
    escapes: bool,
    /// The callee can return this parameter, or one of its local aliases.
    returned: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FunctionSummary {
    parameters: Vec<ParameterSummary>,
}

impl FunctionSummary {
    fn for_function(function: &mir::Function) -> Self {
        Self {
            parameters: vec![ParameterSummary::default(); function.parameters.len()],
        }
    }

    fn merge(&mut self, newer: &Self) -> bool {
        debug_assert_eq!(self.parameters.len(), newer.parameters.len());
        let mut changed = false;
        for (current, update) in self.parameters.iter_mut().zip(&newer.parameters) {
            let merged = ParameterSummary {
                escapes: current.escapes || update.escapes,
                returned: current.returned || update.returned,
            };
            changed |= *current != merged;
            *current = merged;
        }
        changed
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EscapeClassification {
    /// No use visible through local code or known summaries forces the object
    /// to outlive the containing function.
    LocalCandidate,
    /// The allocation must remain persistent under the current analysis.
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

/// Classify every MIR allocation and apply its storage region.
///
/// This is deliberately conservative: only [`EscapeClassification::LocalCandidate`]
/// becomes temporary. Any escape reason keeps the original persistent lifetime.
pub(super) fn assign_allocation_regions(module: &mut mir::Module) {
    let summaries = build_function_summaries(module);
    let classifications = module
        .functions
        .iter()
        .map(|function| classify_function(function, &summaries))
        .collect::<Vec<_>>();

    let mut local_candidates = 0_usize;
    let mut persistent = 0_usize;

    for (function, function_classifications) in module.functions.iter_mut().zip(classifications) {
        let mut function_classifications = function_classifications.into_iter();

        for instruction in function
            .blocks
            .iter_mut()
            .flat_map(|block| &mut block.instructions)
        {
            if !is_dynamic_allocation(instruction) {
                continue;
            }
            let classification = function_classifications
                .next()
                .expect("every MIR allocation has an escape classification");
            let region = match classification {
                EscapeClassification::LocalCandidate => {
                    local_candidates += 1;
                    mir::AllocationRegion::Temporary
                }
                EscapeClassification::Persistent(_) => {
                    persistent += 1;
                    mir::AllocationRegion::Persistent
                }
            };
            set_allocation_region(instruction, region);
        }

        debug_assert!(
            function_classifications.next().is_none(),
            "escape analysis produced more classifications than allocations"
        );
    }

    debug_assert_eq!(
        local_candidates + persistent,
        dynamic_allocation_count(module),
        "every MIR allocation must receive one escape classification"
    );
}

fn build_function_summaries(module: &mir::Module) -> HashMap<mir::SymbolId, FunctionSummary> {
    let indices = module
        .functions
        .iter()
        .enumerate()
        .map(|(index, function)| (function.symbol, index))
        .collect::<HashMap<_, _>>();
    let graph = direct_call_graph(module, &indices);
    let components = strongly_connected_components(&graph);
    let mut summaries = module
        .functions
        .iter()
        .map(|function| (function.symbol, FunctionSummary::for_function(function)))
        .collect::<HashMap<_, _>>();

    // Kosaraju returns caller components before their direct callees for our
    // caller -> callee graph. Reverse that order so external dependencies are
    // stable before solving each recursive component.
    for component in components.iter().rev() {
        loop {
            let mut changed = false;
            for &function_index in component {
                let function = &module.functions[function_index];
                let update = summarize_function(function, &summaries);
                changed |= summaries
                    .get_mut(&function.symbol)
                    .expect("every MIR function has an initialized summary")
                    .merge(&update);
            }
            if !changed {
                break;
            }
        }
    }

    summaries
}

fn direct_call_graph(
    module: &mir::Module,
    indices: &HashMap<mir::SymbolId, usize>,
) -> Vec<Vec<usize>> {
    let mut graph = vec![Vec::new(); module.functions.len()];

    for (caller_index, function) in module.functions.iter().enumerate() {
        for instruction in function.blocks.iter().flat_map(|block| &block.instructions) {
            let mir::Instruction::Call {
                function: callee, ..
            } = instruction
            else {
                continue;
            };
            if let Some(&callee_index) = indices.get(callee) {
                graph[caller_index].push(callee_index);
            }
        }
        graph[caller_index].sort_unstable();
        graph[caller_index].dedup();
    }

    graph
}

fn strongly_connected_components(graph: &[Vec<usize>]) -> Vec<Vec<usize>> {
    fn visit(node: usize, graph: &[Vec<usize>], visited: &mut [bool], order: &mut Vec<usize>) {
        if visited[node] {
            return;
        }
        visited[node] = true;
        for &next in &graph[node] {
            visit(next, graph, visited, order);
        }
        order.push(node);
    }

    fn collect(
        node: usize,
        reverse: &[Vec<usize>],
        visited: &mut [bool],
        component: &mut Vec<usize>,
    ) {
        if visited[node] {
            return;
        }
        visited[node] = true;
        component.push(node);
        for &next in &reverse[node] {
            collect(next, reverse, visited, component);
        }
    }

    let mut visited = vec![false; graph.len()];
    let mut order = Vec::with_capacity(graph.len());
    for node in 0..graph.len() {
        visit(node, graph, &mut visited, &mut order);
    }

    let mut reverse = vec![Vec::new(); graph.len()];
    for (caller, callees) in graph.iter().enumerate() {
        for &callee in callees {
            reverse[callee].push(caller);
        }
    }

    visited.fill(false);
    let mut components = Vec::new();
    for &node in order.iter().rev() {
        if visited[node] {
            continue;
        }
        let mut component = Vec::new();
        collect(node, &reverse, &mut visited, &mut component);
        component.sort_unstable();
        components.push(component);
    }
    components
}

fn summarize_function(
    function: &mir::Function,
    summaries: &HashMap<mir::SymbolId, FunctionSummary>,
) -> FunctionSummary {
    let mut summary = FunctionSummary::for_function(function);

    for (index, parameter) in function.parameters.iter().enumerate() {
        if !is_tracked_reference_type(&parameter.type_) {
            continue;
        }

        let aliases = collect_aliases(function, parameter.id, summaries);
        for block in &function.blocks {
            if block
                .instructions
                .iter()
                .any(|instruction| instruction_escape(instruction, &aliases, summaries).is_some())
            {
                summary.parameters[index].escapes = true;
            }
            if terminator_returns_alias(&block.terminator, &aliases) {
                summary.parameters[index].returned = true;
            }
        }
    }

    summary
}

fn is_tracked_reference_type(type_: &mir::Type) -> bool {
    matches!(
        type_,
        mir::Type::Class(_)
            | mir::Type::Array(_)
            | mir::Type::String
            | mir::Type::List(_)
            // An enum (e.g. `Option<string>`) is a value type, but plain
            // copies of it (`switch`'s scrutinee binding, an intermediate
            // temporary) must still propagate an alias of a tracked origin
            // through to wherever its payload is later read out via
            // `EnumField` (see `direct_alias`), or a reference-typed payload
            // (only possible since `aster.io.ReadLine`'s `Option<string>`;
            // every prior `Option`/`Result` payload was a scalar) looks
            // non-escaping even when the enum obviously escapes.
            | mir::Type::Enum(_)
    )
}

fn is_dynamic_allocation(instruction: &mir::Instruction) -> bool {
    matches!(
        instruction,
        mir::Instruction::AllocateObject { .. }
            | mir::Instruction::AllocateArray { .. }
            | mir::Instruction::AllocateList { .. }
    ) || matches!(
        instruction,
        mir::Instruction::CallIntrinsic { intrinsic, .. }
            if intrinsic.allocation_region().is_some()
    )
}

fn allocation_destination(instruction: &mir::Instruction) -> Option<&mir::Place> {
    match instruction {
        mir::Instruction::AllocateObject { destination, .. }
        | mir::Instruction::AllocateArray { destination, .. }
        | mir::Instruction::AllocateList { destination, .. } => Some(destination),
        mir::Instruction::CallIntrinsic {
            destination,
            intrinsic,
            ..
        } if intrinsic.allocation_region().is_some() => destination.as_ref(),
        _ => None,
    }
}

fn set_allocation_region(instruction: &mut mir::Instruction, region: mir::AllocationRegion) {
    match instruction {
        mir::Instruction::AllocateObject {
            region: current, ..
        }
        | mir::Instruction::AllocateArray {
            region: current, ..
        }
        | mir::Instruction::AllocateList {
            region: current, ..
        } => *current = region,
        mir::Instruction::CallIntrinsic { intrinsic, .. }
            if intrinsic.allocation_region().is_some() =>
        {
            *intrinsic = intrinsic.with_allocation_region(region);
        }
        _ => unreachable!("only dynamic allocations receive storage regions"),
    }
}

fn dynamic_allocation_count(module: &mir::Module) -> usize {
    module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .filter(|instruction| is_dynamic_allocation(instruction))
        .count()
}

fn classify_function(
    function: &mir::Function,
    summaries: &HashMap<mir::SymbolId, FunctionSummary>,
) -> Vec<EscapeClassification> {
    let mut classifications = Vec::new();

    for block in &function.blocks {
        for instruction in &block.instructions {
            if !is_dynamic_allocation(instruction) {
                continue;
            }
            let Some(mir::Place::Local(origin)) = allocation_destination(instruction) else {
                classifications.push(EscapeClassification::Persistent(
                    EscapeReason::NonLocalDestination,
                ));
                continue;
            };
            classifications.push(classify_allocation(function, *origin, summaries));
        }
    }

    classifications
}

fn classify_allocation(
    function: &mir::Function,
    origin: mir::LocalId,
    summaries: &HashMap<mir::SymbolId, FunctionSummary>,
) -> EscapeClassification {
    let aliases = collect_aliases(function, origin, summaries);

    for block in &function.blocks {
        for instruction in &block.instructions {
            if let Some(reason) = instruction_escape(instruction, &aliases, summaries) {
                return EscapeClassification::Persistent(reason);
            }
        }
        if terminator_returns_alias(&block.terminator, &aliases) {
            return EscapeClassification::Persistent(EscapeReason::Returned);
        }
    }

    EscapeClassification::LocalCandidate
}

fn collect_aliases(
    function: &mir::Function,
    origin: mir::LocalId,
    summaries: &HashMap<mir::SymbolId, FunctionSummary>,
) -> HashSet<mir::LocalId> {
    let mut aliases = HashSet::from([origin]);

    loop {
        let mut changed = false;
        for instruction in function.blocks.iter().flat_map(|block| &block.instructions) {
            match instruction {
                mir::Instruction::Assign {
                    target: mir::Place::Local(destination),
                    value,
                } if alias_source(value, &aliases).is_some() => {
                    changed |= aliases.insert(*destination);
                }
                mir::Instruction::Call {
                    destination: Some(mir::Place::Local(destination)),
                    function,
                    arguments,
                    ..
                } if call_returns_alias(*function, arguments, &aliases, summaries) => {
                    changed |= aliases.insert(*destination);
                }
                _ => {}
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
            if is_tracked_reference_type(&value.type_) =>
        {
            direct_alias(operand, aliases)
        }
        _ => None,
    }
}

fn call_returns_alias(
    function: mir::SymbolId,
    arguments: &[mir::Operand],
    aliases: &HashSet<mir::LocalId>,
    summaries: &HashMap<mir::SymbolId, FunctionSummary>,
) -> bool {
    let Some(summary) = summaries.get(&function) else {
        return false;
    };

    arguments.iter().enumerate().any(|(index, argument)| {
        direct_alias(argument, aliases).is_some()
            && summary
                .parameters
                .get(index)
                .is_some_and(|parameter| parameter.returned)
    })
}

#[allow(clippy::too_many_lines)]
fn instruction_escape(
    instruction: &mir::Instruction,
    aliases: &HashSet<mir::LocalId>,
    summaries: &HashMap<mir::SymbolId, FunctionSummary>,
) -> Option<EscapeReason> {
    match instruction {
        mir::Instruction::Assign { target, value } => assignment_escape(target, value, aliases),
        mir::Instruction::Call {
            destination,
            function,
            arguments,
            ..
        } => {
            let Some(summary) = summaries.get(function) else {
                return arguments
                    .iter()
                    .any(|argument| direct_alias(argument, aliases).is_some())
                    .then_some(EscapeReason::PassedToCall);
            };

            if arguments.iter().enumerate().any(|(index, argument)| {
                direct_alias(argument, aliases).is_some()
                    && summary
                        .parameters
                        .get(index)
                        .is_none_or(|parameter| parameter.escapes)
            }) {
                return Some(EscapeReason::PassedToCall);
            }

            if destination
                .as_ref()
                .is_some_and(|place| !matches!(place, mir::Place::Local(_)))
                && call_returns_alias(*function, arguments, aliases, summaries)
            {
                return Some(EscapeReason::Stored);
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
        mir::Instruction::CallIntrinsic {
            intrinsic,
            arguments,
            ..
        } => {
            let borrows_only = matches!(
                *intrinsic,
                mir::Intrinsic::Log
                    | mir::Intrinsic::LogWarning
                    | mir::Intrinsic::LogError
                    | mir::Intrinsic::StringEquals
                    | mir::Intrinsic::StringContains
                    | mir::Intrinsic::StringStartsWith
                    | mir::Intrinsic::StringEndsWith
                    | mir::Intrinsic::StringIndexOf
                    | mir::Intrinsic::StringConcat
                    | mir::Intrinsic::StringConcatTemporary
                    | mir::Intrinsic::StringLength
                    | mir::Intrinsic::StringFromLong
                    | mir::Intrinsic::StringFromLongTemporary
                    | mir::Intrinsic::StringFromULong
                    | mir::Intrinsic::StringFromULongTemporary
                    | mir::Intrinsic::StringFromDouble
                    | mir::Intrinsic::StringFromDoubleTemporary
                    | mir::Intrinsic::StringFromFloat
                    | mir::Intrinsic::StringFromFloatTemporary
                    | mir::Intrinsic::StringFromBool
                    | mir::Intrinsic::StringFromBoolTemporary
                    | mir::Intrinsic::StringFromChar
                    | mir::Intrinsic::StringFromCharTemporary
                    | mir::Intrinsic::StringJoin
                    | mir::Intrinsic::StringJoinTemporary
                    | mir::Intrinsic::StringSubstringFrom
                    | mir::Intrinsic::StringSubstringFromTemporary
                    | mir::Intrinsic::StringSubstringRange
                    | mir::Intrinsic::StringSubstringRangeTemporary
                    | mir::Intrinsic::StringTryParseBool
                    | mir::Intrinsic::StringTryParseInt
                    | mir::Intrinsic::StringTryParseUInt
                    | mir::Intrinsic::StringTryParseLong
                    | mir::Intrinsic::StringTryParseULong
                    | mir::Intrinsic::StringTryParseFloat
                    | mir::Intrinsic::StringTryParseDouble
                    | mir::Intrinsic::ConsoleWrite
                    | mir::Intrinsic::ConsoleWriteLine
                    | mir::Intrinsic::FileReadAllText(_)
                    | mir::Intrinsic::FileReadAllTextTemporary(_)
                    | mir::Intrinsic::FileWriteAllText(_)
                    | mir::Intrinsic::FileListFiles(_)
                    | mir::Intrinsic::FileListFilesTemporary(_)
                    | mir::Intrinsic::ReportRuntimeError(_)
            );
            (!borrows_only
                && arguments
                    .iter()
                    .any(|argument| direct_alias(argument, aliases).is_some()))
            .then_some(EscapeReason::PassedToIntrinsic)
        }
        // Reading/allocating never escapes the receiver on its own. A
        // reference value returned by `Get` needs no rule of its own here:
        // it is only ever a copy of something already forced `Persistent`
        // when it was `Add`ed (or, for a reference embedded in a struct/
        // enum, when that struct/enum literal was constructed — see
        // `assignment_escape`'s `Contained` case) — never a fresh
        // allocation, and never owned by the list's slot.
        // `RemoveAt` rearranges elements already inside the buffer (each
        // already classified when it was `Add`ed) and introduces no new
        // operand that could alias anything — a purely local mutation of
        // the list's own storage, neutral to every allocation's lifetime.
        mir::Instruction::AllocateArray { .. }
        | mir::Instruction::AllocateObject { .. }
        | mir::Instruction::AllocateList { .. }
        | mir::Instruction::ListGet { .. }
        | mir::Instruction::ListRemoveAt { .. } => None,
        // Storing a reference into a list's buffer is an aggregate store,
        // exactly like `Assign{target: Field/Index, value: Use(alias)}`
        // (`assignment_escape` above): conservatively persistent, regardless
        // of the list's own classification. Only the *value* operand can
        // escape here; `list` itself never does merely by receiving an
        // `Add` (mirrors `ArrayLength`/`ListLength` not forcing their own
        // receiver to escape).
        mir::Instruction::ListAdd { value, .. } => direct_alias(value, aliases)
            .is_some()
            .then_some(EscapeReason::Stored),
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
            if matches!(target, mir::Place::Local(_)) && is_tracked_reference_type(&value.type_) {
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
        mir::RvalueKind::ArrayLength(operand) if direct_alias(operand, aliases).is_some() => None,
        mir::RvalueKind::ListLength(operand) if direct_alias(operand, aliases).is_some() => None,
        mir::RvalueKind::ListVersion(operand) if direct_alias(operand, aliases).is_some() => None,
        // Reading an aliased enum's tag (how `switch`/`?` choose a case)
        // only inspects the discriminant, never the payload -- safe on its
        // own regardless of whether the enum escapes. Never observed before
        // an enum type was tracked at all (see `is_tracked_reference_type`).
        mir::RvalueKind::Discriminant(operand) if direct_alias(operand, aliases).is_some() => None,
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
        | mir::RvalueKind::ListLength(operand)
        | mir::RvalueKind::ListVersion(operand)
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

fn terminator_returns_alias(terminator: &mir::Terminator, aliases: &HashSet<mir::LocalId>) -> bool {
    matches!(
        terminator,
        mir::Terminator::Return(Some(value)) if direct_alias(value, aliases).is_some()
    )
}

fn direct_alias(operand: &mir::Operand, aliases: &HashSet<mir::LocalId>) -> Option<mir::LocalId> {
    match &operand.kind {
        mir::OperandKind::Copy(mir::Place::Local(local)) => {
            aliases.contains(local).then_some(*local)
        }
        // Reading a field out of an enum payload (how `switch`/postfix `?`
        // extract `Some(value)`) propagates the base's alias: if the enum
        // itself was tracked (e.g. `aster.io.ReadLine()`'s `Option<string>`),
        // a reference-typed payload copied out of it escapes exactly when
        // whatever receives it escapes, same as copying the base directly.
        // Every prior `Option<T>`/`Result<T,E>` payload was a scalar, where
        // `alias_source`'s `is_tracked_reference_type` gate already excluded
        // this path, so the gap was never observed before a reference-typed
        // payload existed.
        mir::OperandKind::Copy(mir::Place::EnumField { base, .. }) => {
            let mir::Place::Local(local) = base.as_ref() else {
                return None;
            };
            aliases.contains(local).then_some(*local)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{
        EscapeClassification, EscapeReason, assign_allocation_regions, build_function_summaries,
        classify_function, strongly_connected_components,
    };
    use crate::{compile, mir};

    fn compilation(source: &str) -> crate::Compilation {
        compile(source)
            .unwrap_or_else(|diagnostics| panic!("invalid test source: {diagnostics:#?}"))
    }

    fn classifications(source: &str, function_name: &str) -> Vec<EscapeClassification> {
        let compilation = compilation(source);
        let summaries = build_function_summaries(&compilation.mir);
        let function = compilation
            .mir
            .functions
            .iter()
            .find(|function| function.name == function_name && function.owner.is_none())
            .unwrap_or_else(|| panic!("missing MIR function `{function_name}`"));
        classify_function(function, &summaries)
    }

    fn object_regions(source: &str, function_name: &str) -> Vec<mir::AllocationRegion> {
        let compilation = compilation(source);
        let function = compilation
            .mir
            .functions
            .iter()
            .find(|function| function.name == function_name && function.owner.is_none())
            .unwrap_or_else(|| panic!("missing MIR function `{function_name}`"));

        function
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .filter_map(|instruction| {
                let mir::Instruction::AllocateObject { region, .. } = instruction else {
                    return None;
                };
                Some(*region)
            })
            .collect()
    }

    fn array_regions(source: &str, function_name: &str) -> Vec<mir::AllocationRegion> {
        let compilation = compilation(source);
        let function = compilation
            .mir
            .functions
            .iter()
            .find(|function| function.name == function_name && function.owner.is_none())
            .unwrap_or_else(|| panic!("missing MIR function `{function_name}`"));

        function
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .filter_map(|instruction| {
                let mir::Instruction::AllocateArray { region, .. } = instruction else {
                    return None;
                };
                Some(*region)
            })
            .collect()
    }

    fn string_regions(source: &str, function_name: &str) -> Vec<mir::AllocationRegion> {
        let compilation = compilation(source);
        let function = compilation
            .mir
            .functions
            .iter()
            .find(|function| function.name == function_name && function.owner.is_none())
            .unwrap_or_else(|| panic!("missing MIR function `{function_name}`"));

        function
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .filter_map(|instruction| {
                let mir::Instruction::CallIntrinsic { intrinsic, .. } = instruction else {
                    return None;
                };
                intrinsic.string_allocation_region()
            })
            .collect()
    }

    fn list_regions(source: &str, function_name: &str) -> Vec<mir::AllocationRegion> {
        let compilation = compilation(source);
        let function = compilation
            .mir
            .functions
            .iter()
            .find(|function| function.name == function_name && function.owner.is_none())
            .unwrap_or_else(|| panic!("missing MIR function `{function_name}`"));

        function
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .filter_map(|instruction| {
                let mir::Instruction::AllocateList { region, .. } = instruction else {
                    return None;
                };
                Some(*region)
            })
            .collect()
    }

    fn summaries(
        source: &str,
    ) -> (
        crate::Compilation,
        HashMap<mir::SymbolId, super::FunctionSummary>,
    ) {
        let compilation = compilation(source);
        let summaries = build_function_summaries(&compilation.mir);
        (compilation, summaries)
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
    fn safe_free_function_parameter_no_longer_forces_an_escape() {
        let source = "public class Box { public Box() {} } public void Observe(Box value) {} public int Run() { Box box = new Box(); Observe(box); return 0; }";
        assert_eq!(
            classifications(source, "Run"),
            vec![EscapeClassification::LocalCandidate]
        );
    }

    #[test]
    fn transitive_escape_through_known_calls_is_propagated() {
        let source = "public class Box { public Box() {} } public class Holder { public Box value; public Holder(Box initial) { value = initial; } } public void Store(Holder holder, Box value) { holder.value = value; } public void Forward(Holder holder, Box value) { Store(holder, value); } public int Run() { Holder holder = new Holder(new Box()); Box box = new Box(); Forward(holder, box); return 0; }";
        let classifications = classifications(source, "Run");
        assert!(
            classifications.contains(&EscapeClassification::Persistent(
                EscapeReason::PassedToCall
            )),
            "{classifications:#?}"
        );
    }

    #[test]
    fn returned_parameter_alias_is_tracked_in_the_caller() {
        let source = "public class Box { public Box() {} } public Box Identity(Box value) { return value; } public Box Run() { Box box = new Box(); Box alias = Identity(box); return alias; }";
        assert_eq!(
            classifications(source, "Run"),
            vec![EscapeClassification::Persistent(EscapeReason::Returned)]
        );
    }

    #[test]
    fn ignored_returned_alias_can_remain_local() {
        let source = "public class Box { public Box() {} } public Box Identity(Box value) { return value; } public int Run() { Box box = new Box(); Box alias = Identity(box); return 0; }";
        assert_eq!(
            classifications(source, "Run"),
            vec![EscapeClassification::LocalCandidate]
        );
    }

    #[test]
    fn ordinary_non_escaping_method_receiver_is_local() {
        let source = "public class Box { public Box() {} public int Get() { return 1; } } public int Run() { Box box = new Box(); return box.Get(); }";
        assert_eq!(
            classifications(source, "Run"),
            vec![EscapeClassification::LocalCandidate]
        );
    }

    #[test]
    fn monomorphized_generic_summary_preserves_return_aliases() {
        let source = "public class Box { public Box() {} } public T Identity<T>(T value) { return value; } public int Run() { Box box = new Box(); Box alias = Identity(box); return 0; }";
        assert_eq!(
            classifications(source, "Run"),
            vec![EscapeClassification::LocalCandidate]
        );
    }

    #[test]
    fn interface_conversion_remains_conservative() {
        let source = "public interface IBox { int Get(); } public class Box : IBox { public Box() {} public int Get() { return 1; } } public int Run() { Box box = new Box(); IBox view = box; return view.Get(); }";
        assert_eq!(
            classifications(source, "Run"),
            vec![EscapeClassification::Persistent(
                EscapeReason::InterfaceConversion
            )]
        );
    }

    #[test]
    fn recursive_non_escaping_call_reaches_a_fixpoint() {
        let source = "public class Box { public Box() {} } public void Visit(Box value, int depth) { if (depth > 0) { Visit(value, depth - 1); } } public int Run() { Box box = new Box(); Visit(box, 2); return 0; }";
        assert_eq!(
            classifications(source, "Run"),
            vec![EscapeClassification::LocalCandidate]
        );
    }

    #[test]
    fn mutual_recursion_propagates_an_escape_inside_the_component() {
        let source = "public class Box { public Box() {} } public class Holder { public Box value; public Holder(Box initial) { value = initial; } } public void First(Holder holder, Box value, bool stop) { if (stop) { holder.value = value; } else { Second(holder, value, true); } } public void Second(Holder holder, Box value, bool stop) { if (stop) { First(holder, value, true); } else { holder.value = value; } } public int Run() { Holder holder = new Holder(new Box()); Box box = new Box(); First(holder, box, false); return 0; }";
        let classifications = classifications(source, "Run");
        assert!(
            classifications.contains(&EscapeClassification::Persistent(
                EscapeReason::PassedToCall
            )),
            "{classifications:#?}"
        );
    }

    #[test]
    fn constructor_summary_tracks_stored_arguments() {
        let source = "public class Box { public Box() {} } public class Holder { private Box value; public Holder(Box initial) { value = initial; } } public int Run() { Box box = new Box(); Holder holder = new Holder(box); return 0; }";
        let classifications = classifications(source, "Run");
        assert!(
            classifications.contains(&EscapeClassification::Persistent(
                EscapeReason::PassedToCall
            )),
            "{classifications:#?}"
        );
    }

    #[test]
    fn summaries_distinguish_returned_from_other_escape() {
        let source = "public class Box { public Box() {} } public Box Identity(Box value) { return value; } public void Consume(Box value) { Box alias = value; }";
        let (compilation, summaries) = summaries(source);
        let identity = compilation
            .mir
            .functions
            .iter()
            .find(|function| function.name == "Identity")
            .expect("Identity MIR function");
        let consume = compilation
            .mir
            .functions
            .iter()
            .find(|function| function.name == "Consume")
            .expect("Consume MIR function");

        assert_eq!(
            summaries[&identity.symbol].parameters[0],
            super::ParameterSummary {
                escapes: false,
                returned: true,
            }
        );
        assert_eq!(
            summaries[&consume.symbol].parameters[0],
            super::ParameterSummary::default()
        );
    }

    #[test]
    fn finds_direct_and_mutual_recursive_components() {
        let graph = vec![vec![0], vec![2], vec![1], vec![2]];
        let components = strongly_connected_components(&graph);
        assert!(components.contains(&vec![0]));
        assert!(components.contains(&vec![1, 2]));
        assert!(components.contains(&vec![3]));
    }

    #[test]
    fn local_array_is_a_local_candidate() {
        let source = "public int Run() { int[] values = [1, 2, 3]; return values.Length; }";
        assert_eq!(
            classifications(source, "Run"),
            vec![EscapeClassification::LocalCandidate]
        );
    }

    #[test]
    fn returned_array_requires_persistent_storage() {
        let source = "public int[] Make() { return [1, 2, 3]; }";
        assert_eq!(
            classifications(source, "Make"),
            vec![EscapeClassification::Persistent(EscapeReason::Returned)]
        );
    }

    #[test]
    fn local_dynamic_string_is_a_local_candidate() {
        let source = r#"public int Run() { string left = "As"; string value = left + "ter"; return value.Length; }"#;
        assert_eq!(
            classifications(source, "Run"),
            vec![EscapeClassification::LocalCandidate]
        );
    }

    #[test]
    fn returned_dynamic_string_requires_persistent_storage() {
        let source = r#"public string Make() { string left = "As"; return left + "ter"; }"#;
        assert_eq!(
            classifications(source, "Make"),
            vec![EscapeClassification::Persistent(EscapeReason::Returned)]
        );
    }

    #[test]
    fn local_candidate_is_emitted_as_temporary() {
        let source = "public class Box { public Box() {} public int Get() { return 1; } } public int Run() { Box box = new Box(); return box.Get(); }";
        assert_eq!(
            object_regions(source, "Run"),
            vec![mir::AllocationRegion::Temporary]
        );
    }

    #[test]
    fn returned_object_is_emitted_as_persistent() {
        let source =
            "public class Box { public Box() {} } public Box Create() { return new Box(); }";
        assert_eq!(
            object_regions(source, "Create"),
            vec![mir::AllocationRegion::Persistent]
        );
    }

    #[test]
    fn local_array_is_emitted_as_temporary() {
        let source = "public int Run() { int[] values = [1, 2, 3]; return values.Length; }";
        assert_eq!(
            array_regions(source, "Run"),
            vec![mir::AllocationRegion::Temporary]
        );
    }

    #[test]
    fn returned_array_is_emitted_as_persistent() {
        let source = "public int[] Make() { return [1, 2, 3]; }";
        assert_eq!(
            array_regions(source, "Make"),
            vec![mir::AllocationRegion::Persistent]
        );
    }

    #[test]
    fn local_dynamic_string_is_emitted_as_temporary() {
        let source = r#"public int Run() { string left = "As"; string value = left + "ter"; return value.Length; }"#;
        assert_eq!(
            string_regions(source, "Run"),
            vec![mir::AllocationRegion::Temporary]
        );
    }

    #[test]
    fn returned_dynamic_string_is_emitted_as_persistent() {
        let source = r#"public string Make() { string left = "As"; return left + "ter"; }"#;
        assert_eq!(
            string_regions(source, "Make"),
            vec![mir::AllocationRegion::Persistent]
        );
    }

    // `List<T>` has no source syntax yet (no constructor, see `List A`), so
    // these build MIR by hand instead of compiling a program, exactly like
    // `layouts::layout_tests` already does for struct/class layout.

    fn list_local(id: u32, type_: mir::Type) -> mir::Local {
        mir::Local {
            id: mir::LocalId(id),
            symbol: None,
            name: format!("local{id}"),
            type_,
            mutable: true,
            temporary: false,
        }
    }

    fn single_function_module(
        return_type: mir::Type,
        locals: Vec<mir::Local>,
        instructions: Vec<mir::Instruction>,
        terminator: mir::Terminator,
    ) -> mir::Module {
        mir::Module {
            structs: Vec::new(),
            classes: Vec::new(),
            interfaces: Vec::new(),
            enums: Vec::new(),
            interface_implementations: Vec::new(),
            functions: vec![mir::Function {
                constructor: false,
                symbol: mir::SymbolId(1),
                owner: None,
                name: "Run".to_owned(),
                visibility: mir::Visibility::Public,
                parameters: Vec::new(),
                locals,
                return_type,
                entry: mir::BasicBlockId(0),
                blocks: vec![mir::BasicBlock {
                    id: mir::BasicBlockId(0),
                    instructions,
                    terminator,
                }],
            }],
        }
    }

    fn allocated_list_region(module: &mir::Module) -> mir::AllocationRegion {
        module
            .functions
            .iter()
            .flat_map(|function| &function.blocks)
            .flat_map(|block| &block.instructions)
            .find_map(|instruction| match instruction {
                mir::Instruction::AllocateList { region, .. } => Some(*region),
                _ => None,
            })
            .expect("module contains exactly one AllocateList")
    }

    #[test]
    fn a_local_non_escaping_list_is_assigned_to_temporary_storage() {
        let list_type = mir::Type::List(Box::new(mir::Type::Int));
        let mut module = single_function_module(
            mir::Type::Int,
            vec![list_local(0, list_type.clone())],
            vec![mir::Instruction::AllocateList {
                destination: mir::Place::Local(mir::LocalId(0)),
                element_type: mir::Type::Int,
                region: mir::AllocationRegion::Persistent,
            }],
            mir::Terminator::Return(Some(mir::Operand {
                type_: mir::Type::Int,
                kind: mir::OperandKind::Constant(mir::Constant::Integer("0".to_owned())),
            })),
        );
        assign_allocation_regions(&mut module);
        assert_eq!(
            allocated_list_region(&module),
            mir::AllocationRegion::Temporary
        );
    }

    #[test]
    fn a_list_returned_by_alias_is_assigned_to_persistent_storage() {
        let list_type = mir::Type::List(Box::new(mir::Type::Int));
        let mut module = single_function_module(
            list_type.clone(),
            vec![list_local(0, list_type.clone())],
            vec![mir::Instruction::AllocateList {
                destination: mir::Place::Local(mir::LocalId(0)),
                element_type: mir::Type::Int,
                region: mir::AllocationRegion::Temporary,
            }],
            mir::Terminator::Return(Some(mir::Operand {
                type_: list_type,
                kind: mir::OperandKind::Copy(mir::Place::Local(mir::LocalId(0))),
            })),
        );
        assign_allocation_regions(&mut module);
        assert_eq!(
            allocated_list_region(&module),
            mir::AllocationRegion::Persistent
        );
    }

    #[test]
    fn a_list_passed_to_an_unknown_call_is_conservatively_persistent() {
        let list_type = mir::Type::List(Box::new(mir::Type::Int));
        let unknown_function = mir::SymbolId(999);
        let mut module = single_function_module(
            mir::Type::Int,
            vec![list_local(0, list_type.clone())],
            vec![
                mir::Instruction::AllocateList {
                    destination: mir::Place::Local(mir::LocalId(0)),
                    element_type: mir::Type::Int,
                    region: mir::AllocationRegion::Temporary,
                },
                mir::Instruction::Call {
                    destination: None,
                    function: unknown_function,
                    arguments: vec![mir::Operand {
                        type_: list_type,
                        kind: mir::OperandKind::Copy(mir::Place::Local(mir::LocalId(0))),
                    }],
                    return_type: mir::Type::Void,
                },
            ],
            mir::Terminator::Return(None),
        );
        assign_allocation_regions(&mut module);
        assert_eq!(
            allocated_list_region(&module),
            mir::AllocationRegion::Persistent
        );
    }

    #[test]
    fn list_is_a_tracked_reference_type() {
        assert!(super::is_tracked_reference_type(&mir::Type::List(
            Box::new(mir::Type::Int)
        )));
    }

    // Real, compiled `new List<T>()` source (List B1) exercising the same
    // escape-analysis pass through the ordinary pipeline, not hand-built MIR.

    #[test]
    fn a_local_non_escaping_list_from_real_source_is_temporary() {
        let source =
            "public int Run() { List<int> values = new List<int>(); return values.Length; }";
        assert_eq!(
            list_regions(source, "Run"),
            vec![mir::AllocationRegion::Temporary]
        );
    }

    #[test]
    fn a_returned_list_from_real_source_is_persistent() {
        let source = "public List<int> Make() { return new List<int>(); }";
        assert_eq!(
            list_regions(source, "Make"),
            vec![mir::AllocationRegion::Persistent]
        );
    }

    #[test]
    fn a_list_stored_in_a_field_from_real_source_is_persistent() {
        let source = "public class Holder { public List<int> value; public Holder(List<int> initial) { value = initial; } } \
                       public int Run() { Holder holder = new Holder(new List<int>()); List<int> values = new List<int>(); holder.value = values; return values.Length; }";
        let classifications = classifications(source, "Run");
        assert!(
            classifications.contains(&EscapeClassification::Persistent(EscapeReason::Stored)),
            "{classifications:#?}"
        );
    }

    #[test]
    fn a_returned_list_alias_from_real_source_is_persistent() {
        let source = "public List<int> Make() { List<int> first = new List<int>(); List<int> second = first; return second; }";
        assert_eq!(
            list_regions(source, "Make"),
            vec![mir::AllocationRegion::Persistent]
        );
    }

    // --- List B2A: `values.Add(value)` and its effect on escape analysis ---

    #[test]
    fn a_reference_added_to_a_list_is_persistent() {
        let source = "public class Box { public int value; public Box(int value) { this.value = value; } } \
                       public int Run() { List<Box> values = new List<Box>(); Box box = new Box(10); values.Add(box); return box.value; }";
        let classifications = classifications(source, "Run");
        assert!(
            classifications.contains(&EscapeClassification::Persistent(EscapeReason::Stored)),
            "{classifications:#?}"
        );
    }

    #[test]
    fn an_alias_of_a_value_added_to_a_list_is_also_persistent() {
        let source = "public class Box { public int value; public Box(int value) { this.value = value; } } \
                       public int Run() { List<Box> values = new List<Box>(); Box first = new Box(10); Box second = first; values.Add(second); return first.value; }";
        let classifications = classifications(source, "Run");
        assert!(
            classifications
                .iter()
                .filter(|classification| matches!(
                    classification,
                    EscapeClassification::Persistent(EscapeReason::Stored)
                ))
                .count()
                >= 1,
            "{classifications:#?}"
        );
    }

    #[test]
    fn calling_add_does_not_by_itself_force_the_list_to_escape() {
        // Adding to a list is not, on its own, a reason for the *list*
        // (as opposed to the value passed to `Add`) to escape — mirrors how
        // `ArrayLength`/`ListLength` never force their own receiver to
        // escape either.
        let source = "public int Run() { List<int> values = new List<int>(); values.Add(1); values.Add(2); return values.Length; }";
        assert_eq!(
            list_regions(source, "Run"),
            vec![mir::AllocationRegion::Temporary]
        );
    }

    #[test]
    fn transitive_storage_through_a_helper_function_is_persistent() {
        let source = "public class Box { public int value; public Box(int value) { this.value = value; } } \
                       public void StoreInto(List<Box> values, Box value) { values.Add(value); } \
                       public int Run() { List<Box> values = new List<Box>(); Box box = new Box(10); StoreInto(values, box); return box.value; }";
        let classifications = classifications(source, "Run");
        assert!(
            classifications.contains(&EscapeClassification::Persistent(
                EscapeReason::PassedToCall
            )),
            "{classifications:#?}"
        );
    }

    // --- List B2B: `values.Get(index)` and its effect on escape analysis ---

    #[test]
    fn a_reference_read_back_through_get_remains_valid_after_being_added() {
        // `Get`'s result is only ever a copy of something already forced
        // `Persistent` by the corresponding `Add` (see the `ListAdd` arm of
        // `instruction_escape`); this just confirms the combination compiles
        // and the underlying reference is still classified `Persistent`.
        let source = "public class Box { public int value; public Box(int value) { this.value = value; } } \
                       public int Run() { List<Box> values = new List<Box>(); Box box = new Box(10); values.Add(box); \
                       Box loaded = values.Get(0); return loaded.value; }";
        let classifications = classifications(source, "Run");
        assert!(
            classifications.contains(&EscapeClassification::Persistent(EscapeReason::Stored)),
            "{classifications:#?}"
        );
    }

    #[test]
    fn a_reference_field_inside_a_struct_added_to_a_list_is_persistent() {
        // The reference is forced `Persistent` the moment it is embedded in
        // the struct literal (`RvalueKind::Aggregate`'s `Contained` case in
        // `assignment_escape`, pre-existing and unrelated to `List`), before
        // the struct is ever added to any list — demonstrating the existing
        // architecture already handles this without any List-specific rule.
        let source = "public class Box { public int value; public Box(int value) { this.value = value; } } \
                       public struct Wrapper { public Box Value; } \
                       public int Run() { List<Wrapper> values = new List<Wrapper>(); Box box = new Box(10); \
                       Wrapper w = Wrapper { Value: box }; values.Add(w); return box.value; }";
        let classifications = classifications(source, "Run");
        assert!(
            classifications.contains(&EscapeClassification::Persistent(EscapeReason::Contained)),
            "{classifications:#?}"
        );
    }

    #[test]
    fn a_reference_payload_inside_an_enum_added_to_a_list_is_persistent() {
        let source = "public class Box { public int value; public Box(int value) { this.value = value; } } \
                       public enum MaybeBox { None, Some(Box inner) } \
                       public int Run() { List<MaybeBox> values = new List<MaybeBox>(); Box box = new Box(10); \
                       MaybeBox m = MaybeBox.Some(box); values.Add(m); return box.value; }";
        let classifications = classifications(source, "Run");
        assert!(
            classifications.contains(&EscapeClassification::Persistent(EscapeReason::Contained)),
            "{classifications:#?}"
        );
    }

    // --- List C: `values.RemoveAt(index)` and its effect on escape analysis

    #[test]
    fn calling_remove_at_does_not_by_itself_force_the_list_to_escape() {
        // `RemoveAt` introduces no new operand that could alias anything and
        // must not force its own receiver to escape, exactly like `Add`/`Get`.
        let source = "public int Run() { List<int> values = new List<int>(); values.Add(1); \
                       values.Add(2); values.RemoveAt(0); return values.Length; }";
        assert_eq!(
            list_regions(source, "Run"),
            vec![mir::AllocationRegion::Temporary]
        );
    }

    #[test]
    fn remove_at_does_not_weaken_the_persistence_forced_by_add() {
        // The value removed from the list was already forced `Persistent`
        // the moment it was `Add`ed; calling `RemoveAt` afterward must not
        // change that classification.
        let source = "public class Box { public int value; public Box(int value) { this.value = value; } } \
                       public int Run() { List<Box> values = new List<Box>(); Box box = new Box(10); \
                       values.Add(box); values.RemoveAt(0); return box.value; }";
        let classifications = classifications(source, "Run");
        assert!(
            classifications.contains(&EscapeClassification::Persistent(EscapeReason::Stored)),
            "{classifications:#?}"
        );
    }
}
