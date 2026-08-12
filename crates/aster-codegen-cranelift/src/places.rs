use super::{
    BackendError, Codegen, FunctionBuilder, FunctionState, InstBuilder, IntCC, MemFlags, Module,
    Value, is_aggregate, mir, types,
};
use aster_runtime::{ASTER_ARRAY_DATA_OFFSET, ASTER_ARRAY_LENGTH_OFFSET};

impl Codegen {
    pub(super) fn assign_rvalue(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        target: &mir::Place,
        value: &mir::Rvalue,
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        if !is_aggregate(&value.type_) {
            let value = self.translate_rvalue(builder, value, state)?;
            return self.store_scalar(builder, target, value, state);
        }
        let destination = self.place_address(builder, target, state)?;
        match &value.kind {
            mir::RvalueKind::Use(operand) => {
                let source = self.translate_operand(builder, operand, state)?;
                self.copy_value(builder, &value.type_, source, destination)
            }
            mir::RvalueKind::Aggregate(fields) => {
                for field in fields {
                    let layout = self
                        .layouts
                        .fields
                        .get(&field.field)
                        .cloned()
                        .ok_or_else(|| BackendError::new("unknown struct field in MIR"))?;
                    let address = if layout.offset == 0 {
                        destination
                    } else {
                        builder
                            .ins()
                            .iadd_imm(destination, i64::from(layout.offset))
                    };
                    self.assign_operand_to_address(
                        builder,
                        &layout.type_,
                        &field.value,
                        address,
                        state,
                    )?;
                }
                Ok(())
            }
            mir::RvalueKind::EnumConstruct { tag, fields, .. } => {
                let tag = builder.ins().iconst(types::I32, i64::from(*tag));
                builder.ins().store(MemFlags::new(), tag, destination, 0);
                for field in fields {
                    let layout = self
                        .layouts
                        .fields
                        .get(&field.field)
                        .cloned()
                        .ok_or_else(|| BackendError::new("unknown enum payload field in MIR"))?;
                    let address = builder
                        .ins()
                        .iadd_imm(destination, i64::from(layout.offset));
                    self.assign_operand_to_address(
                        builder,
                        &layout.type_,
                        &field.value,
                        address,
                        state,
                    )?;
                }
                Ok(())
            }
            mir::RvalueKind::MakeInterface {
                object,
                class,
                interface,
            } => {
                let object = self.translate_operand(builder, object, state)?;
                builder.ins().store(MemFlags::new(), object, destination, 0);
                let table = self
                    .interface_tables
                    .get(&(*class, *interface))
                    .copied()
                    .ok_or_else(|| BackendError::new("interface conversion has no method table"))?;
                let global = self.jit.declare_data_in_func(table, builder.func);
                let table = builder.ins().global_value(self.pointer_type, global);
                builder.ins().store(
                    MemFlags::new(),
                    table,
                    destination,
                    i32::try_from(self.pointer_type.bytes())
                        .map_err(|_| BackendError::new("pointer offset is too large"))?,
                );
                Ok(())
            }
            _ => Err(BackendError::new(
                "aggregate values only support construction and value copies",
            )),
        }
    }

    fn assign_operand_to_address(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        type_: &mir::Type,
        operand: &mir::Operand,
        destination: Value,
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        let value = self.translate_operand(builder, operand, state)?;
        if is_aggregate(type_) {
            self.copy_value(builder, type_, value, destination)
        } else {
            builder.ins().store(MemFlags::new(), value, destination, 0);
            Ok(())
        }
    }

    pub(super) fn copy_value(
        &self,
        builder: &mut FunctionBuilder<'_>,
        type_: &mir::Type,
        source: Value,
        destination: Value,
    ) -> Result<(), BackendError> {
        if matches!(type_, mir::Type::Interface(_)) {
            for offset in [0_i64, i64::from(self.pointer_type.bytes())] {
                let source = if offset == 0 {
                    source
                } else {
                    builder.ins().iadd_imm(source, offset)
                };
                let destination = if offset == 0 {
                    destination
                } else {
                    builder.ins().iadd_imm(destination, offset)
                };
                let value = builder
                    .ins()
                    .load(self.pointer_type, MemFlags::new(), source, 0);
                builder.ins().store(MemFlags::new(), value, destination, 0);
            }
            return Ok(());
        }
        if let mir::Type::Enum(symbol) = type_ {
            let definition = self
                .layouts
                .enums
                .get(symbol)
                .cloned()
                .ok_or_else(|| BackendError::new("unknown enum type in MIR"))?;
            let tag = builder.ins().load(types::I32, MemFlags::new(), source, 0);
            builder.ins().store(MemFlags::new(), tag, destination, 0);
            let join = builder.create_block();
            for case in definition.cases {
                let copy = builder.create_block();
                let next = builder.create_block();
                let active = builder
                    .ins()
                    .icmp_imm(IntCC::Equal, tag, i64::from(case.tag));
                builder.ins().brif(active, copy, &[], next, &[]);
                builder.switch_to_block(copy);
                for field in case.fields {
                    let layout = self.layouts.fields[&field.symbol].clone();
                    let left = builder.ins().iadd_imm(source, i64::from(layout.offset));
                    let right = builder
                        .ins()
                        .iadd_imm(destination, i64::from(layout.offset));
                    self.copy_value(builder, &field.type_, left, right)?;
                }
                builder.ins().jump(join, &[]);
                builder.switch_to_block(next);
            }
            builder.ins().jump(join, &[]);
            builder.switch_to_block(join);
            return Ok(());
        }
        let mir::Type::User(symbol) = type_ else {
            let clif_type = self.clif_value_type(type_)?;
            let value = builder.ins().load(clif_type, MemFlags::new(), source, 0);
            builder.ins().store(MemFlags::new(), value, destination, 0);
            return Ok(());
        };
        let definition = self
            .layouts
            .structs
            .get(symbol)
            .cloned()
            .ok_or_else(|| BackendError::new("unknown struct type in MIR"))?;
        for field in definition.fields {
            let layout =
                self.layouts.fields.get(&field.symbol).ok_or_else(|| {
                    BackendError::new("struct field has no computed runtime layout")
                })?;
            let (source, destination) = if layout.offset == 0 {
                (source, destination)
            } else {
                (
                    builder.ins().iadd_imm(source, i64::from(layout.offset)),
                    builder
                        .ins()
                        .iadd_imm(destination, i64::from(layout.offset)),
                )
            };
            self.copy_value(builder, &field.type_, source, destination)?;
        }
        Ok(())
    }

