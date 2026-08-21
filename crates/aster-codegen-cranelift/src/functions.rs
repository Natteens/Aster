use super::{
    BackendError, Codegen, FuncId, FunctionBuilder, FunctionBuilderContext, FunctionState, HashMap,
    HashSet, InstBuilder, IntCC, MemFlags, Module, StackSlotData, StackSlotKind, is_aggregate, mir,
    module_error, types,
};

impl Codegen {
    pub(super) fn define_function(
        &mut self,
        function: &mir::Function,
        function_ids: &HashMap<mir::SymbolId, FuncId>,
    ) -> Result<(), BackendError> {
        let mut context = self.jit.make_context();
        context.func.signature = self.signature(function)?;
        let mut builder_context = FunctionBuilderContext::new();
        {
            let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
            self.translate_function(&mut builder, function, function_ids)?;
            builder.seal_all_blocks();
            builder.finalize();
        }
        self.jit
            .define_function(function_ids[&function.symbol], &mut context)
            .map_err(module_error)?;
        self.jit.clear_context(&mut context);
        Ok(())
    }

    /// Define the fixed `(context, payload) -> scalar` worker entry for one
    /// parameterized Task.Run target. The wrapper mechanically reconstructs
    /// concrete ABI arguments from the validated copied frame; it performs
    /// no transfer or callable inference.
    pub(super) fn define_task_trampoline(
        &mut self,
        function: &mir::Function,
        function_id: FuncId,
        trampoline_id: FuncId,
    ) -> Result<(), BackendError> {
        let mut context = self.jit.make_context();
        let mut signature = self.jit.make_signature();
        signature
            .params
            .push(super::AbiParam::new(self.pointer_type));
        signature
            .params
            .push(super::AbiParam::new(self.pointer_type));
        if function.return_type != mir::Type::Void && !is_aggregate(&function.return_type) {
            signature.returns.push(super::AbiParam::new(
                self.clif_value_type(&function.return_type)?,
            ));
        }
        context.func.signature = signature;
        let mut builder_context = FunctionBuilderContext::new();
        {
            let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
            let entry = builder.create_block();
            builder.append_block_params_for_function_params(entry);
            builder.switch_to_block(entry);
            builder.seal_block(entry);
            let context_value = builder.block_params(entry)[0];
            let payload = builder.block_params(entry)[1];
            let layout = self.layouts.task_argument_layout(
                &function
                    .parameters
                    .iter()
                    .map(|parameter| parameter.type_.clone())
                    .collect::<Vec<_>>(),
            )?;
            let mut values = vec![context_value];
            for (parameter, offset) in function.parameters.iter().zip(layout.offsets) {
                if is_aggregate(&parameter.type_) {
                    values.push(builder.ins().iadd_imm(payload, i64::from(offset)));
                } else {
                    let type_ = self.clif_value_type(&parameter.type_)?;
                    values.push(builder.ins().load(
                        type_,
                        MemFlags::new(),
                        payload,
                        i32::try_from(offset).map_err(|_| {
                            BackendError::new("Task.Run argument offset exceeds the JIT ABI")
                        })?,
                    ));
                }
            }
            let target = self.jit.declare_func_in_func(function_id, builder.func);
            let call = builder.ins().call(target, &values);
            if function.return_type == mir::Type::Void || is_aggregate(&function.return_type) {
                builder.ins().return_(&[]);
            } else {
                let result = builder.inst_results(call)[0];
                builder.ins().return_(&[result]);
            }
            builder.finalize();
        }
        self.jit
            .define_function(trampoline_id, &mut context)
            .map_err(module_error)?;
        self.jit.clear_context(&mut context);
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn translate_function(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        function: &mir::Function,
        function_ids: &HashMap<mir::SymbolId, FuncId>,
    ) -> Result<(), BackendError> {
        let blocks = function
            .blocks
            .iter()
            .map(|block| (block.id, builder.create_block()))
            .collect::<HashMap<_, _>>();
        let runtime_failure = builder.create_block();
        let fine_runtime_failure =
            function_contains_temporary_subregions(function).then(|| builder.create_block());
        let fine_active_instructions = fine_runtime_failure
            .is_some()
            .then(|| fine_active_instruction_positions(function))
            .transpose()?;
        let owned_runtime_failure =
            function_contains_owned_regions(function).then(|| builder.create_block());
        let owned_active_instructions = owned_runtime_failure
            .is_some()
            .then(|| owned_active_instruction_positions(function))
            .transpose()?;
        let mut state = FunctionState {
            slots: HashMap::new(),
            execution_context: None,
            hidden_return: None,
            temporary_scope: function_uses_temporary_allocations(function),
            call_depth_guarded: self.call_depth_guarded.contains(&function.symbol),
            runtime_failure,
        };
        for local in function.parameters.iter().chain(&function.locals) {
            let layout = self.layouts.type_layout(&local.type_)?;
            let slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                layout.size,
                layout.align_shift,
            ));
            state.slots.insert(local.id, slot);
        }
        let entry = blocks[&function.entry];
        let abi_entry = builder.create_block();
        builder.append_block_params_for_function_params(abi_entry);
        builder.switch_to_block(abi_entry);
        let values = builder.block_params(abi_entry).to_vec();
        let mut values = values.into_iter();
        state.execution_context = values.next();
        let context = state
            .execution_context
            .ok_or_else(|| BackendError::new("Aster function is missing its ExecutionContext"))?;
        if is_aggregate(&function.return_type) {
            state.hidden_return = values.next();
        }
        for parameter in &function.parameters {
            let value = values.next().ok_or_else(|| {
                BackendError::new("missing Cranelift parameter for Aster function")
            })?;
            let destination =
                self.place_address(builder, &mir::Place::Local(parameter.id), &state)?;
            if is_aggregate(&parameter.type_) {
                self.copy_value(builder, &parameter.type_, value, destination)?;
            } else {
                builder.ins().store(MemFlags::new(), value, destination, 0);
            }
        }
        if state.call_depth_guarded {
            let call_depth_failure = builder.create_block();
            let enter_ref = self
                .jit
                .declare_func_in_func(self.runtime_ids["aster_rt_call_enter"], builder.func);
            let entered = builder.ins().call(enter_ref, &[context]);
            let entered = builder.inst_results(entered)[0];
            builder
                .ins()
                .brif(entered, entry, &[], call_depth_failure, &[]);

            builder.switch_to_block(call_depth_failure);
            self.return_after_failed_call_guard(builder, &function.return_type)?;
        } else {
            builder.ins().jump(entry, &[]);
        }

