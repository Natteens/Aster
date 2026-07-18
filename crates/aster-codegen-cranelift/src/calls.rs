use super::{
    BackendError, Codegen, FuncId, FunctionBuilder, FunctionState, HashMap, InstBuilder, Module,
    is_aggregate, mir, type_name, types,
};

impl Codegen {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn translate_call(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        destination: Option<&mir::Place>,
        function: mir::SymbolId,
        arguments: &[mir::Operand],
        return_type: &mir::Type,
        function_ids: &HashMap<mir::SymbolId, FuncId>,
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        let function_ref = self
            .jit
            .declare_func_in_func(function_ids[&function], builder.func);
        let mut values = vec![
            state
                .execution_context
                .ok_or_else(|| BackendError::new("function is missing its ExecutionContext"))?,
        ];
        if is_aggregate(return_type) {
            let destination = destination
                .ok_or_else(|| BackendError::new("struct call result requires a destination"))?;
            values.push(self.place_address(builder, destination, state)?);
        }
        for argument in arguments {
            values.push(self.translate_operand(builder, argument, state)?);
        }
        let call = builder.ins().call(function_ref, &values);
        if let Some(destination) = destination
            && !is_aggregate(return_type)
        {
            let result = builder.inst_results(call).first().copied().ok_or_else(|| {
                BackendError::new("Cranelift call did not produce its declared result")
            })?;
            self.store_scalar(builder, destination, result, state)?;
        }
        Ok(())
    }

    pub(super) fn translate_intrinsic(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        destination: Option<&mir::Place>,
        intrinsic: mir::Intrinsic,
        arguments: &[mir::Operand],
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        let (symbol, immediate, needs_context) = match intrinsic {
            mir::Intrinsic::Log => ("aster_rt_log", Some(0_i64), false),
            mir::Intrinsic::LogWarning => ("aster_rt_log", Some(1), false),
            mir::Intrinsic::LogError => ("aster_rt_log", Some(2), false),
            mir::Intrinsic::StringEquals => ("aster_rt_string_eq", None, false),
            mir::Intrinsic::StringConcat => ("aster_rt_string_concat", None, true),
            mir::Intrinsic::StringLength => ("aster_rt_string_length", None, true),
            mir::Intrinsic::ReportRuntimeError(kind) => (
                "aster_rt_math_domain_error",
                Some(match kind {
                    mir::RuntimeErrorKind::MathAbsIntOverflow => 0,
                    mir::RuntimeErrorKind::MathAbsLongOverflow => 1,
                    mir::RuntimeErrorKind::MathClampInvalidRange => 2,
                }),
                true,
            ),
        };
        let function_ref = self
            .jit
            .declare_func_in_func(self.runtime_ids[symbol], builder.func);
        let mut values = Vec::with_capacity(arguments.len() + 1);
        if needs_context {
            values.push(state.execution_context.ok_or_else(|| {
                BackendError::new("runtime intrinsic requires an execution context")
            })?);
        }
        if let Some(immediate) = immediate {
            values.push(builder.ins().iconst(types::I32, immediate));
        }
        for argument in arguments {
            values.push(self.translate_operand(builder, argument, state)?);
        }
        let call = builder.ins().call(function_ref, &values);
        if let Some(destination) = destination {
            let result = builder.inst_results(call).first().copied().ok_or_else(|| {
                BackendError::new("runtime intrinsic did not produce its declared result")
            })?;
            self.store_scalar(builder, destination, result, state)?;
        }
        Ok(())
    }

    pub(super) fn translate_array_allocation(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        destination: &mir::Place,
        element_type: &mir::Type,
        length: &mir::Operand,
        requires_default: bool,
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        if requires_default && !self.layouts.zero_initializable(element_type) {
            return Err(BackendError::new(format!(
                "`new {}[length]` has no safe all-zero default; initialize every element with an array literal",
                type_name(element_type)
            )));
        }
        let function_ref = self
            .jit
            .declare_func_in_func(self.runtime_ids["aster_rt_array_new"], builder.func);
        let context = state
            .execution_context
            .ok_or_else(|| BackendError::new("array allocation is missing its ExecutionContext"))?;
        let length = self.translate_operand(builder, length, state)?;
        let size = i64::from(self.layouts.type_layout(element_type)?.size);
        let size = builder.ins().iconst(types::I32, size);
        let call = builder.ins().call(function_ref, &[context, length, size]);
        let array = builder.inst_results(call)[0];
        self.store_scalar(builder, destination, array, state)
    }

    pub(super) fn translate_object_allocation(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        destination: &mir::Place,
        class: mir::SymbolId,
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        let function_ref = self
            .jit
            .declare_func_in_func(self.runtime_ids["aster_rt_object_new"], builder.func);
        let context = state.execution_context.ok_or_else(|| {
            BackendError::new("object allocation is missing its ExecutionContext")
        })?;
        let layout = self
            .layouts
            .types
            .get(&class)
            .ok_or_else(|| BackendError::new("class has no computed object layout"))?;
        let size = builder.ins().iconst(types::I32, i64::from(layout.size));
        let call = builder.ins().call(function_ref, &[context, size]);
        let object = builder.inst_results(call)[0];
        self.store_scalar(builder, destination, object, state)
    }
}
