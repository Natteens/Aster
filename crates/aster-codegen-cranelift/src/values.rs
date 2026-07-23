use super::{
    BackendError, ClifType, Codegen, DataDescription, DataId, FloatCC, FunctionBuilder,
    FunctionState, InstBuilder, IntCC, MemFlags, Module, Ordering, Primitive, Value, mir,
    module_error, types,
};

impl Codegen {
    #[allow(clippy::too_many_lines)]
    pub(super) fn translate_rvalue(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        value: &mir::Rvalue,
        state: &FunctionState,
    ) -> Result<Value, BackendError> {
        match &value.kind {
            mir::RvalueKind::Use(operand) => self.translate_operand(builder, operand, state),
            mir::RvalueKind::Aggregate(_) => Err(BackendError::new(
                "aggregate rvalue reached scalar translation",
            )),
            mir::RvalueKind::EnumConstruct { .. } => Err(BackendError::new(
                "enum construction reached scalar translation",
            )),
            mir::RvalueKind::Discriminant(operand) => {
                let address = self.translate_operand(builder, operand, state)?;
                Ok(builder.ins().load(types::I32, MemFlags::new(), address, 0))
            }
            mir::RvalueKind::MakeInterface { .. } => Err(BackendError::new(
                "interface conversion reached scalar translation",
            )),
            mir::RvalueKind::ArrayLength(array) => {
                let function_ref = self
                    .jit
                    .declare_func_in_func(self.runtime_ids["aster_rt_array_length"], builder.func);
                let context = state.execution_context.ok_or_else(|| {
                    BackendError::new("array length is missing its ExecutionContext")
                })?;
                let array = self.translate_operand(builder, array, state)?;
                let call = builder.ins().call(function_ref, &[context, array]);
                Ok(builder.inst_results(call)[0])
            }
            mir::RvalueKind::ListLength(list) => {
                let function_ref = self
                    .jit
                    .declare_func_in_func(self.runtime_ids["aster_rt_list_length"], builder.func);
                let context = state.execution_context.ok_or_else(|| {
                    BackendError::new("list length is missing its ExecutionContext")
                })?;
                let list = self.translate_operand(builder, list, state)?;
                let call = builder.ins().call(function_ref, &[context, list]);
                Ok(builder.inst_results(call)[0])
            }
            mir::RvalueKind::ListVersion(list) => {
                let function_ref = self
                    .jit
                    .declare_func_in_func(self.runtime_ids["aster_rt_list_version"], builder.func);
                let context = state.execution_context.ok_or_else(|| {
                    BackendError::new("list version is missing its ExecutionContext")
                })?;
                let list = self.translate_operand(builder, list, state)?;
                let call = builder.ins().call(function_ref, &[context, list]);
                Ok(builder.inst_results(call)[0])
            }
            mir::RvalueKind::StringByteLength(value) => {
                let function_ref = self.jit.declare_func_in_func(
                    self.runtime_ids["aster_rt_string_byte_length"],
                    builder.func,
                );
                let context = state.execution_context.ok_or_else(|| {
                    BackendError::new("string byte length is missing its ExecutionContext")
                })?;
                let value = self.translate_operand(builder, value, state)?;
                let call = builder.ins().call(function_ref, &[context, value]);
                Ok(builder.inst_results(call)[0])
            }
            mir::RvalueKind::Cast(operand) => {
                let source = operand.type_.clone();
                let operand = self.translate_operand(builder, operand, state)?;
                cast_value(builder, &source, &value.type_, operand)
            }
            mir::RvalueKind::Unary { operator, operand } => {
                let is_float = matches!(operand.type_, mir::Type::Float | mir::Type::Double);
                let operand = self.translate_operand(builder, operand, state)?;
                Ok(match operator {
                    mir::UnaryOperator::Not => builder.ins().icmp_imm(IntCC::Equal, operand, 0),
                    mir::UnaryOperator::Negate if is_float => builder.ins().fneg(operand),
                    mir::UnaryOperator::Negate => builder.ins().ineg(operand),
                })
            }
            mir::RvalueKind::Binary {
                left,
                operator,
                right,
            } => {
                let operand_type = left.type_.clone();
                let left = self.translate_operand(builder, left, state)?;
                let right = self.translate_operand(builder, right, state)?;
                if matches!(
                    operator,
                    mir::BinaryOperator::Divide | mir::BinaryOperator::Remainder
                ) && !matches!(operand_type, mir::Type::Float | mir::Type::Double)
                {
                    self.translate_integer_division_or_remainder(
                        builder,
                        *operator,
                        &operand_type,
                        left,
                        right,
                        state,
                    )
                } else {
                    translate_binary(builder, *operator, &operand_type, left, right)
                }
            }
            mir::RvalueKind::Equality {
                left,
                right,
                negated,
            } => {
                let left_address = self.translate_operand(builder, left, state)?;
                let right_address = self.translate_operand(builder, right, state)?;
                let equal =
                    self.compare_value_at(builder, &left.type_, left_address, right_address)?;
                Ok(if *negated {
                    builder.ins().icmp_imm(IntCC::Equal, equal, 0)
                } else {
                    equal
                })
            }
        }
    }