        for block in &function.blocks {
            let clif_block = blocks[&block.id];
            builder.switch_to_block(clif_block);
            if block.id == function.entry && state.temporary_scope {
                let context = state.execution_context.ok_or_else(|| {
                    BackendError::new("temporary allocation scope is missing its ExecutionContext")
                })?;
                let function_ref = self.jit.declare_func_in_func(
                    self.runtime_ids["aster_rt_temporary_scope_enter"],
                    builder.func,
                );
                builder.ins().call(function_ref, &[context]);
            }
            for (instruction_index, instruction) in block.instructions.iter().enumerate() {
                state.runtime_failure = if owned_active_instructions
                    .as_ref()
                    .is_some_and(|positions| positions.contains(&(block.id, instruction_index)))
                {
                    owned_runtime_failure.expect("owned failure block exists")
                } else if fine_active_instructions
                    .as_ref()
                    .is_some_and(|positions| positions.contains(&(block.id, instruction_index)))
                {
                    fine_runtime_failure.expect("fine failure block exists")
                } else {
                    runtime_failure
                };
                self.translate_instruction(builder, instruction, function_ids, &state)?;
            }
            self.translate_terminator(builder, &block.terminator, &blocks, &state)?;
        }
        if let Some(fine_runtime_failure) = fine_runtime_failure {
            builder.switch_to_block(fine_runtime_failure);
            self.leave_temporary_subregion(builder, &state)?;
            builder.ins().jump(runtime_failure, &[]);
        }
        if let Some(owned_runtime_failure) = owned_runtime_failure {
            builder.switch_to_block(owned_runtime_failure);
            self.leave_owned_region(builder, &state)?;
            builder.ins().jump(runtime_failure, &[]);
        }
        builder.switch_to_block(runtime_failure);
        self.leave_function(builder, &state)?;
        self.return_after_failed_call_guard(builder, &function.return_type)?;
        Ok(())
    }

    pub(super) fn continue_if_runtime_ok(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        let context = state
            .execution_context
            .ok_or_else(|| BackendError::new("Aster function is missing its ExecutionContext"))?;
        let has_error_ref = self
            .jit
            .declare_func_in_func(self.runtime_ids["aster_rt_has_error"], builder.func);
        let failed = builder.ins().call(has_error_ref, &[context]);
        let failed = builder.inst_results(failed)[0];
        let continuation = builder.create_block();
        builder
            .ins()
            .brif(failed, state.runtime_failure, &[], continuation, &[]);
        builder.switch_to_block(continuation);
        Ok(())
    }

    pub(super) fn continue_if_runtime_status(
        builder: &mut FunctionBuilder<'_>,
        state: &FunctionState,
        status: cranelift_codegen::ir::Value,
    ) {
        let succeeded = builder.ins().icmp_imm(IntCC::Equal, status, 1);
        let continuation = builder.create_block();
        builder
            .ins()
            .brif(succeeded, continuation, &[], state.runtime_failure, &[]);
        builder.switch_to_block(continuation);
    }

    pub(super) fn continue_if_runtime_nonzero_status(
        builder: &mut FunctionBuilder<'_>,
        state: &FunctionState,
        status: cranelift_codegen::ir::Value,
    ) {
        let succeeded = builder.ins().icmp_imm(IntCC::NotEqual, status, 0);
        let continuation = builder.create_block();
        builder
            .ins()
            .brif(succeeded, continuation, &[], state.runtime_failure, &[]);
        builder.switch_to_block(continuation);
    }

    fn return_after_failed_call_guard(
        &self,
        builder: &mut FunctionBuilder<'_>,
        return_type: &mir::Type,
    ) -> Result<(), BackendError> {
        if *return_type == mir::Type::Void || is_aggregate(return_type) {
            builder.ins().return_(&[]);
            return Ok(());
        }
        let value = match self.clif_value_type(return_type)? {
            types::F32 => builder.ins().f32const(0.0),
            types::F64 => builder.ins().f64const(0.0),
            type_ => builder.ins().iconst(type_, 0),
        };
        builder.ins().return_(&[value]);
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn translate_instruction(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        instruction: &mir::Instruction,
        function_ids: &HashMap<mir::SymbolId, FuncId>,
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        match instruction {
            mir::Instruction::OwnedRegionEnter { .. } => {
                let context = state.execution_context.ok_or_else(|| {
                    BackendError::new("owned region is missing its ExecutionContext")
                })?;
                let function_ref = self.jit.declare_func_in_func(
                    self.runtime_ids["aster_rt_owned_region_enter"],
                    builder.func,
                );
                builder.ins().call(function_ref, &[context]);
                self.continue_if_runtime_ok(builder, state)
            }
            mir::Instruction::OwnedRegionExit { .. } => {
                self.leave_owned_region(builder, state)?;
                self.continue_if_runtime_ok(builder, state)
            }
            mir::Instruction::TemporarySubregionEnter { .. } => {
                let context = state.execution_context.ok_or_else(|| {
                    BackendError::new("temporary subregion is missing its ExecutionContext")
                })?;
                let function_ref = self.jit.declare_func_in_func(
                    self.runtime_ids["aster_rt_temporary_subregion_enter"],
                    builder.func,
                );
                builder.ins().call(function_ref, &[context]);
                self.continue_if_runtime_ok(builder, state)
            }
            mir::Instruction::TemporarySubregionExit { .. } => {
                self.leave_temporary_subregion(builder, state)?;
                self.continue_if_runtime_ok(builder, state)
            }
            mir::Instruction::Assign { target, value } => {
                self.assign_rvalue(builder, target, value, state)
            }
            mir::Instruction::Call {
                destination,
                function,
                arguments,
                return_type,
            } => self.translate_call(
                builder,
                destination.as_ref(),
                *function,
                arguments,
                return_type,
                function_ids,
                state,
            ),
            mir::Instruction::ForeignCall {
                destination,
                function,
                arguments,
                return_type,
            } => self.translate_foreign_call(
                builder,
                destination.as_ref(),
                *function,
                arguments,
                return_type,
                state,
            ),
            call @ mir::Instruction::CallInterface { .. } => {
                self.translate_interface_call(builder, call, state)
            }
            mir::Instruction::CallIntrinsic {
                destination,
                intrinsic,
                arguments,
                return_type,
            } => self.translate_intrinsic(
                builder,
                destination.as_ref(),
                *intrinsic,
                arguments,
                return_type,
                state,
            ),
            mir::Instruction::AllocateArray {
                destination,
                element_type,
                length,
                initialization,
                region,
            } => self.translate_array_allocation(
                builder,
                destination,
                element_type,
                length,
                *initialization,
                *region,
                state,
            ),
            mir::Instruction::AllocateObject {
                destination,
                class,
                region,
            } => self.translate_object_allocation(builder, destination, *class, *region, state),
            mir::Instruction::AllocateList {
                destination,
                element_type,
                region,
            } => self.translate_list_allocation(builder, destination, element_type, *region, state),
            mir::Instruction::AllocateDictionary {
                destination,
                key_type,
                value_type,
                region,
            } => self.translate_dictionary_allocation(
                builder,
                destination,
                key_type,
                value_type,
                *region,
                state,
            ),
            mir::Instruction::AllocateStringBuilder {
                destination,
                region,
                ..
            } => self.translate_string_builder_allocation(builder, destination, *region, state),
            mir::Instruction::StringBuilderAppend {
                builder: receiver,
                value,
                ..
            } => self.translate_string_builder_append(builder, receiver, value, state),
            mir::Instruction::StringBuilderToString {
                destination,
                builder: receiver,
                region,
                ..
            } => self.translate_string_builder_to_string(
                builder,
                destination,
                receiver,
                *region,
                state,
            ),
            mir::Instruction::DictionaryAdd {
                destination,
                dictionary,
                key,
                value,
            } => self.translate_dictionary_add_or_set(
                builder,
                destination,
                dictionary,
                key,
                value,
                false,
                state,
            ),
            mir::Instruction::DictionarySet {
                destination,
                dictionary,
                key,
                value,
            } => self.translate_dictionary_add_or_set(
                builder,
                destination,
                dictionary,
                key,
                value,
                true,
                state,
            ),
            mir::Instruction::DictionaryTryGet {
                destination,
                dictionary,
                key,
                value_type,
                option_layout,
            } => self.translate_dictionary_try_get(
                builder,
                destination,
                dictionary,
                key,
                value_type,
                *option_layout,
                state,
            ),
            mir::Instruction::DictionaryContainsKey {
                destination,
                dictionary,
                key,
            } => self.translate_dictionary_contains_or_remove(
                builder,
                destination,
                dictionary,
                key,
                false,
                state,
            ),
            mir::Instruction::DictionaryRemove {
                destination,
                dictionary,
                key,
            } => self.translate_dictionary_contains_or_remove(
                builder,
                destination,
                dictionary,
                key,
                true,
                state,
            ),
            mir::Instruction::DictionaryEntries {
                destination,
                dictionary,
                key_type,
                value_type,
                entry_type,
                entry_layout,
                region,
            } => self.translate_dictionary_entries(
                builder,
                destination,
                dictionary,
                key_type,
                value_type,
                entry_type,
                *entry_layout,
                *region,
                state,
            ),
            mir::Instruction::DictionaryClear { dictionary } => {
                self.translate_dictionary_clear(builder, dictionary, state)
            }
            mir::Instruction::DictionaryKeys {
                destination,
                dictionary,
                key_type,
                region,
            } => self.translate_dictionary_snapshot(
                builder,
                destination,
                dictionary,
                key_type,
                true,
                *region,
                state,
            ),
            mir::Instruction::DictionaryValues {
                destination,
                dictionary,
                value_type,
                region,
            } => self.translate_dictionary_snapshot(
                builder,
                destination,
                dictionary,
                value_type,
                false,
                *region,
                state,
            ),
            mir::Instruction::ListAdd { list, value } => {
                self.translate_list_add(builder, list, value, state)
            }
            mir::Instruction::ListGet {
                destination,
                list,
                index,
                element_type,
            } => self.translate_list_get(builder, destination, list, index, element_type, state),
            mir::Instruction::ListRemoveAt { list, index } => {
                self.translate_list_remove_at(builder, list, index, state)
            }
            mir::Instruction::ListSet { list, index, value } => {
                self.translate_list_set(builder, list, index, value, state)
            }
            mir::Instruction::ListClear { list } => self.translate_list_clear(builder, list, state),
            mir::Instruction::ListToArray {
                destination,
                list,
                element_type,
                region,
            } => self.translate_list_to_array(
                builder,
                destination,
                list,
                element_type,
                *region,
                state,
            ),
            mir::Instruction::StringDecodeNext {
                string,
                cursor,
                char_destination,
                next_cursor_destination,
                ok_destination,
            } => self.translate_string_decode_next(
                builder,
                string,
                cursor,
                char_destination,
                next_cursor_destination,
                ok_destination,
                state,
            ),
        }
    }

    fn translate_foreign_call(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        destination: Option<&mir::Place>,
        function: mir::SymbolId,
        arguments: &[mir::Operand],
        return_type: &mir::Type,
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        let id = *self
            .foreign_ids
            .get(&function)
            .ok_or_else(|| BackendError::new("foreign call references an undeclared binding"))?;
        let function_ref = self.jit.declare_func_in_func(id, builder.func);
        let mut values =
            Vec::with_capacity(arguments.len() + usize::from(*return_type != mir::Type::Void));
        for argument in arguments {
            values.push(self.translate_operand(builder, argument, state)?);
        }
        let result_slot = if *return_type == mir::Type::Void {
            None
        } else {
            let layout = self.layouts.type_layout(return_type)?;
            let slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                layout.size,
                layout.align_shift,
            ));
            let address = builder.ins().stack_addr(self.pointer_type, slot, 0);
            let zero = match self.clif_value_type(return_type)? {
                types::F32 => builder.ins().f32const(0.0),
                types::F64 => builder.ins().f64const(0.0),
                type_ => builder.ins().iconst(type_, 0),
            };
            builder.ins().store(MemFlags::new(), zero, address, 0);
            values.push(address);
            Some((slot, address))
        };
        let call = builder.ins().call(function_ref, &values);
        let status = builder.inst_results(call)[0];
        let succeeded = builder.ins().icmp_imm(IntCC::Equal, status, 0);
        let success = builder.create_block();
        let failure = builder.create_block();
        builder.ins().brif(succeeded, success, &[], failure, &[]);

        builder.switch_to_block(failure);
        let context = state
            .execution_context
            .ok_or_else(|| BackendError::new("foreign call is missing its ExecutionContext"))?;
        let kind = builder.ins().iconst(types::I32, 0);
        let status = builder.ins().sextend(types::I64, status);
        let report = self
            .jit
            .declare_func_in_func(self.runtime_ids["aster_rt_foreign_error"], builder.func);
        builder.ins().call(report, &[context, kind, status]);
        builder.ins().jump(state.runtime_failure, &[]);

        builder.switch_to_block(success);
        if let (Some(destination), Some((_slot, address))) = (destination, result_slot) {
            let value = builder.ins().load(
                self.clif_value_type(return_type)?,
                MemFlags::new(),
                address,
                0,
            );
            if *return_type == mir::Type::Bool {
                let invalid = builder.ins().icmp_imm(IntCC::UnsignedGreaterThan, value, 1);
                self.validate_foreign_scalar(builder, invalid, value, 1, state)?;
            } else if *return_type == mir::Type::Char {
                let too_large =
                    builder
                        .ins()
                        .icmp_imm(IntCC::UnsignedGreaterThan, value, 0x10_FFFF);
                let surrogate_low =
                    builder
                        .ins()
                        .icmp_imm(IntCC::UnsignedGreaterThanOrEqual, value, 0xD800);
                let surrogate_high =
                    builder
                        .ins()
                        .icmp_imm(IntCC::UnsignedLessThanOrEqual, value, 0xDFFF);
                let surrogate = builder.ins().band(surrogate_low, surrogate_high);
                let invalid = builder.ins().bor(too_large, surrogate);
                self.validate_foreign_scalar(builder, invalid, value, 2, state)?;
            }
            self.store_scalar(builder, destination, value, state)?;
        }
        Ok(())
    }

    fn validate_foreign_scalar(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        invalid: super::Value,
        value: super::Value,
        kind: i64,
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        let valid_block = builder.create_block();
        let invalid_block = builder.create_block();
        builder
            .ins()
            .brif(invalid, invalid_block, &[], valid_block, &[]);
        builder.switch_to_block(invalid_block);
        let context = state.execution_context.ok_or_else(|| {
            BackendError::new("foreign result validation is missing its ExecutionContext")
        })?;
        let kind = builder.ins().iconst(types::I32, kind);
        let value = builder.ins().uextend(types::I64, value);
        let report = self
            .jit
            .declare_func_in_func(self.runtime_ids["aster_rt_foreign_error"], builder.func);
        builder.ins().call(report, &[context, kind, value]);
        builder.ins().jump(state.runtime_failure, &[]);
        builder.switch_to_block(valid_block);
        Ok(())
    }
}