    pub(super) fn place_address(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        place: &mir::Place,
        state: &FunctionState,
    ) -> Result<Value, BackendError> {
        match place {
            mir::Place::Local(local) => {
                let slot = state
                    .slots
                    .get(local)
                    .copied()
                    .ok_or_else(|| BackendError::new("MIR references an unknown local"))?;
                Ok(builder.ins().stack_addr(self.pointer_type, slot, 0))
            }
            mir::Place::Field { base, field } => {
                let base = self.place_address(builder, base, state)?;
                let field =
                    self.layouts.fields.get(field).ok_or_else(|| {
                        BackendError::new("MIR references an unknown struct field")
                    })?;
                if field.offset == 0 {
                    Ok(base)
                } else {
                    Ok(builder.ins().iadd_imm(base, i64::from(field.offset)))
                }
            }
            mir::Place::EnumField { base, field, .. } => {
                let base = self.place_address(builder, base, state)?;
                let field = self
                    .layouts
                    .fields
                    .get(field)
                    .ok_or_else(|| BackendError::new("MIR references an unknown enum field"))?;
                Ok(builder.ins().iadd_imm(base, i64::from(field.offset)))
            }
            mir::Place::Index {
                array,
                index,
                element_type,
            } => self.array_element_address(builder, array, index, element_type, state),
            mir::Place::ObjectField { object, field } => {
                let object = self.translate_operand(builder, object, state)?;
                let field =
                    self.layouts.fields.get(field).ok_or_else(|| {
                        BackendError::new("MIR references an unknown object field")
                    })?;
                if field.offset == 0 {
                    Ok(object)
                } else {
                    Ok(builder.ins().iadd_imm(object, i64::from(field.offset)))
                }
            }
            mir::Place::Symbol(_) => Err(BackendError::new(
                "module storage is not executable in the current JIT",
            )),
        }
    }

    fn array_element_address(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        array: &mir::Operand,
        index: &mir::Operand,
        element_type: &mir::Type,
        state: &FunctionState,
    ) -> Result<Value, BackendError> {
        let context = state
            .execution_context
            .ok_or_else(|| BackendError::new("array access is missing its ExecutionContext"))?;
        let array_length_offset = i32::try_from(ASTER_ARRAY_LENGTH_OFFSET)
            .map_err(|_| BackendError::new("array header length offset is too large"))?;
        let array_data_offset = i32::try_from(ASTER_ARRAY_DATA_OFFSET)
            .map_err(|_| BackendError::new("array header data offset is too large"))?;
        let array = self.translate_operand(builder, array, state)?;
        let index = self.translate_operand(builder, index, state)?;
        let in_bounds = builder.create_block();
        let out_of_bounds = builder.create_block();
        let join = builder.create_block();
        builder.append_block_param(join, self.pointer_type);

        let non_negative = builder
            .ins()
            .icmp_imm(IntCC::SignedGreaterThanOrEqual, index, 0);
        let length = builder
            .ins()
            .load(types::I32, MemFlags::new(), array, array_length_offset);
        let before_end = builder.ins().icmp(IntCC::SignedLessThan, index, length);
        let valid = builder.ins().band(non_negative, before_end);
        builder
            .ins()
            .brif(valid, in_bounds, &[], out_of_bounds, &[]);

        builder.switch_to_block(out_of_bounds);
        let function_ref = self
            .jit
            .declare_func_in_func(self.runtime_ids["aster_rt_array_element"], builder.func);
        let call = builder.ins().call(function_ref, &[context, array, index]);
        let address = builder.inst_results(call)[0];
        builder.ins().jump(join, &[address.into()]);

        builder.switch_to_block(in_bounds);
        let data = builder
            .ins()
            .load(self.pointer_type, MemFlags::new(), array, array_data_offset);
        let index = builder.ins().uextend(self.pointer_type, index);
        let stride = i64::from(self.layouts.type_layout(element_type)?.size);
        let offset = if stride == 1 {
            index
        } else {
            builder.ins().imul_imm(index, stride)
        };
        let address = builder.ins().iadd(data, offset);
        builder.ins().jump(join, &[address.into()]);

        builder.switch_to_block(join);
        Ok(builder.block_params(join)[0])
    }

    pub(super) fn store_scalar(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        place: &mir::Place,
        value: Value,
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        let address = self.place_address(builder, place, state)?;
        builder.ins().store(MemFlags::new(), value, address, 0);
        Ok(())
    }
}
