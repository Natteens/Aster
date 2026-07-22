use super::{
    BackendError, Codegen, FuncId, FunctionBuilder, FunctionBuilderContext, FunctionState, HashMap,
    InstBuilder, MemFlags, Module, StackSlotData, StackSlotKind, is_aggregate, mir, module_error,
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
        let mut state = FunctionState {
            slots: HashMap::new(),
            execution_context: None,
            hidden_return: None,
            temporary_scope: function_uses_temporary_allocations(function),
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
        builder.append_block_params_for_function_params(entry);

        for block in &function.blocks {
            let clif_block = blocks[&block.id];
            builder.switch_to_block(clif_block);
            if block.id == function.entry {
                let values = builder.block_params(entry).to_vec();
                let mut values = values.into_iter();
                state.execution_context = values.next();
                if state.temporary_scope {
                    let context = state.execution_context.ok_or_else(|| {
                        BackendError::new(
                            "temporary allocation scope is missing its ExecutionContext",
                        )
                    })?;
                    let function_ref = self.jit.declare_func_in_func(
                        self.runtime_ids["aster_rt_temporary_scope_enter"],
                        builder.func,
                    );
                    builder.ins().call(function_ref, &[context]);
                }
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
            }
            for instruction in &block.instructions {
                self.translate_instruction(builder, instruction, function_ids, &state)?;
            }
            self.translate_terminator(builder, &block.terminator, &blocks, &state)?;
        }
        Ok(())
    }

    fn translate_instruction(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        instruction: &mir::Instruction,
        function_ids: &HashMap<mir::SymbolId, FuncId>,
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        match instruction {
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
        }
    }
}

fn function_uses_temporary_allocations(function: &mir::Function) -> bool {
    function
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .any(|instruction| match instruction {
            mir::Instruction::AllocateObject {
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
            } => true,
            mir::Instruction::CallIntrinsic { intrinsic, .. } => {
                intrinsic.string_allocation_region() == Some(mir::AllocationRegion::Temporary)
            }
            _ => false,
        })
}