fn function_uses_temporary_allocations(function: &mir::Function) -> bool {
    function
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .any(|instruction| match instruction {
            mir::Instruction::TemporarySubregionEnter { .. }
            | mir::Instruction::TemporarySubregionExit { .. }
            | mir::Instruction::AllocateObject {
                region: mir::AllocationRegion::Temporary,
                ..
            }
            | mir::Instruction::AllocateArray {
                region: mir::AllocationRegion::Temporary,
                ..
            }
            | mir::Instruction::AllocateList {
                region: mir::AllocationRegion::Temporary,
                ..
            }
            | mir::Instruction::AllocateDictionary {
                region: mir::AllocationRegion::Temporary,
                ..
            }
            | mir::Instruction::AllocateStringBuilder {
                region: mir::AllocationRegion::Temporary,
                ..
            }
            | mir::Instruction::StringBuilderToString {
                region: mir::AllocationRegion::Temporary,
                ..
            }
            | mir::Instruction::DictionaryEntries {
                region: mir::AllocationRegion::Temporary,
                ..
            } => true,
            mir::Instruction::CallIntrinsic { intrinsic, .. } => {
                intrinsic.allocation_region() == Some(mir::AllocationRegion::Temporary)
            }
            _ => false,
        })
}

fn function_contains_temporary_subregions(function: &mir::Function) -> bool {
    function
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .any(|instruction| {
            matches!(
                instruction,
                mir::Instruction::TemporarySubregionEnter { .. }
                    | mir::Instruction::TemporarySubregionExit { .. }
            )
        })
}

