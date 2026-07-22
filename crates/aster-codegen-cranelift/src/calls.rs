use super::{
    BackendError, Codegen, FuncId, FunctionBuilder, FunctionState, HashMap, InstBuilder, MemFlags,
    Module, StackSlotData, StackSlotKind, cast_value, is_aggregate, mir, scalar_from_bits,
    scalar_kind, scalar_to_bits, type_name, types,
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

    #[allow(clippy::too_many_lines)]
    pub(super) fn translate_intrinsic(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        destination: Option<&mir::Place>,
        intrinsic: mir::Intrinsic,
        arguments: &[mir::Operand],
        return_type: &mir::Type,
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        match intrinsic {
            mir::Intrinsic::TaskRun => {
                return self.translate_task_run(builder, destination, arguments, state);
            }
            mir::Intrinsic::TaskWait => {
                return self.translate_task_wait(
                    builder,
                    destination,
                    arguments,
                    return_type,
                    state,
                );
            }
            mir::Intrinsic::StringFromLong
            | mir::Intrinsic::StringFromLongTemporary
            | mir::Intrinsic::StringFromULong
            | mir::Intrinsic::StringFromULongTemporary => {
                return self.translate_string_from_integer(
                    builder,
                    destination,
                    intrinsic,
                    arguments,
                    state,
                );
            }
            mir::Intrinsic::StringFromDouble | mir::Intrinsic::StringFromDoubleTemporary => {
                return self.translate_string_from_double(
                    builder,
                    destination,
                    intrinsic,
                    arguments,
                    state,
                );
            }
            mir::Intrinsic::StringJoin | mir::Intrinsic::StringJoinTemporary => {
                return self.translate_string_join(
                    builder,
                    destination,
                    intrinsic,
                    arguments,
                    state,
                );
            }
            mir::Intrinsic::AsyncSpawn
            | mir::Intrinsic::AsyncState
            | mir::Intrinsic::AsyncSetState
            | mir::Intrinsic::AsyncStoreSlot
            | mir::Intrinsic::AsyncLoadSlot
            | mir::Intrinsic::AsyncSpawnInner
            | mir::Intrinsic::AsyncAwaitResult
            | mir::Intrinsic::AsyncSetResult
            | mir::Intrinsic::ParallelFor
            | mir::Intrinsic::ParallelForEach
            | mir::Intrinsic::ParallelReduce => {
                return self.translate_async_intrinsic(
                    builder,
                    destination,
                    intrinsic,
                    arguments,
                    return_type,
                    state,
                );
            }
            _ => {}
        }
        let (symbol, immediate, needs_context) = match intrinsic {
            mir::Intrinsic::Log => ("aster_rt_log", Some(0_i64), false),
            mir::Intrinsic::LogWarning => ("aster_rt_log", Some(1), false),
            mir::Intrinsic::LogError => ("aster_rt_log", Some(2), false),
            mir::Intrinsic::StringEquals => ("aster_rt_string_eq", None, false),
            mir::Intrinsic::StringConcat => ("aster_rt_string_concat", None, true),
            mir::Intrinsic::StringConcatTemporary => {
                ("aster_rt_string_concat_temporary", None, true)
            }
            mir::Intrinsic::StringLength => ("aster_rt_string_length", None, true),
            mir::Intrinsic::StringFromBool => ("aster_rt_string_from_bool", None, true),
            mir::Intrinsic::StringFromBoolTemporary => {
                ("aster_rt_string_from_bool_temporary", None, true)
            }
            mir::Intrinsic::StringFromChar => ("aster_rt_string_from_char", None, true),
            mir::Intrinsic::StringFromCharTemporary => {
                ("aster_rt_string_from_char_temporary", None, true)
            }
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
            | mir::Intrinsic::StringFromLongTemporary
            | mir::Intrinsic::StringFromULong
            | mir::Intrinsic::StringFromULongTemporary
            | mir::Intrinsic::StringFromDouble
            | mir::Intrinsic::StringFromDoubleTemporary
            | mir::Intrinsic::StringJoin
            | mir::Intrinsic::StringJoinTemporary
            | mir::Intrinsic::TaskRun
            | mir::Intrinsic::TaskWait
            | mir::Intrinsic::AsyncSpawn
            | mir::Intrinsic::AsyncState
            | mir::Intrinsic::AsyncSetState
            | mir::Intrinsic::AsyncStoreSlot
            | mir::Intrinsic::AsyncLoadSlot
            | mir::Intrinsic::AsyncSpawnInner
            | mir::Intrinsic::AsyncAwaitResult
            | mir::Intrinsic::AsyncSetResult
            | mir::Intrinsic::ParallelFor
            | mir::Intrinsic::ParallelForEach
            | mir::Intrinsic::ParallelReduce => {
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

    /// `aster.core.Task.Run(function)`. `arguments` holds exactly one
    /// `OperandKind::Function` operand (validated shape), so the target
    /// symbol is emitted as a compile-time constant, never loaded through
    /// `translate_operand`.
    fn translate_task_run(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        destination: Option<&mir::Place>,
        arguments: &[mir::Operand],
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        let [
            mir::Operand {
                kind: mir::OperandKind::Function(symbol),
                ..
            },
        ] = arguments
        else {
            return Err(BackendError::new(
                "Task.Run requires exactly one resolved function argument",
            ));
        };
        let context = state
            .execution_context
            .ok_or_else(|| BackendError::new("Task.Run requires an execution context"))?;
        let symbol_constant = builder.ins().iconst(types::I32, i64::from(symbol.0));
        let function_ref = self
            .jit
            .declare_func_in_func(self.runtime_ids["aster_task_run"], builder.func);
        let call = builder
            .ins()
            .call(function_ref, &[context, symbol_constant]);
        if let Some(destination) = destination {
            let result =
                builder.inst_results(call).first().copied().ok_or_else(|| {
                    BackendError::new("Task.Run did not produce its declared result")
                })?;
            self.store_scalar(builder, destination, result, state)?;
        }
        Ok(())
    }

    /// `task.Wait()`. Dispatches to the `aster_task_wait_*` symbol matching
    /// `return_type`'s Cranelift-level representation.
    fn translate_task_wait(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        destination: Option<&mir::Place>,
        arguments: &[mir::Operand],
        return_type: &mir::Type,
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        let [task] = arguments else {
            return Err(BackendError::new(
                "Task<T>.Wait requires exactly one task argument",
            ));
        };
        let symbol = super::task_abi::wait_symbol_for(return_type).ok_or_else(|| {
            BackendError::new(format!(
                "`Task<{}>.Wait` cannot execute yet",
                type_name(return_type)
            ))
        })?;
        let context = state
            .execution_context
            .ok_or_else(|| BackendError::new("Task<T>.Wait requires an execution context"))?;
        let handle = self.translate_operand(builder, task, state)?;
        let function_ref = self
            .jit
            .declare_func_in_func(self.runtime_ids[symbol], builder.func);
        let call = builder.ins().call(function_ref, &[context, handle]);
        if let Some(destination) = destination {
            let result = builder.inst_results(call).first().copied().ok_or_else(|| {
                BackendError::new("Task<T>.Wait did not produce its declared result")
            })?;
            self.store_scalar(builder, destination, result, state)?;
        }
        Ok(())
    }

    /// Translate every async state-machine and `Parallel` intrinsic. Each is a
    /// single ABI call into `task_runtime` via `async_abi`; scalars cross as a
    /// `(kind, i64 bits)` pair and generated `Function` operands cross as their
    /// concrete `SymbolId`, never by name.
    #[allow(clippy::too_many_lines)]
    fn translate_async_intrinsic(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        destination: Option<&mir::Place>,
        intrinsic: mir::Intrinsic,
        arguments: &[mir::Operand],
        return_type: &mir::Type,
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        let context = state
            .execution_context
            .ok_or_else(|| BackendError::new("async intrinsic requires an execution context"))?;
        match intrinsic {
            mir::Intrinsic::AsyncSpawn => {
                let move_next = builder
                    .ins()
                    .iconst(types::I32, function_symbol(&arguments[0])?);
                let slot_count = self.translate_operand(builder, &arguments[1], state)?;
                let call = self.call_runtime(
                    builder,
                    "aster_async_spawn",
                    &[context, move_next, slot_count],
                );
                self.store_intrinsic_result(builder, destination, call, state)?;
            }
            mir::Intrinsic::AsyncState => {
                let handle = self.translate_operand(builder, &arguments[0], state)?;
                let call = self.call_runtime(builder, "aster_async_state", &[context, handle]);
                self.store_intrinsic_result(builder, destination, call, state)?;
            }
            mir::Intrinsic::AsyncSetState => {
                let handle = self.translate_operand(builder, &arguments[0], state)?;
                let new_state = self.translate_operand(builder, &arguments[1], state)?;
                self.call_runtime(
                    builder,
                    "aster_async_set_state",
                    &[context, handle, new_state],
                );
            }
            mir::Intrinsic::AsyncStoreSlot => {
                let handle = self.translate_operand(builder, &arguments[0], state)?;
                let index = self.translate_operand(builder, &arguments[1], state)?;
                let value_operand = &arguments[2];
                let kind = builder
                    .ins()
                    .iconst(types::I32, scalar_kind(&value_operand.type_)?);
                let value = self.translate_operand(builder, value_operand, state)?;
                let bits = scalar_to_bits(builder, &value_operand.type_, value)?;
                self.call_runtime(
                    builder,
                    "aster_async_store_slot",
                    &[context, handle, index, kind, bits],
                );
            }
            mir::Intrinsic::AsyncLoadSlot => {
                let handle = self.translate_operand(builder, &arguments[0], state)?;
                let index = self.translate_operand(builder, &arguments[1], state)?;
                let call =
                    self.call_runtime(builder, "aster_async_load_slot", &[context, handle, index]);
                self.store_scalar_from_bits(builder, destination, call, return_type, state)?;
            }
            mir::Intrinsic::AsyncSpawnInner => {
                let handle = self.translate_operand(builder, &arguments[0], state)?;
                let inner = builder
                    .ins()
                    .iconst(types::I32, function_symbol(&arguments[1])?);
                self.call_runtime(
                    builder,
                    "aster_async_spawn_inner",
                    &[context, handle, inner],
                );
            }
            mir::Intrinsic::AsyncAwaitResult => {
                let handle = self.translate_operand(builder, &arguments[0], state)?;
                let call =
                    self.call_runtime(builder, "aster_async_await_result", &[context, handle]);
                self.store_scalar_from_bits(builder, destination, call, return_type, state)?;
            }
            mir::Intrinsic::AsyncSetResult => {
                let handle = self.translate_operand(builder, &arguments[0], state)?;
                let value_operand = &arguments[1];
                let kind = builder
                    .ins()
                    .iconst(types::I32, scalar_kind(&value_operand.type_)?);
                let value = self.translate_operand(builder, value_operand, state)?;
                let bits = scalar_to_bits(builder, &value_operand.type_, value)?;
                self.call_runtime(
                    builder,
                    "aster_async_set_result",
                    &[context, handle, kind, bits],
                );
            }
            mir::Intrinsic::ParallelFor => {
                let start = self.translate_operand(builder, &arguments[0], state)?;
                let end = self.translate_operand(builder, &arguments[1], state)?;
                let body = builder
                    .ins()
                    .iconst(types::I32, function_symbol(&arguments[2])?);
                self.call_runtime(builder, "aster_parallel_for", &[context, start, end, body]);
            }
            mir::Intrinsic::ParallelForEach => {
                let values = self.translate_operand(builder, &arguments[0], state)?;
                let body_operand = &arguments[1];
                let body = builder
                    .ins()
                    .iconst(types::I32, function_symbol(body_operand)?);
                // The element scalar type rides on the resolved body operand.
                let kind = builder
                    .ins()
                    .iconst(types::I32, scalar_kind(&body_operand.type_)?);
                self.call_runtime(
                    builder,
                    "aster_parallel_for_each",
                    &[context, values, body, kind],
                );
            }
            mir::Intrinsic::ParallelReduce => {
                let values = self.translate_operand(builder, &arguments[0], state)?;
                let identity_operand = &arguments[1];
                let identity_kind = builder
                    .ins()
                    .iconst(types::I32, scalar_kind(&identity_operand.type_)?);
                let identity_value = self.translate_operand(builder, identity_operand, state)?;
                let identity_bits =
                    scalar_to_bits(builder, &identity_operand.type_, identity_value)?;
                let accumulate_operand = &arguments[2];
                let accumulate = builder
                    .ins()
                    .iconst(types::I32, function_symbol(accumulate_operand)?);
                // The element scalar type rides on the resolved `Accumulate`
                // operand, exactly like `ParallelForEach`'s body operand.
                let element_kind = builder
                    .ins()
                    .iconst(types::I32, scalar_kind(&accumulate_operand.type_)?);
                let combine_operand = &arguments[3];
                let combine = builder
                    .ins()
                    .iconst(types::I32, function_symbol(combine_operand)?);
                let call = self.call_runtime(
                    builder,
                    "aster_parallel_reduce",
                    &[
                        context,
                        values,
                        identity_bits,
                        identity_kind,
                        element_kind,
                        accumulate,
                        combine,
                    ],
                );
                self.store_scalar_from_bits(builder, destination, call, return_type, state)?;
            }
            _ => unreachable!("caller matched only async and Parallel intrinsics"),
        }
        Ok(())
    }

    /// Declare `name` in this function and emit a call to it, returning the
    /// call instruction so the caller can read its result if any.
    fn call_runtime(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        name: &str,
        arguments: &[super::Value],
    ) -> cranelift_codegen::ir::Inst {
        let function_ref = self
            .jit
            .declare_func_in_func(self.runtime_ids[name], builder.func);
        builder.ins().call(function_ref, arguments)
    }

    /// Narrow a runtime call's `i64` result back to `type_` and store it.
    fn store_scalar_from_bits(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        destination: Option<&mir::Place>,
        call: cranelift_codegen::ir::Inst,
        type_: &mir::Type,
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        if let Some(destination) = destination {
            let bits = builder.inst_results(call).first().copied().ok_or_else(|| {
                BackendError::new("async intrinsic did not produce its declared result")
            })?;
            let value = scalar_from_bits(builder, type_, bits)?;
            self.store_scalar(builder, destination, value, state)?;
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
        let (symbol, widened_type) = match intrinsic {
            mir::Intrinsic::StringFromLong => ("aster_rt_string_from_long", mir::Type::Long),
            mir::Intrinsic::StringFromLongTemporary => {
                ("aster_rt_string_from_long_temporary", mir::Type::Long)
            }
            mir::Intrinsic::StringFromULong => ("aster_rt_string_from_ulong", mir::Type::ULong),
            mir::Intrinsic::StringFromULongTemporary => {
                ("aster_rt_string_from_ulong_temporary", mir::Type::ULong)
            }
            _ => unreachable!("caller matched only integer-to-string intrinsics"),
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
        intrinsic: mir::Intrinsic,
        arguments: &[mir::Operand],
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        let argument = arguments.first().ok_or_else(|| {
            BackendError::new("string interpolation conversion requires one argument")
        })?;
        let value = self.translate_operand(builder, argument, state)?;
        let value = cast_value(builder, &argument.type_, &mir::Type::Double, value)?;
        let symbol = match intrinsic {
            mir::Intrinsic::StringFromDouble => "aster_rt_string_from_double",
            mir::Intrinsic::StringFromDoubleTemporary => "aster_rt_string_from_double_temporary",
            _ => unreachable!("caller matched only double-to-string intrinsics"),
        };
        let function_ref = self
            .jit
            .declare_func_in_func(self.runtime_ids[symbol], builder.func);
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
        intrinsic: mir::Intrinsic,
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
        let symbol = match intrinsic {
            mir::Intrinsic::StringJoin => "aster_rt_string_join",
            mir::Intrinsic::StringJoinTemporary => "aster_rt_string_join_temporary",
            _ => unreachable!("caller matched only string-join intrinsics"),
        };
        let function_ref = self
            .jit
            .declare_func_in_func(self.runtime_ids[symbol], builder.func);
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
        if requires_default && !self.layouts.zero_initializable(element_type) {
            return Err(BackendError::new(format!(
                "`new {}[length]` has no safe all-zero default; initialize every element with an array literal",
                type_name(element_type)
            )));
        }
        let symbol = match region {
            mir::AllocationRegion::Persistent => "aster_rt_array_new",
            mir::AllocationRegion::Temporary => "aster_rt_array_new_temporary",
        };
        let function_ref = self
            .jit
            .declare_func_in_func(self.runtime_ids[symbol], builder.func);
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
        let symbol = match region {
            mir::AllocationRegion::Persistent => "aster_rt_object_new",
            mir::AllocationRegion::Temporary => "aster_rt_object_new_temporary",
        };
        let function_ref = self
            .jit
            .declare_func_in_func(self.runtime_ids[symbol], builder.func);
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

    pub(super) fn translate_list_allocation(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        destination: &mir::Place,
        element_type: &mir::Type,
        region: mir::AllocationRegion,
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        let symbol = match region {
            mir::AllocationRegion::Persistent => "aster_rt_list_new",
            mir::AllocationRegion::Temporary => "aster_rt_list_new_temporary",
        };
        let function_ref = self
            .jit
            .declare_func_in_func(self.runtime_ids[symbol], builder.func);
        let context = state
            .execution_context
            .ok_or_else(|| BackendError::new("list allocation is missing its ExecutionContext"))?;
        let layout = self.layouts.type_layout(element_type)?;
        let size = builder.ins().iconst(types::I32, i64::from(layout.size));
        let align = builder
            .ins()
            .iconst(types::I32, i64::from(1_u32 << layout.align_shift));
        // `type_key` is a plain 64-bit structural identity (see
        // `aster_mir::type_key`); `as i64` only reinterprets its bits for the
        // runtime ABI's signed 64-bit carrier, never changes the value.
        #[allow(clippy::cast_possible_wrap)]
        let type_key_bits = mir::type_key(element_type) as i64;
        let type_key = builder.ins().iconst(types::I64, type_key_bits);
        let call = builder
            .ins()
            .call(function_ref, &[context, size, align, type_key]);
        let list = builder.inst_results(call)[0];
        self.store_scalar(builder, destination, list, state)
    }

    /// `list.Add(value)`: materializes `value`'s full representation at a
    /// stable address (an aggregate's address, already produced by
    /// `translate_operand`; otherwise a fresh stack slot holding the
    /// translated scalar/pointer value) and hands it to the universal
    /// `aster_rt_list_add`, which copies exactly `element_size` bytes from
    /// that address — never a `transmute`, never a type-specific ABI.
    pub(super) fn translate_list_add(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        list: &mir::Operand,
        value: &mir::Operand,
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        let list_value = self.translate_operand(builder, list, state)?;
        let layout = self.layouts.type_layout(&value.type_)?;
        let source_address = if is_aggregate(&value.type_) {
            self.translate_operand(builder, value, state)?
        } else {
            let value_ssa = self.translate_operand(builder, value, state)?;
            let slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                layout.size,
                layout.align_shift,
            ));
            builder.ins().stack_store(value_ssa, slot, 0);
            builder.ins().stack_addr(self.pointer_type, slot, 0)
        };
        let function_ref = self
            .jit
            .declare_func_in_func(self.runtime_ids["aster_rt_list_add"], builder.func);
        let context = state
            .execution_context
            .ok_or_else(|| BackendError::new("list.Add is missing its ExecutionContext"))?;
        let size = builder.ins().iconst(types::I32, i64::from(layout.size));
        let align = builder
            .ins()
            .iconst(types::I32, i64::from(1_u32 << layout.align_shift));
        #[allow(clippy::cast_possible_wrap)]
        let type_key_bits = mir::type_key(&value.type_) as i64;
        let type_key = builder.ins().iconst(types::I64, type_key_bits);
        builder.ins().call(
            function_ref,
            &[context, list_value, size, align, type_key, source_address],
        );
        Ok(())
    }

    /// `list.RemoveAt(index)`: the element type is derived from `list.type_`
    /// (validated to be `List(T)` before this ever runs), exactly like
    /// `ListAdd`/`ListGet` derive size/align/type key rather than storing
    /// them redundantly.
    pub(super) fn translate_list_remove_at(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        list: &mir::Operand,
        index: &mir::Operand,
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        let mir::Type::List(element_type) = &list.type_ else {
            return Err(BackendError::new("ListRemoveAt receiver is not List<T>"));
        };
        let list_value = self.translate_operand(builder, list, state)?;
        let index_value = self.translate_operand(builder, index, state)?;
        let layout = self.layouts.type_layout(element_type)?;
        let function_ref = self
            .jit
            .declare_func_in_func(self.runtime_ids["aster_rt_list_remove_at"], builder.func);
        let context = state
            .execution_context
            .ok_or_else(|| BackendError::new("list.RemoveAt is missing its ExecutionContext"))?;
        let size = builder.ins().iconst(types::I32, i64::from(layout.size));
        let align = builder
            .ins()
            .iconst(types::I32, i64::from(1_u32 << layout.align_shift));
        #[allow(clippy::cast_possible_wrap)]
        let type_key_bits = mir::type_key(element_type) as i64;
        let type_key = builder.ins().iconst(types::I64, type_key_bits);
        builder.ins().call(
            function_ref,
            &[context, list_value, size, align, type_key, index_value],
        );
        Ok(())
    }

    /// `list.Get(index)`: writes the copied element into a fresh address —
    /// the destination place itself for an aggregate (structs/enums/
    /// interfaces already model returning a value as "write into this
    /// address"), or a temporary stack slot for a scalar/reference, reloaded
    /// with the correct Cranelift type afterward. Never surfaces the
    /// buffer's own address.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn translate_list_get(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        destination: &mir::Place,
        list: &mir::Operand,
        index: &mir::Operand,
        element_type: &mir::Type,
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        let list_value = self.translate_operand(builder, list, state)?;
        let index_value = self.translate_operand(builder, index, state)?;
        let layout = self.layouts.type_layout(element_type)?;
        let write_address = if is_aggregate(element_type) {
            self.place_address(builder, destination, state)?
        } else {
            let slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                layout.size,
                layout.align_shift,
            ));
            builder.ins().stack_addr(self.pointer_type, slot, 0)
        };
        let function_ref = self
            .jit
            .declare_func_in_func(self.runtime_ids["aster_rt_list_get"], builder.func);
        let context = state
            .execution_context
            .ok_or_else(|| BackendError::new("list.Get is missing its ExecutionContext"))?;
        let size = builder.ins().iconst(types::I32, i64::from(layout.size));
        let align = builder
            .ins()
            .iconst(types::I32, i64::from(1_u32 << layout.align_shift));
        #[allow(clippy::cast_possible_wrap)]
        let type_key_bits = mir::type_key(element_type) as i64;
        let type_key = builder.ins().iconst(types::I64, type_key_bits);
        builder.ins().call(
            function_ref,
            &[
                context,
                list_value,
                size,
                align,
                type_key,
                index_value,
                write_address,
            ],
        );
        if !is_aggregate(element_type) {
            let value_type = self.clif_value_type(element_type)?;
            let loaded = builder
                .ins()
                .load(value_type, MemFlags::new(), write_address, 0);
            self.store_scalar(builder, destination, loaded, state)?;
        }
        Ok(())
    }
}

/// The concrete `SymbolId` (as an `i64` immediate) carried by a resolved
/// `OperandKind::Function` operand, so generated code references a target by
/// identity, never by name.
fn function_symbol(operand: &mir::Operand) -> Result<i64, BackendError> {
    match &operand.kind {
        mir::OperandKind::Function(symbol) => Ok(i64::from(symbol.0)),
        _ => Err(BackendError::new(
            "expected a resolved function operand for a concurrency intrinsic",
        )),
    }
}