    fn translate_integer_division_or_remainder(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        operator: mir::BinaryOperator,
        operand_type: &mir::Type,
        left: Value,
        right: Value,
        state: &FunctionState,
    ) -> Result<Value, BackendError> {
        const DIVISION_BY_ZERO: i64 = 0;
        const REMAINDER_BY_ZERO: i64 = 1;
        const DIVISION_OVERFLOW: i64 = 2;
        const REMAINDER_OVERFLOW: i64 = 3;

        let primitive = primitive(operand_type)
            .ok_or_else(|| BackendError::new("integer operation has a non-primitive type"))?;
        if !primitive.is_integer() && primitive != Primitive::Char {
            return Err(BackendError::new(
                "integer operation has a non-integer operand type",
            ));
        }
        let (zero_code, overflow_code) = match operator {
            mir::BinaryOperator::Divide => (DIVISION_BY_ZERO, DIVISION_OVERFLOW),
            mir::BinaryOperator::Remainder => (REMAINDER_BY_ZERO, REMAINDER_OVERFLOW),
            _ => {
                return Err(BackendError::new(
                    "integer division guard received an invalid operator",
                ));
            }
        };
        let context = state.execution_context.ok_or_else(|| {
            BackendError::new("integer operation is missing its ExecutionContext")
        })?;
        let reporter = self.jit.declare_func_in_func(
            self.runtime_ids["aster_rt_integer_arithmetic_error"],
            builder.func,
        );
        let value_type = builder.func.dfg.value_type(left);
        let zero_error = builder.create_block();
        let nonzero = builder.create_block();
        let valid = builder.create_block();
        let join = builder.create_block();
        builder.append_block_param(join, value_type);

        let right_is_zero = builder.ins().icmp_imm(IntCC::Equal, right, 0);
        builder
            .ins()
            .brif(right_is_zero, zero_error, &[], nonzero, &[]);

        builder.switch_to_block(zero_error);
        let zero_code = builder.ins().iconst(types::I32, zero_code);
        builder.ins().call(reporter, &[context, zero_code]);
        let dummy = builder.ins().iconst(value_type, 0);
        builder.ins().jump(join, &[dummy.into()]);

        builder.switch_to_block(nonzero);
        let unsigned = is_unsigned_integer(operand_type);
        if unsigned {
            builder.ins().jump(valid, &[]);
        } else {
            let overflow_error = builder.create_block();
            let minimum = signed_integer_minimum(operand_type)?;
            let left_is_minimum = builder.ins().icmp_imm(IntCC::Equal, left, minimum);
            let right_is_negative_one = builder.ins().icmp_imm(IntCC::Equal, right, -1);
            let overflows = builder.ins().band(left_is_minimum, right_is_negative_one);
            builder
                .ins()
                .brif(overflows, overflow_error, &[], valid, &[]);

            builder.switch_to_block(overflow_error);
            let overflow_code = builder.ins().iconst(types::I32, overflow_code);
            builder.ins().call(reporter, &[context, overflow_code]);
            let dummy = builder.ins().iconst(value_type, 0);
            builder.ins().jump(join, &[dummy.into()]);
        }

        builder.switch_to_block(valid);
        let result = match (operator, unsigned) {
            (mir::BinaryOperator::Divide, true) => builder.ins().udiv(left, right),
            (mir::BinaryOperator::Divide, false) => builder.ins().sdiv(left, right),
            (mir::BinaryOperator::Remainder, true) => builder.ins().urem(left, right),
            (mir::BinaryOperator::Remainder, false) => builder.ins().srem(left, right),
            _ => {
                return Err(BackendError::new(
                    "integer division guard received an invalid operator",
                ));
            }
        };
        builder.ins().jump(join, &[result.into()]);
        builder.switch_to_block(join);
        Ok(builder.block_params(join)[0])
    }

