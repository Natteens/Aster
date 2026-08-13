use super::{
    BackendError, Codegen, FuncId, FunctionBuilder, FunctionBuilderContext, FunctionState, HashMap,
    InstBuilder, MemFlags, Module, StackSlotData, StackSlotKind, is_aggregate, mir, module_error,
    types,
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
            for instruction in &block.instructions {
                if matches!(instruction, mir::Instruction::TemporarySubregionExit { .. }) {
                    state.runtime_failure = runtime_failure;
                }
                self.translate_instruction(builder, instruction, function_ids, &state)?;
                if matches!(
                    instruction,
                    mir::Instruction::TemporarySubregionEnter { .. }
                ) {
                    state.runtime_failure = fine_runtime_failure.ok_or_else(|| {
                        BackendError::new("temporary subregion is missing its failure cleanup")
                    })?;
                }
            }
            self.translate_terminator(builder, &block.terminator, &blocks, &state)?;
        }
        if let Some(fine_runtime_failure) = fine_runtime_failure {
            builder.switch_to_block(fine_runtime_failure);
            self.leave_temporary_subregion(builder, &state)?;
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
                requires_default,
                region,
            } => self.translate_array_allocation(
                builder,
                destination,
                element_type,
                length,
                *requires_default,
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
