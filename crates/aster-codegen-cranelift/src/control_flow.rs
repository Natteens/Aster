use super::{
    BackendError, Block, Codegen, FunctionBuilder, FunctionState, HashMap, InstBuilder, Module,
    TrapCode, is_aggregate, mir,
};

impl Codegen {
    pub(super) fn translate_terminator(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        terminator: &mir::Terminator,
        blocks: &HashMap<mir::BasicBlockId, Block>,
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        match terminator {
            mir::Terminator::Goto(target) => {
                builder.ins().jump(blocks[target], &[]);
            }
            mir::Terminator::Branch {
                condition,
                then_block,
                else_block,
            } => {
                let condition = self.translate_operand(builder, condition, state)?;
                builder
                    .ins()
                    .brif(condition, blocks[then_block], &[], blocks[else_block], &[]);
            }
            mir::Terminator::Return(value) => {
                if let Some(value) = value
                    && is_aggregate(&value.type_)
                {
                    let source = self.translate_operand(builder, value, state)?;
                    let destination = state.hidden_return.ok_or_else(|| {
                        BackendError::new("struct return is missing its hidden destination")
                    })?;
                    self.copy_value(builder, &value.type_, source, destination)?;
                    self.leave_function(builder, state)?;
                    builder.ins().return_(&[]);
                    return Ok(());
                }
                let values = value
                    .as_ref()
                    .map(|value| self.translate_operand(builder, value, state))
                    .transpose()?
                    .into_iter()
                    .collect::<Vec<_>>();
                self.leave_function(builder, state)?;
                builder.ins().return_(&values);
            }
            mir::Terminator::End => {
                self.leave_function(builder, state)?;
                builder.ins().return_(&[]);
            }
            mir::Terminator::Unreachable => {
                builder.ins().trap(TrapCode::unwrap_user(1));
            }
        }
        Ok(())
    }

    pub(super) fn leave_function(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        let context = state
            .execution_context
            .ok_or_else(|| BackendError::new("Aster function is missing its ExecutionContext"))?;
        if state.temporary_scope {
            let function_ref = self.jit.declare_func_in_func(
                self.runtime_ids["aster_rt_temporary_scope_leave"],
                builder.func,
            );
            builder.ins().call(function_ref, &[context]);
        }
        if state.call_depth_guarded {
            let leave_ref = self
                .jit
                .declare_func_in_func(self.runtime_ids["aster_rt_call_leave"], builder.func);
            builder.ins().call(leave_ref, &[context]);
        }
        Ok(())
    }
}
