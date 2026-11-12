//! Virtual dispatch (object model B1, sections 4.2 and 5): virtual calls
//! through the class descriptor's virtual table and the stubs that fill the
//! abstract slots.

use aster_runtime::descriptor::ClassDescriptor;

use super::{
    AbiParam, BackendError, Codegen, FuncId, FunctionBuilder, FunctionBuilderContext,
    FunctionState, HashMap, InstBuilder, Linkage, MemFlags, Module, Signature, is_aggregate, mir,
    module_error, types,
};

impl Codegen {
    /// The Cranelift signature of a call with these argument types (receiver
    /// first for a method) and return type: the context first, a hidden
    /// destination pointer for an aggregate result, then the arguments. It is
    /// the signature [`Self::signature`] gives every defined function.
    pub(super) fn call_signature<'a>(
        &self,
        parameters: impl IntoIterator<Item = &'a mir::Type>,
        return_type: &mir::Type,
    ) -> Result<Signature, BackendError> {
        let mut signature = self.jit.make_signature();
        signature.params.push(AbiParam::new(self.pointer_type));
        if is_aggregate(return_type) {
            signature.params.push(AbiParam::new(self.pointer_type));
        }
        for parameter in parameters {
            let type_ = if is_aggregate(parameter)
                || matches!(parameter, mir::Type::Array(_) | mir::Type::Class(_))
            {
                self.pointer_type
            } else {
                self.clif_value_type(parameter)?
            };
            signature.params.push(AbiParam::new(type_));
        }
        if *return_type != mir::Type::Void && !is_aggregate(return_type) {
            signature
                .returns
                .push(AbiParam::new(self.clif_value_type(return_type)?));
        }
        Ok(signature)
    }

    /// Define one stub per abstract slot (keyed by the method that introduced
    /// it). The stub has the slot's signature, records a controlled runtime
    /// error and returns a zero value: reaching it means a descriptor was built
    /// by hand, because an abstract class is never instantiated and a
    /// non-abstract class has no abstract slot (both validated).
    pub(super) fn define_abstract_stubs(
        &mut self,
        module: &mir::Module,
    ) -> Result<HashMap<mir::SymbolId, FuncId>, BackendError> {
        let mut stubs = HashMap::new();
        for slot in module
            .classes
            .iter()
            .flat_map(|class| &class.vtable)
            .filter(|slot| slot.implementation.is_none())
        {
            if stubs.contains_key(&slot.method) {
                continue;
            }
            // Every class is a pointer in the ABI, so any class type stands in
            // for the receiver.
            let receiver = mir::Type::Class(mir::SymbolId(0));
            let signature = self.call_signature(
                std::iter::once(&receiver).chain(&slot.parameters),
                &slot.return_type,
            )?;
            let name = format!("aster_abstract_stub_{}", slot.method.0);
            let id = self
                .jit
                .declare_function(&name, Linkage::Local, &signature)
                .map_err(module_error)?;
            let mut context = self.jit.make_context();
            context.func.signature = signature;
            let mut builder_context = FunctionBuilderContext::new();
            {
                let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
                let entry = builder.create_block();
                builder.append_block_params_for_function_params(entry);
                builder.switch_to_block(entry);
                let execution_context = builder.block_params(entry)[0];
                let code = builder.ins().iconst(types::I32, ABSTRACT_CALL_ERROR_CODE);
                let report = self.jit.declare_func_in_func(
                    self.runtime_ids["aster_rt_math_domain_error"],
                    builder.func,
                );
                builder.ins().call(report, &[execution_context, code]);
                self.return_after_failed_call_guard(&mut builder, &slot.return_type)?;
                builder.seal_all_blocks();
                builder.finalize();
            }
            self.jit
                .define_function(id, &mut context)
                .map_err(module_error)?;
            self.jit.clear_context(&mut context);
            stubs.insert(slot.method, id);
        }
        Ok(stubs)
    }

    /// `receiver.vtable[slot](arguments...)`: load the descriptor from the
    /// object header, the function address from its virtual table, and call it
    /// indirectly with the receiver first. The slot index is a compile-time
    /// constant that MIR validation checked against the receiver class's
    /// table, and every descriptor derived from that class has at least that
    /// many entries (descriptor validation at module build).
    pub(super) fn translate_virtual_call(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        call: &mir::Instruction,
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        let mir::Instruction::CallVirtual {
            destination,
            receiver,
            method,
            slot,
            arguments,
            return_type,
        } = call
        else {
            unreachable!("virtual call translator received another instruction")
        };
        let object = self.translate_operand(builder, receiver, state)?;
        let descriptor = builder
            .ins()
            .load(self.pointer_type, MemFlags::new(), object, 0);
        let table_offset = i32::try_from(std::mem::offset_of!(ClassDescriptor, vtable))
            .map_err(|_| BackendError::new("descriptor virtual table offset is too large"))?;
        let table =
            builder
                .ins()
                .load(self.pointer_type, MemFlags::new(), descriptor, table_offset);
        let pointer_bytes = i32::try_from(self.pointer_type.bytes())
            .map_err(|_| BackendError::new("pointer offset is too large"))?;
        let slot_offset = i32::try_from(*slot)
            .ok()
            .and_then(|slot| slot.checked_mul(pointer_bytes))
            .ok_or_else(|| BackendError::new("virtual slot offset is too large"))?;
        let function_pointer =
            builder
                .ins()
                .load(self.pointer_type, MemFlags::new(), table, slot_offset);
        let mut values =
            vec![state.execution_context.ok_or_else(|| {
                BackendError::new("virtual call is missing its ExecutionContext")
            })?];
        if is_aggregate(return_type) {
            let destination = destination.as_ref().ok_or_else(|| {
                BackendError::new("aggregate virtual call requires a destination")
            })?;
            values.push(self.place_address(builder, destination, state)?);
        }
        values.push(object);
        for argument in arguments {
            values.push(self.translate_operand(builder, argument, state)?);
        }
        let signature = self.call_signature(
            std::iter::once(&receiver.type_)
                .chain(arguments.iter().map(|argument| &argument.type_)),
            return_type,
        )?;
        let signature = builder.import_signature(signature);
        let call = builder
            .ins()
            .call_indirect(signature, function_pointer, &values);
        if self.runtime_fallible_interface_methods.contains(method) {
            self.continue_if_runtime_ok(builder, state)?;
        }
        if let Some(destination) = destination.as_ref()
            && !is_aggregate(return_type)
        {
            let result = builder
                .inst_results(call)
                .first()
                .copied()
                .ok_or_else(|| BackendError::new("indirect virtual call produced no result"))?;
            self.store_scalar(builder, destination, result, state)?;
        }
        Ok(())
    }
}

/// Code passed to `aster_rt_math_domain_error` for a call of an abstract slot.
const ABSTRACT_CALL_ERROR_CODE: i64 = 9;