    #[allow(clippy::too_many_lines)]
    fn compare_value_at(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        type_: &mir::Type,
        left_address: Value,
        right_address: Value,
    ) -> Result<Value, BackendError> {
        match type_ {
            mir::Type::Interface(_) => {
                let left = builder
                    .ins()
                    .load(self.pointer_type, MemFlags::new(), left_address, 0);
                let right =
                    builder
                        .ins()
                        .load(self.pointer_type, MemFlags::new(), right_address, 0);
                Ok(builder.ins().icmp(IntCC::Equal, left, right))
            }
            mir::Type::User(symbol) => {
                let definition = self.layouts.structs.get(symbol).cloned().ok_or_else(|| {
                    BackendError::new("struct equality references an unknown layout")
                })?;
                let mut result = builder.ins().iconst(types::I8, 1);
                for field in definition.fields {
                    let layout = self.layouts.fields[&field.symbol].clone();
                    let left = if layout.offset == 0 {
                        left_address
                    } else {
                        builder
                            .ins()
                            .iadd_imm(left_address, i64::from(layout.offset))
                    };
                    let right = if layout.offset == 0 {
                        right_address
                    } else {
                        builder
                            .ins()
                            .iadd_imm(right_address, i64::from(layout.offset))
                    };
                    let field_equal = self.compare_value_at(builder, &layout.type_, left, right)?;
                    result = builder.ins().band(result, field_equal);
                }
                Ok(result)
            }
            mir::Type::Enum(symbol) => {
                let definition =
                    self.layouts.enums.get(symbol).cloned().ok_or_else(|| {
                        BackendError::new("enum equality references unknown layout")
                    })?;
                let left_tag = builder
                    .ins()
                    .load(types::I32, MemFlags::new(), left_address, 0);
                let right_tag = builder
                    .ins()
                    .load(types::I32, MemFlags::new(), right_address, 0);
                let tags_equal = builder.ins().icmp(IntCC::Equal, left_tag, right_tag);
                let tag_match = builder.create_block();
                let unequal = builder.create_block();
                let join = builder.create_block();
                builder.append_block_param(join, types::I8);
                builder.ins().brif(tags_equal, tag_match, &[], unequal, &[]);
                builder.switch_to_block(unequal);
                let false_value = builder.ins().iconst(types::I8, 0);
                builder.ins().jump(join, &[false_value.into()]);
                builder.switch_to_block(tag_match);
                for case in definition.cases {
                    let compare = builder.create_block();
                    let next = builder.create_block();
                    let active =
                        builder
                            .ins()
                            .icmp_imm(IntCC::Equal, left_tag, i64::from(case.tag));
                    builder.ins().brif(active, compare, &[], next, &[]);
                    builder.switch_to_block(compare);
                    let mut result = builder.ins().iconst(types::I8, 1);
                    for field in case.fields {
                        let layout = self.layouts.fields[&field.symbol].clone();
                        let left = builder
                            .ins()
                            .iadd_imm(left_address, i64::from(layout.offset));
                        let right = builder
                            .ins()
                            .iadd_imm(right_address, i64::from(layout.offset));
                        let equal = self.compare_value_at(builder, &field.type_, left, right)?;
                        result = builder.ins().band(result, equal);
                    }
                    builder.ins().jump(join, &[result.into()]);
                    builder.switch_to_block(next);
                }
                let invalid = builder.ins().iconst(types::I8, 0);
                builder.ins().jump(join, &[invalid.into()]);
                builder.switch_to_block(join);
                Ok(builder.block_params(join)[0])
            }
            mir::Type::String => {
                let left = builder
                    .ins()
                    .load(self.pointer_type, MemFlags::new(), left_address, 0);
                let right =
                    builder
                        .ins()
                        .load(self.pointer_type, MemFlags::new(), right_address, 0);
                let function = self
                    .jit
                    .declare_func_in_func(self.runtime_ids["aster_rt_string_eq"], builder.func);
                let call = builder.ins().call(function, &[left, right]);
                Ok(builder.inst_results(call)[0])
            }
            _ => {
                let clif_type = self.clif_value_type(type_)?;
                let left = builder
                    .ins()
                    .load(clif_type, MemFlags::new(), left_address, 0);
                let right = builder
                    .ins()
                    .load(clif_type, MemFlags::new(), right_address, 0);
                translate_binary(builder, mir::BinaryOperator::Equal, type_, left, right)
            }
        }
    }

