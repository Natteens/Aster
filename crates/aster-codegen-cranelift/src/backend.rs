use super::{
    AbiParam, BackendError, Codegen, HashMap, HashSet, JITModule, Layouts, Linkage, Module, fmt,
    mir, runtime_functions, runtime_type,
};

impl Codegen {
    pub(super) fn new(mut jit: JITModule, module: &mir::Module) -> Result<Self, BackendError> {
        let pointer_type = jit.target_config().pointer_type();
        let pointer_bytes = pointer_type.bytes();
        let mut runtime_ids = HashMap::new();
        for function in runtime_functions() {
            let mut signature = jit.make_signature();
            for parameter in function.signature.parameters {
                signature
                    .params
                    .push(AbiParam::new(runtime_type(*parameter, pointer_type)));
            }
            if let Some(result) = function.signature.result {
                signature
                    .returns
                    .push(AbiParam::new(runtime_type(result, pointer_type)));
            }
            let id = jit
                .declare_function(function.name, Linkage::Import, &signature)
                .map_err(module_error)?;
            runtime_ids.insert(function.name, id);
        }
        let (call_depth_guarded, runtime_fallible_functions, runtime_fallible_interface_methods) =
            analyze_calls(module);
        let mut codegen = Self {
            jit,
            pointer_type,
            string_data: HashMap::new(),
            runtime_ids,
            interface_tables: HashMap::new(),
            interface_methods: module
                .interfaces
                .iter()
                .flat_map(|interface| {
                    interface
                        .methods
                        .iter()
                        .cloned()
                        .enumerate()
                        .map(|(slot, method)| (method.symbol, (interface.symbol, slot, method)))
                })
                .collect(),
            call_depth_guarded,
            runtime_fallible_functions,
            runtime_fallible_interface_methods,
            layouts: Layouts::new(module, pointer_bytes)?,
        };
        codegen.declare_task_functions()?;
        codegen.declare_async_functions()?;
        Ok(codegen)
    }
}

fn analyze_calls(
    module: &mir::Module,
) -> (
    HashSet<mir::SymbolId>,
    HashSet<mir::SymbolId>,
    HashSet<mir::SymbolId>,
) {
    let mut interface_targets = HashMap::<mir::SymbolId, Vec<mir::SymbolId>>::new();
    for interface in &module.interfaces {
        for (slot, method) in interface.methods.iter().enumerate() {
            let targets = module
                .interface_implementations
                .iter()
                .filter(|implementation| implementation.interface == interface.symbol)
                .filter_map(|implementation| implementation.methods.get(slot).copied())
                .collect();
            interface_targets.insert(method.symbol, targets);
        }
    }

    let call_targets = module
        .functions
        .iter()
        .map(|function| {
            let mut targets = HashSet::new();
            for instruction in function.blocks.iter().flat_map(|block| &block.instructions) {
                match instruction {
                    mir::Instruction::Call { function, .. } => {
                        targets.insert(*function);
                    }
                    mir::Instruction::CallInterface { method, .. } => {
                        targets
                            .extend(interface_targets.get(method).into_iter().flatten().copied());
                    }
                    _ => {}
                }
            }
            (function.symbol, targets)
        })
        .collect::<HashMap<_, _>>();

    let mut guarded = HashSet::new();
    for function in &module.functions {
        let start = function.symbol;
        let mut pending = call_targets
            .get(&start)
            .into_iter()
            .flatten()
            .copied()
            .collect::<Vec<_>>();
        let mut visited = HashSet::new();
        while let Some(target) = pending.pop() {
            if target == start {
                guarded.insert(start);
                break;
            }
            if visited.insert(target) {
                pending.extend(call_targets.get(&target).into_iter().flatten().copied());
            }
        }
    }

    let mut fallible = guarded.clone();
    for function in &module.functions {
        if function
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .any(instruction_can_fail_without_calling_aster)
        {
            fallible.insert(function.symbol);
        }
    }

    loop {
        let mut changed = false;
        for function in &module.functions {
            if fallible.contains(&function.symbol) {
                continue;
            }
            let calls_fallible = function
                .blocks
                .iter()
                .flat_map(|block| &block.instructions)
                .any(|instruction| match instruction {
                    mir::Instruction::Call { function, .. } => fallible.contains(function),
                    mir::Instruction::CallInterface { method, .. } => {
                        interface_targets.get(method).is_some_and(|targets| {
                            targets.iter().any(|target| fallible.contains(target))
                        })
                    }
                    _ => false,
                });
            if calls_fallible {
                changed |= fallible.insert(function.symbol);
            }
        }
        if !changed {
            break;
        }
    }

    let fallible_interface_methods = interface_targets
        .into_iter()
        .filter_map(|(method, targets)| {
            targets
                .iter()
                .any(|target| fallible.contains(target))
                .then_some(method)
        })
        .collect();
    (guarded, fallible, fallible_interface_methods)
}

fn instruction_can_fail_without_calling_aster(instruction: &mir::Instruction) -> bool {
    match instruction {
        mir::Instruction::Assign { .. }
        | mir::Instruction::Call { .. }
        | mir::Instruction::CallInterface { .. } => false,
        mir::Instruction::TemporarySubregionEnter { .. }
        | mir::Instruction::TemporarySubregionExit { .. }
        | mir::Instruction::CallIntrinsic { .. }
        | mir::Instruction::AllocateArray { .. }
        | mir::Instruction::AllocateObject { .. }
        | mir::Instruction::AllocateList { .. }
        | mir::Instruction::AllocateDictionary { .. }
        | mir::Instruction::AllocateStringBuilder { .. }
        | mir::Instruction::StringBuilderAppend { .. }
        | mir::Instruction::StringBuilderToString { .. }
        | mir::Instruction::DictionaryAdd { .. }
        | mir::Instruction::DictionarySet { .. }
        | mir::Instruction::DictionaryTryGet { .. }
        | mir::Instruction::DictionaryContainsKey { .. }
        | mir::Instruction::DictionaryRemove { .. }
        | mir::Instruction::DictionaryEntries { .. }
        | mir::Instruction::ListAdd { .. }
        | mir::Instruction::ListGet { .. }
        | mir::Instruction::ListRemoveAt { .. }
        | mir::Instruction::StringDecodeNext { .. } => true,
    }
}

pub(super) fn module_error(error: impl fmt::Debug + fmt::Display) -> BackendError {
    BackendError::new(format!("Cranelift JIT error: {error}\n{error:?}"))
}
