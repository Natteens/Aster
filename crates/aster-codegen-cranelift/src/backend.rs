use super::{
    AbiParam, BackendError, Codegen, HashMap, JITModule, Layouts, Linkage, Module, fmt, mir,
    runtime_functions, runtime_type,
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
            layouts: Layouts::new(module, pointer_bytes)?,
        };
        codegen.declare_task_functions()?;
        codegen.declare_async_functions()?;
        Ok(codegen)
    }
}

pub(super) fn module_error(error: impl fmt::Debug + fmt::Display) -> BackendError {
    BackendError::new(format!("Cranelift JIT error: {error}\n{error:?}"))
}