    pub(super) fn translate_operand(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        operand: &mir::Operand,
        state: &FunctionState,
    ) -> Result<Value, BackendError> {
        match &operand.kind {
            mir::OperandKind::Constant(mir::Constant::Integer(value)) => {
                let clif_type = self.clif_value_type(&operand.type_)?;
                let bits = integer_constant_bits(value, &operand.type_)?;
                Ok(builder.ins().iconst(clif_type, bits))
            }
            mir::OperandKind::Constant(mir::Constant::Float(value)) => {
                if operand.type_ == mir::Type::Double {
                    let value = value.parse::<f64>().map_err(|_| {
                        BackendError::new(format!("`{value}` is not a valid `double` literal"))
                    })?;
                    Ok(builder.ins().f64const(value))
                } else {
                    let value = value.parse::<f32>().map_err(|_| {
                        BackendError::new(format!("`{value}` is not a valid `float` literal"))
                    })?;
                    Ok(builder.ins().f32const(value))
                }
            }
            mir::OperandKind::Constant(mir::Constant::Character(value)) => Ok(builder
                .ins()
                .iconst(types::I32, i64::from(u32::from(*value)))),
            mir::OperandKind::Constant(mir::Constant::Boolean(value)) => {
                Ok(builder.ins().iconst(types::I8, i64::from(*value)))
            }
            mir::OperandKind::Constant(mir::Constant::String(value)) => {
                let data = self.string_literal(value)?;
                let global = self.jit.declare_data_in_func(data, builder.func);
                Ok(builder.ins().global_value(self.pointer_type, global))
            }
            mir::OperandKind::Copy(place) => {
                let address = self.place_address(builder, place, state)?;
                if is_aggregate(&operand.type_) {
                    Ok(address)
                } else {
                    let type_ = self.clif_value_type(&operand.type_)?;
                    Ok(builder.ins().load(type_, MemFlags::new(), address, 0))
                }
            }
            _ => Err(BackendError::new(
                "unsupported operand reached Cranelift after backend validation",
            )),
        }
    }

    /// Intern one string literal as 8-byte-aligned JIT data in the runtime ABI
    /// layout. The data lives exactly as long as the JIT module.
    fn string_literal(&mut self, value: &str) -> Result<DataId, BackendError> {
        if let Some(id) = self.string_data.get(value) {
            return Ok(*id);
        }
        let id = self
            .jit
            .declare_anonymous_data(false, false)
            .map_err(module_error)?;
        let mut description = DataDescription::new();
        description.define(aster_runtime::encode_str(value).into_boxed_slice());
        description.set_align(8);
        self.jit
            .define_data(id, &description)
            .map_err(module_error)?;
        self.string_data.insert(value.to_owned(), id);
        Ok(id)
    }
}

/// The `scalar` kind tag for `type_`, matching the runtime's [`crate::scalar`]
/// constants, so a stored value can be rebuilt as the exact same variant.
pub(super) fn scalar_kind(type_: &mir::Type) -> Result<i64, BackendError> {
    use crate::scalar;
    Ok(i64::from(match type_ {
        mir::Type::Bool => scalar::BOOL,
        mir::Type::SByte => scalar::SBYTE,
        mir::Type::Byte => scalar::BYTE,
        mir::Type::Short => scalar::SHORT,
        mir::Type::UShort => scalar::USHORT,
        mir::Type::Int => scalar::INT,
        mir::Type::UInt => scalar::UINT,
        mir::Type::Long => scalar::LONG,
        mir::Type::ULong => scalar::ULONG,
        mir::Type::Float => scalar::FLOAT,
        mir::Type::Double => scalar::DOUBLE,
        mir::Type::Char => scalar::CHAR,
        _ => {
            return Err(BackendError::new(format!(
                "type `{}` is not a transferable scalar",
                type_name(type_)
            )));
        }
    }))
}

