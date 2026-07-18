use super::{FunctionLowerer, hir, mir};

impl FunctionLowerer {
    #[allow(clippy::too_many_lines)]
    pub(super) fn lower_expression(
        &mut self,
        expression: &hir::Expression,
    ) -> Option<mir::Operand> {
        match &expression.kind {
            hir::ExpressionKind::Literal(literal) => Some(mir::Operand {
                type_: expression.type_.clone(),
                kind: mir::OperandKind::Constant(constant(literal)),
            }),
            hir::ExpressionKind::Symbol(symbol) => {
                Some(self.symbol_operand(*symbol, &expression.type_))
            }
            hir::ExpressionKind::StructLiteral { fields, .. } => {
                Some(self.lower_struct_literal(&expression.type_, fields))
            }
            hir::ExpressionKind::EnumValue {
                case, tag, fields, ..
            } => {
                let fields = fields
                    .iter()
                    .map(|field| mir::FieldOperand {
                        field: field.field,
                        value: self
                            .lower_expression(&field.value)
                            .expect("validated enum payload produces a value"),
                    })
                    .collect();
                Some(self.temporary(
                    expression.type_.clone(),
                    mir::RvalueKind::EnumConstruct {
                        case: *case,
                        tag: *tag,
                        fields,
                    },
                ))
            }
            hir::ExpressionKind::NewObject {
                class_symbol,
                constructor,
                arguments,
            } => Some(self.lower_new_object(
                &expression.type_,
                *class_symbol,
                *constructor,
                arguments,
            )),
            hir::ExpressionKind::ArrayLiteral(elements) => {
                Some(self.lower_array_literal(&expression.type_, elements))
            }
            hir::ExpressionKind::NewArray {
                element_type,
                length,
            } => Some(self.lower_new_array(&expression.type_, element_type, length)),
            hir::ExpressionKind::Index { .. } | hir::ExpressionKind::Member { .. } => {
                Some(self.place_operand(expression))
            }
            hir::ExpressionKind::ArrayLength(array) => {
                let array = self
                    .lower_expression(array)
                    .expect("validated array produces a value");
                Some(self.temporary(
                    expression.type_.clone(),
                    mir::RvalueKind::ArrayLength(array),
                ))
            }
            hir::ExpressionKind::StringLength(value) => {
                let value = self
                    .lower_expression(value)
                    .expect("validated string produces a value");
                let destination = self.new_temporary(hir::Type::Int);
                let place = mir::Place::Local(destination);
                self.instruction(mir::Instruction::CallIntrinsic {
                    destination: Some(place.clone()),
                    intrinsic: mir::Intrinsic::StringLength,
                    arguments: vec![value],
                    return_type: mir::Type::Int,
                });
                Some(mir::Operand {
                    type_: mir::Type::Int,
                    kind: mir::OperandKind::Copy(place),
                })
            }
            hir::ExpressionKind::Call { callee, arguments } => {
                self.lower_call(callee, arguments, &expression.type_)
            }
            hir::ExpressionKind::PropertyAssignment {
                object,
                getter,
                setter,
                operator,
                value,
            } => Some(self.lower_property_assignment(
                object,
                *getter,
                *setter,
                *operator,
                value,
                &expression.type_,
            )),
            hir::ExpressionKind::LogCall { level, argument } => {
                let intrinsic = match level {
                    hir::LogLevel::Log => mir::Intrinsic::Log,
                    hir::LogLevel::Warning => mir::Intrinsic::LogWarning,
                    hir::LogLevel::Error => mir::Intrinsic::LogError,
                };
                let argument = self
                    .lower_expression(argument)
                    .expect("validated log argument produces a value");
                self.instruction(mir::Instruction::CallIntrinsic {
                    destination: None,
                    intrinsic,
                    arguments: vec![argument],
                    return_type: mir::Type::Void,
                });
                None
            }
            hir::ExpressionKind::Convert { operand } => {
                let value = self
                    .lower_expression(operand)
                    .expect("validated conversion operand produces a value");
                Some(self.temporary(expression.type_.clone(), mir::RvalueKind::Cast(value)))
            }
            hir::ExpressionKind::UpcastInterface {
                object,
                class,
                interface,
            } => {
                let object = self
                    .lower_expression(object)
                    .expect("validated interface conversion has an object");
                Some(self.temporary(
                    expression.type_.clone(),
                    mir::RvalueKind::MakeInterface {
                        object,
                        class: *class,
                        interface: *interface,
                    },
                ))
            }
            hir::ExpressionKind::Unary { operator, operand } => {
                let operand = self
                    .lower_expression(operand)
                    .expect("validated unary operand produces a value");
                Some(self.temporary(
                    expression.type_.clone(),
                    mir::RvalueKind::Unary {
                        operator: unary(*operator),
                        operand,
                    },
                ))
            }
            hir::ExpressionKind::IncrementDecrement {
                operator,
                prefix,
                target,
            } => Some(self.lower_increment_decrement(*operator, *prefix, target)),
            hir::ExpressionKind::Conditional {
                condition,
                when_true,
                when_false,
            } => Some(self.lower_conditional(condition, when_true, when_false, &expression.type_)),
            hir::ExpressionKind::PropagateResult {
                operand,
                success_type,
                error_type,
                ok_case,
                ok_field,
                error_case,
                error_field,
                error_tag,
                return_type,
                return_error_case,
                return_error_field,
                return_error_tag,
                ..
            } => Some(self.lower_propagate_result(
                operand,
                success_type,
                error_type,
                *ok_case,
                *ok_field,
                *error_case,
                *error_field,
                *error_tag,
                return_type,
                *return_error_case,
                *return_error_field,
                *return_error_tag,
            )),
            hir::ExpressionKind::Binary {
                left,
                operator,
                right,
            } => {
                if matches!(
                    operator,
                    hir::BinaryOperator::LogicalAnd | hir::BinaryOperator::LogicalOr
                ) {
                    return Some(self.lower_short_circuit(*operator, left, right));
                }
                if left.type_ == hir::Type::String && *operator == hir::BinaryOperator::Add {
                    let left = self
                        .lower_expression(left)
                        .expect("validated string operand produces a value");
                    let right = self
                        .lower_expression(right)
                        .expect("validated string operand produces a value");
                    return Some(self.emit_string_concat(left, right));
                }
                if left.type_ == hir::Type::String
                    && matches!(
                        operator,
                        hir::BinaryOperator::Equal | hir::BinaryOperator::NotEqual
                    )
                {
                    return Some(self.lower_string_equality(*operator, left, right));
                }
                if matches!(
                    left.type_,
                    hir::Type::User(_) | hir::Type::Interface(_) | hir::Type::Enum(_)
                ) && matches!(
                    operator,
                    hir::BinaryOperator::Equal | hir::BinaryOperator::NotEqual
                ) {
                    let left = self
                        .lower_expression(left)
                        .expect("equality operand produces a value");
                    let right = self
                        .lower_expression(right)
                        .expect("equality operand produces a value");
                    return Some(self.temporary(
                        hir::Type::Bool,
                        mir::RvalueKind::Equality {
                            left,
                            right,
                            negated: *operator == hir::BinaryOperator::NotEqual,
                        },
                    ));
                }
                let left = self
                    .lower_expression(left)
                    .expect("validated binary operand produces a value");
                let right = self
                    .lower_expression(right)
                    .expect("validated binary operand produces a value");
                Some(self.temporary(
                    expression.type_.clone(),
                    mir::RvalueKind::Binary {
                        left,
                        operator: binary(*operator),
                        right,
                    },
                ))
            }
            hir::ExpressionKind::Assignment {
                target,
                operator,
                value,
            } => Some(self.lower_assignment(target, *operator, value)),
        }
    }

