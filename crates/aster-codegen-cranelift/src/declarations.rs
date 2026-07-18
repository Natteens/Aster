use super::{
    AbiParam, BackendError, ClifType, Codegen, FuncId, HashMap, Linkage, Module, Primitive,
    RuntimeType, Signature, is_aggregate, mir, module_error, primitive, type_name, types,
};

impl Codegen {
    pub(super) fn declare_functions(
        &mut self,
        mir_module: &mir::Module,
    ) -> Result<HashMap<mir::SymbolId, FuncId>, BackendError> {
        let mut functions = HashMap::new();
        for function in &mir_module.functions {
            let signature = self.signature(function)?;
            let linkage = if function.visibility == mir::Visibility::Public {
                Linkage::Export
            } else {
                Linkage::Local
            };
            let name = format!("aster_{}_{}", function.symbol.0, function.name);
            let id = self
                .jit
                .declare_function(&name, linkage, &signature)
                .map_err(module_error)?;
            functions.insert(function.symbol, id);
        }
        Ok(functions)
    }

    pub(super) fn signature(&self, function: &mir::Function) -> Result<Signature, BackendError> {
        let mut signature = self.jit.make_signature();
        // Every Aster function receives the host-owned context first. Internal
        // calls forward the same pointer; source-level signatures stay unchanged.
        signature.params.push(AbiParam::new(self.pointer_type));
        if is_aggregate(&function.return_type) {
            signature.params.push(AbiParam::new(self.pointer_type));
        }
        for parameter in &function.parameters {
            let type_ = if is_aggregate(&parameter.type_)
                || matches!(parameter.type_, mir::Type::Array(_) | mir::Type::Class(_))
            {
                self.pointer_type
            } else {
                self.clif_value_type(&parameter.type_)?
            };
            signature.params.push(AbiParam::new(type_));
        }
        if function.return_type != mir::Type::Void && !is_aggregate(&function.return_type) {
            signature
                .returns
                .push(AbiParam::new(self.clif_value_type(&function.return_type)?));
        }
        Ok(signature)
    }

    pub(super) fn clif_value_type(&self, type_: &mir::Type) -> Result<ClifType, BackendError> {
        if matches!(type_, mir::Type::Array(_) | mir::Type::Class(_)) {
            return Ok(self.pointer_type);
        }
        match primitive(type_) {
            Some(Primitive::Bool | Primitive::SByte | Primitive::Byte) => Ok(types::I8),
            Some(Primitive::Short | Primitive::UShort) => Ok(types::I16),
            // `char` is an i32 Unicode scalar in this ABI.
            Some(Primitive::Char | Primitive::Int | Primitive::UInt) => Ok(types::I32),
            Some(Primitive::Long | Primitive::ULong) => Ok(types::I64),
            Some(Primitive::Float) => Ok(types::F32),
            Some(Primitive::Double) => Ok(types::F64),
            Some(Primitive::String) => Ok(self.pointer_type),
            Some(Primitive::Decimal) | None => Err(BackendError::new(format!(
                "type `{}` cannot be represented by the current JIT",
                type_name(type_)
            ))),
        }
    }
}

pub(super) fn runtime_type(type_: RuntimeType, pointer: ClifType) -> ClifType {
    match type_ {
        RuntimeType::I8 => types::I8,
        RuntimeType::I32 => types::I32,
        RuntimeType::I64 => types::I64,
        RuntimeType::Pointer => pointer,
    }
}