/// Widen `value` (of scalar `type_`) into the 64-bit carrier used by the
/// async/parallel ABI: zero-extend for narrow integers, reinterpret the IEEE
/// bit pattern for floats.
pub(super) fn scalar_to_bits(
    builder: &mut FunctionBuilder<'_>,
    type_: &mir::Type,
    value: Value,
) -> Result<Value, BackendError> {
    Ok(match type_ {
        mir::Type::Bool
        | mir::Type::SByte
        | mir::Type::Byte
        | mir::Type::Short
        | mir::Type::UShort
        | mir::Type::Int
        | mir::Type::UInt
        | mir::Type::Char => builder.ins().uextend(types::I64, value),
        mir::Type::Long | mir::Type::ULong => value,
        mir::Type::Float => {
            let bits = builder.ins().bitcast(types::I32, MemFlags::new(), value);
            builder.ins().uextend(types::I64, bits)
        }
        mir::Type::Double => builder.ins().bitcast(types::I64, MemFlags::new(), value),
        _ => {
            return Err(BackendError::new(format!(
                "type `{}` is not a transferable scalar",
                type_name(type_)
            )));
        }
    })
}

/// Narrow the 64-bit carrier `bits` back to a value of scalar `type_`, the
/// inverse of [`scalar_to_bits`].
pub(super) fn scalar_from_bits(
    builder: &mut FunctionBuilder<'_>,
    type_: &mir::Type,
    bits: Value,
) -> Result<Value, BackendError> {
    Ok(match type_ {
        mir::Type::Bool | mir::Type::SByte | mir::Type::Byte => {
            builder.ins().ireduce(types::I8, bits)
        }
        mir::Type::Short | mir::Type::UShort => builder.ins().ireduce(types::I16, bits),
        mir::Type::Int | mir::Type::UInt | mir::Type::Char => {
            builder.ins().ireduce(types::I32, bits)
        }
        mir::Type::Long | mir::Type::ULong => bits,
        mir::Type::Float => {
            let narrow = builder.ins().ireduce(types::I32, bits);
            builder.ins().bitcast(types::F32, MemFlags::new(), narrow)
        }
        mir::Type::Double => builder.ins().bitcast(types::F64, MemFlags::new(), bits),
        _ => {
            return Err(BackendError::new(format!(
                "type `{}` is not a transferable scalar",
                type_name(type_)
            )));
        }
    })
}

pub(super) fn type_name(type_: &mir::Type) -> &'static str {
    primitive(type_).map_or_else(
        || match type_ {
            mir::Type::Void => "void",
            mir::Type::User(_) => "user type",
            mir::Type::Class(_) => "class",
            mir::Type::Interface(_) => "interface",
            mir::Type::Enum(_) => "enum",
            mir::Type::Array(_) => "array",
            mir::Type::Task(_) => "task",
            mir::Type::List(_) => "list",
            mir::Type::Unknown => "unknown",
            _ => unreachable!("every primitive MIR type has an aster-types adapter"),
        },
        Primitive::name,
    )
}

/// The single adapter between MIR's resolved type representation and the
/// backend-neutral primitive model. Cranelift-specific representation choices
/// remain local to this crate.
pub(super) fn primitive(type_: &mir::Type) -> Option<Primitive> {
    Some(match type_ {
        mir::Type::Bool => Primitive::Bool,
        mir::Type::Char => Primitive::Char,
        mir::Type::SByte => Primitive::SByte,
        mir::Type::Byte => Primitive::Byte,
        mir::Type::Short => Primitive::Short,
        mir::Type::UShort => Primitive::UShort,
        mir::Type::Int => Primitive::Int,
        mir::Type::UInt => Primitive::UInt,
        mir::Type::Long => Primitive::Long,
        mir::Type::ULong => Primitive::ULong,
        mir::Type::Float => Primitive::Float,
        mir::Type::Double => Primitive::Double,
        mir::Type::Decimal => Primitive::Decimal,
        mir::Type::String => Primitive::String,
        mir::Type::Void
        | mir::Type::User(_)
        | mir::Type::Class(_)
        | mir::Type::Interface(_)
        | mir::Type::Enum(_)
        | mir::Type::Array(_)
        | mir::Type::Task(_)
        | mir::Type::List(_)
        | mir::Type::Unknown => {
            return None;
        }
    })
}

pub(super) fn is_aggregate(type_: &mir::Type) -> bool {
    matches!(
        type_,
        mir::Type::User(_) | mir::Type::Interface(_) | mir::Type::Enum(_)
    )
}