    fn lower_struct_literal(
        &mut self,
        type_: &hir::Type,
        fields: &[hir::FieldValue],
    ) -> mir::Operand {
        let fields = fields
            .iter()
            .map(|field| mir::FieldOperand {
                field: field.field,
                value: self
                    .lower_expression(&field.value)
                    .expect("validated struct field produces a value"),
            })
            .collect();
        self.temporary(type_.clone(), mir::RvalueKind::Aggregate(fields))
    }

    fn lower_new_object(
        &mut self,
        type_: &hir::Type,
        class: hir::SymbolId,
        constructor: hir::SymbolId,
        arguments: &[hir::Expression],
    ) -> mir::Operand {
        let local = self.new_temporary(type_.clone());
        let place = mir::Place::Local(local);
        self.instruction(mir::Instruction::AllocateObject {
            destination: place.clone(),
            class,
        });
        let receiver = mir::Operand {
            type_: type_.clone(),
            kind: mir::OperandKind::Copy(place.clone()),
        };
        let mut lowered = vec![receiver.clone()];
        lowered.extend(
            arguments
                .iter()
                .filter_map(|argument| self.lower_expression(argument)),
        );
        self.instruction(mir::Instruction::Call {
            destination: None,
            function: constructor,
            arguments: lowered,
            return_type: mir::Type::Void,
        });
        receiver
    }

