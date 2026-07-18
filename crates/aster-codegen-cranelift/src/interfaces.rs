use super::{
    AbiParam, BackendError, Codegen, DataDescription, FuncId, FunctionBuilder, FunctionState,
    HashMap, InstBuilder, MemFlags, Module, Signature, is_aggregate, mir, module_error,
};

impl Codegen {
    pub(super) fn define_interface_tables(
        &mut self,
        module: &mir::Module,
        function_ids: &HashMap<mir::SymbolId, FuncId>,
    ) -> Result<(), BackendError> {
        for implementation in &module.interface_implementations {
            let id = self
                .jit
                .declare_anonymous_data(false, false)
                .map_err(module_error)?;
            let mut description = DataDescription::new();
            let size = usize::try_from(self.pointer_type.bytes())
                .map_err(|_| BackendError::new("pointer size is not addressable"))?
                .checked_mul(implementation.methods.len())
                .ok_or_else(|| BackendError::new("interface table is too large"))?;
            description.define_zeroinit(size);
            description.set_align(u64::from(self.pointer_type.bytes()));
            for (slot, method) in implementation.methods.iter().enumerate() {
                let function_id = function_ids.get(method).copied().ok_or_else(|| {
                    BackendError::new("interface table references an unknown concrete method")
                })?;
                let function = self.jit.declare_func_in_data(function_id, &mut description);
                let offset = u32::try_from(slot)
                    .ok()
                    .and_then(|slot| slot.checked_mul(self.pointer_type.bytes()))
                    .ok_or_else(|| BackendError::new("interface table offset is too large"))?;
                description.write_function_addr(offset, function);
            }
            self.jit
                .define_data(id, &description)
                .map_err(module_error)?;
            self.interface_tables
                .insert((implementation.class, implementation.interface), id);
        }
        Ok(())
    }

    fn interface_signature(
        &self,
        method: &mir::InterfaceMethodDefinition,
    ) -> Result<Signature, BackendError> {
        let mut signature = self.jit.make_signature();
        signature.params.push(AbiParam::new(self.pointer_type));
        if is_aggregate(&method.return_type) {
            signature.params.push(AbiParam::new(self.pointer_type));
        }
        signature.params.push(AbiParam::new(self.pointer_type));
        for parameter in &method.parameters {
            let type_ = if is_aggregate(parameter)
                || matches!(parameter, mir::Type::Array(_) | mir::Type::Class(_))
            {
                self.pointer_type
            } else {
                self.clif_value_type(parameter)?
            };
            signature.params.push(AbiParam::new(type_));
        }
        if method.return_type != mir::Type::Void && !is_aggregate(&method.return_type) {
            signature
                .returns
                .push(AbiParam::new(self.clif_value_type(&method.return_type)?));
        }
        Ok(signature)
    }

    pub(super) fn translate_interface_call(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        call: &mir::Instruction,
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        let mir::Instruction::CallInterface {
            destination,
            receiver,
            method,
            arguments,
            return_type,
        } = call
        else {
            unreachable!("interface call translator received another instruction")
        };
        let (_, slot, definition) = self
            .interface_methods
            .get(method)
            .cloned()
            .ok_or_else(|| BackendError::new("unknown interface method in MIR"))?;
        let interface = self.translate_operand(builder, receiver, state)?;
        let object = builder
            .ins()
            .load(self.pointer_type, MemFlags::new(), interface, 0);
        let pointer_bytes = i32::try_from(self.pointer_type.bytes())
            .map_err(|_| BackendError::new("pointer offset is too large"))?;
        let table =
            builder
                .ins()
                .load(self.pointer_type, MemFlags::new(), interface, pointer_bytes);
        let method_offset = i32::try_from(slot)
            .ok()
            .and_then(|slot| slot.checked_mul(pointer_bytes))
            .ok_or_else(|| BackendError::new("interface method offset is too large"))?;
        let method_pointer =
            builder
                .ins()
                .load(self.pointer_type, MemFlags::new(), table, method_offset);
        let mut values =
            vec![state.execution_context.ok_or_else(|| {
                BackendError::new("interface call is missing its ExecutionContext")
            })?];
        if is_aggregate(return_type) {
            let destination = destination.as_ref().ok_or_else(|| {
                BackendError::new("aggregate interface call requires a destination")
            })?;
            values.push(self.place_address(builder, destination, state)?);
        }
        values.push(object);
        for argument in arguments {
            values.push(self.translate_operand(builder, argument, state)?);
        }
        let signature = builder.import_signature(self.interface_signature(&definition)?);
        let call = builder
            .ins()
            .call_indirect(signature, method_pointer, &values);
        if let Some(destination) = destination.as_ref()
            && !is_aggregate(return_type)
        {
            let result =
                builder.inst_results(call).first().copied().ok_or_else(|| {
                    BackendError::new("indirect interface call produced no result")
                })?;
            self.store_scalar(builder, destination, result, state)?;
        }
        Ok(())
    }
}