fn translate_binary(
    builder: &mut FunctionBuilder<'_>,
    operator: mir::BinaryOperator,
    operand_type: &mir::Type,
    left: Value,
    right: Value,
) -> Result<Value, BackendError> {
    if matches!(operand_type, mir::Type::Float | mir::Type::Double) {
        return translate_float_binary(builder, operator, left, right);
    }
    let unsigned = is_unsigned_integer(operand_type);
    Ok(match operator {
        mir::BinaryOperator::Multiply => builder.ins().imul(left, right),
        mir::BinaryOperator::Divide | mir::BinaryOperator::Remainder => {
            return Err(BackendError::new(
                "integer division or remainder reached codegen without a guard",
            ));
        }
        mir::BinaryOperator::Add => builder.ins().iadd(left, right),
        mir::BinaryOperator::Subtract => builder.ins().isub(left, right),
        mir::BinaryOperator::Less if unsigned => {
            builder.ins().icmp(IntCC::UnsignedLessThan, left, right)
        }
        mir::BinaryOperator::Less => builder.ins().icmp(IntCC::SignedLessThan, left, right),
        mir::BinaryOperator::LessEqual if unsigned => {
            builder
                .ins()
                .icmp(IntCC::UnsignedLessThanOrEqual, left, right)
        }
        mir::BinaryOperator::LessEqual => {
            builder
                .ins()
                .icmp(IntCC::SignedLessThanOrEqual, left, right)
        }
        mir::BinaryOperator::Greater if unsigned => {
            builder.ins().icmp(IntCC::UnsignedGreaterThan, left, right)
        }
        mir::BinaryOperator::Greater => builder.ins().icmp(IntCC::SignedGreaterThan, left, right),
        mir::BinaryOperator::GreaterEqual if unsigned => {
            builder
                .ins()
                .icmp(IntCC::UnsignedGreaterThanOrEqual, left, right)
        }
        mir::BinaryOperator::GreaterEqual => {
            builder
                .ins()
                .icmp(IntCC::SignedGreaterThanOrEqual, left, right)
        }
        mir::BinaryOperator::Equal => builder.ins().icmp(IntCC::Equal, left, right),
        mir::BinaryOperator::NotEqual => builder.ins().icmp(IntCC::NotEqual, left, right),
        mir::BinaryOperator::LogicalAnd => builder.ins().band(left, right),
        mir::BinaryOperator::LogicalOr => builder.ins().bor(left, right),
    })
}

/// Unsigned integer types (plus `char` and `bool`, which never mix signs in
/// comparisons that reach here).
fn is_unsigned_integer(type_: &mir::Type) -> bool {
    primitive(type_)
        .is_some_and(|primitive| primitive.is_unsigned() || primitive == Primitive::Char)
}

/// Bit width of an integer-representable type. `bool` and `char` have integer
/// machine representations even though they are not arithmetic integers.
fn integer_width(type_: &mir::Type) -> Option<u16> {
    let primitive = primitive(type_)?;
    (primitive.is_integer() || matches!(primitive, Primitive::Bool | Primitive::Char))
        .then(|| primitive.bit_width())
        .flatten()
}

fn signed_integer_minimum(type_: &mir::Type) -> Result<i64, BackendError> {
    match integer_width(type_) {
        Some(8) => Ok(i64::from(i8::MIN)),
        Some(16) => Ok(i64::from(i16::MIN)),
        Some(32) => Ok(i64::from(i32::MIN)),
        Some(64) => Ok(i64::MIN),
        _ => Err(BackendError::new(
            "signed integer operation has an unsupported width",
        )),
    }
}

/// Parse an integer constant into its i64 bit representation, checking range.
#[allow(clippy::cast_possible_wrap)]
pub(super) fn integer_constant_bits(text: &str, type_: &mir::Type) -> Result<i64, BackendError> {
    let bits = match type_ {
        mir::Type::SByte => text.parse::<i8>().map(i64::from).ok(),
        mir::Type::Byte => text.parse::<u8>().map(i64::from).ok(),
        mir::Type::Short => text.parse::<i16>().map(i64::from).ok(),
        mir::Type::UShort => text.parse::<u16>().map(i64::from).ok(),
        mir::Type::Int => text.parse::<i32>().map(i64::from).ok(),
        mir::Type::UInt => text.parse::<u32>().map(i64::from).ok(),
        mir::Type::Long => text.parse::<i64>().ok(),
        mir::Type::ULong => text.parse::<u64>().map(|value| value as i64).ok(),
        _ => None,
    };
    bits.ok_or_else(|| {
        BackendError::new(format!(
            "integer `{text}` is outside `{}` range",
            type_name(type_)
        ))
    })
}