    fn lower_array_literal(
        &mut self,
        type_: &hir::Type,
        elements: &[hir::Expression],
    ) -> mir::Operand {
        let hir::Type::Array(element_type) = type_ else {
            unreachable!("validated array literal has array type")
        };
        let local = self.new_temporary(type_.clone());
        let array = mir::Operand {
            type_: type_.clone(),
            kind: mir::OperandKind::Copy(mir::Place::Local(local)),
        };
        self.instruction(mir::Instruction::AllocateArray {
            destination: mir::Place::Local(local),
            element_type: (**element_type).clone(),
            length: int_operand(elements.len()),
            requires_default: false,
        });
        for (index, element) in elements.iter().enumerate() {
            let value = self
                .lower_expression(element)
                .expect("validated array element produces a value");
            self.assign(
                mir::Place::Index {
                    array: Box::new(array.clone()),
                    index: Box::new(int_operand(index)),
                    element_type: (**element_type).clone(),
                },
                mir::Rvalue {
                    type_: (**element_type).clone(),
                    kind: mir::RvalueKind::Use(value),
                },
            );
        }
        array
    }

    fn lower_new_array(
        &mut self,
        type_: &hir::Type,
        element_type: &hir::Type,
        length: &hir::Expression,
    ) -> mir::Operand {
        let local = self.new_temporary(type_.clone());
        let length = self
            .lower_expression(length)
            .expect("validated array length produces a value");
        self.instruction(mir::Instruction::AllocateArray {
            destination: mir::Place::Local(local),
            element_type: element_type.clone(),
            length,
            requires_default: true,
        });
        mir::Operand {
            type_: type_.clone(),
            kind: mir::OperandKind::Copy(mir::Place::Local(local)),
        }
    }

    /// `?:` evaluates only the selected branch; both branches assign one result
    /// temporary and join afterwards.
    fn lower_conditional(
        &mut self,
        condition: &hir::Expression,
        when_true: &hir::Expression,
        when_false: &hir::Expression,
        type_: &hir::Type,
    ) -> mir::Operand {
        let condition = self
            .lower_expression(condition)
            .expect("validated condition produces a value");
        let result = self.new_temporary(type_.clone());
        let then_id = self.new_block();
        let else_id = self.new_block();
        let join_id = self.new_block();
        self.terminate_current(mir::Terminator::Branch {
            condition,
            then_block: then_id,
            else_block: else_id,
        });
        for (block, branch) in [(then_id, when_true), (else_id, when_false)] {
            self.current = Some(block);
            let value = self
                .lower_expression(branch)
                .expect("validated `?:` branch produces a value");
            self.assign(
                mir::Place::Local(result),
                mir::Rvalue {
                    type_: type_.clone(),
                    kind: mir::RvalueKind::Use(value),
                },
            );
            self.terminate_current(mir::Terminator::Goto(join_id));
        }
        self.current = Some(join_id);
        mir::Operand {
            type_: type_.clone(),
            kind: mir::OperandKind::Copy(mir::Place::Local(result)),
        }
    }

    /// `&&` and `||` evaluate their right operand only when the left operand
    /// does not decide the result.
    fn lower_short_circuit(
        &mut self,
        operator: hir::BinaryOperator,
        left: &hir::Expression,
        right: &hir::Expression,
    ) -> mir::Operand {
        let result = self.new_temporary(hir::Type::Bool);
        let left = self
            .lower_expression(left)
            .expect("validated logical operand produces a value");
        self.assign(
            mir::Place::Local(result),
            mir::Rvalue {
                type_: hir::Type::Bool,
                kind: mir::RvalueKind::Use(left),
            },
        );
        let rhs_id = self.new_block();
        let join_id = self.new_block();
        let condition = mir::Operand {
            type_: hir::Type::Bool,
            kind: mir::OperandKind::Copy(mir::Place::Local(result)),
        };
        let (then_block, else_block) = if operator == hir::BinaryOperator::LogicalAnd {
            (rhs_id, join_id)
        } else {
            (join_id, rhs_id)
        };
        self.terminate_current(mir::Terminator::Branch {
            condition,
            then_block,
            else_block,
        });
        self.current = Some(rhs_id);
        let right = self
            .lower_expression(right)
            .expect("validated logical operand produces a value");
        self.assign(
            mir::Place::Local(result),
            mir::Rvalue {
                type_: hir::Type::Bool,
                kind: mir::RvalueKind::Use(right),
            },
        );
        self.terminate_current(mir::Terminator::Goto(join_id));
        self.current = Some(join_id);
        mir::Operand {
            type_: hir::Type::Bool,
            kind: mir::OperandKind::Copy(mir::Place::Local(result)),
        }
    }

    /// `string == string` and `string != string` compare by content through
    /// the `aster_rt_string_eq` runtime intrinsic.
    fn lower_string_equality(
        &mut self,
        operator: hir::BinaryOperator,
        left: &hir::Expression,
        right: &hir::Expression,
    ) -> mir::Operand {
        let left = self
            .lower_expression(left)
            .expect("validated string operand produces a value");
        let right = self
            .lower_expression(right)
            .expect("validated string operand produces a value");
        let equals = self.new_temporary(hir::Type::Bool);
        self.instruction(mir::Instruction::CallIntrinsic {
            destination: Some(mir::Place::Local(equals)),
            intrinsic: mir::Intrinsic::StringEquals,
            arguments: vec![left, right],
            return_type: mir::Type::Bool,
        });
        let result = mir::Operand {
            type_: mir::Type::Bool,
            kind: mir::OperandKind::Copy(mir::Place::Local(equals)),
        };
        if operator == hir::BinaryOperator::Equal {
            result
        } else {
            self.temporary(
                hir::Type::Bool,
                mir::RvalueKind::Unary {
                    operator: mir::UnaryOperator::Not,
                    operand: result,
                },
            )
        }
    }

