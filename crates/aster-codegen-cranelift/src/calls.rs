use super::{
    BackendError, Codegen, FuncId, FunctionBuilder, FunctionState, HashMap, InstBuilder, MemFlags,
    Module, StackSlotData, StackSlotKind, Value, cast_value, is_aggregate, mir, scalar_from_bits,
    scalar_kind, scalar_to_bits, type_name, types,
};

/// Every layout fact needed to construct a concrete `Result<T, IOError>`
/// value from the runtime side of the ABI. See
/// [`Codegen::result_io_error_layout`].
struct ResultIoErrorLayout {
    total_size: u32,
    ok_tag: u32,
    error_tag: u32,
    ok_offset: u32,
    error_offset: u32,
    kind_offset: u32,
    oscode_offset: u32,
    /// `IOErrorKind` case tags, indexed in the fixed order
    /// `aster_runtime::PortableIoErrorKind`'s variants declare (`NotFound`,
    /// `PermissionDenied`, `AlreadyExists`, `InvalidPath`, `InvalidUtf8`,
    /// `NotFile`, `NotDirectory`, `LimitExceeded`, `Other`).
    kind_case_tags: [u32; 9],
}

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
        if self.runtime_fallible_functions.contains(&function) {
            self.continue_if_runtime_ok(builder, state)?;
        }
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
            mir::Intrinsic::StringFromFloat | mir::Intrinsic::StringFromFloatTemporary => {
                return self.translate_string_from_float(
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
            mir::Intrinsic::StringTryParseBool
            | mir::Intrinsic::StringTryParseInt
            | mir::Intrinsic::StringTryParseUInt
            | mir::Intrinsic::StringTryParseLong
            | mir::Intrinsic::StringTryParseULong
            | mir::Intrinsic::StringTryParseFloat
            | mir::Intrinsic::StringTryParseDouble => {
                return self.translate_string_try_parse(
                    builder,
                    destination,
                    intrinsic,
                    arguments,
                    return_type,
                    state,
                );
            }
            mir::Intrinsic::ConsoleWrite
            | mir::Intrinsic::ConsoleWriteLine
            | mir::Intrinsic::ConsoleReadLine
            | mir::Intrinsic::ConsoleReadLineTemporary => {
                return self.translate_console_io(
                    builder,
                    destination,
                    intrinsic,
                    arguments,
                    return_type,
                    state,
                );
            }
            mir::Intrinsic::FileReadAllText(_)
            | mir::Intrinsic::FileReadAllTextTemporary(_)
            | mir::Intrinsic::FileWriteAllText(_)
            | mir::Intrinsic::FileListFiles(_)
            | mir::Intrinsic::FileListFilesTemporary(_) => {
                return self.translate_file_io(
                    builder,
                    destination,
                    intrinsic,
                    arguments,
                    return_type,
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
            mir::Intrinsic::StringContains => ("aster_rt_string_contains", None, true),
            mir::Intrinsic::StringStartsWith => ("aster_rt_string_starts_with", None, true),
            mir::Intrinsic::StringEndsWith => ("aster_rt_string_ends_with", None, true),
            mir::Intrinsic::StringIndexOf => ("aster_rt_string_index_of", None, true),
            mir::Intrinsic::StringSubstringFrom => ("aster_rt_string_substring_from", None, true),
            mir::Intrinsic::StringSubstringFromTemporary => {
                ("aster_rt_string_substring_from_temporary", None, true)
            }
            mir::Intrinsic::StringSubstringRange => ("aster_rt_string_substring_range", None, true),
            mir::Intrinsic::StringSubstringRangeTemporary => {
                ("aster_rt_string_substring_range_temporary", None, true)
            }
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
            mir::Intrinsic::ListVersionMismatch => ("aster_rt_list_version_mismatch", None, true),
            mir::Intrinsic::StringFromLong
            | mir::Intrinsic::StringFromLongTemporary
            | mir::Intrinsic::StringFromULong
            | mir::Intrinsic::StringFromULongTemporary
            | mir::Intrinsic::StringFromDouble
            | mir::Intrinsic::StringFromDoubleTemporary
            | mir::Intrinsic::StringFromFloat
            | mir::Intrinsic::StringFromFloatTemporary
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
            | mir::Intrinsic::ParallelReduce
            | mir::Intrinsic::StringTryParseBool
            | mir::Intrinsic::StringTryParseInt
            | mir::Intrinsic::StringTryParseUInt
            | mir::Intrinsic::StringTryParseLong
            | mir::Intrinsic::StringTryParseULong
            | mir::Intrinsic::StringTryParseFloat
            | mir::Intrinsic::StringTryParseDouble
            | mir::Intrinsic::ConsoleWrite
            | mir::Intrinsic::ConsoleWriteLine
            | mir::Intrinsic::ConsoleReadLine
            | mir::Intrinsic::ConsoleReadLineTemporary
            | mir::Intrinsic::FileReadAllText(_)
            | mir::Intrinsic::FileReadAllTextTemporary(_)
            | mir::Intrinsic::FileWriteAllText(_)
            | mir::Intrinsic::FileListFiles(_)
            | mir::Intrinsic::FileListFilesTemporary(_) => {
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

    /// `StringFromFloat`: pass a `float` source straight through, at its
    /// native 32-bit width, so it never rounds twice (once when widened to
    /// `double`, once when the runtime formats it).
    fn translate_string_from_float(
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
        let symbol = match intrinsic {
            mir::Intrinsic::StringFromFloat => "aster_rt_string_from_float",
            mir::Intrinsic::StringFromFloatTemporary => "aster_rt_string_from_float_temporary",
            _ => unreachable!("caller matched only float-to-string intrinsics"),
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

    /// `string.TryParseBool/Int/UInt/Long/ULong()`: the runtime writes the
    /// complete `Option<T>` representation directly into the destination
    /// address, exactly like any other aggregate result (`AllocateObject`,
    /// `ListGet`). `total_size`, `some_tag`/`none_tag`, and `payload_offset`
    /// all come from `Layouts`, the same shared layout system every other
    /// aggregate uses -- never a parallel, TryParse-specific computation.
    fn translate_string_try_parse(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        destination: Option<&mir::Place>,
        intrinsic: mir::Intrinsic,
        arguments: &[mir::Operand],
        return_type: &mir::Type,
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        let [receiver] = arguments else {
            return Err(BackendError::new(
                "string.TryParse* requires exactly one receiver argument",
            ));
        };
        let destination = destination
            .ok_or_else(|| BackendError::new("string.TryParse* requires a destination"))?;
        let mir::Type::Enum(symbol) = return_type else {
            return Err(BackendError::new(
                "string.TryParse* must return a concrete Option<T>",
            ));
        };
        let (some_tag, none_tag, payload_offset) = {
            let definition = self.layouts.enums.get(symbol).ok_or_else(|| {
                BackendError::new("string.TryParse* returns an unknown enum type")
            })?;
            let none_case = definition
                .cases
                .iter()
                .find(|case| case.fields.is_empty())
                .ok_or_else(|| {
                    BackendError::new("string.TryParse* target is not shaped like Option<T>")
                })?;
            let some_case = definition
                .cases
                .iter()
                .find(|case| case.fields.len() == 1)
                .ok_or_else(|| {
                    BackendError::new("string.TryParse* target is not shaped like Option<T>")
                })?;
            let value_field = &some_case.fields[0];
            let field_layout = self
                .layouts
                .fields
                .get(&value_field.symbol)
                .ok_or_else(|| {
                    BackendError::new("string.TryParse* payload field has no computed layout")
                })?;
            (some_case.tag, none_case.tag, field_layout.offset)
        };
        let layout = self.layouts.type_layout(return_type)?;

        let receiver_value = self.translate_operand(builder, receiver, state)?;
        let destination_address = self.place_address(builder, destination, state)?;
        let context = state
            .execution_context
            .ok_or_else(|| BackendError::new("string.TryParse* is missing its ExecutionContext"))?;
        let symbol_name = match intrinsic {
            mir::Intrinsic::StringTryParseBool => "aster_rt_string_try_parse_bool",
            mir::Intrinsic::StringTryParseInt => "aster_rt_string_try_parse_int",
            mir::Intrinsic::StringTryParseUInt => "aster_rt_string_try_parse_uint",
            mir::Intrinsic::StringTryParseLong => "aster_rt_string_try_parse_long",
            mir::Intrinsic::StringTryParseULong => "aster_rt_string_try_parse_ulong",
            mir::Intrinsic::StringTryParseFloat => "aster_rt_string_try_parse_float",
            mir::Intrinsic::StringTryParseDouble => "aster_rt_string_try_parse_double",
            _ => unreachable!("caller matched only string-try-parse intrinsics"),
        };
        let function_ref = self
            .jit
            .declare_func_in_func(self.runtime_ids[symbol_name], builder.func);
        let total_size = builder.ins().iconst(types::I32, i64::from(layout.size));
        let some_tag_value = builder.ins().iconst(types::I32, i64::from(some_tag));
        let none_tag_value = builder.ins().iconst(types::I32, i64::from(none_tag));
        let payload_offset_value = builder.ins().iconst(types::I32, i64::from(payload_offset));
        builder.ins().call(
            function_ref,
            &[
                context,
                receiver_value,
                destination_address,
                total_size,
                some_tag_value,
                none_tag_value,
                payload_offset_value,
            ],
        );
        Ok(())
    }

    /// `aster.io.Write`/`WriteLine`/`ReadLine`, bound to their official
    /// declarations by `SymbolId` (never by name) at HIR lowering. `Write`/
    /// `WriteLine` pass their `string` argument straight through, unchanged,
    /// to the runtime; `ReadLine` writes a full `Option<string>` into its
    /// destination using the same layout-driven ABI as `string.TryParse*`.
    fn translate_console_io(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        destination: Option<&mir::Place>,
        intrinsic: mir::Intrinsic,
        arguments: &[mir::Operand],
        return_type: &mir::Type,
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        let context = state.execution_context.ok_or_else(|| {
            BackendError::new("aster.io intrinsic is missing its ExecutionContext")
        })?;
        match intrinsic {
            mir::Intrinsic::ConsoleWrite | mir::Intrinsic::ConsoleWriteLine => {
                let [value] = arguments else {
                    return Err(BackendError::new(
                        "aster.io.Write/WriteLine requires exactly one argument",
                    ));
                };
                let value = self.translate_operand(builder, value, state)?;
                let symbol = match intrinsic {
                    mir::Intrinsic::ConsoleWrite => "aster_rt_io_write",
                    mir::Intrinsic::ConsoleWriteLine => "aster_rt_io_write_line",
                    _ => unreachable!("caller matched only console write intrinsics"),
                };
                let function_ref = self
                    .jit
                    .declare_func_in_func(self.runtime_ids[symbol], builder.func);
                builder.ins().call(function_ref, &[context, value]);
                Ok(())
            }
            mir::Intrinsic::ConsoleReadLine | mir::Intrinsic::ConsoleReadLineTemporary => {
                let destination = destination
                    .ok_or_else(|| BackendError::new("aster.io.ReadLine requires a destination"))?;
                let mir::Type::Enum(symbol) = return_type else {
                    return Err(BackendError::new(
                        "aster.io.ReadLine must return a concrete Option<string>",
                    ));
                };
                let (some_tag, none_tag, payload_offset) = {
                    let definition = self.layouts.enums.get(symbol).ok_or_else(|| {
                        BackendError::new("aster.io.ReadLine returns an unknown enum type")
                    })?;
                    let none_case = definition
                        .cases
                        .iter()
                        .find(|case| case.fields.is_empty())
                        .ok_or_else(|| {
                            BackendError::new(
                                "aster.io.ReadLine target is not shaped like Option<T>",
                            )
                        })?;
                    let some_case = definition
                        .cases
                        .iter()
                        .find(|case| case.fields.len() == 1)
                        .ok_or_else(|| {
                            BackendError::new(
                                "aster.io.ReadLine target is not shaped like Option<T>",
                            )
                        })?;
                    let value_field = &some_case.fields[0];
                    let field_layout =
                        self.layouts
                            .fields
                            .get(&value_field.symbol)
                            .ok_or_else(|| {
                                BackendError::new(
                                    "aster.io.ReadLine payload field has no computed layout",
                                )
                            })?;
                    (some_case.tag, none_case.tag, field_layout.offset)
                };
                let layout = self.layouts.type_layout(return_type)?;
                let destination_address = self.place_address(builder, destination, state)?;
                let symbol_name = match intrinsic {
                    mir::Intrinsic::ConsoleReadLine => "aster_rt_io_read_line",
                    mir::Intrinsic::ConsoleReadLineTemporary => "aster_rt_io_read_line_temporary",
                    _ => unreachable!("caller matched only console read-line intrinsics"),
                };
                let function_ref = self
                    .jit
                    .declare_func_in_func(self.runtime_ids[symbol_name], builder.func);
                let total_size = builder.ins().iconst(types::I32, i64::from(layout.size));
                let some_tag_value = builder.ins().iconst(types::I32, i64::from(some_tag));
                let none_tag_value = builder.ins().iconst(types::I32, i64::from(none_tag));
                let payload_offset_value =
                    builder.ins().iconst(types::I32, i64::from(payload_offset));
                builder.ins().call(
                    function_ref,
                    &[
                        context,
                        destination_address,
                        total_size,
                        some_tag_value,
                        none_tag_value,
                        payload_offset_value,
                    ],
                );
                Ok(())
            }
            _ => unreachable!("caller matched only console intrinsics"),
        }
    }

    /// Every layout fact `aster.io.ReadAllText`/`WriteAllText` need to build
    /// a concrete `Result<T, IOError>` value, resolved by *symbol* equality
    /// only -- `symbols` was resolved once, during HIR lowering, from the
    /// official declarations' own case/field names (see
    /// `hir::FileIoResultLayout`); this function never compares a name. Each
    /// symbol is looked up in the same `Layouts`/`EnumDefinition`/
    /// `StructDefinition` data every other aggregate result already uses, so
    /// a symbol that does not actually appear where expected (adulterated
    /// MIR) is a controlled error, not a silent fallback.
    #[allow(clippy::too_many_lines)]
    fn result_io_error_layout(
        &mut self,
        return_type: &mir::Type,
        symbols: &mir::FileIoResultLayout,
        operation: &str,
    ) -> Result<ResultIoErrorLayout, BackendError> {
        let mir::Type::Enum(result_symbol) = return_type else {
            return Err(BackendError::new(format!(
                "{operation} must return a concrete Result<T, IOError>"
            )));
        };
        let result_definition =
            self.layouts
                .enums
                .get(result_symbol)
                .cloned()
                .ok_or_else(|| {
                    BackendError::new(format!("{operation} returns an unknown enum type"))
                })?;
        let ok_case = result_definition
            .cases
            .iter()
            .find(|case| case.symbol == symbols.ok_case)
            .ok_or_else(|| {
                BackendError::new(format!(
                    "{operation} Ok case symbol does not match the resolved Result specialization"
                ))
            })?;
        let error_case = result_definition
            .cases
            .iter()
            .find(|case| case.symbol == symbols.error_case)
            .ok_or_else(|| {
                BackendError::new(format!(
                    "{operation} Error case symbol does not match the resolved Result specialization"
                ))
            })?;
        let ok_field = ok_case
            .fields
            .iter()
            .find(|field| field.symbol == symbols.ok_field)
            .ok_or_else(|| {
                BackendError::new(format!(
                    "{operation} Ok field symbol does not match the resolved Ok case"
                ))
            })?;
        let error_field = error_case
            .fields
            .iter()
            .find(|field| field.symbol == symbols.error_field)
            .ok_or_else(|| {
                BackendError::new(format!(
                    "{operation} Error field symbol does not match the resolved Error case"
                ))
            })?;
        let ok_offset = self
            .layouts
            .fields
            .get(&ok_field.symbol)
            .ok_or_else(|| BackendError::new(format!("{operation} Ok payload has no layout")))?
            .offset;
        let error_offset = self
            .layouts
            .fields
            .get(&error_field.symbol)
            .ok_or_else(|| BackendError::new(format!("{operation} Error payload has no layout")))?
            .offset;
        let mir::Type::User(io_error_symbol) = &error_field.type_ else {
            return Err(BackendError::new(format!(
                "{operation} Error payload is not a concrete IOError"
            )));
        };
        let io_error_definition = self
            .layouts
            .structs
            .get(io_error_symbol)
            .cloned()
            .ok_or_else(|| {
                BackendError::new(format!("{operation} Error payload is not a known struct"))
            })?;
        let kind_field = io_error_definition
            .fields
            .iter()
            .find(|field| field.symbol == symbols.io_error_kind_field)
            .ok_or_else(|| {
                BackendError::new(format!(
                    "{operation} IOError.Kind field symbol does not match the resolved struct"
                ))
            })?;
        let oscode_field = io_error_definition
            .fields
            .iter()
            .find(|field| field.symbol == symbols.io_error_os_code_field)
            .ok_or_else(|| {
                BackendError::new(format!(
                    "{operation} IOError.OsCode field symbol does not match the resolved struct"
                ))
            })?;
        let kind_offset = self
            .layouts
            .fields
            .get(&kind_field.symbol)
            .ok_or_else(|| BackendError::new(format!("{operation} IOError.Kind has no layout")))?
            .offset;
        let oscode_offset = self
            .layouts
            .fields
            .get(&oscode_field.symbol)
            .ok_or_else(|| BackendError::new(format!("{operation} IOError.OsCode has no layout")))?
            .offset;
        let mir::Type::Enum(kind_symbol) = &kind_field.type_ else {
            return Err(BackendError::new(format!(
                "{operation} IOError.Kind is not a concrete enum"
            )));
        };
        let kind_definition = self
            .layouts
            .enums
            .get(kind_symbol)
            .cloned()
            .ok_or_else(|| {
                BackendError::new(format!("{operation} IOErrorKind is not a known enum"))
            })?;
        let mut kind_case_tags = [0_u32; 9];
        for (index, case_symbol) in symbols.portable_kind_cases.iter().enumerate() {
            let case = kind_definition
                .cases
                .iter()
                .find(|case| case.symbol == *case_symbol)
                .ok_or_else(|| {
                    BackendError::new(format!(
                        "{operation} IOErrorKind case symbol at index {index} does not match the resolved enum"
                    ))
                })?;
            kind_case_tags[index] = case.tag;
        }
        let layout = self.layouts.type_layout(return_type)?;
        Ok(ResultIoErrorLayout {
            total_size: layout.size,
            ok_tag: ok_case.tag,
            error_tag: error_case.tag,
            ok_offset,
            error_offset,
            kind_offset,
            oscode_offset,
            kind_case_tags,
        })
    }

    /// `aster.io.ReadAllText`/`WriteAllText`, bound to their official
    /// declarations by `SymbolId` (never by name) at HIR lowering. Both
    /// write a complete `Result<T, IOError>` directly into their
    /// destination, using [`Self::result_io_error_layout`] for every tag and
    /// offset -- exactly the same layout-driven ABI as `aster.io.ReadLine`/
    /// `string.TryParse*`, generalized to a `Result` whose `Error` payload is
    /// itself a struct.
    #[allow(clippy::too_many_lines, clippy::similar_names)]
    fn translate_file_io(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        destination: Option<&mir::Place>,
        intrinsic: mir::Intrinsic,
        arguments: &[mir::Operand],
        return_type: &mir::Type,
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        let (operation, symbols) = match intrinsic {
            mir::Intrinsic::FileReadAllText(symbols)
            | mir::Intrinsic::FileReadAllTextTemporary(symbols) => {
                ("aster.io.ReadAllText", symbols)
            }
            mir::Intrinsic::FileWriteAllText(symbols) => ("aster.io.WriteAllText", symbols),
            mir::Intrinsic::FileListFiles(symbols)
            | mir::Intrinsic::FileListFilesTemporary(symbols) => ("aster.io.ListFiles", symbols),
            _ => unreachable!("caller matched only file-io intrinsics"),
        };
        let layout = self.result_io_error_layout(return_type, &symbols, operation)?;
        let destination = destination
            .ok_or_else(|| BackendError::new(format!("{operation} requires a destination")))?;
        let destination_address = self.place_address(builder, destination, state)?;
        let context = state.execution_context.ok_or_else(|| {
            BackendError::new(format!("{operation} is missing its ExecutionContext"))
        })?;

        let tags_slot = builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            9 * 4,
            2,
        ));
        for (index, tag) in layout.kind_case_tags.iter().enumerate() {
            let value = builder.ins().iconst(types::I32, i64::from(*tag));
            let offset = i32::try_from(index * 4)
                .ok()
                .ok_or_else(|| BackendError::new(format!("{operation} tag table overflowed")))?;
            builder.ins().stack_store(value, tags_slot, offset);
        }
        let tags_pointer = builder.ins().stack_addr(self.pointer_type, tags_slot, 0);

        let total_size = builder
            .ins()
            .iconst(types::I32, i64::from(layout.total_size));
        let ok_tag = builder.ins().iconst(types::I32, i64::from(layout.ok_tag));
        let error_tag = builder
            .ins()
            .iconst(types::I32, i64::from(layout.error_tag));
        let ok_offset = builder
            .ins()
            .iconst(types::I32, i64::from(layout.ok_offset));
        let error_offset = builder
            .ins()
            .iconst(types::I32, i64::from(layout.error_offset));
        let kind_offset = builder
            .ins()
            .iconst(types::I32, i64::from(layout.kind_offset));
        let oscode_offset = builder
            .ins()
            .iconst(types::I32, i64::from(layout.oscode_offset));

        match intrinsic {
            mir::Intrinsic::FileReadAllText(_) | mir::Intrinsic::FileReadAllTextTemporary(_) => {
                let [path] = arguments else {
                    return Err(BackendError::new(
                        "aster.io.ReadAllText requires exactly one argument",
                    ));
                };
                let path_value = self.translate_operand(builder, path, state)?;
                let symbol = match intrinsic {
                    mir::Intrinsic::FileReadAllText(_) => "aster_rt_io_read_all_text",
                    mir::Intrinsic::FileReadAllTextTemporary(_) => {
                        "aster_rt_io_read_all_text_temporary"
                    }
                    _ => unreachable!("caller matched only file-read intrinsics"),
                };
                let function_ref = self
                    .jit
                    .declare_func_in_func(self.runtime_ids[symbol], builder.func);
                builder.ins().call(
                    function_ref,
                    &[
                        context,
                        path_value,
                        destination_address,
                        total_size,
                        ok_tag,
                        error_tag,
                        ok_offset,
                        error_offset,
                        kind_offset,
                        oscode_offset,
                        tags_pointer,
                    ],
                );
            }
            mir::Intrinsic::FileListFiles(_) | mir::Intrinsic::FileListFilesTemporary(_) => {
                let [directory] = arguments else {
                    return Err(BackendError::new(
                        "aster.io.ListFiles requires exactly one argument",
                    ));
                };
                let directory_value = self.translate_operand(builder, directory, state)?;
                let symbol = match intrinsic {
                    mir::Intrinsic::FileListFiles(_) => "aster_rt_io_list_files",
                    mir::Intrinsic::FileListFilesTemporary(_) => "aster_rt_io_list_files_temporary",
                    _ => unreachable!("caller matched only file-list intrinsics"),
                };
                let function_ref = self
                    .jit
                    .declare_func_in_func(self.runtime_ids[symbol], builder.func);
                builder.ins().call(
                    function_ref,
                    &[
                        context,
                        directory_value,
                        destination_address,
                        total_size,
                        ok_tag,
                        error_tag,
                        ok_offset,
                        error_offset,
                        kind_offset,
                        oscode_offset,
                        tags_pointer,
                    ],
                );
            }
            mir::Intrinsic::FileWriteAllText(_) => {
                let [path, content] = arguments else {
                    return Err(BackendError::new(
                        "aster.io.WriteAllText requires exactly two arguments",
                    ));
                };
                let path_value = self.translate_operand(builder, path, state)?;
                let content_value = self.translate_operand(builder, content, state)?;
                let function_ref = self.jit.declare_func_in_func(
                    self.runtime_ids["aster_rt_io_write_all_text"],
                    builder.func,
                );
                builder.ins().call(
                    function_ref,
                    &[
                        context,
                        path_value,
                        content_value,
                        destination_address,
                        total_size,
                        ok_tag,
                        error_tag,
                        ok_offset,
                        error_offset,
                        kind_offset,
                        oscode_offset,
                        tags_pointer,
                    ],
                );
            }
            _ => unreachable!("caller matched only file-io intrinsics"),
        }
        Ok(())
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
        self.continue_if_runtime_ok(builder, state)?;
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
        self.continue_if_runtime_ok(builder, state)?;
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
        self.continue_if_runtime_ok(builder, state)?;
        self.store_scalar(builder, destination, list, state)
    }

    pub(super) fn translate_dictionary_allocation(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        destination: &mir::Place,
        key_type: &mir::Type,
        value_type: &mir::Type,
        region: mir::AllocationRegion,
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        let symbol = match region {
            mir::AllocationRegion::Persistent => "aster_rt_dictionary_new",
            mir::AllocationRegion::Temporary => "aster_rt_dictionary_new_temporary",
        };
        let function_ref = self
            .jit
            .declare_func_in_func(self.runtime_ids[symbol], builder.func);
        let context = state.execution_context.ok_or_else(|| {
            BackendError::new("dictionary allocation is missing its ExecutionContext")
        })?;
        let key = self.layouts.type_layout(key_type)?;
        let value = self.layouts.type_layout(value_type)?;
        let key_kind = builder
            .ins()
            .iconst(types::I32, dictionary_key_kind(key_type)?);
        let key_size = builder.ins().iconst(types::I32, i64::from(key.size));
        let key_align = builder
            .ins()
            .iconst(types::I32, i64::from(1_u32 << key.align_shift));
        let value_size = builder.ins().iconst(types::I32, i64::from(value.size));
        let value_align = builder
            .ins()
            .iconst(types::I32, i64::from(1_u32 << value.align_shift));
        #[allow(clippy::cast_possible_wrap)]
        let key_type_key = builder
            .ins()
            .iconst(types::I64, mir::type_key(key_type) as i64);
        #[allow(clippy::cast_possible_wrap)]
        let value_type_key = builder
            .ins()
            .iconst(types::I64, mir::type_key(value_type) as i64);
        let call = builder.ins().call(
            function_ref,
            &[
                context,
                key_kind,
                key_size,
                key_align,
                key_type_key,
                value_size,
                value_align,
                value_type_key,
            ],
        );
        let dictionary = builder.inst_results(call)[0];
        self.continue_if_runtime_ok(builder, state)?;
        self.store_scalar(builder, destination, dictionary, state)
    }

    fn dictionary_operand_address(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        operand: &mir::Operand,
        state: &FunctionState,
    ) -> Result<Value, BackendError> {
        if is_aggregate(&operand.type_) {
            return self.translate_operand(builder, operand, state);
        }
        let layout = self.layouts.type_layout(&operand.type_)?;
        let value = self.translate_operand(builder, operand, state)?;
        let slot = builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            layout.size,
            layout.align_shift,
        ));
        builder.ins().stack_store(value, slot, 0);
        Ok(builder.ins().stack_addr(self.pointer_type, slot, 0))
    }

    fn dictionary_runtime_metadata(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        key_type: &mir::Type,
        value_type: &mir::Type,
    ) -> Result<[Value; 7], BackendError> {
        let key = self.layouts.type_layout(key_type)?;
        let value = self.layouts.type_layout(value_type)?;
        let key_kind = builder
            .ins()
            .iconst(types::I32, dictionary_key_kind(key_type)?);
        let key_size = builder.ins().iconst(types::I32, i64::from(key.size));
        let key_align = builder
            .ins()
            .iconst(types::I32, i64::from(1_u32 << key.align_shift));
        #[allow(clippy::cast_possible_wrap)]
        let key_type_key = builder
            .ins()
            .iconst(types::I64, mir::type_key(key_type) as i64);
        let value_size = builder.ins().iconst(types::I32, i64::from(value.size));
        let value_align = builder
            .ins()
            .iconst(types::I32, i64::from(1_u32 << value.align_shift));
        #[allow(clippy::cast_possible_wrap)]
        let value_type_key = builder
            .ins()
            .iconst(types::I64, mir::type_key(value_type) as i64);
        Ok([
            key_kind,
            key_size,
            key_align,
            key_type_key,
            value_size,
            value_align,
            value_type_key,
        ])
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn translate_dictionary_add_or_set(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        destination: &mir::Place,
        dictionary: &mir::Operand,
        key: &mir::Operand,
        value: &mir::Operand,
        replace: bool,
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        let mir::Type::Dictionary(key_type, value_type) = &dictionary.type_ else {
            return Err(BackendError::new(
                "Dictionary Add/Set receiver is not Dictionary<K, V>",
            ));
        };
        let dictionary_value = self.translate_operand(builder, dictionary, state)?;
        let key_address = self.dictionary_operand_address(builder, key, state)?;
        let value_address = self.dictionary_operand_address(builder, value, state)?;
        let metadata = self.dictionary_runtime_metadata(builder, key_type, value_type)?;
        let context = state.execution_context.ok_or_else(|| {
            BackendError::new("Dictionary Add/Set is missing its ExecutionContext")
        })?;
        let symbol = if replace {
            "aster_rt_dictionary_set"
        } else {
            "aster_rt_dictionary_add"
        };
        let function_ref = self
            .jit
            .declare_func_in_func(self.runtime_ids[symbol], builder.func);
        let mut arguments = vec![context, dictionary_value];
        arguments.extend(metadata);
        arguments.extend([key_address, value_address]);
        let call = builder.ins().call(function_ref, &arguments);
        self.store_scalar(builder, destination, builder.inst_results(call)[0], state)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn translate_dictionary_contains_or_remove(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        destination: &mir::Place,
        dictionary: &mir::Operand,
        key: &mir::Operand,
        remove: bool,
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        let mir::Type::Dictionary(key_type, value_type) = &dictionary.type_ else {
            return Err(BackendError::new(
                "Dictionary ContainsKey/Remove receiver is not Dictionary<K, V>",
            ));
        };
        let dictionary_value = self.translate_operand(builder, dictionary, state)?;
        let key_address = self.dictionary_operand_address(builder, key, state)?;
        let metadata = self.dictionary_runtime_metadata(builder, key_type, value_type)?;
        let context = state.execution_context.ok_or_else(|| {
            BackendError::new("Dictionary operation is missing its ExecutionContext")
        })?;
        let symbol = if remove {
            "aster_rt_dictionary_remove"
        } else {
            "aster_rt_dictionary_contains_key"
        };
        let function_ref = self
            .jit
            .declare_func_in_func(self.runtime_ids[symbol], builder.func);
        let mut arguments = vec![context, dictionary_value];
        arguments.extend(metadata);
        arguments.push(key_address);
        let call = builder.ins().call(function_ref, &arguments);
        self.store_scalar(builder, destination, builder.inst_results(call)[0], state)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn translate_dictionary_try_get(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        destination: &mir::Place,
        dictionary: &mir::Operand,
        key: &mir::Operand,
        value_type: &mir::Type,
        option_layout: mir::DictionaryOptionLayout,
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        let mir::Type::Dictionary(key_type, dictionary_value_type) = &dictionary.type_ else {
            return Err(BackendError::new(
                "Dictionary.TryGet receiver is not Dictionary<K, V>",
            ));
        };
        if dictionary_value_type.as_ref() != value_type {
            return Err(BackendError::new(
                "Dictionary.TryGet value metadata disagrees with its receiver",
            ));
        }
        let (option_type, payload_offset) =
            self.dictionary_option_layout(value_type, option_layout)?;
        let option_type = mir::Type::Enum(option_type);
        let total_size = self.layouts.type_layout(&option_type)?.size;
        let dictionary_value = self.translate_operand(builder, dictionary, state)?;
        let key_address = self.dictionary_operand_address(builder, key, state)?;
        let destination_address = self.place_address(builder, destination, state)?;
        let metadata =
            self.dictionary_runtime_metadata(builder, key_type, dictionary_value_type)?;
        let context = state.execution_context.ok_or_else(|| {
            BackendError::new("Dictionary.TryGet is missing its ExecutionContext")
        })?;
        let function_ref = self.jit.declare_func_in_func(
            self.runtime_ids["aster_rt_dictionary_try_get"],
            builder.func,
        );
        let mut arguments = vec![context, dictionary_value];
        arguments.extend(metadata);
        arguments.extend([
            key_address,
            destination_address,
            builder.ins().iconst(types::I32, i64::from(total_size)),
            builder
                .ins()
                .iconst(types::I32, i64::from(option_layout.some_tag)),
            builder
                .ins()
                .iconst(types::I32, i64::from(option_layout.none_tag)),
            builder.ins().iconst(types::I32, i64::from(payload_offset)),
        ]);
        builder.ins().call(function_ref, &arguments);
        Ok(())
    }

    fn dictionary_option_layout(
        &self,
        value_type: &mir::Type,
        layout: mir::DictionaryOptionLayout,
    ) -> Result<(mir::SymbolId, u32), BackendError> {
        for (symbol, definition) in &self.layouts.enums {
            let some = definition.cases.iter().find(|case| {
                case.symbol == layout.some_case
                    && case.tag == layout.some_tag
                    && case.fields.len() == 1
                    && case.fields[0].symbol == layout.some_field
                    && case.fields[0].type_ == *value_type
            });
            let none = definition.cases.iter().find(|case| {
                case.symbol == layout.none_case
                    && case.tag == layout.none_tag
                    && case.fields.is_empty()
            });
            if some.is_some() && none.is_some() {
                let field = self.layouts.fields.get(&layout.some_field).ok_or_else(|| {
                    BackendError::new("Dictionary.TryGet payload has no computed layout")
                })?;
                return Ok((*symbol, field.offset));
            }
        }
        Err(BackendError::new(
            "Dictionary.TryGet carries invalid nominal Option metadata",
        ))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn translate_dictionary_entries(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        destination: &mir::Place,
        dictionary: &mir::Operand,
        key_type: &mir::Type,
        value_type: &mir::Type,
        entry_type: &mir::Type,
        entry_layout: mir::DictionaryEntryLayout,
        region: mir::AllocationRegion,
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        let mir::Type::Dictionary(receiver_key, receiver_value) = &dictionary.type_ else {
            return Err(BackendError::new(
                "Dictionary.Entries receiver is not Dictionary<K, V>",
            ));
        };
        if receiver_key.as_ref() != key_type || receiver_value.as_ref() != value_type {
            return Err(BackendError::new(
                "Dictionary.Entries metadata disagrees with its receiver",
            ));
        }
        let mir::Type::User(entry_symbol) = entry_type else {
            return Err(BackendError::new(
                "Dictionary.Entries entry type is not a concrete struct",
            ));
        };
        let entry_definition = self.layouts.structs.get(entry_symbol).ok_or_else(|| {
            BackendError::new("Dictionary.Entries entry type is not defined by the module")
        })?;
        if !entry_definition
            .fields
            .iter()
            .any(|field| field.symbol == entry_layout.key_field)
            || !entry_definition
                .fields
                .iter()
                .any(|field| field.symbol == entry_layout.value_field)
        {
            return Err(BackendError::new(
                "Dictionary.Entries fields do not belong to its nominal entry type",
            ));
        }
        let key_field = self
            .layouts
            .fields
            .get(&entry_layout.key_field)
            .ok_or_else(|| BackendError::new("DictionaryEntry.Key has no computed layout"))?;
        let value_field = self
            .layouts
            .fields
            .get(&entry_layout.value_field)
            .ok_or_else(|| BackendError::new("DictionaryEntry.Value has no computed layout"))?;
        if key_field.type_ != *key_type || value_field.type_ != *value_type {
            return Err(BackendError::new(
                "Dictionary.Entries carries incompatible field metadata",
            ));
        }
        let entry_size = self.layouts.type_layout(entry_type)?.size;
        let key_offset = key_field.offset;
        let value_offset = value_field.offset;
        let dictionary_value = self.translate_operand(builder, dictionary, state)?;
        let metadata = self.dictionary_runtime_metadata(builder, key_type, value_type)?;
        let context = state.execution_context.ok_or_else(|| {
            BackendError::new("Dictionary.Entries is missing its ExecutionContext")
        })?;
        let function_ref = self.jit.declare_func_in_func(
            self.runtime_ids["aster_rt_dictionary_entries"],
            builder.func,
        );
        let mut arguments = vec![context, dictionary_value];
        arguments.extend(metadata);
        arguments.extend([
            builder.ins().iconst(types::I32, i64::from(entry_size)),
            builder.ins().iconst(types::I32, i64::from(key_offset)),
            builder.ins().iconst(types::I32, i64::from(value_offset)),
            builder.ins().iconst(
                types::I8,
                i64::from(region == mir::AllocationRegion::Temporary),
            ),
        ]);
        let call = builder.ins().call(function_ref, &arguments);
        let entries = builder.inst_results(call)[0];
        self.continue_if_runtime_ok(builder, state)?;
        self.store_scalar(builder, destination, entries, state)
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

    /// `foreach`'s cursor decode over `string`: the decoded scalar and the
    /// next cursor are always plain scalars (`char`/`int`, never
    /// aggregates), so each gets its own small stack slot -- never the
    /// string's own buffer address -- reloaded and stored into its declared
    /// destination place. The call's own boolean result is stored into a
    /// third destination the generated CFG branches on immediately
    /// afterward, rather than assuming the loop should keep going.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn translate_string_decode_next(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        string: &mir::Operand,
        cursor: &mir::Operand,
        char_destination: &mir::Place,
        next_cursor_destination: &mir::Place,
        ok_destination: &mir::Place,
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        let string_value = self.translate_operand(builder, string, state)?;
        let cursor_value = self.translate_operand(builder, cursor, state)?;
        let scalar_slot =
            builder.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, 4, 2));
        let next_cursor_slot =
            builder.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, 4, 2));
        let scalar_address = builder.ins().stack_addr(self.pointer_type, scalar_slot, 0);
        let next_cursor_address = builder
            .ins()
            .stack_addr(self.pointer_type, next_cursor_slot, 0);
        let function_ref = self.jit.declare_func_in_func(
            self.runtime_ids["aster_rt_string_decode_next"],
            builder.func,
        );
        let context = state.execution_context.ok_or_else(|| {
            BackendError::new("string foreach decode is missing its ExecutionContext")
        })?;
        let call = builder.ins().call(
            function_ref,
            &[
                context,
                string_value,
                cursor_value,
                scalar_address,
                next_cursor_address,
            ],
        );
        let ok = builder.inst_results(call)[0];
        let scalar = builder
            .ins()
            .load(types::I32, MemFlags::new(), scalar_address, 0);
        let next_cursor = builder
            .ins()
            .load(types::I32, MemFlags::new(), next_cursor_address, 0);
        self.store_scalar(builder, char_destination, scalar, state)?;
        self.store_scalar(builder, next_cursor_destination, next_cursor, state)?;
        self.store_scalar(builder, ok_destination, ok, state)?;
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

fn dictionary_key_kind(type_: &mir::Type) -> Result<i64, BackendError> {
    let kind = match type_ {
        mir::Type::Bool => aster_runtime::DictionaryKeyKind::Bool,
        mir::Type::Char => aster_runtime::DictionaryKeyKind::Char,
        mir::Type::SByte => aster_runtime::DictionaryKeyKind::SByte,
        mir::Type::Byte => aster_runtime::DictionaryKeyKind::Byte,
        mir::Type::Short => aster_runtime::DictionaryKeyKind::Short,
        mir::Type::UShort => aster_runtime::DictionaryKeyKind::UShort,
        mir::Type::Int => aster_runtime::DictionaryKeyKind::Int,
        mir::Type::UInt => aster_runtime::DictionaryKeyKind::UInt,
        mir::Type::Long => aster_runtime::DictionaryKeyKind::Long,
        mir::Type::ULong => aster_runtime::DictionaryKeyKind::ULong,
        mir::Type::String => aster_runtime::DictionaryKeyKind::String,
        _ => {
            return Err(BackendError::new(
                "unsupported concrete Dictionary key type reached codegen",
            ));
        }
    };
    Ok(i64::from(kind as u8))
}