/// IEEE-754 arithmetic and comparisons; comparisons are "ordered", so any
/// comparison involving NaN is false except `!=`, which is true.
fn translate_float_binary(
    builder: &mut FunctionBuilder<'_>,
    operator: mir::BinaryOperator,
    left: Value,
    right: Value,
) -> Result<Value, BackendError> {
    Ok(match operator {
        mir::BinaryOperator::Multiply => builder.ins().fmul(left, right),
        mir::BinaryOperator::Divide => builder.ins().fdiv(left, right),
        mir::BinaryOperator::Add => builder.ins().fadd(left, right),
        mir::BinaryOperator::Subtract => builder.ins().fsub(left, right),
        mir::BinaryOperator::Less => builder.ins().fcmp(FloatCC::LessThan, left, right),
        mir::BinaryOperator::LessEqual => builder.ins().fcmp(FloatCC::LessThanOrEqual, left, right),
        mir::BinaryOperator::Greater => builder.ins().fcmp(FloatCC::GreaterThan, left, right),
        mir::BinaryOperator::GreaterEqual => {
            builder.ins().fcmp(FloatCC::GreaterThanOrEqual, left, right)
        }
        mir::BinaryOperator::Equal => builder.ins().fcmp(FloatCC::Equal, left, right),
        mir::BinaryOperator::NotEqual => builder.ins().fcmp(FloatCC::NotEqual, left, right),
        mir::BinaryOperator::Remainder => {
            return Err(BackendError::new(
                "floating-point remainder is not yet supported by the JIT",
            ));
        }
        mir::BinaryOperator::LogicalAnd | mir::BinaryOperator::LogicalOr => {
            return Err(BackendError::new(
                "logical operators require boolean operands",
            ));
        }
    })
}

/// Convert a value between primitive representations. Integer width changes
/// extend by the signedness of the source (two's complement) or truncate;
/// float-to-integer casts saturate at the target range and convert NaN to
/// zero instead of trapping.
pub(super) fn cast_value(
    builder: &mut FunctionBuilder<'_>,
    source: &mir::Type,
    target: &mir::Type,
    value: Value,
) -> Result<Value, BackendError> {
    use mir::Type::{Double, Float};
    let float = |type_: &mir::Type| matches!(type_, Float | Double);
    if source == target {
        return Ok(value);
    }
    Ok(match (integer_width(source), integer_width(target)) {
        (Some(from), Some(to)) => {
            let clif_target = clif_integer(to)?;
            match to.cmp(&from) {
                Ordering::Greater if is_unsigned_integer(source) => {
                    builder.ins().uextend(clif_target, value)
                }
                Ordering::Greater => builder.ins().sextend(clif_target, value),
                Ordering::Less => builder.ins().ireduce(clif_target, value),
                Ordering::Equal => value,
            }
        }
        (Some(_), None) if float(target) => {
            let clif_target = if *target == Double {
                types::F64
            } else {
                types::F32
            };
            if is_unsigned_integer(source) {
                builder.ins().fcvt_from_uint(clif_target, value)
            } else {
                builder.ins().fcvt_from_sint(clif_target, value)
            }
        }
        (None, Some(to)) if float(source) => {
            let clif_target = clif_integer(to)?;
            if is_unsigned_integer(target) {
                builder.ins().fcvt_to_uint_sat(clif_target, value)
            } else {
                builder.ins().fcvt_to_sint_sat(clif_target, value)
            }
        }
        (None, None) if *source == Float && *target == Double => {
            builder.ins().fpromote(types::F64, value)
        }
        (None, None) if *source == Double && *target == Float => {
            builder.ins().fdemote(types::F32, value)
        }
        _ => {
            return Err(BackendError::new(format!(
                "cannot convert `{}` to `{}` in the current JIT",
                type_name(source),
                type_name(target)
            )));
        }
    })
}

fn clif_integer(width: u16) -> Result<ClifType, BackendError> {
    match width {
        8 => Ok(types::I8),
        16 => Ok(types::I16),
        32 => Ok(types::I32),
        64 => Ok(types::I64),
        _ => Err(BackendError::new(format!(
            "unsupported integer width `{width}` reached Cranelift"
        ))),
    }
}