    pub(super) fn emit_string_concat(
        &mut self,
        left: mir::Operand,
        right: mir::Operand,
    ) -> mir::Operand {
        let destination = self.new_temporary(hir::Type::String);
        let place = mir::Place::Local(destination);
        self.instruction(mir::Instruction::CallIntrinsic {
            destination: Some(place.clone()),
            intrinsic: mir::Intrinsic::StringConcat,
            arguments: vec![left, right],
            return_type: mir::Type::String,
        });
        mir::Operand {
            type_: mir::Type::String,
            kind: mir::OperandKind::Copy(place),
        }
    }
}

pub(super) fn lower_intrinsic(intrinsic: hir::Intrinsic) -> mir::Intrinsic {
    match intrinsic {
        hir::Intrinsic::ReportRuntimeError(kind) => {
            mir::Intrinsic::ReportRuntimeError(match kind {
                hir::RuntimeErrorKind::MathAbsIntOverflow => {
                    mir::RuntimeErrorKind::MathAbsIntOverflow
                }
                hir::RuntimeErrorKind::MathAbsLongOverflow => {
                    mir::RuntimeErrorKind::MathAbsLongOverflow
                }
                hir::RuntimeErrorKind::MathClampInvalidRange => {
                    mir::RuntimeErrorKind::MathClampInvalidRange
                }
            })
        }
    }
}

pub(super) fn expression_symbol(expression: &hir::Expression) -> Option<hir::SymbolId> {
    match expression.kind {
        hir::ExpressionKind::Symbol(symbol) | hir::ExpressionKind::Member { symbol, .. } => {
            Some(symbol)
        }
        _ => None,
    }
}

pub(super) fn one_constant(type_: &hir::Type) -> mir::Constant {
    match type_ {
        hir::Type::Float | hir::Type::Double => mir::Constant::Float("1".to_owned()),
        _ => mir::Constant::Integer("1".to_owned()),
    }
}

pub(super) fn boolean_operand(value: bool) -> mir::Operand {
    mir::Operand {
        type_: mir::Type::Bool,
        kind: mir::OperandKind::Constant(mir::Constant::Boolean(value)),
    }
}

fn int_operand(value: usize) -> mir::Operand {
    mir::Operand {
        type_: mir::Type::Int,
        kind: mir::OperandKind::Constant(mir::Constant::Integer(value.to_string())),
    }
}

fn constant(value: &hir::Literal) -> mir::Constant {
    match value {
        hir::Literal::Integer(value) => mir::Constant::Integer(value.clone()),
        hir::Literal::Float(value) => mir::Constant::Float(value.clone()),
        hir::Literal::Decimal(value) => mir::Constant::Decimal(value.clone()),
        hir::Literal::String(value) => mir::Constant::String(value.clone()),
        hir::Literal::Character(value) => mir::Constant::Character(*value),
        hir::Literal::Boolean(value) => mir::Constant::Boolean(*value),
    }
}

pub(super) fn compound_operator(operator: hir::AssignmentOperator) -> mir::BinaryOperator {
    match operator {
        hir::AssignmentOperator::AddAssign => mir::BinaryOperator::Add,
        hir::AssignmentOperator::SubtractAssign => mir::BinaryOperator::Subtract,
        hir::AssignmentOperator::MultiplyAssign => mir::BinaryOperator::Multiply,
        hir::AssignmentOperator::DivideAssign => mir::BinaryOperator::Divide,
        hir::AssignmentOperator::Assign => unreachable!("plain assignment has no binary operator"),
    }
}

macro_rules! convert_operator {
    ($function:ident, $source:ty, $target:ty, [$($variant:ident),+ $(,)?]) => {
        fn $function(value: $source) -> $target {
            match value { $(<$source>::$variant => <$target>::$variant,)+ }
        }
    };
}

convert_operator!(unary, hir::UnaryOperator, mir::UnaryOperator, [Not, Negate]);
convert_operator!(
    binary,
    hir::BinaryOperator,
    mir::BinaryOperator,
    [
        Multiply,
        Divide,
        Remainder,
        Add,
        Subtract,
        Less,
        LessEqual,
        Greater,
        GreaterEqual,
        Equal,
        NotEqual,
        LogicalAnd,
        LogicalOr
    ]
);
