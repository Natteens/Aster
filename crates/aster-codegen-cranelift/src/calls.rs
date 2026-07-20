use super::{
    BackendError, Codegen, FuncId, FunctionBuilder, FunctionState, HashMap, InstBuilder, Module,
    StackSlotData, StackSlotKind, cast_value, is_aggregate, mir, type_name, types,
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
        match intrinsic {
            mir::Intrinsic::StringFromLong | mir::Intrinsic::StringFromULong => {
                return self.translate_string_from_integer(
                    builder,
                    destination,
                    intrinsic,
                    arguments,
                    state,
                );
            }
            mir::Intrinsic::StringFromDouble => {
                return self.translate_string_from_double(builder, destination, arguments, state);
            }
            mir::Intrinsic::StringJoin => {
                return self.translate_string_join(builder, destination, arguments, state);
            }
            _ => {}
        }
        let (symbol, immediate, needs_context) = match intrinsic {
            mir::Intrinsic::Log => ("aster_rt_log", Some(0_i64), false),
            mir::Intrinsic::LogWarning => ("aster_rt_log", Some(1), false),
            mir::Intrinsic::LogError => ("aster_rt_log", Some(2), false),
            mir::Intrinsic::StringEquals => ("aster_rt_string_eq", None, false),
            mir::Intrinsic::StringConcat => ("aster_rt_string_concat", None, true),
            mir::Intrinsic::StringLength => ("aster_rt_string_length", None, true),
            mir::Intrinsic::StringFromBool => ("aster_rt_string_from_bool", None, true),
            mir::Intrinsic::StringFromChar => ("aster_rt_string_from_char", None, true),
            mir::Intrinsic::ReportRuntimeError(kind) => (
                "aster_rt_math_domain_error",
                Some(match kind {
                    mir::RuntimeErrorKind::MathAbsIntOverflow => 0,
                    mir::RuntimeErrorKind::MathAbsLongOverflow => 1,
                    mir::RuntimeErrorKind::MathClampInvalidRange => 2,
                }),
                true,
            ),
            mir::Intrinsic::StringFromLong
            | mir::Intrinsic::StringFromULong
            | mir::Intrinsic::StringFromDouble
            | mir::Intrinsic::StringJoin => {
                unreachable!("handled by the dedicated translators above")
            }
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

    /// `StringFromLong`/`StringFromULong`: widen the source integer to the
    /// runtime routine's fixed 64-bit parameter (signed or unsigned per the
    /// source type), so the runtime needs exactly one conversion routine per
    /// signedness regardless of the Aster integer width involved.
    fn translate_string_from_integer(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        destination: Option<&mir::Place>,
        intrinsic: mir::Intrinsic,
        arguments: &[mir::Operand],
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        let symbol = match intrinsic {
            mir::Intrinsic::StringFromLong => "aster_rt_string_from_long",
            mir::Intrinsic::StringFromULong => "aster_rt_string_from_ulong",
            _ => unreachable!("caller matched only these two intrinsics"),
        };
        let widened_type = if symbol == "aster_rt_string_from_long" {
            mir::Type::Long
        } else {
            mir::Type::ULong
        };
        let argument = arguments.first().ok_or_else(|| {
            BackendError::new("string interpolation conversion requires one argument")
        })?;
        let value = self.translate_operand(builder, argument, state)?;
        let value = cast_value(builder, &argument.type_, &widened_type, value)?;
        let function_ref = self
            .jit
            .declare_func_in_func(self.runtime_ids[symbol], builder.func);
        let context = state
            .execution_context
            .ok_or_else(|| BackendError::new("runtime intrinsic requires an execution context"))?;
        let call = builder.ins().call(function_ref, &[context, value]);
        self.store_intrinsic_result(builder, destination, call, state)
    }

    /// `StringFromDouble`: promote a `float` source to `double` so the
    /// runtime needs only one floating-point conversion routine.
    fn translate_string_from_double(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        destination: Option<&mir::Place>,
        arguments: &[mir::Operand],
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        let argument = arguments.first().ok_or_else(|| {
            BackendError::new("string interpolation conversion requires one argument")
        })?;
        let value = self.translate_operand(builder, argument, state)?;
        let value = cast_value(builder, &argument.type_, &mir::Type::Double, value)?;
        let function_ref = self.jit.declare_func_in_func(
            self.runtime_ids["aster_rt_string_from_double"],
            builder.func,
        );
        let context = state
            .execution_context
            .ok_or_else(|| BackendError::new("runtime intrinsic requires an execution context"))?;
        let call = builder.ins().call(function_ref, &[context, value]);
        self.store_intrinsic_result(builder, destination, call, state)
    }

    /// `StringJoin`: back string interpolation's final concatenation. Every
    /// argument is already a `string` pointer; they are written into one
    /// stack-allocated array and passed as `(pointer, count)`, so the
    /// runtime computes the combined length and copies every part exactly
    /// once — a single allocation regardless of how many parts are joined.
    fn translate_string_join(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        destination: Option<&mir::Place>,
        arguments: &[mir::Operand],
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        let pointer_bytes = self.pointer_type.bytes();
        let count = u32::try_from(arguments.len())
            .ok()
            .filter(|count| i32::try_from(*count).is_ok())
            .ok_or_else(|| BackendError::new("too many interpolated string segments"))?;
        let size = count
            .checked_mul(pointer_bytes)
            .ok_or_else(|| BackendError::new("too many interpolated string segments"))?;
        let align_shift = u8::try_from(pointer_bytes.trailing_zeros()).unwrap_or(3);
        let slot = builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            size,
            align_shift,
        ));
        for (index, argument) in arguments.iter().enumerate() {
            let value = self.translate_operand(builder, argument, state)?;
            let offset = i32::try_from(index)
                .ok()
                .and_then(|index| index.checked_mul(i32::try_from(pointer_bytes).ok()?))
                .ok_or_else(|| BackendError::new("too many interpolated string segments"))?;
            builder.ins().stack_store(value, slot, offset);
        }
        let array_pointer = builder.ins().stack_addr(self.pointer_type, slot, 0);
        let function_ref = self
            .jit
            .declare_func_in_func(self.runtime_ids["aster_rt_string_join"], builder.func);
        let context = state
            .execution_context
            .ok_or_else(|| BackendError::new("runtime intrinsic requires an execution context"))?;
        let count_value = builder.ins().iconst(types::I32, i64::from(count));
        let call = builder
            .ins()
            .call(function_ref, &[context, array_pointer, count_value]);
        self.store_intrinsic_result(builder, destination, call, state)
    }

    fn store_intrinsic_result(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        destination: Option<&mir::Place>,
        call: cranelift_codegen::ir::Inst,
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        if let Some(destination) = destination {
            let result = builder.inst_results(call).first().copied().ok_or_else(|| {
                BackendError::new("runtime intrinsic did not produce its declared result")
            })?;
            self.store_scalar(builder, destination, result, state)?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn translate_array_allocation(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        destination: &mir::Place,
        element_type: &mir::Type,
        length: &mir::Operand,
        requires_default: bool,
        region: mir::AllocationRegion,
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        if region != mir::AllocationRegion::Persistent {
            return Err(BackendError::new(
                "temporary array allocations are not yet supported by the Cranelift JIT",
            ));
        }
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
        region: mir::AllocationRegion,
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        if region != mir::AllocationRegion::Persistent {
            return Err(BackendError::new(
                "temporary object allocations are not yet supported by the Cranelift JIT",
            ));
        }
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