fn function_contains_owned_regions(function: &mir::Function) -> bool {
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
}

fn owned_active_instruction_positions(
    function: &mir::Function,
) -> Result<HashSet<(mir::BasicBlockId, usize)>, BackendError> {
    let mut positions = HashSet::new();
    for block in &function.blocks {
        let mut active = None;
        for (index, instruction) in block.instructions.iter().enumerate() {
            match instruction {
                mir::Instruction::OwnedRegionEnter { id } if active.replace(*id).is_none() => {}
                mir::Instruction::OwnedRegionExit { id, .. } if active == Some(*id) => {
                    active = None;
                }
                mir::Instruction::OwnedRegionEnter { .. }
                | mir::Instruction::OwnedRegionExit { .. } => {
                    return Err(BackendError::new("malformed executable owned region"));
                }
                _ if active.is_some() => {
                    positions.insert((block.id, index));
                }
                _ => {}
            }
        }
        if active.is_some() {
            return Err(BackendError::new(
                "executable owned region crosses a basic-block boundary",
            ));
        }
    }
    Ok(positions)
}

/// The backend validator has already established that executable subregions
/// form an acyclic, balanced CFG.  Reconstruct the active state per original
/// instruction so a generated allocation-failure edge uses the fine cleanup
/// block on every branch, rather than relying on source/block Vec order.
fn fine_active_instruction_positions(
    function: &mir::Function,
) -> Result<HashSet<(mir::BasicBlockId, usize)>, BackendError> {
    let mut blocks = HashMap::new();
    for (index, block) in function.blocks.iter().enumerate() {
        if blocks.insert(block.id, index).is_some() {
            return Err(BackendError::new(
                "duplicate executable temporary-subregion block",
            ));
        }
    }
    let mut successors = HashMap::new();
    for block in &function.blocks {
        let targets = match block.terminator {
            mir::Terminator::Goto(target) => vec![target],
            mir::Terminator::Branch {
                then_block,
                else_block,
                ..
            } => vec![then_block, else_block],
            mir::Terminator::Return(_) | mir::Terminator::End => Vec::new(),
            mir::Terminator::Unreachable => {
                return Err(BackendError::new(
                    "unreachable executable temporary subregion",
                ));
            }
        };
        successors.insert(block.id, targets);
    }
    let mut state = HashMap::from([(function.entry, None)]);
    let mut pending = vec![function.entry];
    let mut positions = HashSet::new();
    while let Some(block_id) = pending.pop() {
        let mut active = state[&block_id];
        let block = &function.blocks[blocks[&block_id]];
        for (index, instruction) in block.instructions.iter().enumerate() {
            match instruction {
                mir::Instruction::TemporarySubregionEnter { id } => active = Some(*id),
                mir::Instruction::TemporarySubregionExit { .. } => active = None,
                _ if active.is_some() => {
                    positions.insert((block_id, index));
                }
                _ => {}
            }
        }
        for successor in &successors[&block_id] {
            if let Some(previous) = state.insert(*successor, active) {
                if previous != active {
                    return Err(BackendError::new(
                        "inconsistent executable temporary-subregion state",
                    ));
                }
            } else {
                pending.push(*successor);
            }
        }
    }
    Ok(positions)
}
